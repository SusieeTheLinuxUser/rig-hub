//! Rig Hub: one app for your mouse, cooler LCD, headset and mic.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod audio;
mod diagram;
mod keys;
mod kraken;
#[cfg(target_os = "linux")]
mod mouse;
mod theme;
mod workers;

use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Frame, Pos2, Rect, RichText, Sense, Slider, Stroke,
    TextureHandle, Ui, Vec2, vec2,
};
use resvg::{tiny_skia, usvg};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use theme::{Palette, bold};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use workers::{CoolerCmd, Dev, HeadsetCmd, MicCmd, Settings, Shared, State, Workers};

const REPO: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ICON_SVG: &[u8] = include_bytes!("../assets/rig-hub.svg");

// ------------------------------------------------------------- platform bits

#[cfg(target_os = "linux")]
pub fn hwmon(prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/hwmon").ok()?.flatten().map(|e| e.path()).find(|p| {
        std::fs::read_to_string(p.join("name")).is_ok_and(|n| n.trim().starts_with(prefix))
    })
}

#[cfg(target_os = "linux")]
pub fn cpu_temp() -> Option<f32> {
    let dir = hwmon("k10temp").or_else(|| hwmon("coretemp"))?;
    let milli: f32 = std::fs::read_to_string(dir.join("temp1_input")).ok()?.trim().parse().ok()?;
    Some(milli / 1000.0)
}

/// App icon as straight (non-premultiplied) RGBA.
fn icon_rgba(size: u32) -> Vec<u8> {
    let tree = usvg::Tree::from_data(ICON_SVG, &usvg::Options::default()).expect("bundled icon");
    let mut pm = tiny_skia::Pixmap::new(size, size).unwrap();
    let s = size as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// Use the desktop's own sans font when fontconfig can tell us which one it is.
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut defaults = fonts.families[&FontFamily::Proportional].clone();
    let find = |pattern: &str| -> Option<Vec<u8>> {
        let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
        std::fs::read(String::from_utf8(out.stdout).ok()?).ok()
    };
    if let Some(regular) = find("sans-serif") {
        fonts.font_data.insert("sys".into(), Arc::new(egui::FontData::from_owned(regular)));
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "sys".into());
    }
    if let Some(b) = find("sans-serif:weight=bold") {
        fonts.font_data.insert("sys-bold".into(), Arc::new(egui::FontData::from_owned(b)));
        defaults.insert(0, "sys-bold".into());
    }
    fonts.families.insert(FontFamily::Name("bold".into()), defaults);
    ctx.set_fonts(fonts);
}

/// One window only: a second launch pokes the running app to show itself and exits.
#[cfg(unix)]
fn single_instance() -> Result<Option<std::os::unix::net::UnixListener>, ()> {
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("rig-hub.sock");
    if UnixStream::connect(&path).is_ok() {
        return Err(());
    }
    let _ = std::fs::remove_file(&path); // stale socket left by a crash
    Ok(UnixListener::bind(&path).ok())
}

fn open_url(url: &str) {
    let _ = webbrowser::open(url);
}

fn bug_report_url(st: &State) -> String {
    fn line<T>(d: &Dev<T>, name: impl Fn(&T) -> String) -> String {
        match d {
            Dev::Ok(t) => name(t),
            Dev::Missing(why) => format!("not found ({why})"),
            Dev::Loading => "loading".into(),
        }
    }
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    #[cfg(target_os = "linux")]
    let mouse = line(&st.mouse, |m| format!("{} ({})", m.name, m.model));
    #[cfg(not(target_os = "linux"))]
    let mouse = "n/a".to_string();
    let body = format!(
        "**What happened?**\n\n\n**What did you expect?**\n\n\n**Steps to reproduce**\n1. \n\n\
         ---\n<details><summary>System (filled in by Rig Hub)</summary>\n\n\
         - Rig Hub {VERSION}\n- OS: {} {}\n- Kernel: {}\n- Desktop: {desktop} ({session})\n\
         - Cooler: {}\n- Mouse: {mouse}\n- Headset: {}\n- Mic: {}\n\n</details>\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        kernel.trim(),
        line(&st.cooler, |c| c.name.to_string()),
        line(&st.headset, |h| h.name.clone()),
        line(&st.mic, |m| format!("ALSA card {}", m.card)),
    );
    format!("{REPO}/issues/new?labels=bug&title={}&body={}", urlencoding::encode("Bug: "), urlencoding::encode(&body))
}

// --------------------------------------------------------------------- tray

enum TrayMsg {
    Show,
    Menu(MenuId),
}

struct Tray {
    _icon: tray_icon::TrayIcon,
    headset: MenuItem,
    liquid: MenuItem,
    dpi: MenuItem,
    show: MenuItem,
    bug: MenuItem,
    quit: MenuItem,
    last: String,
}

