//! Shared state, settings, and one background thread per device. The UI only
//! reads `State` and sends commands; all device I/O happens here.

use crate::{audio, kraken};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// --------------------------------------------------------------- settings

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub close_to_tray: bool,
    pub notify_dpi: bool,
    pub notify_battery: bool,
    pub lcd_image: Option<PathBuf>,
    pub lcd_angle: f32,
    // headsetcontrol can't read these back, so we remember what was set
    pub sidetone: u32,
    pub idle_minutes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Midnight".into(),
            close_to_tray: true,
            notify_dpi: true,
            notify_battery: true,
            lcd_image: None,
            lcd_angle: 0.0,
            sidetone: 0,
            idle_minutes: 10,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("rig-hub").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = settings_path() else { return };
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap());
    }
}

// ------------------------------------------------------------------ state

#[derive(Default)]
pub enum Dev<T> {
    #[default]
    Loading,
    Missing(String),
    Ok(T),
}

impl<T> Dev<T> {
    pub fn ok(&self) -> Option<&T> {
        match self {
            Dev::Ok(t) => Some(t),
            _ => None,
        }
    }
}

pub struct Cooler {
    pub name: &'static str,
    pub brightness: u8,
    pub status: Option<kraken::Status>,
}

pub struct Mic {
    pub card: String,
    pub monitor: Option<audio::Level>, // 'Mic' playback = hear yourself
    pub gain: Option<audio::Level>,    // 'Mic' capture, switch = not muted
    pub speaker: Option<audio::Level>, // headphone jack volume
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

#[derive(Default)]
pub struct State {
    pub settings: Settings,
    pub cooler: Dev<Cooler>,
    pub cpu_c: Option<f32>,
    #[cfg(target_os = "linux")]
    pub mouse: Dev<crate::mouse::Mouse>,
    /// live DPI stage from the mouse itself (HID++), if readable
    pub dpi_stage: Option<u8>,
    pub headset: Dev<audio::Headset>,
    /// everything else with a battery, from UPower
    pub batteries: Vec<audio::PowerDevice>,
    pub mic: Dev<Mic>,
    pub busy: Option<&'static str>,
    pub toast: Option<Toast>,
}

pub type Shared = Arc<Mutex<State>>;

/// Repaints whichever window is open; on Linux closing to the tray destroys the window and reopening makes a new one.
#[derive(Clone, Default)]
pub struct Context(Arc<Mutex<egui::Context>>);

impl Context {
    pub fn set(&self, ctx: &egui::Context) {
        *self.0.lock().unwrap() = ctx.clone();
    }

