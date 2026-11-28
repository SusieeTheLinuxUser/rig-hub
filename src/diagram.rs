//! Interactive mouse picture. Uses the device artwork that Piper ships
//! (piper.gresource, GPL-2.0-or-later), read from the user's own install.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use resvg::{tiny_skia, usvg};
use std::process::Command;

const GRESOURCE: &str = "/usr/share/piper/piper.gresource";
const SVG_DIR: &str = "/org/freedesktop/Piper/svgs/";

fn extract(name: &str) -> Option<Vec<u8>> {
    let out = Command::new("gresource").args(["extract", GRESOURCE, &format!("{SVG_DIR}{name}")]).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// The Svg= entry the lookup ini picks for `model`, or None. The name always
/// comes from the ini's own Svg= line, never from `model`, so no model string
/// can point the extraction outside the gresource.
fn lookup_svg(ini: &str, model: &str) -> Option<String> {
    let mut svg = None;
    let mut matched = false;
    for line in ini.lines() {
        if line.starts_with('[') {
            matched = false;
        } else if let Some(m) = line.strip_prefix("DeviceMatch=") {
            matched = m.split(';').any(|m| !m.is_empty() && model.starts_with(m));
        } else if let Some(s) = line.strip_prefix("Svg=")
            && matched
        {
            svg = Some(s.trim().to_string());
        }
    }
    svg
}

/// Piper's artwork for a ratbag model string like "usb:046d:c08b:0".
pub fn piper_svg(model: &str) -> Option<Vec<u8>> {
    let ini = String::from_utf8(extract("svg-lookup.ini")?).ok()?;
    extract(&lookup_svg(&ini, model)?)
}

/// (width, height, highlighted button, color) the cached texture was drawn for.
type TextureKey = (u32, u32, Option<usize>, Color32);

pub struct Diagram {
    tree: usvg::Tree,
    texture: Option<(TextureHandle, TextureKey)>,
}

/// Room left and right of the mouse for labels.
const SIDE: f32 = 190.0;

impl Diagram {
    pub fn new(svg: &[u8]) -> Option<Self> {
        let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).ok()?;
        Some(Self { tree, texture: None })
    }

    fn render(&self, w: u32, h: u32, scale: f32, highlight: Option<usize>, accent: Color32) -> egui::ColorImage {
        let mut pm = tiny_skia::Pixmap::new(w, h).expect("non-zero size");
        let t = tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&self.tree, t, &mut pm.as_mut());
        if let Some(node) = highlight.and_then(|i| self.tree.node_by_id(&format!("button{i}"))) {
            let mut over = tiny_skia::Pixmap::new(w, h).expect("non-zero size");
            if let Some(b) = node.abs_layer_bounding_box() {
                resvg::render_node(node, t.pre_translate(b.x(), b.y()), &mut over.as_mut());
            }
            // tint the button's own pixels with the accent color
            for (dst, src) in pm.pixels_mut().iter_mut().zip(over.pixels()) {
                if src.alpha() > 0 {
                    let a = src.alpha() as f32 / 255.0 * 0.85;
                    let mix = |d: u8, c: u8| (d as f32 * (1.0 - a) + c as f32 * a) as u8;
                    let c = dst.demultiply();
                    *dst = tiny_skia::ColorU8::from_rgba(mix(c.red(), accent.r()), mix(c.green(), accent.g()), mix(c.blue(), accent.b()), c.alpha()).premultiply();
                }
            }
        }
        egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pm.data())
    }

    /// Draws the mouse with one label per button; returns the clicked button index.
    pub fn show(&mut self, ui: &mut egui::Ui, labels: &[(usize, String)], selected: Option<usize>, accent: Color32, text: Color32, sub: Color32) -> Option<usize> {
        let size = self.tree.size();
        let avail = ui.available_width().min(900.0);
        let scale = ((avail - 2.0 * SIDE) / size.width()).clamp(0.3, 1.1);
        let (sw, sh) = (size.width() * scale, size.height() * scale);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(sw + 2.0 * SIDE, sh), Sense::hover());
        let img_rect = Rect::from_min_size(rect.min + Vec2::new(SIDE, 0.0), Vec2::new(sw, sh));

        let ppp = ui.ctx().pixels_per_point();
        let (pw, ph) = ((sw * ppp).round() as u32, (sh * ppp).round() as u32);
        let key = (pw, ph, selected, accent);
        if self.texture.as_ref().map(|t| t.1) != Some(key) {
            let img = self.render(pw.max(1), ph.max(1), scale * ppp, selected, accent);
            let tex = ui.ctx().load_texture("mouse-diagram", img, egui::TextureOptions::LINEAR);
            self.texture = Some((tex, key));
        }
        let tex = &self.texture.as_ref().unwrap().0;
        ui.painter().image(tex.id(), img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

        let mut clicked = None;
        for (i, label) in labels {
            let Some(leader) = self.tree.node_by_id(&format!("button{i}-leader")) else { continue };
            // no bbox (resvg 0.50): empty node, nothing to anchor the label to
            let Some(b) = leader.abs_bounding_box() else { continue };
            let left = b.x() < size.width() / 2.0;
            let y = img_rect.top() + (b.y() + b.height() / 2.0) * scale;
            let (anchor, pos) = if left {
                (Align2::RIGHT_CENTER, Pos2::new(img_rect.left() - 8.0, y))
            } else {
                (Align2::LEFT_CENTER, Pos2::new(img_rect.right() + 8.0, y))
            };
            let is_sel = selected == Some(*i);
            let galley_rect = ui.painter().text(
                pos,
                anchor,
                label,
                FontId::proportional(14.0),
                if is_sel { accent } else { text },
            );
            let hit = galley_rect.expand(4.0);
            let resp = ui.interact(hit, ui.scope_id().with(("btn", *i)), Sense::click());
            if resp.hovered() && !is_sel {
                ui.painter().rect_stroke(hit, 6.0, Stroke::new(1.0, sub), egui::StrokeKind::Outside);
            }
            if is_sel {
                ui.painter().rect_stroke(hit, 6.0, Stroke::new(1.5, accent), egui::StrokeKind::Outside);
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                clicked = Some(*i);
            }
            // the button itself is clickable too
            if let Some(btn) = self.tree.node_by_id(&format!("button{i}"))
                && let Some(bb) = btn.abs_bounding_box()
            {
                let r = Rect::from_min_size(
                    img_rect.min + Vec2::new(bb.x() * scale, bb.y() * scale),
                    Vec2::new(bb.width() * scale, bb.height() * scale),
                );
                if ui.interact(r, ui.scope_id().with(("shape", *i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    clicked = Some(*i);
                }
            }
        }
        clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the shape of piper's svg-lookup.ini: DeviceMatch is a ';' list of model
    // prefixes, Svg= names the file inside the gresource
    const INI: &str = "[svg:g502-hero]\nDeviceMatch=usb:046d:c08b;usb:046d:c33b\nSvg=g502-hero.svg\n\n[svg:g305]\nDeviceMatch=usb:046d:c07f\nSvg=g305.svg\n";

    #[test]
    fn lookup_matches_the_model_prefix() {
        assert_eq!(lookup_svg(INI, "usb:046d:c08b:0").as_deref(), Some("g502-hero.svg"));
        assert_eq!(lookup_svg(INI, "usb:046d:c33b").as_deref(), Some("g502-hero.svg"));
        assert_eq!(lookup_svg(INI, "usb:046d:c07f:1").as_deref(), Some("g305.svg"));
        assert_eq!(lookup_svg(INI, "usb:1234:5678:0"), None);
        assert_eq!(lookup_svg(INI, ""), None);
    }

    #[test]
    fn a_weird_model_only_selects_it_never_names_the_file() {
        // /, .. and spaces in the model change at most WHICH entry is picked
        for evil in ["../etc/passwd", "usb:../..", "a b c", "  ", "\n[svg:evil]\nSvg=evil.svg"] {
            assert_eq!(lookup_svg(INI, evil), None, "{evil:?} must not match");
        }
        // a model with path bits that happens to match still resolves to the ini's
        // own name, which carries none of the model's path bits
        for evil in ["usb:046d:c08b/../../x", "usb:046d:c08b with spaces"] {
            assert_eq!(lookup_svg(INI, evil).as_deref(), Some("g502-hero.svg"), "{evil:?} may select, not name");
        }
    }

    #[test]
    fn sections_reset_the_match_and_empty_segments_never_match() {
        // the section without its own DeviceMatch must not inherit the previous one
        let boundary = "[svg:a]\nDeviceMatch=usb:046d:c08b\nSvg=a.svg\n\n[svg:b]\nSvg=b.svg\n";
        assert_eq!(lookup_svg(boundary, "usb:046d:c08b:0").as_deref(), Some("a.svg"));
        // a leading or trailing ';' makes an empty segment, which matches nothing
        let padded = "[svg:a]\nDeviceMatch=;usb:046d:c08b;\nSvg=a.svg\n";
        assert_eq!(lookup_svg(padded, ""), None);
        assert_eq!(lookup_svg(padded, "usb:046d:c08b:0").as_deref(), Some("a.svg"));
        // a CRLF ini file works the same
        assert_eq!(lookup_svg(&INI.replace('\n', "\r\n"), "usb:046d:c08b:0").as_deref(), Some("g502-hero.svg"));
    }
}