impl Tray {
    fn new(ctx: &egui::Context, tx: Sender<TrayMsg>) -> Option<Self> {
        let info = |t: &str| MenuItem::new(t, false, None);
        let (headset, liquid, dpi) = (info("Headset: …"), info("Liquid: …"), info("Mouse: …"));
        let show = MenuItem::new("Open Rig Hub", true, None);
        let bug = MenuItem::new("Report a bug…", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&headset, &liquid, &dpi, &PredefinedMenuItem::separator(), &show, &bug, &quit]).ok()?;
        let icon = tray_icon::Icon::from_rgba(icon_rgba(64), 64, 64).ok()?;
        let tray = tray_icon::TrayIconBuilder::new()
            .with_id("rig-hub")
            .with_title("Rig Hub")
            .with_tooltip("Rig Hub")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()?;
        let (c, t) = (ctx.clone(), tx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = t.send(TrayMsg::Menu(e.id));
            c.request_repaint();
        }));
        let c = ctx.clone();
        tray_icon::TrayIconEvent::set_event_handler(Some(move |e| {
            if let tray_icon::TrayIconEvent::Click { button: tray_icon::MouseButton::Left, .. } = e {
                let _ = tx.send(TrayMsg::Show);
                c.request_repaint();
            }
        }));
        Some(Self { _icon: tray, headset, liquid, dpi, show, bug, quit, last: String::new() })
    }

    fn refresh(&mut self, st: &State) {
        let headset = match st.headset.ok().map(|h| &h.battery) {
            Some(audio::Battery::Level(p)) => format!("🎧 Headset  {p}%"),
            Some(audio::Battery::Charging(Some(p))) => format!("🎧 Headset  {p}% ⚡"),
            Some(audio::Battery::Charging(None)) => "🎧 Headset  charging ⚡".into(),
            _ => "🎧 Headset  off".into(),
        };
        let liquid = match st.cooler.ok().and_then(|c| c.status.as_ref()) {
            Some(s) => format!("💧 Liquid  {:.1} °C", s.liquid_c),
            None => "💧 Liquid  –".into(),
        };
        let dpi = match current_dpi(st) {
            Some(d) => format!("🖱 Mouse  {d} DPI"),
            None => "🖱 Mouse  –".into(),
        };
        let all = format!("{headset}\n{liquid}\n{dpi}");
        if all == self.last {
            return;
        }
        self.headset.set_text(&headset);
        self.liquid.set_text(&liquid);
        self.dpi.set_text(&dpi);
        let _ = self._icon.set_tooltip(Some(&all));
        self.last = all;
    }
}

#[cfg(target_os = "linux")]
fn current_dpi(st: &State) -> Option<u32> {
    let res = &st.mouse.ok()?.active()?.resolutions;
    match st.dpi_stage {
        Some(i) => res.iter().find(|r| r.index == i as u32),
        None => res.iter().find(|r| r.active),
    }
    .map(|r| r.dpi)
}

#[cfg(not(target_os = "linux"))]
fn current_dpi(_: &State) -> Option<u32> {
    None
}

// ---------------------------------------------------------------- widgets

fn card<R>(ui: &mut Ui, p: &Palette, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(14)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title.to_uppercase()).font(bold(12.0)).color(p.sub));
                ui.add_space(2.0);
            }
            add(ui)
        })
        .inner
}

fn tile(ui: &mut Ui, p: &Palette, label: &str, value: &str, sub: &str, color: Color32) {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(14)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(label.to_uppercase()).font(bold(11.5)).color(p.sub));
            ui.label(RichText::new(value).font(bold(28.0)).color(color));
            ui.label(RichText::new(sub).small().color(p.sub));
        });
}

fn header(ui: &mut Ui, p: &Palette, title: &str, sub: &str) {
    ui.label(RichText::new(title).font(bold(28.0)));
    if !sub.is_empty() {
        ui.label(RichText::new(sub).color(p.sub));
    }
    ui.add_space(6.0);
}

fn missing(ui: &mut Ui, p: &Palette, what: &str, why: &str, help: &[&str]) {
    card(ui, p, "", |ui| {
        ui.label(RichText::new(what).font(bold(17.0)));
        ui.label(RichText::new(why).color(p.sub));
        for h in help {
            ui.add_space(4.0);
            ui.label(RichText::new(*h).monospace().color(p.text));
        }
    });
}

fn loading(ui: &mut Ui, p: &Palette) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new("Looking for the device…").color(p.sub));
    });
}

/// iOS-style switch.
fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool, label: &str, hint: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let size = vec2(42.0, 24.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            *on = !*on;
            changed = true;
        }
        let t = ui.ctx().animate_bool(ui.scope_id().with(label), *on);
        let fill = if *on { p.accent } else { p.surface2 };
        ui.painter().rect(rect, 12.0, fill, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        let x = egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, t);
        ui.painter().circle_filled(Pos2::new(x, rect.center().y), 9.0, Color32::WHITE);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(label);
            if !hint.is_empty() {
                ui.label(RichText::new(hint).small().color(p.sub));
            }
        });
    });
    changed
}

/// Slider that only reports a value when the user lets go, so devices
/// aren't spammed with every intermediate value while dragging.
fn live_slider(ui: &mut Ui, id: &str, current: u32, range: std::ops::RangeInclusive<u32>, fmt: impl Fn(u32) -> String) -> Option<u32> {
    let id = ui.scope_id().with(id);
    let mut v = ui.data(|d| d.get_temp::<u32>(id)).unwrap_or(current);
    let r = ui.add(Slider::new(&mut v, range).custom_formatter(|n, _| fmt(n as u32)));
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        ui.data_mut(|d| d.remove::<u32>(id));
        return Some(v);
    }
    if r.dragged() {
        ui.data_mut(|d| d.insert_temp(id, v));
    }
    None
}

fn pill(ui: &mut Ui, text: &str, fill: Color32, fg: Color32) {
    Frame::new().fill(fill).corner_radius(20).inner_margin(egui::Margin::symmetric(12, 5)).show(ui, |ui| {
        ui.label(RichText::new(text).font(bold(13.5)).color(fg));
    });
}