    pub fn request_repaint(&self) {
        self.0.lock().unwrap().request_repaint();
    }
}

fn update(st: &Shared, ctx: &Context, f: impl FnOnce(&mut State)) {
    f(&mut st.lock().unwrap());
    ctx.request_repaint();
}

fn toast(st: &Shared, ctx: &Context, text: impl Into<String>, error: bool) {
    update(st, ctx, |s| s.toast = Some(Toast { text: text.into(), error, at: Instant::now() }));
}

/// Report the result of a user action as a toast.
fn report(st: &Shared, ctx: &Context, r: anyhow::Result<()>, ok: &str) {
    match r {
        Ok(()) => toast(st, ctx, ok, false),
        Err(e) => toast(st, ctx, format!("{e:#}"), true),
    }
}

pub fn notify(summary: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("Rig Hub")
        .icon("rig-hub")
        .summary(summary)
        .body(body)
        .show();
}

// --------------------------------------------------------------- commands

pub enum CoolerCmd {
    Brightness(u8),
    Liquid,
    Show(PathBuf, f32),
}

#[cfg(target_os = "linux")]
pub enum MouseCmd {
    Action(zbus::OwnedObjectPath, crate::mouse::Action),
    Dpi(crate::mouse::Resolution, u32),
    Activate(zbus::OwnedObjectPath, &'static str),
}

/// (new, previous): the previous value goes back into settings if the headset rejects the new one.
pub enum HeadsetCmd {
    Sidetone(u32, u32),
    Idle(u32, u32),
}

pub struct MicCmd {
    pub control: &'static str,
    pub direction: &'static str,
    pub value: String,
}

#[derive(Clone)]
pub struct Workers {
    pub cooler: Sender<CoolerCmd>,
    #[cfg(target_os = "linux")]
    pub mouse: Sender<MouseCmd>,
    pub headset: Sender<HeadsetCmd>,
    pub mic: Sender<MicCmd>,
}

pub fn spawn(st: Shared, ctx: Context) -> Workers {
    fn go<C: Send + 'static>(name: &str, st: &Shared, ctx: &Context, f: fn(Shared, Context, Receiver<C>)) -> Sender<C> {
        let (tx, rx) = channel();
        let (st, ctx) = (st.clone(), ctx.clone());
        std::thread::Builder::new().name(name.into()).spawn(move || f(st, ctx, rx)).unwrap();
        tx
    }
    Workers {
        cooler: go("cooler", &st, &ctx, cooler),
        #[cfg(target_os = "linux")]
        mouse: go("mouse", &st, &ctx, mouse),
        headset: go("headset", &st, &ctx, headset),
        mic: go("mic", &st, &ctx, mic),
    }
}

// ----------------------------------------------------------------- cooler

fn cooler(st: Shared, ctx: Context, rx: Receiver<CoolerCmd>) {
    let mut api = hidapi::HidApi::new().ok();
    let mut kraken: Option<kraken::Kraken> = None;
    loop {
        if kraken.is_none() {
            if let Some(api) = api.as_mut() {
                let _ = api.refresh_devices();
                kraken = kraken::Kraken::open(api).ok();
            }
            let dev = match &kraken {
                Some(k) => Dev::Ok(Cooler {
                    name: k.name,
                    brightness: k.lcd_info().map(|i| i.0).unwrap_or(100),
                    status: None,
                }),
                None => Dev::Missing("No NZXT Kraken Z / Elite / Plus found.".into()),
            };
            update(&st, &ctx, |s| s.cooler = dev);
        }
        #[cfg(target_os = "linux")]
        {
            let status = kraken::status().ok();
            let cpu = crate::cpu_temp();
            update(&st, &ctx, |s| {
                s.cpu_c = cpu;
                if let Dev::Ok(c) = &mut s.cooler {
                    c.status = status;
                }
            });
        }
        let cmd = match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let Some(k) = &kraken else { continue };
        let r = match cmd {
            CoolerCmd::Brightness(pct) => k.set_brightness(pct).map(|()| {
                update(&st, &ctx, |s| {
                    if let Dev::Ok(c) = &mut s.cooler {
                        c.brightness = pct;
                    }
                });
                "Brightness set"
            }),
            CoolerCmd::Liquid => k.show_liquid().map(|()| "Showing liquid temperature"),
            CoolerCmd::Show(path, angle) => {
                update(&st, &ctx, |s| s.busy = Some("Uploading to the LCD…"));
                let gif = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
                let r = if gif { k.show_gif(&path, angle) } else { k.show_image(&path, angle) };
                update(&st, &ctx, |s| s.busy = None);
                r.map(|()| "LCD updated")
            }
        };
        match r {
            Ok(msg) => toast(&st, &ctx, msg, false),
            Err(e) => {
                toast(&st, &ctx, format!("Cooler: {e:#}"), true);
                kraken = None; // reopen next round in case it was unplugged
            }
        }
    }
}

// ------------------------------------------------------------------ mouse

#[cfg(target_os = "linux")]
fn mouse(st: Shared, ctx: Context, rx: Receiver<MouseCmd>) {
    use crate::mouse::{HidppDpi, Ratbag};

    let missing = |msg: String| update(&st, &ctx, |s| {
        s.mouse = Dev::Missing(msg);
        s.dpi_stage = None; // the next mouse may not report one
    });
    let rb = loop {
        match Ratbag::connect() {
            Ok(rb) => break rb,
            Err(e) => missing(format!("Can't reach the system D-Bus: {e}")),
        }
        std::thread::sleep(Duration::from_secs(10));
    };
    let mut api = hidapi::HidApi::new().ok();
    let mut hidpp: Option<HidppDpi> = None;
    let mut last: Option<(u16, u8)> = None; // (profile, stage) as the mouse reports it
    let mut popup: Option<notify_rust::NotificationHandle> = None;
    let mut reload = true;
    // look again now and then while missing (ratbagd started later, mouse plugged in or back from an unplug)
    // or without HID++, whose failing reads are otherwise the only sign of an unplug
    let mut retry_at: Option<Instant> = None;

    loop {
        if reload || retry_at.is_some_and(|t| Instant::now() >= t) {
            reload = false;
            retry_at = None;
            match rb.load() {
                Ok(m) => {
                    if hidpp.is_none() {
                        // the device list is a snapshot: a mouse plugged in since isn't on it
                        hidpp = api.as_mut().and_then(|api| {
                            let _ = api.refresh_devices();
                            HidppDpi::open(api, &m.model).ok()
                        });
                    }
                    if hidpp.is_none() {
                        retry_at = Some(Instant::now() + Duration::from_secs(30));
                    }
                    update(&st, &ctx, |s| s.mouse = Dev::Ok(m));
                }
                Err(e) => {
                    missing(format!("{e:#}"));
                    hidpp = None; // ratbagd may pick a different mouse next time
                    last = None;
                    retry_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }

        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(cmd) => {
                let device = match st.lock().unwrap().mouse.ok() {
                    Some(m) => m.path.clone(),
                    None => continue,
                };
                update(&st, &ctx, |s| s.busy = Some("Saving to the mouse…"));
                let r = match cmd {
                    MouseCmd::Action(button, action) => rb.set_action(&button, &action),
                    MouseCmd::Dpi(res, dpi) => rb.set_dpi(&res, dpi),
                    MouseCmd::Activate(path, iface) => rb.set_active(&path, iface),
                    };
                let r = r.and_then(|()| rb.commit(&device));
                update(&st, &ctx, |s| s.busy = None);
                report(&st, &ctx, r, "Saved to the mouse");
                reload = true;
                last = None;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        // live DPI tracking: catches the mouse's own DPI buttons
        let Some(h) = &hidpp else { continue };
        let now = match (h.profile(), h.stage()) {
            (Ok(p), Ok(s)) => (p, s),
            _ => {
                hidpp = None; // unplugged or asleep; reopen on next reload
                reload = true;
                continue;
            }
        };
        if last == Some(now) {
            continue;
        }
        let profile_changed = last.is_some_and(|l| l.0 != now.0);
        let stage_changed = last.is_some_and(|l| l.1 != now.1);
        last = Some(now);
        update(&st, &ctx, |s| s.dpi_stage = Some(now.1));
        if profile_changed {
            reload = true; // ratbagd doesn't follow the mouse's profile button either
            continue;
        }
        if !stage_changed || !st.lock().unwrap().settings.notify_dpi {
            continue;
        }
        let dpi = st.lock().unwrap().mouse.ok().and_then(|m| {
            let res = &m.active()?.resolutions;
            let n = res.iter().position(|r| r.index == now.1 as u32)?;
            Some(format!("{} DPI  ·  stage {} of {}", res[n].dpi, n + 1, res.len()))
        });
        let body = dpi.unwrap_or_else(|| format!("Stage {}", now.1 + 1));
        // reuse one notification so fast clicking doesn't stack a pile of them
        match &mut popup {
            Some(n) => {
                n.body(&body);
                let _ = n.update();
            }
            None => {
                popup = notify_rust::Notification::new()
                    .appname("Rig Hub")
                    .icon("input-mouse")
                    .summary("Mouse DPI")
                    .body(&body)
                    .hint(notify_rust::Hint::Transient(true))
                    .timeout(1500)
                    .show()
                    .ok();
            }
        }
    }
}

// ---------------------------------------------------------------- headset

fn headset(st: Shared, ctx: Context, rx: Receiver<HeadsetCmd>) {
    let mut warned = false;
    loop {
        let dev = match audio::headset() {
            Ok(Some(h)) => Dev::Ok(h),
            Ok(None) => Dev::Missing("No headset found by headsetcontrol.".into()),
            Err(_) => Dev::Missing("Needs HeadsetControl (github.com/Sapd/HeadsetControl).".into()),
        };
        if let Dev::Ok(h) = &dev {
            match h.battery {
                audio::Battery::Level(p) if p <= 15 && !warned => {
                    warned = true;
                    if st.lock().unwrap().settings.notify_battery {
                        notify("Headset battery low", &format!("{} is at {p}%", h.name));
                    }
                }
                audio::Battery::Level(p) if p > 20 => warned = false,
                audio::Battery::Charging(_) => warned = false,
                _ => {}
            }
        }
        let batteries = audio::batteries();
        update(&st, &ctx, |s| {
            s.headset = dev;
            s.batteries = batteries;
        });

        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(HeadsetCmd::Sidetone(v, prev)) => set_or_revert(&st, &ctx, "-s", v, prev, |s| &mut s.sidetone, "Sidetone set"),
            Ok(HeadsetCmd::Idle(v, prev)) => set_or_revert(&st, &ctx, "-i", v, prev, |s| &mut s.idle_minutes, "Auto-off set"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The slider already saved `v`; put `prev` back if the headset didn't take it (unless it moved on since).
fn set_or_revert(st: &Shared, ctx: &Context, flag: &str, v: u32, prev: u32, field: fn(&mut Settings) -> &mut u32, ok: &str) {
    let r = audio::headset_set(flag, v);
    if r.is_err() {
        update(st, ctx, |s| {
            let f = field(&mut s.settings);
            if *f == v {
                *f = prev;
            }
        });
    }
    report(st, ctx, r, ok);
}

// -------------------------------------------------------------------- mic

fn mic(st: Shared, ctx: Context, rx: Receiver<MicCmd>) {
    loop {
        let dev = match audio::find_card("Yeti").or_else(|| audio::find_card("Blue Microphones")) {
            Some(card) => {
                let (monitor, gain) = audio::mixer_get(&card, "Mic");
                let (speaker, _) = audio::mixer_get(&card, "Speaker");
                Dev::Ok(Mic { card, monitor, gain, speaker })
            }
            None => Dev::Missing("No Blue Yeti found.".into()),
        };
        let card = dev.ok().map(|m| m.card.clone());
        update(&st, &ctx, |s| s.mic = dev);

        // short poll: the mute button on the mic flips the capture switch
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(cmd) => {
                if let Some(card) = card
                    && let Err(e) = audio::mixer_set(&card, cmd.control, cmd.direction, &cmd.value)
                {
                    toast(&st, &ctx, format!("Mic: {e:#}"), true);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
