//! Color themes. Each is a small palette mapped onto egui's visuals.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

#[derive(Clone, Copy)]
pub struct Palette {
    pub name: &'static str,
    pub dark: bool,
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub sub: Color32,
    pub accent: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub const THEMES: &[Palette] = &[
    Palette { name: "Midnight", dark: true, bg: hex(0x0E1016), surface: hex(0x161922), surface2: hex(0x1F2330), border: hex(0x2A2F3D), text: hex(0xE6E8EF), sub: hex(0x8B93A7), accent: hex(0x8B5CF6), good: hex(0x34D399), warn: hex(0xFBBF24), bad: hex(0xF87171) },
    Palette { name: "Daylight", dark: false, bg: hex(0xF3F4F7), surface: hex(0xFFFFFF), surface2: hex(0xECEEF3), border: hex(0xDDE1E8), text: hex(0x1B1F2A), sub: hex(0x5D6577), accent: hex(0x6D28D9), good: hex(0x059669), warn: hex(0xD97706), bad: hex(0xDC2626) },
    Palette { name: "Catppuccin Mocha", dark: true, bg: hex(0x11111B), surface: hex(0x1E1E2E), surface2: hex(0x313244), border: hex(0x45475A), text: hex(0xCDD6F4), sub: hex(0xA6ADC8), accent: hex(0xCBA6F7), good: hex(0xA6E3A1), warn: hex(0xF9E2AF), bad: hex(0xF38BA8) },
    Palette { name: "Nord", dark: true, bg: hex(0x272C36), surface: hex(0x2E3440), surface2: hex(0x3B4252), border: hex(0x4C566A), text: hex(0xECEFF4), sub: hex(0xA3ACBD), accent: hex(0x88C0D0), good: hex(0xA3BE8C), warn: hex(0xEBCB8B), bad: hex(0xBF616A) },
    Palette { name: "Dracula", dark: true, bg: hex(0x1E1F29), surface: hex(0x282A36), surface2: hex(0x343746), border: hex(0x44475A), text: hex(0xF8F8F2), sub: hex(0xA0A4C0), accent: hex(0xFF79C6), good: hex(0x50FA7B), warn: hex(0xF1FA8C), bad: hex(0xFF5555) },
    Palette { name: "Gruvbox", dark: true, bg: hex(0x1D2021), surface: hex(0x282828), surface2: hex(0x3C3836), border: hex(0x504945), text: hex(0xEBDBB2), sub: hex(0xA89984), accent: hex(0xFE8019), good: hex(0xB8BB26), warn: hex(0xFABD2F), bad: hex(0xFB4934) },
];

pub fn by_name(name: &str) -> Palette {
    *THEMES.iter().find(|t| t.name == name).unwrap_or(&THEMES[0])
}

/// Text color that stays readable on top of `c`.
pub fn on(c: Color32) -> Color32 {
    let l = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    if l > 150.0 { hex(0x111111) } else { Color32::WHITE }
}

pub fn apply(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    let r = CornerRadius::same(8);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface2;
    v.faint_bg_color = p.surface2;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, on(p.accent));
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.slider_trailing_fill = true;
    for (w, fill, stroke) in [
        (&mut v.widgets.noninteractive, p.surface, p.border),
        (&mut v.widgets.inactive, p.surface2, p.border),
        (&mut v.widgets.hovered, p.surface2, p.accent),
        (&mut v.widgets.active, p.accent, p.accent),
        (&mut v.widgets.open, p.surface2, p.accent),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, stroke);
        w.corner_radius = r;
        w.fg_stroke = Stroke::new(1.5, p.text);
    }
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.sub);
    v.widgets.active.fg_stroke = Stroke::new(1.5, on(p.accent));
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 10.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.slider_width = 260.0;
        s.spacing.interact_size.y = 30.0;
        use egui::{FontId, TextStyle::*};
        s.text_styles = [
            (Heading, bold(24.0)),
            (Body, FontId::proportional(15.0)),
            (Button, FontId::proportional(15.0)),
            (Small, FontId::proportional(12.5)),
            (Monospace, FontId::monospace(14.0)),
        ]
        .into();
    });
}

/// The "bold" font family is registered at startup (falls back to regular).
pub fn bold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("bold".into()))
}