// --------------------------------------------------------------------- app

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Cooler,
    Mouse,
    Headset,
    Mic,
    Settings,
}

struct App {
    st: Shared,
    w: Workers,
    page: Page,
    saved: Settings,
    theme: String,
    logo: TextureHandle,
    tray: Option<Tray>,
    tray_rx: Receiver<TrayMsg>,
    diagram: Option<(String, Option<diagram::Diagram>)>, // (model, artwork)
    selected: Option<usize>,
    capture: bool,
    preview: Option<(PathBuf, f32, Option<TextureHandle>)>,
    draft_angle: Option<f32>,
    pick: bool,
    screenshot: Option<(PathBuf, Instant)>,
}

impl App {
    fn new(cc: &eframe::CreationContext, page: Page, screenshot: Option<PathBuf>, #[cfg(unix)] instance: Option<std::os::unix::net::UnixListener>) -> Self {
        let ctx = &cc.egui_ctx;
        setup_fonts(ctx);
        let settings = Settings::load();
        theme::apply(ctx, &theme::by_name(&settings.theme));
        let st = Arc::new(Mutex::new(State { settings: settings.clone(), ..Default::default() }));
        let w = workers::spawn(st.clone(), ctx.clone());
        let (tx, tray_rx) = channel();
        #[cfg(unix)]
        if let Some(l) = instance {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                for _ in l.incoming() {
                    let _ = tx.send(TrayMsg::Show);
                    ctx.request_repaint();
                }
            });
        }
        let logo = ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([96, 96], &icon_rgba(96)),
            egui::TextureOptions::LINEAR,
        );
        Self {
            st,
            w,
            page,
            theme: settings.theme.clone(),
            saved: settings,
            logo,
            tray: Tray::new(ctx, tx),
            tray_rx,
            diagram: None,
            selected: None,
            capture: false,
            preview: None,
            draft_angle: None,
            pick: false,
            screenshot: screenshot.map(|p| (p, Instant::now())),
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        while let Ok(msg) = self.tray_rx.try_recv() {
            let t = self.tray.as_ref();
            let id = match msg {
                TrayMsg::Show => None,
                TrayMsg::Menu(id) => Some(id),
            };
            if id.is_none() || id.as_ref() == t.map(|t| t.show.id()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if id.as_ref() == t.map(|t| t.bug.id()) {
                open_url(&bug_report_url(&self.st.lock().unwrap()));
            } else if id.as_ref() == t.map(|t| t.quit.id()) {
                std::process::exit(0);
            }
        }

        let st = self.st.lock().unwrap();
        if let Some(t) = &mut self.tray {
            t.refresh(&st);
        }
        let close_to_tray = st.settings.close_to_tray && self.tray.is_some();
        drop(st);
        if ctx.input(|i| i.viewport().close_requested()) && close_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        if let Some((path, at)) = &self.screenshot {
            if at.elapsed() > Duration::from_secs(4) {
                let path = path.clone();
                self.screenshot = None;
                ctx.request_screenshot(move |img| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                    let _ = image::save_buffer(&path, &rgba, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgba8);
                    std::process::exit(0);
                });
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        if std::mem::take(&mut self.pick) {
            self.pick_lcd_image();
        }
        let ctx = ui.ctx().clone();
        let st = self.st.clone();
        let mut st = st.lock().unwrap();
        if st.settings.theme != self.theme {
            self.theme = st.settings.theme.clone();
            theme::apply(&ctx, &theme::by_name(&self.theme));
        }
        let p = theme::by_name(&self.theme);

        egui::Panel::left("nav")
            .exact_size(224.0)
            .resizable(false)
            .frame(Frame::new().fill(p.surface).inner_margin(14).stroke(Stroke::new(1.0, p.border)))
            .show(ui, |ui| self.nav(ui, &p, &st));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 28, right: 28, top: 24, bottom: 16 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.page {
                    Page::Cooler => self.cooler(ui, &p, &mut st),
                    #[cfg(target_os = "linux")]
                    Page::Mouse => self.mouse(ui, &p, &mut st),
                    #[cfg(not(target_os = "linux"))]
                    Page::Mouse => missing(ui, &p, "Linux only for now", "Mouse support uses libratbag, which is Linux-only.", &[]),
                    Page::Headset => self.headset(ui, &p, &mut st),
                    Page::Mic => self.mic(ui, &p, &mut st),
                    Page::Settings => self.settings(ui, &p, &mut st),
                });
            });

        toasts(&ctx, &p, &mut st);

        if st.settings != self.saved {
            st.settings.save();
            self.saved = st.settings.clone();
        }
    }
}

fn toasts(ctx: &egui::Context, p: &Palette, st: &mut State) {
    if st.toast.as_ref().is_some_and(|t| t.at.elapsed() > Duration::from_secs(if t.error { 8 } else { 3 })) {
        st.toast = None;
    }
    if st.busy.is_none() && st.toast.is_none() {
        return;
    }
    egui::Area::new(egui::Id::unique("toast")).anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0)).show(ctx, |ui| {
        Frame::new()
            .fill(p.surface2)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(90) })
            .show(ui, |ui| {
                ui.set_max_width(420.0);
                if let Some(b) = st.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(b);
                    });
                } else if let Some(t) = &st.toast {
                    let (icon, c) = if t.error { ("⚠", p.bad) } else { ("✔", p.good) };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon).color(c));
                        ui.label(&t.text);
                    });
                }
            });
    });
    ctx.request_repaint_after(Duration::from_millis(500));
}

