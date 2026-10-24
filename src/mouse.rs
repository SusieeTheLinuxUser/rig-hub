//! Gaming mouse: profiles, DPI stages and buttons via ratbagd (libratbag's
//! D-Bus daemon), plus live DPI-stage tracking for Logitech HID++ mice.

use anyhow::{Context, Result, bail};
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};
use zbus::{OwnedObjectPath, OwnedVariant as OwnedValue, Variant as Value};
use zbus::{Connection, Proxy, proxy::CacheProperties};

const BUS: &str = "org.freedesktop.ratbag1";

// ratbag action types
pub const NONE: u32 = 0;
pub const BUTTON: u32 = 1;
pub const SPECIAL: u32 = 2;
pub const KEY: u32 = 3;
pub const MACRO: u32 = 4;

/// ratbag "special" actions: value = (1 << 30) + n
pub const SPECIALS: &[&str] = &[
    "Unknown", "Double click", "Wheel left", "Wheel right", "Wheel up", "Wheel down",
    "Ratchet mode switch", "DPI cycle up", "DPI cycle down", "DPI up", "DPI down",
    "DPI shift (sniper)", "Default DPI", "Profile cycle up", "Profile cycle down",
    "Profile up", "Profile down", "Second mode", "Battery level",
];
const SPECIAL_BASE: u32 = 1 << 30;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Button(u32),
    Special(u32),
    Key(u32),
    Macro,
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Action::None => "Disabled".into(),
            Action::Button(1) => "Left click".into(),
            Action::Button(2) => "Right click".into(),
            Action::Button(3) => "Middle click".into(),
            Action::Button(4) => "Back".into(),
            Action::Button(5) => "Forward".into(),
            Action::Button(n) => format!("Mouse button {n}"),
            Action::Special(v) => SPECIALS
                .get(v.wrapping_sub(SPECIAL_BASE) as usize)
                .copied()
                .unwrap_or("Special")
                .into(),
            Action::Key(code) => format!("Key: {}", crate::keys::name(*code)),
            Action::Macro => "Macro".into(),
        }
    }

    fn to_dbus(&self) -> (u32, Value<'static>) {
        match *self {
            Action::None => (NONE, Value::U32(0)),
            Action::Button(n) => (BUTTON, Value::U32(n)),
            Action::Special(v) => (SPECIAL, Value::U32(v)),
            Action::Key(k) => (KEY, Value::U32(k)),
            Action::Macro => unreachable!("macros are read-only here"),
        }
    }

    /// Every action a button with these action types can be set to.
    pub fn choices(types: &[u32]) -> Vec<Action> {
        let mut v = Vec::new();
        if types.contains(&BUTTON) {
            v.extend((1..=11).map(Action::Button));
        }
        if types.contains(&SPECIAL) {
            v.extend((1..SPECIALS.len() as u32 - 1).map(|n| Action::Special(SPECIAL_BASE + n)));
        }
        if types.contains(&NONE) {
            v.push(Action::None);
        }
        v
    }
}

#[derive(Clone, Debug)]
pub struct Resolution {
    pub path: OwnedObjectPath,
    pub index: u32,
    pub dpi: u32,
    pub pair: bool, // device stores (x, y) instead of a single value
    pub allowed: Vec<u32>,
    pub active: bool,
    pub default: bool,
}

#[derive(Clone, Debug)]
pub struct Button {
    pub path: OwnedObjectPath,
    pub index: u32,
    pub action: Action,
    pub types: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub path: OwnedObjectPath,
    pub index: u32,
    pub active: bool,
    pub resolutions: Vec<Resolution>,
    pub buttons: Vec<Button>,
}

#[derive(Clone, Debug)]
pub struct Mouse {
    pub path: OwnedObjectPath,
    pub name: String,
    pub model: String,
    pub profiles: Vec<Profile>,
}

impl Mouse {
    pub fn active(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.active)
    }
}

async fn proxy<'a>(c: &Connection, path: &'a OwnedObjectPath, iface: &'a str) -> Result<Proxy<'a>> {
    Ok(zbus::proxy::Builder::new(c)
        .destination(BUS)
        .path(path.as_ref())
        .interface(format!("{BUS}.{iface}"))
        .cache_properties(CacheProperties::No)
        .build()
        .await?)
}

