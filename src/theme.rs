//! Windows 10 look: Segoe UI, square corners, a navigation pane and the standard blue accent.
//! Dark by default, with a light variant in the settings.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Margin, Pos2, Rounding, Shape, Stroke, Vec2};
use std::sync::atomic::{AtomicBool, Ordering};

pub const ACCENT: Color32 = Color32::from_rgb(0, 120, 215);
const TITLE_FONT: &str = "title";
static LIGHT: AtomicBool = AtomicBool::new(false);

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
    pub const MENU: &str = "\u{E700}";
    pub const BRUSH: &str = "\u{E771}";
    pub const SYNC: &str = "\u{E895}";
    pub const EYE: &str = "\u{E7B3}";
    pub const COPY: &str = "\u{E8C8}";
}

/// This PC's key: only the first part is visible until the eye is pressed.
#[derive(Default)]
pub struct KeyView {
    shown: bool,
    copied: Option<std::time::Instant>,
}

impl KeyView {
    pub fn show(&mut self, ui: &mut egui::Ui, key: &str) {
        let text = if self.shown {
            key.to_string()
        } else {
            let mut parts = key.split('-');
            let first = parts.next().unwrap_or("");
            parts.fold(first.to_string(), |text, _| text + "-••••")
        };
        let label = ui.add(egui::Label::new(egui::RichText::new(text).strong()).selectable(self.shown));
        // Reserve the width of the full key so the buttons stay in place when it opens.
        let full = egui::WidgetText::from(egui::RichText::new(key).strong())
            .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
            .size()
            .x;
        ui.add_space((full - label.rect.width()).max(0.0));
        let hint = if self.shown { "Сховати ключ" } else { "Показати ключ повністю" };
        if ui.add(egui::Button::new(icon::EYE).selected(self.shown)).on_hover_text(hint).clicked() {
            self.shown = !self.shown;
        }
        if ui.button(icon::COPY).on_hover_text("Скопіювати ключ").clicked() {
            ui.output_mut(|o| o.copied_text = key.to_string());
            self.copied = Some(std::time::Instant::now());
        }
        if self.copied.is_some_and(|t| t.elapsed().as_secs() < 2) {
            ui.weak("Скопійовано");
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
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

pub fn apply(ctx: &egui::Context, light: bool) {
    let mut fonts = FontDefinitions::default();
    if load_font(&mut fonts, "segoe", r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "segoe".to_string());
    }
    if load_font(&mut fonts, "mdl2", r"C:\Windows\Fonts\segmdl2.ttf") {
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().push("mdl2".to_string());
    }
    // Page titles in Windows 10 use the thin cut of Segoe UI.
    let mut titles = fonts.families[&FontFamily::Proportional].clone();
    if load_font(&mut fonts, "segoe_light", r"C:\Windows\Fonts\segoeuil.ttf") {
        titles.insert(0, "segoe_light".to_string());
    }
    fonts.families.insert(FontFamily::Name(TITLE_FONT.into()), titles);
    ctx.set_fonts(fonts);
    set_mode(ctx, light);
}

/// Switch between the dark and the light look. Also recolours the window's title bar.
pub fn set_mode(ctx: &egui::Context, light: bool) {
    LIGHT.store(light, Ordering::Relaxed);
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if light { egui::SystemTheme::Light } else { egui::SystemTheme::Dark }));

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(11.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(24.0, FontFamily::Name(TITLE_FONT.into())));

    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    let square = Rounding::ZERO;
    v.window_rounding = square;
    v.menu_rounding = square;
    v.hyperlink_color = if light { ACCENT } else { Color32::from_rgb(96, 205, 255) };
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = square;
        w.expansion = 0.0;
    }
    if light {
        v.panel_fill = Color32::from_rgb(250, 250, 250);
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::WHITE;
        v.selection.bg_fill = Color32::from_rgb(204, 228, 247);
        v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(20, 20, 20));
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(230, 230, 230);
        v.widgets.inactive.bg_fill = Color32::from_rgb(230, 230, 230);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(230, 230, 230);
        v.widgets.hovered.bg_stroke = Stroke::new(2.0_f32, Color32::from_gray(122));
        v.widgets.active.weak_bg_fill = Color32::from_rgb(204, 204, 204);
    } else {
        v.panel_fill = Color32::from_rgb(31, 31, 31);
        v.window_fill = Color32::from_rgb(43, 43, 43);
        v.faint_bg_color = Color32::from_rgb(43, 43, 43);
        v.extreme_bg_color = Color32::from_rgb(10, 10, 10);
        v.selection.bg_fill = Color32::from_rgb(0, 90, 158);
        v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(232));
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_gray(64));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(242));
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(51, 51, 51);
        v.widgets.inactive.bg_fill = Color32::from_rgb(51, 51, 51);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_gray(64));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(51, 51, 51);
        v.widgets.hovered.bg_fill = Color32::from_rgb(64, 64, 64);
        v.widgets.hovered.bg_stroke = Stroke::new(2.0_f32, Color32::from_gray(133));
        v.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(102, 102, 102);
        v.widgets.active.bg_fill = Color32::from_rgb(102, 102, 102);
        v.widgets.active.bg_stroke = Stroke::new(2.0_f32, Color32::from_gray(133));
        v.widgets.open.weak_bg_fill = Color32::from_rgb(51, 51, 51);
    }
    style.visuals = v;
    ctx.set_style(style);
}

fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

fn rail_fill() -> Color32 {
    if is_light() { Color32::from_rgb(240, 240, 240) } else { Color32::from_rgb(43, 43, 43) }
}

pub fn hover_fill() -> Color32 {
    if is_light() { Color32::from_rgb(225, 225, 225) } else { Color32::from_rgb(58, 58, 58) }
}

pub fn border() -> Color32 {
    if is_light() { Color32::from_gray(210) } else { Color32::from_gray(70) }
}

/// Error text that stays readable on both backgrounds.
pub fn danger() -> Color32 {
    if is_light() { Color32::from_rgb(232, 17, 35) } else { Color32::from_rgb(255, 120, 130) }
}

pub fn warn_fill() -> Color32 {
    if is_light() { Color32::from_rgb(255, 244, 206) } else { Color32::from_rgb(88, 68, 0) }
}

pub fn bar_frame() -> egui::Frame {
    egui::Frame::none().fill(rail_fill()).inner_margin(Margin::symmetric(8.0, 5.0))
}

pub fn rail_frame() -> egui::Frame {
    egui::Frame::none().fill(rail_fill())
}

/// Roomy margins of a Windows 10 page.
pub fn page_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::central_panel(&ctx.style()).inner_margin(Margin::symmetric(24.0, 16.0))
}

pub fn accent_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(Color32::WHITE)).fill(ACCENT)
}

/// One row of a Windows 10 navigation pane. In a compact pane only the glyph is visible.
pub fn nav_item(ui: &mut egui::Ui, glyph: &str, label: &str, selected: bool, expanded: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if response.hovered() {
        painter.rect_filled(rect, 0.0, hover_fill());
    }
    if selected {
        let bar = egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 8.0), Vec2::new(3.0, rect.height() - 16.0));
        painter.rect_filled(bar, 0.0, ACCENT);
    }
    let color = ui.visuals().strong_text_color();
    let y = rect.center().y;
    painter.text(Pos2::new(rect.left() + 24.0, y), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(16.0), color);
    if expanded {
        painter.text(Pos2::new(rect.left() + 48.0, y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.0), color);
        response
    } else if label.is_empty() {
        response
    } else {
        response.on_hover_text(label)
    }
}

/// Windows 10 toggle switch with a label that can be clicked too.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, label: impl Into<egui::WidgetText>) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, switch) = ui.allocate_exact_size(Vec2::new(44.0, 20.0), egui::Sense::click());
        let text = ui.add(egui::Label::new(label).sense(egui::Sense::click()).selectable(false));
        let mut response = switch | text;
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        let how = ui.ctx().animate_bool(response.id, *on);
        let outline = ui.visuals().strong_text_color();
        let (fill, line, knob) = if *on { (ACCENT, ACCENT, Color32::WHITE) } else { (Color32::TRANSPARENT, outline, outline) };
        let body = rect.shrink(1.0);
        ui.painter().rect(body, body.height() / 2.0, fill, Stroke::new(2.0_f32, line));
        let x = egui::lerp((body.left() + 9.0)..=(body.right() - 9.0), how);
        ui.painter().circle_filled(Pos2::new(x, body.center().y), 5.0, knob);
        response
    })
    .inner
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