impl App {
    fn nav(&mut self, ui: &mut Ui, p: &Palette, st: &State) {
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(vec2(36.0, 36.0)));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("Rig Hub").font(bold(19.0)));
                ui.label(RichText::new(format!("alpha {}", VERSION.split("-alpha").next().unwrap_or(VERSION))).small().color(p.sub));
            });
        });
        ui.add_space(18.0);

        fn dot<T>(d: &Dev<T>, p: &Palette) -> Color32 {
            match d {
                Dev::Ok(_) => p.good,
                Dev::Missing(_) => p.sub.gamma_multiply(0.5),
                Dev::Loading => p.warn,
            }
        }
        #[cfg(target_os = "linux")]
        let mouse_dot = dot(&st.mouse, p);
        #[cfg(not(target_os = "linux"))]
        let mouse_dot = p.sub.gamma_multiply(0.5);
        let items = [
            (Page::Cooler, "❄", "Cooler", Some(dot(&st.cooler, p))),
            (Page::Mouse, "🖱", "Mouse", Some(mouse_dot)),
            (Page::Headset, "🎧", "Headset", Some(dot(&st.headset, p))),
            (Page::Mic, "🎤", "Microphone", Some(dot(&st.mic, p))),
            (Page::Settings, "⚙", "Settings", None),
        ];
        for (page, icon, label, status) in items {
            if nav_item(ui, p, icon, label, self.page == page, status) {
                self.page = page;
                self.capture = false;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            if ui.add(egui::Button::new("🐞  Report a bug").min_size(vec2(ui.available_width(), 34.0))).clicked() {
                open_url(&bug_report_url(st));
            }
            if let Some(h) = st.headset.ok() {
                let text = match h.battery {
                    audio::Battery::Level(n) => format!("🎧 {n}%"),
                    audio::Battery::Charging(_) => "🎧 charging".into(),
                    audio::Battery::Off => "🎧 off".into(),
                };
                ui.label(RichText::new(text).small().color(p.sub));
            }
        });
    }

    // ------------------------------------------------------------ cooler

    fn cooler(&mut self, ui: &mut Ui, p: &Palette, st: &mut State) {
        let c = match &mut st.cooler {
            Dev::Ok(c) => c,
            Dev::Loading => return loading(ui, p),
            Dev::Missing(why) => {
                header(ui, p, "Cooler", "");
                return missing(ui, p, "No cooler found", why, &[]);
            }
        };
        header(ui, p, "Cooler", c.name);

        let s = c.status.clone();
        let temp_color = |t: f32, warm: f32, hot: f32| if t >= hot { p.bad } else if t >= warm { p.warn } else { p.text };
        ui.columns(4, |cols| {
            match &s {
                Some(s) => {
                    tile(&mut cols[0], p, "Liquid", &format!("{:.1}°", s.liquid_c), "coolant temperature", temp_color(s.liquid_c, 40.0, 50.0));
                    tile(&mut cols[2], p, "Pump", &format!("{}", s.pump_rpm), &format!("rpm · {:.0}% duty", s.pump_pct), p.text);
                    tile(&mut cols[3], p, "Fans", &format!("{}", s.fan_rpm), &format!("rpm · {:.0}% duty", s.fan_pct), p.text);
                }
                None => {
                    for i in [0, 2, 3] {
                        tile(&mut cols[i], p, ["Liquid", "", "Pump", "Fans"][i], "–", "kernel driver not loaded", p.sub);
                    }
                }
            }
            match st.cpu_c {
                Some(t) => tile(&mut cols[1], p, "CPU", &format!("{t:.0}°"), "package temperature", temp_color(t, 75.0, 90.0)),
                None => tile(&mut cols[1], p, "CPU", "–", "no sensor", p.sub),
            }
        });
        ui.add_space(4.0);

        let Dev::Ok(c) = &mut st.cooler else { return };
        let settings = &mut st.settings;
        card(ui, p, "LCD screen", |ui| {
            ui.horizontal_top(|ui| {
                let side = 220.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
                self.lcd_preview(ui, p, rect, settings, s.as_ref().map(|s| s.liquid_c));
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Show").color(p.sub));
                    ui.horizontal(|ui| {
                        if ui.selectable_label(settings.lcd_image.is_none(), "💧 Liquid temperature").clicked() {
                            settings.lcd_image = None;
                            let _ = self.w.cooler.send(CoolerCmd::Liquid);
                        }
                        let label = match &settings.lcd_image {
                            Some(path) => format!("🖼 {}", path.file_name().unwrap_or_default().to_string_lossy()),
                            None => "🖼 Image or GIF…".into(),
                        };
                        if ui.selectable_label(settings.lcd_image.is_some(), label).clicked() {
                            self.pick = true;
                        }
                        if settings.lcd_image.is_some() && ui.button("Change…").clicked() {
                            self.pick = true;
                        }
                    });
                    ui.add_space(8.0);

                    ui.label(RichText::new("Brightness").color(p.sub));
                    if let Some(v) = live_slider(ui, "brightness", c.brightness as u32, 0..=100, |v| format!("{v}%")) {
                        c.brightness = v as u8;
                        let _ = self.w.cooler.send(CoolerCmd::Brightness(v as u8));
                    }
                    ui.add_space(8.0);

                    ui.label(RichText::new("Rotation").color(p.sub));
                    ui.add_enabled_ui(settings.lcd_image.is_some(), |ui| {
                        let id = ui.scope_id().with("angle");
                        let mut a = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(settings.lcd_angle);
                        let mut commit = None;
                        ui.horizontal(|ui| {
                            if ui.button("⟲ 90°").clicked() {
                                commit = Some(a - 90.0);
                            }
                            if ui.button("⟳ 90°").clicked() {
                                commit = Some(a + 90.0);
                            }
                            let r = ui.add(Slider::new(&mut a, -180.0..=180.0).step_by(15.0).suffix("°"));
                            if r.dragged() {
                                ui.data_mut(|d| d.insert_temp(id, a));
                            }
                            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                                commit = Some(a);
                            }
                            // the preview follows the slider; the upload waits for release
                            self.draft_angle = r.dragged().then_some(a);
                        });
                        if let Some(a) = commit {
                            ui.data_mut(|d| d.remove::<f32>(id));
                            settings.lcd_angle = (a + 180.0).rem_euclid(360.0) - 180.0;
                            if let Some(path) = &settings.lcd_image {
                                let _ = self.w.cooler.send(CoolerCmd::Show(path.clone(), settings.lcd_angle));
                            }
                        }
                    });
                    if settings.lcd_image.is_none() {
                        ui.label(RichText::new("Rotation applies to images and GIFs.").small().color(p.sub));
                    }
                });
            });
        });
    }

    fn pick_lcd_image(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Pick an image or GIF for the cooler")
            .add_filter("Images and GIFs", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
            .pick_file()
        else {
            return;
        };
        let mut st = self.st.lock().unwrap();
        let angle = st.settings.lcd_angle;
        st.settings.lcd_image = Some(path.clone());
        let _ = self.w.cooler.send(CoolerCmd::Show(path, angle));
    }

    /// Round preview of what the LCD will show.
    fn lcd_preview(&mut self, ui: &mut Ui, p: &Palette, rect: Rect, settings: &Settings, liquid: Option<f32>) {
        let painter = ui.painter();
        let (c, r) = (rect.center(), rect.width() / 2.0);
        painter.circle_filled(c, r, Color32::BLACK);
        match &settings.lcd_image {
            None => {
                let t = liquid.map_or("–".into(), |t| format!("{t:.0}°"));
                painter.text(c, Align2::CENTER_CENTER, t, bold(64.0), Color32::WHITE);
                painter.text(c + vec2(0.0, 48.0), Align2::CENTER_CENTER, "LIQUID", bold(13.0), Color32::from_gray(150));
            }
            Some(path) => {
                let angle = self.draft_angle.unwrap_or(settings.lcd_angle);
                if self.preview.as_ref().is_none_or(|(pp, a, _)| pp != path || *a != angle) {
                    let tex = preview_image(path, angle).map(|img| ui.ctx().load_texture("lcd", img, egui::TextureOptions::LINEAR));
                    self.preview = Some((path.clone(), angle, tex));
                }
                match &self.preview.as_ref().unwrap().2 {
                    Some(tex) => {
                        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        // mask the square corners down to the round screen
                        painter.circle_stroke(c, r * 1.21, Stroke::new(r * 0.42, p.surface));
                    }
                    None => {
                        painter.text(c, Align2::CENTER_CENTER, "can't open file", FontId::proportional(14.0), p.bad);
                    }
                }
            }
        }
        painter.circle_stroke(c, r, Stroke::new(3.0, p.border));
    }
}

