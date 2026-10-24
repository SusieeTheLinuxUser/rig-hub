//! NZXT Kraken (Z / 2024 family): status and LCD.
//!
//! The LCD protocol is ported from liquidctl's `kraken3` driver
//! (GPL-3.0-or-later, © liquidctl contributors). Differences from liquidctl:
//! - replies are matched on their prefix (command + 1) instead of taking the
//!   next report, because the kernel driver keeps status reports streaming;
//! - rotation is done here in software, in any angle (the Elite V2 ignores
//!   liquidctl's 90° orientation command).

use anyhow::{Context, Result, bail, ensure};
use hidapi::{HidApi, HidDevice};
use image::{DynamicImage, RgbaImage, imageops::FilterType};
use nusb::MaybeFuture;
use nusb::transfer::{Bulk, Out};
use std::path::Path;
use std::time::Duration;

const VID: u16 = 0x1E71;
const REPORT: usize = 64;
const LCD_TOTAL_MEMORY: usize = 24320; // in 1 KiB pages
const MAX_READ_ATTEMPTS: usize = 12;

/// (product id, name, lcd side in px, bulk chunk size)
const MODELS: &[(u16, &str, u32, usize)] = &[
    (0x3008, "NZXT Kraken Z", 320, 512),
    (0x3012, "NZXT Kraken Elite (2024)", 640, 2 * 1024 * 1024),
    (0x3014, "NZXT Kraken Plus (2024)", 240, 2 * 1024 * 1024),
];

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub liquid_c: f32,
    pub pump_rpm: u32,
    pub pump_pct: f32,
    pub fan_rpm: u32,
    pub fan_pct: f32,
}

pub struct Kraken {
    pub name: &'static str,
    side: u32,
    chunk: usize,
    hid: HidDevice,
    pid: u16,
}

impl Kraken {
    pub fn open(api: &HidApi) -> Result<Self> {
        for info in api.device_list() {
            if info.vendor_id() != VID {
                continue;
            }
            if let Some(&(pid, name, side, chunk)) =
                MODELS.iter().find(|m| m.0 == info.product_id())
            {
                let hid = info.open_device(api).context("open Kraken HID")?;
                return Ok(Self { name, side, chunk, hid, pid });
            }
        }
        bail!("no supported NZXT Kraken found")
    }

    fn write(&self, data: &[u8]) -> Result<()> {
        let mut buf = [0u8; REPORT];
        buf[..data.len()].copy_from_slice(data);
        self.hid.write(&buf)?;
        Ok(())
    }