async fn get<T>(p: &Proxy<'_>, name: &str) -> Result<T>
where
    T: TryFrom<OwnedValue>,
    T::Error: Into<zbus::Error>,
{
    let v: OwnedValue = p.get_property(name).await?;
    Ok(T::try_from(v).map_err(Into::into)?)
}

fn dpi_of(v: &Value<'_>) -> Option<(u32, bool)> {
    match v {
        Value::U32(d) => Some((*d, false)),
        Value::Structure(s) => match s.fields() {
            [Value::U32(x), _] => Some((*x, true)),
            _ => None,
        },
        Value::Variant(inner) => dpi_of(inner),
        _ => None,
    }
}

fn action_of(kind: u32, v: &Value<'_>) -> Action {
    let n = match v {
        Value::U32(n) => *n,
        Value::Variant(inner) => return action_of(kind, inner),
        _ => 0,
    };
    match kind {
        BUTTON => Action::Button(n),
        SPECIAL => Action::Special(n),
        KEY => Action::Key(n),
        MACRO => Action::Macro,
        _ => Action::None,
    }
}

/// Talks to ratbagd. All calls block; run them on a worker thread.
pub struct Ratbag {
    conn: Connection,
}

impl Ratbag {
    pub fn connect() -> Result<Self> {
        let conn = zbus::block_on(Connection::system())?;
        Ok(Self { conn })
    }

    pub fn load(&self) -> Result<Mouse> {
        zbus::block_on(self.load_async())
    }

    async fn load_async(&self) -> Result<Mouse> {
        let c = &self.conn;
        let root: OwnedObjectPath = "/org/freedesktop/ratbag1".try_into()?;
        let devices: Vec<OwnedObjectPath> = get(&proxy(c, &root, "Manager").await?, "Devices").await?;
        let path = devices.into_iter().next().context("ratbagd sees no mouse")?;
        let dev = proxy(c, &path, "Device").await?;
        let mut mouse = Mouse {
            name: get(&dev, "Name").await?,
            model: get(&dev, "Model").await?,
            profiles: Vec::new(),
            path: path.clone(),
        };
        let profiles: Vec<OwnedObjectPath> = get(&dev, "Profiles").await?;
        for pp in profiles {
            let p = proxy(c, &pp, "Profile").await?;
            let active: bool = get(&p, "IsActive").await?;
            // the G502 reports its active onboard profile as "disabled"; keep it
            if !active && get::<bool>(&p, "Disabled").await.unwrap_or(false) {
                continue;
            }
            let mut profile = Profile {
                index: get(&p, "Index").await?,
                active,
                resolutions: Vec::new(),
                buttons: Vec::new(),
                path: pp.clone(),
            };
            let res: Vec<OwnedObjectPath> = get(&p, "Resolutions").await?;
            for rp in res {
                let r = proxy(c, &rp, "Resolution").await?;
                if get::<bool>(&r, "IsDisabled").await.unwrap_or(false) {
                    continue;
                }
                let v: OwnedValue = r.get_property("Resolution").await?;
                let (dpi, pair) = dpi_of(&v).unwrap_or((0, false));
                profile.resolutions.push(Resolution {
                    index: get(&r, "Index").await?,
                    dpi,
                    pair,
                    allowed: get(&r, "Resolutions").await?,
                    active: get(&r, "IsActive").await?,
                    default: get(&r, "IsDefault").await?,
                    path: rp.clone(),
                });
            }
            let buttons: Vec<OwnedObjectPath> = get(&p, "Buttons").await?;
            for bp in buttons {
                let b = proxy(c, &bp, "Button").await?;
                let (kind, value): (u32, OwnedValue) = b.get_property("Mapping").await?;
                profile.buttons.push(Button {
                    index: get(&b, "Index").await?,
                    action: action_of(kind, &value),
                    types: get(&b, "ActionTypes").await?,
                    path: bp.clone(),
                });
            }
            mouse.profiles.push(profile);
        }
        Ok(mouse)
    }

    pub fn set_action(&self, button: &OwnedObjectPath, action: &Action) -> Result<()> {
        zbus::block_on(async {
            let p = proxy(&self.conn, button, "Button").await?;
            p.set_property("Mapping", action.to_dbus()).await?;
            Ok(())
        })
    }

    pub fn set_dpi(&self, res: &Resolution, dpi: u32) -> Result<()> {
        zbus::block_on(async {
            let p = proxy(&self.conn, &res.path, "Resolution").await?;
            let v = if res.pair {
                Value::from((dpi, dpi))
            } else {
                Value::U32(dpi)
            };
            p.set_property("Resolution", v).await?;
            Ok(())
        })
    }

    /// `iface` is "Profile" or "Resolution"; both have SetActive().
    pub fn set_active(&self, path: &OwnedObjectPath, iface: &str) -> Result<()> {
        zbus::block_on(async {
            proxy(&self.conn, path, iface).await?.call::<_, _, u32>("SetActive", &()).await?;
            Ok(())
        })
    }

    pub fn commit(&self, device: &OwnedObjectPath) -> Result<()> {
        zbus::block_on(async {
            proxy(&self.conn, device, "Device").await?.call::<_, _, u32>("Commit", &()).await?;
            Ok(())
        })
    }
}

/// Reads the active DPI stage straight from a Logitech HID++ 2.0 mouse
/// (feature 0x8100 Onboard Profiles, getCurrentDpiIndex). ratbagd doesn't
/// notice when the stage is changed with the mouse's own DPI buttons.
pub struct HidppDpi {
    dev: HidDevice,
    feature: u8,
}

const SWID: u8 = 0x0A;

impl HidppDpi {
    /// `model` is ratbagd's model string, e.g. "usb:046d:c08b:0".
    pub fn open(api: &HidApi, model: &str) -> Result<Self> {
        let mut parts = model.split(':').skip(1);
        let vid = u16::from_str_radix(parts.next().unwrap_or(""), 16)?;
        let pid = u16::from_str_radix(parts.next().unwrap_or(""), 16)?;
        if vid != 0x046D {
            bail!("not a Logitech mouse");
        }
        for info in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
            let Ok(dev) = info.open_device(api) else { continue };
            let mut me = Self { dev, feature: 0 };
            // root feature (index 0) getFeature(0x8100)
            if let Ok(r) = me.request(0x00, 0, &[0x81, 0x00]) {
                if r[4] != 0 {
                    me.feature = r[4];
                    return Ok(me);
                }
            }
        }
        bail!("mouse has no HID++ onboard-profiles feature")
    }

    fn request(&self, feature: u8, func: u8, params: &[u8]) -> Result<[u8; 20]> {
        let mut msg = [0u8; 7];
        msg[..4].copy_from_slice(&[0x10, 0xFF, feature, (func << 4) | SWID]);
        msg[4..4 + params.len()].copy_from_slice(params);
        self.dev.write(&msg)?;
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut buf = [0u8; 20];
        while Instant::now() < deadline {
            let n = self.dev.read_timeout(&mut buf, 100)?;
            // only HID++ replies to us; any other report on this interface is dropped
            if n >= 5 && matches!(buf[0], 0x10 | 0x11) && buf[1] == 0xFF && buf[3] & 0x0F == SWID {
                if buf[2] == 0x8F || buf[2] == 0xFF {
                    bail!("HID++ error {:#04x}", buf[5]);
                }
                if buf[2] == feature {
                    return Ok(buf);
                }
            }
        }
        bail!("no HID++ reply")
    }

    /// 0-based index of the active DPI stage.
    pub fn stage(&self) -> Result<u8> {
        Ok(self.request(self.feature, 0x0B, &[])?[4])
    }
}

impl HidppDpi {
    /// 0-based index of the active onboard profile.
    pub fn profile(&self) -> Result<u16> {
        let r = self.request(self.feature, 0x04, &[])?;
        Ok(u16::from_be_bytes([r[4], r[5]]))
    }
}