fn preview_image(path: &Path, angle: f32) -> Option<egui::ColorImage> {
    let img = image::open(path).ok()?;
    let img = img.resize_to_fill(220, 220, image::imageops::FilterType::Triangle).to_rgba8();
    let img = imageproc::geometric_transformations::rotate_about_center(
        &img,
        angle.to_radians(),
        imageproc::geometric_transformations::Interpolation::Bilinear,
        imageproc::geometric_transformations::Border::Constant(image::Rgba([0, 0, 0, 255])),
    );
    Some(egui::ColorImage::from_rgba_unmultiplied([220, 220], img.as_raw()))
}

fn nav_item(ui: &mut Ui, p: &Palette, icon: &str, label: &str, selected: bool, status: Option<Color32>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let painter = ui.painter();
    let fill = if selected {
        p.accent.gamma_multiply(0.16)
    } else if resp.hovered() {
        p.surface2
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, 10.0, fill);
    if selected {
        painter.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 10.0), vec2(3.0, 20.0)), 2.0, p.accent);
    }
    let color = if selected { p.text } else { p.sub };
    painter.text(rect.left_center() + vec2(16.0, 0.0), Align2::LEFT_CENTER, icon, FontId::proportional(17.0), if selected { p.accent } else { p.sub });
    painter.text(rect.left_center() + vec2(46.0, 0.0), Align2::LEFT_CENTER, label, if selected { bold(15.0) } else { FontId::proportional(15.0) }, color);
    if let Some(c) = status {
        painter.circle_filled(rect.right_center() - vec2(16.0, 0.0), 4.0, c);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

// ---------------------------------------------------------------- mouse

/// What the button physically is, so people don't have to guess what "button 6" means.
#[cfg(target_os = "linux")]
fn button_name(model: &str, i: u32) -> String {
    const COMMON: &[&str] = &["Left click", "Right click", "Wheel click", "Back (thumb)", "Forward (thumb)"];
    const G502: &[&str] = &["Sniper (thumb)", "DPI down", "DPI up", "Below the wheel", "Wheel tilt right", "Wheel tilt left"];
    if let Some(n) = COMMON.get(i as usize) {
        return n.to_string();
    }
    if model.starts_with("usb:046d:c08b") {
        if let Some(n) = G502.get(i as usize - COMMON.len()) {
            return n.to_string();
        }
    }
    format!("Extra button {}", i - 4)
}

#[cfg(target_os = "linux")]
impl App {
    fn mouse(&mut self, ui: &mut Ui, p: &Palette, st: &mut State) {
        use mouse::{Action, KEY, MACRO};
        use workers::MouseCmd;

        let m = match &mut st.mouse {
            Dev::Ok(m) => m,
            Dev::Loading => return loading(ui, p),
            Dev::Missing(why) => {
                header(ui, p, "Mouse", "");
                return missing(ui, p, "No mouse found", why, &[
                    "Rig Hub talks to mice through libratbag. Install it and start the daemon:",
                    "  sudo pacman -S libratbag      # Arch / CachyOS",
                    "  sudo apt install ratbagd      # Debian / Ubuntu",
                    "  sudo systemctl enable --now ratbagd",
                ]);
            }
        };
        header(ui, p, "Mouse", &m.name);

        if self.diagram.as_ref().is_none_or(|d| d.0 != m.model) {
            let art = diagram::piper_svg(&m.model).and_then(|svg| diagram::Diagram::new(&svg));
            self.diagram = Some((m.model.clone(), art));
        }

        // profile picker + live DPI
        let stage = st.dpi_stage;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Profile").color(p.sub));
            for prof in &m.profiles {
                if ui.selectable_label(prof.active, format!("  {}  ", prof.index + 1)).clicked() && !prof.active {
                    let _ = self.w.mouse.send(MouseCmd::Activate(prof.path.clone(), "Profile"));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dpi = m.active().and_then(|prof| {
                    let res = &prof.resolutions;
                    match stage {
                        Some(i) => res.iter().find(|r| r.index == i as u32),
                        None => res.iter().find(|r| r.active),
                    }
                });
                if let Some(r) = dpi {
                    pill(ui, &format!("{} DPI", r.dpi), p.accent, theme::on(p.accent));
                    ui.label(RichText::new("now").color(p.sub));
                }
            });
        });
        ui.add_space(4.0);

        let model = m.model.clone();
        let Some(prof) = m.profiles.iter_mut().find(|p| p.active) else {
            return missing(ui, p, "No active profile", "ratbagd reports no active profile.", &[]);
        };

        card(ui, p, "Buttons", |ui| {
            ui.label(RichText::new("Click a button on the mouse or its label to change what it does.").color(p.sub));
            ui.add_space(6.0);
            let art = self.diagram.as_mut().and_then(|d| d.1.as_mut());
            match art {
                Some(art) => {
                    let labels: Vec<(usize, String)> = prof.buttons.iter().map(|b| (b.index as usize, b.action.label())).collect();
                    ui.vertical_centered(|ui| {
                        if let Some(i) = art.show(ui, &labels, self.selected, p.accent, p.text, p.sub) {
                            self.selected = Some(i);
                            self.capture = false;
                        }
                    });
                }
                None => {
                    // no artwork (Piper not installed): plain list
                    egui::Grid::new("buttons").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                        for b in &prof.buttons {
                            let sel = self.selected == Some(b.index as usize);
                            if ui.selectable_label(sel, button_name(&model, b.index)).clicked() {
                                self.selected = Some(b.index as usize);
                                self.capture = false;
                            }
                            ui.label(RichText::new(b.action.label()).color(p.sub));
                            ui.end_row();
                        }
                    });
                    ui.label(RichText::new("Install Piper to get a picture of your mouse here.").small().color(p.sub));
                }
            }
        });

        if let Some(b) = self.selected.and_then(|i| prof.buttons.iter_mut().find(|b| b.index as usize == i)) {
            let mut set = None;
            card(ui, p, &format!("{} · button {}", button_name(&model, b.index), b.index + 1), |ui| {
                if b.action == Action::Macro || b.types == [MACRO] {
                    ui.label("This button runs a macro. Macros can be edited in Piper for now.");
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Does").color(p.sub));
                    egui::ComboBox::from_id_salt("action").width(240.0).selected_text(b.action.label()).show_ui(ui, |ui| {
                        for a in Action::choices(&b.types) {
                            if ui.selectable_label(a == b.action, a.label()).clicked() {
                                set = Some(a);
                            }
                        }
                    });
                });
                if !b.types.contains(&KEY) {
                    return;
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Or types").color(p.sub));
                    if self.capture {
                        pill(ui, "Press a key…", p.accent, theme::on(p.accent));
                        let pressed = ui.input(|i| {
                            i.events.iter().find_map(|e| match e {
                                egui::Event::Key { key, pressed: true, .. } => Some(*key),
                                _ => None,
                            })
                        });
                        if let Some(code) = pressed.and_then(keys::from_egui) {
                            set = Some(Action::Key(code));
                            self.capture = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.capture = false;
                        }
                    } else if ui.button("⌨ Record a key").clicked() {
                        self.capture = true;
                    }
                    egui::ComboBox::from_id_salt("special-keys").selected_text("Media & modifier keys").show_ui(ui, |ui| {
                        for (code, name) in keys::uncapturable() {
                            if ui.selectable_label(b.action == Action::Key(code), name).clicked() {
                                set = Some(Action::Key(code));
                            }
                        }
                    });
                });
            });
            if let Some(a) = set {
                let _ = self.w.mouse.send(MouseCmd::Action(b.path.clone(), a.clone()));
                b.action = a; // optimistic; the worker reloads after saving
            }
        }

        card(ui, p, "DPI stages", |ui| {
            ui.label(RichText::new("Your mouse's DPI buttons step through these.").color(p.sub));
            ui.add_space(4.0);
            let n = prof.resolutions.len();
            for (i, r) in prof.resolutions.iter_mut().enumerate() {
                let active = stage.map_or(r.active, |s| s as u32 == r.index);
                ui.horizontal(|ui| {
                    let (fill, fg) = if active { (p.accent, theme::on(p.accent)) } else { (p.surface2, p.sub) };
                    pill(ui, &format!("{}", i + 1), fill, fg);
                    let new = if r.allowed.len() > 1 {
                        let at = r.allowed.iter().position(|&d| d >= r.dpi).unwrap_or(0) as u32;
                        let allowed = r.allowed.clone();
                        live_slider(ui, &format!("dpi{i}"), at, 0..=allowed.len() as u32 - 1, move |n| format!("{} DPI", allowed[n as usize]))
                            .map(|n| r.allowed[n as usize])
                    } else {
                        ui.label(format!("{} DPI", r.dpi));
                        None
                    };
                    if let Some(dpi) = new.filter(|&d| d != r.dpi) {
                        let _ = self.w.mouse.send(MouseCmd::Dpi(r.clone(), dpi));
                        r.dpi = dpi;
                    }
                    if r.default {
                        ui.label(RichText::new("default").small().color(p.sub));
                    }
                    if !active && ui.small_button("Use now").clicked() {
                        let _ = self.w.mouse.send(MouseCmd::Activate(r.path.clone(), "Resolution"));
                    }
                });
            }
            if n == 0 {
                ui.label(RichText::new("This mouse reports no DPI stages.").color(p.sub));
            }
        });
    }
}

