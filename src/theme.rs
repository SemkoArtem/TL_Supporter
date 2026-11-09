//! Windows 10 look: Segoe UI, square corners, light surfaces and the standard blue accent.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Margin, Pos2, Rounding, Shape, Stroke, Vec2};

pub const ACCENT: Color32 = Color32::from_rgb(0, 120, 215);

/// Glyphs from the Segoe MDL2 Assets font that ships with Windows 10.
pub mod icon {
    pub const MIC: &str = "\u{E720}";
    pub const MIC_OFF: &str = "\u{EC54}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const LOCK: &str = "\u{E72E}";
    pub const LIST: &str = "\u{E8FD}";
    pub const GRID: &str = "\u{F0E2}";
    pub const HEADPHONE: &str = "\u{E7F6}";
    pub const MONITOR: &str = "\u{E7F4}";
    pub const PEN: &str = "\u{E76D}";
    pub const HIGHLIGHT: &str = "\u{E7E6}";
    pub const RECT: &str = "\u{E739}";
    pub const ARROW: &str = "\u{E72A}";
    pub const LASER: &str = "\u{E81D}";
    pub const POINTER: &str = "\u{E7C9}";
    pub const ERASE: &str = "\u{E75C}";
    pub const HELP: &str = "\u{E897}";
    pub const SEND: &str = "\u{E724}";
    pub const CLOSE: &str = "\u{E711}";
    pub const KEYBOARD: &str = "\u{E765}";
    pub const HISTORY: &str = "\u{E81C}";
    pub const MINI: &str = "\u{E73F}";
    pub const PEOPLE: &str = "\u{E716}";
    pub const MORE: &str = "\u{E712}";
}

fn load_font(fonts: &mut FontDefinitions, name: &str, path: &str) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => {
            fonts.font_data.insert(name.to_string(), FontData::from_owned(bytes));
            true
        }
        Err(_) => false,
    }
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if load_font(&mut fonts, "segoe", r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "segoe".to_string());
    }
    if load_font(&mut fonts, "mdl2", r"C:\Windows\Fonts\segmdl2.ttf") {
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().push("mdl2".to_string());
    }
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    style.spacing.button_padding = Vec2::new(9.0, 4.0);
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));

    let mut v = egui::Visuals::light();
    let square = Rounding::ZERO;
    v.window_rounding = square;
    v.menu_rounding = square;
    v.panel_fill = Color32::from_rgb(250, 250, 250);
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE;
    v.selection.bg_fill = Color32::from_rgb(204, 228, 247);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(20, 20, 20));
    v.hyperlink_color = ACCENT;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = square;
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(232, 232, 232);
    v.widgets.inactive.bg_fill = Color32::from_rgb(232, 232, 232);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(218, 218, 218);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(204, 204, 204);
    style.visuals = v;
    ctx.set_style(style);
}

pub fn bar_frame() -> egui::Frame {
    egui::Frame::none().fill(Color32::from_rgb(240, 240, 240)).inner_margin(Margin::symmetric(8.0, 5.0))
}

pub fn nav(ui: &mut egui::Ui, glyph: &str, label: &str, selected: bool) -> egui::Response {
    ui.selectable_label(selected, format!("{glyph}  {label}"))
}

pub fn paint_cursor(p: &egui::Painter, at: Pos2) {
    let points = vec![
        at,
        at + Vec2::new(0.0, 16.0),
        at + Vec2::new(4.0, 12.5),
        at + Vec2::new(7.0, 18.0),
        at + Vec2::new(9.5, 17.0),
        at + Vec2::new(6.5, 11.5),
        at + Vec2::new(11.5, 11.5),
    ];
    p.add(Shape::closed_line(points.clone(), Stroke::new(2.5_f32, Color32::WHITE)));
    p.add(Shape::closed_line(points, Stroke::new(1.2_f32, Color32::BLACK)));
}

/// Windows virtual-key code for an egui key.
pub fn virtual_key(key: egui::Key) -> Option<u16> {
    use egui::Key::*;
    Some(match key {
        Enter => 0x0D,
        Tab => 0x09,
        Backspace => 0x08,
        Escape => 0x1B,
        Space => 0x20,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        Delete => 0x2E,
        Insert => 0x2D,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        F1 => 0x70,
        F2 => 0x71,
        F3 => 0x72,
        F4 => 0x73,
        F5 => 0x74,
        F6 => 0x75,
        F7 => 0x76,
        F8 => 0x77,
        F9 => 0x78,
        F10 => 0x79,
        F11 => 0x7A,
        F12 => 0x7B,
        other => {
            let name = other.name();
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_none() && c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase() as u16
            } else {
                return None;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_named_keys_map() {
        assert_eq!(virtual_key(egui::Key::A), Some(0x41));
        assert_eq!(virtual_key(egui::Key::Num5), Some(0x35));
        assert_eq!(virtual_key(egui::Key::Enter), Some(0x0D));
        assert_eq!(virtual_key(egui::Key::F12), Some(0x7B));
    }
}
