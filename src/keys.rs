//! Linux evdev key codes <-> names <-> egui keys, for "button sends key X".

use eframe::egui::Key;

/// (evdev code, display name, egui key if egui can capture it)
const KEYS: &[(u32, &str, Option<Key>)] = &[
    (1, "Esc", Some(Key::Escape)),
    (2, "1", Some(Key::Num1)), (3, "2", Some(Key::Num2)), (4, "3", Some(Key::Num3)),
    (5, "4", Some(Key::Num4)), (6, "5", Some(Key::Num5)), (7, "6", Some(Key::Num6)),
    (8, "7", Some(Key::Num7)), (9, "8", Some(Key::Num8)), (10, "9", Some(Key::Num9)),
    (11, "0", Some(Key::Num0)), (12, "-", Some(Key::Minus)), (13, "=", Some(Key::Equals)),
    (14, "Backspace", Some(Key::Backspace)), (15, "Tab", Some(Key::Tab)),
    (16, "Q", Some(Key::Q)), (17, "W", Some(Key::W)), (18, "E", Some(Key::E)),
    (19, "R", Some(Key::R)), (20, "T", Some(Key::T)), (21, "Y", Some(Key::Y)),
    (22, "U", Some(Key::U)), (23, "I", Some(Key::I)), (24, "O", Some(Key::O)),
    (25, "P", Some(Key::P)), (26, "[", Some(Key::OpenBracket)), (27, "]", Some(Key::CloseBracket)),
    (28, "Enter", Some(Key::Enter)), (29, "Left Ctrl", None),
    (30, "A", Some(Key::A)), (31, "S", Some(Key::S)), (32, "D", Some(Key::D)),
    (33, "F", Some(Key::F)), (34, "G", Some(Key::G)), (35, "H", Some(Key::H)),
    (36, "J", Some(Key::J)), (37, "K", Some(Key::K)), (38, "L", Some(Key::L)),
    (39, ";", Some(Key::Semicolon)), (40, "'", Some(Key::Quote)), (41, "`", Some(Key::Backtick)),
    (42, "Left Shift", None), (43, "\\", Some(Key::Backslash)),
    (44, "Z", Some(Key::Z)), (45, "X", Some(Key::X)), (46, "C", Some(Key::C)),
    (47, "V", Some(Key::V)), (48, "B", Some(Key::B)), (49, "N", Some(Key::N)),
    (50, "M", Some(Key::M)), (51, ",", Some(Key::Comma)), (52, ".", Some(Key::Period)),
    (53, "/", Some(Key::Slash)), (54, "Right Shift", None), (56, "Left Alt", None),
    (57, "Space", Some(Key::Space)), (58, "Caps Lock", None),
    (59, "F1", Some(Key::F1)), (60, "F2", Some(Key::F2)), (61, "F3", Some(Key::F3)),
    (62, "F4", Some(Key::F4)), (63, "F5", Some(Key::F5)), (64, "F6", Some(Key::F6)),
    (65, "F7", Some(Key::F7)), (66, "F8", Some(Key::F8)), (67, "F9", Some(Key::F9)),
    (68, "F10", Some(Key::F10)), (87, "F11", Some(Key::F11)), (88, "F12", Some(Key::F12)),
    (97, "Right Ctrl", None), (100, "Right Alt", None),
    (102, "Home", Some(Key::Home)), (103, "Up", Some(Key::ArrowUp)),
    (104, "Page Up", Some(Key::PageUp)), (105, "Left", Some(Key::ArrowLeft)),
    (106, "Right", Some(Key::ArrowRight)), (107, "End", Some(Key::End)),
    (108, "Down", Some(Key::ArrowDown)), (109, "Page Down", Some(Key::PageDown)),
    (110, "Insert", Some(Key::Insert)), (111, "Delete", Some(Key::Delete)),
    (113, "Mute", None), (114, "Volume down", None), (115, "Volume up", None),
    (125, "Super", None), (133, "Copy", Some(Key::Copy)), (135, "Paste", Some(Key::Paste)),
    (137, "Cut", Some(Key::Cut)), (163, "Next track", None), (164, "Play/Pause", None),
    (165, "Previous track", None),
    (183, "F13", Some(Key::F13)), (184, "F14", Some(Key::F14)), (185, "F15", Some(Key::F15)),
    (186, "F16", Some(Key::F16)), (187, "F17", Some(Key::F17)), (188, "F18", Some(Key::F18)),
    (189, "F19", Some(Key::F19)), (190, "F20", Some(Key::F20)),
];

pub fn name(code: u32) -> String {
    KEYS.iter()
        .find(|k| k.0 == code)
        .map(|k| k.1.to_string())
        .unwrap_or_else(|| format!("code {code}"))
}

pub fn from_egui(key: Key) -> Option<u32> {
    KEYS.iter().find(|k| k.2 == Some(key)).map(|k| k.0)
}

/// Keys egui can't capture from a key press (modifiers, media keys).
pub fn uncapturable() -> impl Iterator<Item = (u32, &'static str)> {
    KEYS.iter().filter(|k| k.2.is_none()).map(|k| (k.0, k.1))
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        use eframe::egui::Key;
        assert_eq!(super::from_egui(Key::A), Some(30));
        assert_eq!(super::name(30), "A");
        assert_eq!(super::name(164), "Play/Pause");
        assert_eq!(super::name(9999), "code 9999");
        assert!(super::uncapturable().any(|(c, _)| c == 29));
    }
}