// -------------------------------------------------------------- headset

impl App {
    fn headset(&mut self, ui: &mut Ui, p: &Palette, st: &mut State) {
        let h = match &st.headset {
            Dev::Ok(h) => h.clone(),
            Dev::Loading => return loading(ui, p),
            Dev::Missing(why) => {
                header(ui, p, "Headset", "");
                return missing(ui, p, "No headset found", why, &[
                    "  sudo pacman -S headsetcontrol      # Arch / CachyOS",
                    "  sudo apt install headsetcontrol    # Debian / Ubuntu",
                ]);
            }
        };
        header(ui, p, "Headset", &h.name);

        let (value, sub, level) = match h.battery {
            audio::Battery::Level(n) => (format!("{n}%"), "on battery".to_string(), Some(n)),
            audio::Battery::Charging(n) => (n.map_or("⚡".into(), |n| format!("{n}%")), "charging".into(), n),
            audio::Battery::Off => ("off".into(), "turned off or out of range".into(), None),
        };
        let color = match level {
            Some(n) if n <= 15 => p.bad,
            Some(n) if n <= 35 => p.warn,
            Some(_) => p.good,
            None => p.sub,
        };
        card(ui, p, "Battery", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&value).font(bold(40.0)).color(color));
                ui.label(RichText::new(&sub).color(p.sub));
            });
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), Sense::hover());
            ui.painter().rect_filled(rect, 5.0, p.surface2);
            if let Some(n) = level {
                let w = rect.width() * n as f32 / 100.0;
                ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(w, rect.height())), 5.0, color);
            }
        });

        let s = &mut st.settings;
        if h.sidetone {
            card(ui, p, "Sidetone", |ui| {
                ui.label(RichText::new("Hear your own voice in the headset while you talk.").color(p.sub));
                if let Some(v) = live_slider(ui, "sidetone", s.sidetone, 0..=128, |v| if v == 0 { "off".into() } else { format!("{}%", v * 100 / 128) }) {
                    s.sidetone = v;
                    let _ = self.w.headset.send(HeadsetCmd::Sidetone(v));
                }
            });
        }
        if h.inactive_time {
            card(ui, p, "Auto power-off", |ui| {
                ui.label(RichText::new("Turn the headset off after this long without sound.").color(p.sub));
                if let Some(v) = live_slider(ui, "idle", s.idle_minutes, 0..=90, |v| if v == 0 { "never".into() } else { format!("{v} min") }) {
                    s.idle_minutes = v;
                    let _ = self.w.headset.send(HeadsetCmd::Idle(v));
                }
            });
        }
    }

    // ---------------------------------------------------------------- mic

    fn mic(&mut self, ui: &mut Ui, p: &Palette, st: &mut State) {
        let m = match &mut st.mic {
            Dev::Ok(m) => m,
            Dev::Loading => return loading(ui, p),
            Dev::Missing(why) => {
                header(ui, p, "Microphone", "");
                return missing(ui, p, "No microphone found", why, &[]);
            }
        };
        header(ui, p, "Microphone", "Blue Yeti");
        let send = |control, direction, value: String| {
            let _ = self.w.mic.send(MicCmd { control, direction, value });
        };

        if let Some(g) = &mut m.gain {
            let live = g.on;
            let (text, fill) = if live { ("🎤  Live", p.good) } else { ("🔇  Muted", p.bad) };
            let btn = egui::Button::new(RichText::new(text).font(bold(18.0)).color(theme::on(fill)))
                .fill(fill)
                .corner_radius(14)
                .min_size(vec2(200.0, 56.0));
            if ui.add(btn).on_hover_text("Same as the mute button on the mic").clicked() {
                g.on = !live;
                send("Mic", "capture", if live { "nocap" } else { "cap" }.into());
            }
            ui.add_space(6.0);
            card(ui, p, "Input gain", |ui| {
                ui.label(RichText::new("How loud you are to others.").color(p.sub));
                let max = g.max;
                if let Some(v) = live_slider(ui, "gain", g.value, 0..=max, move |v| format!("{}%", v * 100 / max.max(1))) {
                    g.value = v;
                    send("Mic", "capture", v.to_string());
                }
            });
        }
        if let Some(l) = &mut m.monitor {
            card(ui, p, "Monitoring", |ui| {
                ui.label(RichText::new("Hear yourself through the headphone jack on the mic.").color(p.sub));
                let max = l.max;
                if let Some(v) = live_slider(ui, "monitor", l.value, 0..=max, move |v| format!("{}%", v * 100 / max.max(1))) {
                    l.value = v;
                    send("Mic", "playback", v.to_string());
                }
            });
        }
        if let Some(l) = &mut m.speaker {
            card(ui, p, "Headphone jack volume", |ui| {
                let max = l.max;
                if let Some(v) = live_slider(ui, "speaker", l.value, 0..=max, move |v| format!("{}%", v * 100 / max.max(1))) {
                    l.value = v;
                    send("Speaker", "playback", v.to_string());
                }
            });
        }
    }

    // ----------------------------------------------------------- settings

    fn settings(&mut self, ui: &mut Ui, p: &Palette, st: &mut State) {
        header(ui, p, "Settings", "");
        let s = &mut st.settings;
        card(ui, p, "Theme", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
                for t in theme::THEMES {
                    let sel = s.theme == t.name;
                    let (rect, resp) = ui.allocate_exact_size(vec2(150.0, 84.0), Sense::click());
                    let painter = ui.painter();
                    painter.rect(rect, 12.0, t.bg, Stroke::new(if sel { 2.5 } else { 1.0 }, if sel { p.accent } else { p.border }), egui::StrokeKind::Inside);
                    let bar = Rect::from_min_size(rect.min + vec2(12.0, 12.0), vec2(rect.width() - 24.0, 26.0));
                    painter.rect_filled(bar, 6.0, t.surface);
                    for (i, c) in [t.accent, t.good, t.warn, t.bad].into_iter().enumerate() {
                        painter.circle_filled(bar.left_center() + vec2(14.0 + i as f32 * 18.0, 0.0), 6.0, c);
                    }
                    painter.text(rect.left_bottom() + vec2(12.0, -14.0), Align2::LEFT_CENTER, t.name, bold(13.5), t.text);
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        s.theme = t.name.into();
                    }
                }
            });
        });
        card(ui, p, "Behaviour", |ui| {
            toggle(ui, p, &mut s.close_to_tray, "Keep running in the tray when closed", "Battery, temps and DPI stay one click away in the system tray.");
            toggle(ui, p, &mut s.notify_dpi, "Notify when DPI changes", "Pop up the new DPI when you press the DPI buttons on your mouse.");
            toggle(ui, p, &mut s.notify_battery, "Warn when headset battery is low", "At 15% and below.");
        });
        let url = bug_report_url(st);
        card(ui, p, "About", |ui| {
            ui.label(format!("Rig Hub {VERSION}  ·  GPL-3.0-or-later"));
            ui.label(RichText::new("Thanks to libratbag, Piper, liquidctl and HeadsetControl, which this app builds on.").color(p.sub));
            ui.horizontal(|ui| {
                if ui.button("🐞  Report a bug").clicked() {
                    open_url(&url);
                }
                if ui.button("💡  Suggest a feature").clicked() {
                    open_url(&format!("{REPO}/issues/new?labels=enhancement"));
                }
                if ui.button("⭐  GitHub").clicked() {
                    open_url(REPO);
                }
            });
        });
    }
}