    fn clear(&self) {
        let mut buf = [0u8; REPORT];
        while matches!(self.hid.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
    }

    fn read_until(&self, prefix: [u8; 2]) -> Result<[u8; REPORT]> {
        let mut buf = [0u8; REPORT];
        for _ in 0..MAX_READ_ATTEMPTS {
            let n = self.hid.read_timeout(&mut buf, 1000)?;
            if n >= 2 && buf[..2] == prefix {
                return Ok(buf);
            }
        }
        bail!("Kraken didn't answer {:02x} {:02x}", prefix[0], prefix[1])
    }

    /// Send a command and wait for its reply (reply prefix = command + 1).
    fn transact(&self, data: &[u8]) -> Result<[u8; REPORT]> {
        self.clear();
        self.write(data)?;
        self.read_until([data[0] + 1, data[1]])
    }

    /// (brightness %, orientation 0..3)
    pub fn lcd_info(&self) -> Result<(u8, u8)> {
        let msg = self.transact(&[0x30, 0x01])?;
        Ok((msg[0x18], msg[0x1A]))
    }

    pub fn set_brightness(&self, pct: u8) -> Result<()> {
        let (_, orientation) = self.lcd_info()?;
        self.write(&[0x30, 0x02, 0x01, pct.min(100), 0, 0, 0x01, orientation])
    }

    /// Show the cooler's built-in liquid temperature screen.
    pub fn show_liquid(&self) -> Result<()> {
        self.switch_bucket(0, 0x02)?;
        Ok(())
    }

    /// Show a still image, rotated clockwise by `angle` degrees.
    pub fn show_image(&self, path: &Path, angle: f32) -> Result<()> {
        let img = image::open(path).with_context(|| format!("open {}", path.display()))?;
        let frame = self.prepare(img.to_rgba8(), angle);
        // liquidctl's static format: R, G, B, 0 per pixel
        let data: Vec<u8> = frame.pixels().flat_map(|p| [p[0], p[1], p[2], 0]).collect();
        self.send(&data, 0x02)
    }

    /// Show an animated GIF, every frame rotated clockwise by `angle` degrees.
    pub fn show_gif(&self, path: &Path, angle: f32) -> Result<()> {
        use image::AnimationDecoder;
        use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
        let file = std::io::BufReader::new(std::fs::File::open(path)?);
        let frames = GifDecoder::new(file)?.into_frames().collect_frames()?;
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new_with_speed(&mut out, 10);
            enc.set_repeat(Repeat::Infinite)?;
            for f in frames {
                let delay = f.delay();
                let img = self.prepare(f.into_buffer(), angle);
                enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay))?;
            }
        }
        ensure!(
            out.len() / 1000 < LCD_TOTAL_MEMORY,
            "GIF is {} MB after resizing; the LCD holds 24 MB",
            out.len() / 1_000_000
        );
        self.send(&out, 0x01)
    }

    fn prepare(&self, img: RgbaImage, angle: f32) -> RgbaImage {
        let img = DynamicImage::ImageRgba8(img)
            .resize_to_fill(self.side, self.side, FilterType::CatmullRom)
            .to_rgba8();
        let angle = angle.rem_euclid(360.0);
        if angle == 0.0 {
            return img;
        }
        imageproc::geometric_transformations::rotate_about_center(
            &img,
            angle.to_radians(),
            imageproc::geometric_transformations::Interpolation::Bilinear,
            imageproc::geometric_transformations::Border::Constant(image::Rgba([0, 0, 0, 255])),
        )
    }

    // ---- bucket memory management (liquidctl `_send_data` and friends) ----

    fn send(&self, data: &[u8], kind: u8) -> Result<()> {
        let mut bulk = self.open_bulk()?;
        self.transact(&[0x36, 0x03])?; // unknown, liquidctl sends it first

        let buckets: Vec<[u8; REPORT]> = (0..16u8)
            .map(|i| self.transact(&[0x30, 0x04, i]))
            .collect::<Result<_>>()?;
        let free = buckets.iter().position(|b| b[15..].iter().all(|&x| x == 0));
        let mut index = self.prepare_bucket(free.unwrap_or(0) as u8, free.is_none())?;

        let mut header = vec![0x12, 0xFA, 0x01, 0xE8, 0xAB, 0xCD, 0xEF, 0x98, 0x76, 0x54, 0x32, 0x10];
        header.extend([kind, 0, 0, 0]);
        header.extend((data.len() as u32).to_le_bytes());

        let pages = (header.len() + data.len()).div_ceil(1024);
        let start = match memory_offset(&buckets, index as usize, pages) {
            Some(start) => start,
            None => {
                self.delete_all_buckets()?;
                index = 0;
                [0, 0]
            }
        };
        let size = (pages as u16).to_le_bytes();
        let ok = self.transact(&[0x32, 0x01, index, index + 1, start[0], start[1], size[0], size[1], 0x01])?;
        ensure!(ok[14] == 1, "Kraken refused the LCD memory setup");

        self.transact(&[0x36, 0x01, index])?; // start transfer
        bulk_write(&mut bulk, header)?;
        for chunk in data.chunks(self.chunk) {
            bulk_write(&mut bulk, chunk.to_vec())?;
        }
        self.write(&[0x36, 0x02])?; // end transfer
        ensure!(self.switch_bucket(index, 0x04)?, "Kraken didn't switch to the new image");
        Ok(())
    }

    fn open_bulk(&self) -> Result<nusb::Endpoint<Bulk, Out>> {
        let info = nusb::list_devices()
            .wait()?
            .find(|d| d.vendor_id() == VID && d.product_id() == self.pid)
            .context("Kraken USB device not found")?;
        let iface = info.open().wait()?.claim_interface(0).wait()?;
        Ok(iface.endpoint::<Bulk, Out>(0x02)?)
    }

    fn prepare_bucket(&self, index: u8, filled: bool) -> Result<u8> {
        ensure!(index < 16, "no free LCD memory slot");
        if !self.delete_bucket(index)? {
            return self.prepare_bucket(index + 1, true);
        }
        if filled {
            return self.prepare_bucket(index, false);
        }
        Ok(index)
    }

    fn delete_bucket(&self, index: u8) -> Result<bool> {
        Ok(self.transact(&[0x32, 0x02, index])?[14] == 1)
    }

    fn delete_all_buckets(&self) -> Result<()> {
        self.switch_bucket(0, 0x02)?;
        for i in 0..16 {
            self.delete_bucket(i)?;
        }
        Ok(())
    }

    fn switch_bucket(&self, index: u8, mode: u8) -> Result<bool> {
        Ok(self.transact(&[0x38, 0x01, mode, index])?[14] == 1)
    }
}