// -------------------------------------------------------------------- main

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let (mut page, mut screenshot) = (Page::Cooler, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--page" => {
                page = match args.next().as_deref() {
                    Some("mouse") => Page::Mouse,
                    Some("headset") => Page::Headset,
                    Some("mic") => Page::Mic,
                    Some("settings") => Page::Settings,
                    _ => Page::Cooler,
                }
            }
            "--screenshot" => screenshot = args.next().map(PathBuf::from),
            "--version" => {
                println!("rig-hub {VERSION}");
                return Ok(());
            }
            _ => {
                println!("usage: rig-hub [--page cooler|mouse|headset|mic|settings]");
                return Ok(());
            }
        }
    }

    #[cfg(unix)]
    let instance = match screenshot {
        Some(_) => None, // test runs don't take the lock
        None => match single_instance() {
            Ok(l) => l,
            Err(()) => return Ok(()),
        },
    };
    let icon = egui::IconData { rgba: icon_rgba(128), width: 128, height: 128 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rig Hub")
            .with_app_id("rig-hub")
            .with_icon(icon)
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("Rig Hub", options, Box::new(move |cc| Ok(Box::new(App::new(cc, page, screenshot, #[cfg(unix)] instance)))))
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn second_launch_is_refused() {
        let first = super::single_instance().expect("no other Rig Hub running during tests");
        assert!(first.is_some());
        assert!(super::single_instance().is_err());
        drop(first);
        // a dead socket file must not block the next launch
        assert!(super::single_instance().unwrap().is_some());
    }
}