fn bulk_write(ep: &mut nusb::Endpoint<Bulk, Out>, data: Vec<u8>) -> Result<()> {
    ep.submit(data.into());
    ep.wait_next_complete(Duration::from_secs(5))
        .context("LCD upload timed out")?
        .status?;
    Ok(())
}

/// Where to put `pages` of data for bucket `index`; None = memory full, wipe and restart.
/// Straight port of liquidctl's `_get_bucket_memory_offset`.
fn memory_offset(buckets: &[[u8; REPORT]], index: usize, pages: usize) -> Option<[u8; 2]> {
    let cur = &buckets[index];
    let field = |b: &[u8; REPORT], at: usize| u16::from_le_bytes([b[at], b[at + 1]]) as usize;
    let (cur_off, cur_size) = (field(cur, 17), field(cur, 19));
    if pages <= cur_size {
        return Some([cur[17], cur[18]]);
    }
    let (mut min_used, mut max_used, mut overlap) = (cur_off, 0, false);
    for (i, b) in buckets.iter().enumerate() {
        let start = field(b, 17);
        let end = start + field(b, 19);
        max_used = max_used.max(end);
        min_used = min_used.min(start);
        if (start > cur_off && start < cur_off + pages)
            || (start < cur_off && end > start)
            || (start == cur_off && i != index)
        {
            overlap = true;
        }
    }
    if !overlap {
        return Some([cur[17], cur[18]]);
    }
    if max_used + pages < LCD_TOTAL_MEMORY {
        return Some((max_used as u16).to_le_bytes());
    }
    if pages < min_used {
        return Some([0, 0]);
    }
    None
}

/// Liquid/pump/fan from the nzxt-kraken3 kernel driver (no USB traffic).
#[cfg(target_os = "linux")]
pub fn status() -> Result<Status> {
    let dir = crate::hwmon("kraken").context("nzxt-kraken3 kernel driver not loaded")?;
    let read = |f: &str| -> f32 {
        std::fs::read_to_string(dir.join(f)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0)
    };
    Ok(Status {
        liquid_c: read("temp1_input") / 1000.0,
        pump_rpm: read("fan1_input") as u32,
        pump_pct: read("pwm1") * 100.0 / 255.0,
        fan_rpm: read("fan2_input") as u32,
        fan_pct: read("pwm2") * 100.0 / 255.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(off: u16, size: u16) -> [u8; REPORT] {
        let mut b = [0u8; REPORT];
        b[17..19].copy_from_slice(&off.to_le_bytes());
        b[19..21].copy_from_slice(&size.to_le_bytes());
        b
    }

    #[test]
    fn offsets_match_liquidctl() {
        let empty = vec![[0u8; REPORT]; 16];
        // empty memory: write at 0
        assert_eq!(memory_offset(&empty, 0, 1600), Some([0, 0]));
        // fits in the bucket's current space: reuse it
        let mut b = empty.clone();
        b[2] = bucket(5000, 2000);
        assert_eq!(memory_offset(&b, 2, 1600), Some(5000u16.to_le_bytes()));
        // too big and another bucket sits right after: append after max used
        b[3] = bucket(6000, 1000);
        assert_eq!(memory_offset(&b, 2, 3000), Some(7000u16.to_le_bytes()));
        // memory full: None (caller wipes all buckets)
        b[4] = bucket(7000, 17000);
        assert_eq!(memory_offset(&b, 2, 3000), None);
    }
}
