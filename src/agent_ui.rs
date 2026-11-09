//! What the employee sees: drawings over all windows, a small panel and the tray icon.
//! Both windows are hidden from screen capture and never take keyboard focus.

use crate::capture;
use crate::model::Policy;
use crate::node::Node;
use crate::proto::{DrawOp, LeadActivity, Tool};
use crate::util::wide;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CONSOLE_TITLE: &str = "TL Supporter — Керівник";

const WM_WAKE: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const TIMER_ANIM: usize = 1;
const HOTKEY_HIDE: i32 = 1;
const WDA_EXCLUDEFROMCAPTURE_VALUE: u32 = 0x11;

pub enum UiCmd {
    Draw { lead: String, name: String, color: [u8; 3], op: DrawOp },
    ClearLead(String),
    Activity(Vec<LeadActivity>),
    Hint { lead: String, name: String, color: [u8; 3], id: u32, text: String },
    HelpTaken(String),
    HelpSent,
    HelpNoLeads,
    HelpIncoming(String),
    Active(bool),
    LeadsOnline(usize),
    WindowsLocked(bool),
    Policy(Policy),
}

#[derive(Clone)]
pub struct UiHandle {
    tx: Sender<UiCmd>,
    thread: Arc<AtomicU32>,
}

impl UiHandle {
    pub fn new() -> (Self, Receiver<UiCmd>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx, thread: Arc::new(AtomicU32::new(0)) }, rx)
    }

    pub fn send(&self, c: UiCmd) {
        let _ = self.tx.send(c);
        let t = self.thread.load(Ordering::SeqCst);
        if t != 0 {
            unsafe {
                let _ = PostThreadMessageW(t, WM_WAKE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

struct Dib {
    hdc: HDC,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let hdc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            SelectObject(hdc, bmp);
            Some(Self { hdc, bits: bits as *mut u8, w, h })
        }
    }

    fn present(&self, hwnd: HWND, x: i32, y: i32, pix: &Pixmap) {
        unsafe {
            let dst = std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize);
            for (d, s) in dst.chunks_exact_mut(4).zip(pix.data().chunks_exact(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
            let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&POINT { x, y }),
                Some(&SIZE { cx: self.w, cy: self.h }),
                self.hdc,
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

struct Font {
    font: fontdue::Font,
}

impl Font {
    fn load() -> Option<Self> {
        let bytes = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").ok()?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()?;
        Some(Self { font })
    }

    fn width(&self, text: &str, px: f32) -> f32 {
        text.chars().map(|c| self.font.metrics(c, px).advance_width).sum()
    }

    fn draw(&self, pix: &mut Pixmap, text: &str, x: f32, baseline: f32, px: f32, color: [u8; 3]) {
        let (w, h) = (pix.width() as i32, pix.height() as i32);
        let data = pix.data_mut();
        let mut pen = x;
        for c in text.chars() {
            let (m, bitmap) = self.font.rasterize(c, px);
            let left = pen as i32 + m.xmin;
            let top = baseline as i32 - m.height as i32 - m.ymin;
            for gy in 0..m.height as i32 {
                for gx in 0..m.width as i32 {
                    let (dx, dy) = (left + gx, top + gy);
                    if dx < 0 || dy < 0 || dx >= w || dy >= h {
                        continue;
                    }
                    let a = bitmap[(gy * m.width as i32 + gx) as usize] as u32;
                    if a == 0 {
                        continue;
                    }
                    let p = ((dy * w + dx) * 4) as usize;
                    for i in 0..3 {
                        data[p + i] = ((color[i] as u32 * a + data[p + i] as u32 * (255 - a)) / 255) as u8;
                    }
                    data[p + 3] = (a + data[p + 3] as u32 * (255 - a) / 255) as u8;
                }
            }
            pen += m.advance_width;
        }
    }

    fn wrap(&self, text: &str, px: f32, max: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.split('\n') {
            let mut cur = String::new();
            for word in para.split(' ') {
                let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                if self.width(&trial, px) > max && !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                    cur = word.to_string();
                } else {
                    cur = trial;
                }
            }
            lines.push(cur);
        }
        lines
    }
}

#[derive(Clone)]
struct DrawItem {
    lead: String,
    name: String,
    id: u32,
    tool: Tool,
    monitor: u8,
    color: [u8; 3],
    points: Vec<[f32; 2]>,
    stamps: Vec<Instant>,
    ended: Option<Instant>,
}

struct Overlay {
    hwnd: HWND,
    rect: (i32, i32, i32, i32),
    dib: Dib,
    pix: Pixmap,
    visible: bool,
}

struct Hint {
    lead: String,
    name: String,
    color: [u8; 3],
    id: u32,
    text: String,
}

#[derive(Clone)]
enum Action {
    CloseHint(usize),
    CopyHint(usize),
    Help,
    HelpReason(String),
    HelpCancel,
    HideDrawings,
}

const FADE: Duration = Duration::from_millis(1500);
const LASER_LIFE: Duration = Duration::from_millis(900);

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

enum Ev {
    PanelClick(i32, i32),
    TrayMenu,
    TrayOpen,
}

fn push_event(e: Ev) {
    EVENTS.with(|v| v.borrow_mut().push(e));
}

unsafe extern "system" fn overlay_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

unsafe extern "system" fn panel_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            let x = (l.0 & 0xFFFF) as i16 as i32;
            let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;
            push_event(Ev::PanelClick(x, y));
            LRESULT(0)
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

unsafe extern "system" fn tray_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m == WM_TRAY {
        let code = (l.0 & 0xFFFF) as u32;
        if code == WM_RBUTTONUP || code == WM_CONTEXTMENU {
            push_event(Ev::TrayMenu);
        } else if code == WM_LBUTTONDBLCLK {
            push_event(Ev::TrayOpen);
        }
        return LRESULT(0);
    }
    DefWindowProcW(h, m, w, l)
}

fn register_class(name: PCWSTR, proc_: WNDPROC) {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let c = WNDCLASSW {
            lpfnWndProc: proc_,
            hInstance: hinstance.into(),
            lpszClassName: name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&c);
    }
}

fn exclude_from_capture(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WINDOW_DISPLAY_AFFINITY};
    // A test run keeps these windows visible to screenshots so the result can be inspected.
    if std::env::var("TLS_DATA_DIR").is_ok() && std::env::var("TLS_SHOW_IN_CAPTURE").is_ok() {
        return;
    }
    unsafe {
        let _ = SetWindowDisplayAffinity(hwnd, WINDOW_DISPLAY_AFFINITY(WDA_EXCLUDEFROMCAPTURE_VALUE));
    }
}

fn create_layered(class: PCWSTR, x: i32, y: i32, w: i32, h: i32, click_through: bool) -> HWND {
    unsafe {
        let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        if click_through {
            ex |= WS_EX_TRANSPARENT;
        }
        let hwnd = CreateWindowExW(ex, class, w!("TLS"), WS_POPUP, x, y, w, h, None, None, GetModuleHandleW(None).unwrap_or_default(), None)
            .unwrap_or_default();
        exclude_from_capture(hwnd);
        hwnd
    }
}

/// 32x32 RGBA picture of the program icon, also used for the console window.
pub fn icon_rgba() -> Vec<u8> {
    icon_pixmap([0, 120, 215]).data().to_vec()
}

fn icon_pixmap(rgb: [u8; 3]) -> Pixmap {
    const S: u32 = 32;
    let mut pix = Pixmap::new(S, S).unwrap();
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color_rgba8(rgb[0], rgb[1], rgb[2], 255);
    let circle = PathBuilder::from_circle(16.0, 16.0, 14.0).unwrap();
    pix.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), None);
    paint.set_color_rgba8(255, 255, 255, 255);
    let t = Rect::from_xywh(9.0, 8.0, 14.0, 4.0).unwrap();
    pix.fill_rect(t, &paint, Transform::identity(), None);
    let stem = Rect::from_xywh(14.0, 12.0, 4.0, 12.0).unwrap();
    pix.fill_rect(stem, &paint, Transform::identity(), None);
    pix
}

fn tray_icon(rgb: [u8; 3]) -> HICON {
    const S: u32 = 32;
    let pix = icon_pixmap(rgb);
    let mut bgra = pix.data().to_vec();
    for c in bgra.chunks_exact_mut(4) {
        c.swap(0, 2);
    }
    let mask = vec![0u8; (S * S / 8) as usize];
    unsafe {
        CreateIcon(GetModuleHandleW(None).unwrap_or_default(), S as i32, S as i32, 1, 32, mask.as_ptr(), bgra.as_ptr()).unwrap_or_default()
    }
}

/// Bring the lead console to the front.
pub fn restore_console_window() {
    restore_window(CONSOLE_TITLE);
}

/// Show a window of this program and bring it to the front.
pub fn restore_window(title: &str) {
    let title = wide(title);
    unsafe {
        if let Ok(h) = FindWindowW(None, PCWSTR(title.as_ptr())) {
            // SW_RESTORE would turn a maximized window that was only hidden into a smaller one.
            let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
            let _ = SetForegroundWindow(h);
        }
    }
}

/// The console window of this very process.
fn own_console_window() -> Option<HWND> {
    let title = wide(CONSOLE_TITLE);
    let mut after = HWND::default();
    unsafe {
        loop {
            let h = FindWindowExW(HWND::default(), after, PCWSTR::null(), PCWSTR(title.as_ptr())).ok()?;
            if h.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == GetCurrentProcessId() {
                return Some(h);
            }
            after = h;
        }
    }
}

/// Hide the console so that only the tray icon stays.
pub fn hide_console_window() {
    if let Some(h) = own_console_window() {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

pub fn console_visible() -> bool {
    own_console_window().map(|h| unsafe { IsWindowVisible(h).as_bool() }).unwrap_or(false)
}

/// Blink the console button on the taskbar without taking the lead away from what they do.
pub fn flash_console_window() {
    let title = wide(CONSOLE_TITLE);
    unsafe {
        if let Ok(h) = FindWindowW(None, PCWSTR(title.as_ptr())) {
            // A console hidden in the tray has no taskbar button to blink, so give it one.
            if !IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_SHOWMINNOACTIVE);
            }
            let info = FLASHWINFO {
                cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                hwnd: h,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&info);
        }
    }
}

/// Ask for the team admin password and close the program if it is right.
pub fn open_exit_prompt() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--exit-prompt").spawn();
    }
}

pub fn open_settings_process() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").spawn();
    }
}

struct Ui {
    node: Arc<Node>,
    is_lead_pc: bool,
    tray_hwnd: HWND,
    tray_data: NOTIFYICONDATAW,
    tray_color: [u8; 3],
    panel_hwnd: HWND,
    panel_dib: Option<Dib>,
    panel_visible: bool,
    panel_dirty: bool,
    actions: Vec<(Rect, Action)>,
    overlays: Vec<Overlay>,
    items: Vec<DrawItem>,
    hints: Vec<Hint>,
    activity: Vec<LeadActivity>,
    policy: Policy,
    font: Option<Font>,
    help_menu: bool,
    help_waiting: bool,
    status: Option<(String, Instant)>,
    hidden_until: Option<Instant>,
    active: bool,
    leads_online: usize,
    timer_on: bool,
}

const PANEL_W: i32 = 300;

impl Ui {
    fn update_tray(&mut self) {
        let color = if self.active {
            [0, 120, 215]
        } else if self.leads_online > 0 {
            [16, 137, 62]
        } else {
            [122, 117, 116]
        };
        if color == self.tray_color {
            return;
        }
        self.tray_color = color;
        self.tray_data.hIcon = tray_icon(color);
        self.tray_data.uFlags = NIF_ICON;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.tray_data);
        }
    }

    fn balloon(&mut self, title: &str, text: &str) {
        let copy = |dst: &mut [u16], s: &str| {
            let w = wide(s);
            let n = w.len().min(dst.len() - 1);
            dst[..n].copy_from_slice(&w[..n]);
            dst[n] = 0;
        };
        copy(&mut self.tray_data.szInfoTitle, title);
        copy(&mut self.tray_data.szInfo, text);
        self.tray_data.uFlags = NIF_INFO;
        self.tray_data.dwInfoFlags = NIIF_INFO;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.tray_data);
        }
    }

    fn on_cmd(&mut self, c: UiCmd) {
        match c {
            UiCmd::Draw { lead, name, color, op } => self.on_draw(lead, name, color, op),
            UiCmd::ClearLead(id) => {
                self.items.retain(|i| i.lead != id);
                self.hints.retain(|h| h.lead != id);
                self.panel_dirty = true;
            }
            UiCmd::Activity(a) => {
                self.activity = a;
                self.panel_dirty = true;
            }
            UiCmd::Hint { lead, name, color, id, text } => {
                self.hints.push(Hint { lead, name, color, id, text });
                self.panel_dirty = true;
            }
            UiCmd::HelpTaken(name) => {
                self.help_waiting = false;
                self.status = Some((format!("{name} іде на допомогу"), Instant::now()));
                self.panel_dirty = true;
            }
            UiCmd::HelpSent => {
                self.help_waiting = true;
                self.status = Some(("Запит надіслано".into(), Instant::now()));
                self.panel_dirty = true;
            }
            UiCmd::HelpNoLeads => {
                self.status = Some(("Немає керівників онлайн".into(), Instant::now()));
                self.panel_dirty = true;
            }
            UiCmd::HelpIncoming(name) => {
                self.balloon("Потрібна допомога", &name);
                flash_console_window();
            }
            UiCmd::Active(a) => {
                self.active = a;
                self.update_tray();
            }
            UiCmd::LeadsOnline(n) => {
                self.leads_online = n;
                self.update_tray();
            }
            UiCmd::WindowsLocked(locked) => {
                if locked {
                    self.items.clear();
                }
            }
            UiCmd::Policy(p) => {
                self.policy = p;
                if !self.policy.allow_hide_key {
                    self.hidden_until = None;
                }
                self.panel_dirty = true;
            }
        }
    }

    fn on_draw(&mut self, lead: String, name: String, color: [u8; 3], op: DrawOp) {
        let now = Instant::now();
        match op {
            DrawOp::Begin { id, tool, monitor, x, y } => {
                self.items.push(DrawItem { lead, name, id, tool, monitor, color, points: vec![[x, y]], stamps: vec![now], ended: None });
            }
            DrawOp::Point { id, x, y } => {
                if let Some(i) = self.items.iter_mut().rev().find(|i| i.lead == lead && i.id == id) {
                    if matches!(i.tool, Tool::Rect | Tool::Arrow) && i.points.len() == 2 {
                        i.points[1] = [x, y];
                    } else {
                        i.points.push([x, y]);
                        i.stamps.push(now);
                    }
                }
            }
            DrawOp::End { id } => {
                if let Some(i) = self.items.iter_mut().rev().find(|i| i.lead == lead && i.id == id) {
                    i.ended = Some(now);
                }
            }
            DrawOp::ClearMine => self.items.retain(|i| i.lead != lead),
            DrawOp::ClearAll => self.items.clear(),
        }
    }

    fn fade_after(&self) -> Duration {
        Duration::from_secs(self.policy.stroke_fade_sec.max(1) as u64)
    }

    fn item_alpha(&self, item: &DrawItem, now: Instant) -> f32 {
        match item.ended {
            None => 1.0,
            Some(e) => {
                let t = now.duration_since(e);
                let after = self.fade_after();
                if t <= after {
                    1.0
                } else {
                    (1.0 - (t - after).as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
                }
            }
        }
    }

    fn ensure_overlays(&mut self) {
        let rects = capture::monitor_rects();
        if self.overlays.len() == rects.len() && self.overlays.iter().zip(&rects).all(|(o, r)| o.rect == *r) {
            return;
        }
        for o in self.overlays.drain(..) {
            unsafe {
                let _ = DestroyWindow(o.hwnd);
                let _ = DeleteDC(o.dib.hdc);
            }
        }
        for r in rects {
            let (Some(pix), Some(dib)) = (Pixmap::new(r.2 as u32, r.3 as u32), Dib::new(r.2, r.3)) else { continue };
            let hwnd = create_layered(w!("TLSOverlay"), r.0, r.1, r.2, r.3, true);
            self.overlays.push(Overlay { hwnd, rect: r, dib, pix, visible: false });
        }
    }

    fn render_overlays(&mut self) {
        let now = Instant::now();
        let fade_total = self.fade_after() + FADE;
        self.items.retain(|i| match (i.tool, i.ended) {
            (Tool::Laser, Some(e)) => now.duration_since(e) < LASER_LIFE,
            (_, Some(e)) => now.duration_since(e) < fade_total,
            _ => true,
        });
        if let Some(until) = self.hidden_until {
            if now >= until {
                self.hidden_until = None;
                self.panel_dirty = true;
            }
        }
        if self.overlays.is_empty() && self.items.is_empty() {
            return;
        }
        self.ensure_overlays();
        let hidden = self.hidden_until.is_some();
        let items = self.items.clone();
        let alphas: Vec<f32> = items.iter().map(|i| self.item_alpha(i, now)).collect();
        let font = self.font.take();
        for (index, o) in self.overlays.iter_mut().enumerate() {
            let mine: Vec<usize> =
                (0..items.len()).filter(|i| items[*i].monitor as usize == index && !hidden).collect();
            if mine.is_empty() {
                if o.visible {
                    unsafe {
                        let _ = ShowWindow(o.hwnd, SW_HIDE);
                    }
                    o.visible = false;
                }
                continue;
            }
            o.pix.fill(Color::TRANSPARENT);
            for i in mine {
                render_item(&mut o.pix, &items[i], alphas[i], now, font.as_ref());
            }
            o.dib.present(o.hwnd, o.rect.0, o.rect.1, &o.pix);
            unsafe {
                if !o.visible {
                    let _ = ShowWindow(o.hwnd, SW_SHOWNOACTIVATE);
                    o.visible = true;
                }
                // Stay above a full-screen remote desktop window.
                let _ = SetWindowPos(o.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        self.font = font;
    }

    fn visible_activity(&self) -> Vec<LeadActivity> {
        self.activity.iter().filter(|a| a.drawing || a.speaking || a.controlling).cloned().collect()
    }

    fn render_panel(&mut self) {
        if !self.panel_dirty {
            return;
        }
        self.panel_dirty = false;
        if let Some((_, t)) = &self.status {
            if t.elapsed() > Duration::from_secs(6) {
                self.status = None;
            }
        }
        let Some(font) = self.font.take() else { return };
        let body = 14.0;
        let wrap_w = PANEL_W as f32 - 40.0;
        let activity = self.visible_activity();
        let mut height = 0.0f32;
        let hint_lines: Vec<Vec<String>> = self.hints.iter().map(|h| font.wrap(&h.text, body, wrap_w)).collect();
        for l in &hint_lines {
            height += 58.0 + l.len() as f32 * 19.0 + 6.0;
        }
        if self.status.is_some() {
            height += 36.0;
        }
        height += activity.len() as f32 * 30.0;
        if !self.items.is_empty() && self.policy.allow_hide_key || self.hidden_until.is_some() {
            height += 32.0;
        }
        if self.help_menu {
            height += (self.policy.help_reasons.len() as f32 + 1.0) * 32.0;
        }
        if self.help_waiting {
            height += 32.0;
        }
        let show_button = self.policy.show_help_button && !self.is_lead_pc;
        if show_button {
            height += 44.0;
        }
        if height <= 0.0 {
            if self.panel_visible {
                unsafe {
                    let _ = ShowWindow(self.panel_hwnd, SW_HIDE);
                }
                self.panel_visible = false;
            }
            self.font = Some(font);
            return;
        }
        let h = height.ceil() as i32 + 2;
        let Some(mut pix) = Pixmap::new(PANEL_W as u32, h as u32) else {
            self.font = Some(font);
            return;
        };
        self.actions.clear();
        let mut y = 0.0f32;
        let mut paint = Paint::default();
        paint.anti_alias = true;
        let fill = |pix: &mut Pixmap, paint: &mut Paint, r: Rect, c: [u8; 4]| {
            paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
            pix.fill_rect(r, paint, Transform::identity(), None);
        };
        for (i, hint) in self.hints.iter().enumerate() {
            let lines = &hint_lines[i];
            let box_h = 58.0 + lines.len() as f32 * 19.0;
            let bg = Rect::from_xywh(0.0, y, PANEL_W as f32, box_h).unwrap();
            fill(&mut pix, &mut paint, bg, [255, 255, 255, 250]);
            let c = hint.color;
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, 5.0, box_h).unwrap(), [c[0], c[1], c[2], 255]);
            font.draw(&mut pix, &hint.name, 15.0, y + 20.0, 13.0, c);
            for (n, line) in lines.iter().enumerate() {
                font.draw(&mut pix, line, 15.0, y + 40.0 + n as f32 * 19.0, body, [30, 30, 30]);
            }
            let btn_y = y + box_h - 24.0;
            font.draw(&mut pix, "Копіювати", 15.0, btn_y + 15.0, 12.0, [0, 120, 215]);
            self.actions.push((Rect::from_xywh(10.0, btn_y, 80.0, 22.0).unwrap(), Action::CopyHint(i)));
            font.draw(&mut pix, "Закрити", PANEL_W as f32 - 62.0, btn_y + 15.0, 12.0, [0, 120, 215]);
            self.actions.push((Rect::from_xywh(PANEL_W as f32 - 70.0, btn_y, 65.0, 22.0).unwrap(), Action::CloseHint(i)));
            y += box_h + 6.0;
        }
        if let Some((text, _)) = &self.status {
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 32.0).unwrap(), [30, 30, 30, 235]);
            font.draw(&mut pix, text, 12.0, y + 21.0, 13.0, [255, 255, 255]);
            y += 36.0;
        }
        for a in &activity {
            let c = crate::util::color_for(&a.id);
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [255, 255, 255, 240]);
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, 5.0, 28.0).unwrap(), [c[0], c[1], c[2], 255]);
            let verb = if a.controlling {
                "керує"
            } else if a.speaking {
                "говорить"
            } else {
                "малює"
            };
            font.draw(&mut pix, &format!("{}: {verb}", a.name), 14.0, y + 19.0, 13.0, [30, 30, 30]);
            y += 30.0;
        }
        if !self.items.is_empty() && self.policy.allow_hide_key || self.hidden_until.is_some() {
            let label = if self.hidden_until.is_some() { "Показати малюнки" } else { "Сховати малюнки" };
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [240, 240, 240, 245]);
            font.draw(&mut pix, label, 12.0, y + 19.0, 13.0, [30, 30, 30]);
            self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), Action::HideDrawings));
            y += 32.0;
        }
        if self.help_menu {
            let mut labels: Vec<(String, String)> = vec![("Без причини".to_string(), String::new())];
            labels.extend(self.policy.help_reasons.iter().map(|r| (r.clone(), r.clone())));
            for (label, reason) in labels {
                fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 30.0).unwrap(), [255, 255, 255, 250]);
                font.draw(&mut pix, &label, 12.0, y + 20.0, 13.0, [30, 30, 30]);
                self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 30.0).unwrap(), Action::HelpReason(reason)));
                y += 32.0;
            }
        }
        if self.help_waiting {
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [240, 240, 240, 245]);
            font.draw(&mut pix, "Скасувати запит", 12.0, y + 19.0, 13.0, [30, 30, 30]);
            self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), Action::HelpCancel));
            y += 32.0;
        }
        if show_button {
            let x = PANEL_W as f32 - 40.0;
            fill(&mut pix, &mut paint, Rect::from_xywh(x, y, 40.0, 40.0).unwrap(), [0, 120, 215, 215]);
            let tw = font.width("?", 22.0);
            font.draw(&mut pix, "?", x + (40.0 - tw) / 2.0, y + 28.0, 22.0, [255, 255, 255]);
            self.actions.push((Rect::from_xywh(x, y, 40.0, 40.0).unwrap(), Action::Help));
        }
        self.font = Some(font);
        let rects = capture::monitor_rects();
        let primary = rects.iter().find(|r| r.0 == 0 && r.1 == 0).or(rects.first()).copied().unwrap_or((0, 0, 1920, 1080));
        let (px, py) = (primary.0 + primary.2 - PANEL_W, primary.1 + (primary.3 - h) * 3 / 4 - h / 4);
        let py = py.max(primary.1);
        let Some(dib) = Dib::new(PANEL_W, h) else { return };
        unsafe {
            let _ = SetWindowPos(self.panel_hwnd, HWND_TOPMOST, px, py, PANEL_W, h, SWP_NOACTIVATE);
        }
        dib.present(self.panel_hwnd, px, py, &pix);
        if let Some(old) = self.panel_dib.replace(dib) {
            unsafe {
                let _ = DeleteDC(old.hdc);
            }
        }
        if !self.panel_visible {
            unsafe {
                let _ = ShowWindow(self.panel_hwnd, SW_SHOWNOACTIVATE);
            }
            self.panel_visible = true;
        }
    }

    fn do_action(&mut self, a: Action) {
        match a {
            Action::CloseHint(i) => {
                if i < self.hints.len() {
                    let h = self.hints.remove(i);
                    self.node.agent_hint_closed(&h.lead, h.id);
                }
            }
            Action::CopyHint(i) => {
                if let Some(h) = self.hints.get(i) {
                    crate::sysutil::clipboard_set(&h.text);
                }
            }
            Action::Help => self.help_menu = !self.help_menu,
            Action::HelpReason(r) => {
                self.help_menu = false;
                self.node.agent_request_help(r);
            }
            Action::HelpCancel => {
                self.help_waiting = false;
                self.node.agent_cancel_help();
            }
            Action::HideDrawings => self.toggle_hidden(),
        }
        self.panel_dirty = true;
    }

    fn toggle_hidden(&mut self) {
        if !self.policy.allow_hide_key {
            return;
        }
        self.hidden_until = match self.hidden_until {
            Some(_) => None,
            None => Some(Instant::now() + Duration::from_secs(self.policy.hide_return_min.max(1) as u64 * 60)),
        };
        self.panel_dirty = true;
    }

    fn tray_menu(&mut self) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            if self.is_lead_pc {
                let _ = AppendMenuW(menu, MF_STRING, 1, w!("Відкрити консоль"));
            }
            let _ = AppendMenuW(menu, MF_STRING, 2, w!("Потрібна допомога"));
            let _ = AppendMenuW(menu, MF_STRING, 3, w!("Налаштування…"));
            let _ = AppendMenuW(menu, MF_STRING, 4, w!("Закрити програму…"));
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = SetForegroundWindow(self.tray_hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY, p.x, p.y, 0, self.tray_hwnd, None);
            let _ = DestroyMenu(menu);
            match cmd.0 {
                1 => restore_console_window(),
                2 => self.node.agent_request_help(String::new()),
                3 => open_settings_process(),
                4 => open_exit_prompt(),
                _ => {}
            }
        }
    }

    fn drain_events(&mut self) {
        let events: Vec<Ev> = EVENTS.with(|v| std::mem::take(&mut *v.borrow_mut()));
        for e in events {
            match e {
                Ev::PanelClick(x, y) => {
                    let hit = self
                        .actions
                        .iter()
                        .find(|(r, _)| (x as f32) >= r.left() && (x as f32) < r.right() && (y as f32) >= r.top() && (y as f32) < r.bottom())
                        .map(|(_, a)| a.clone());
                    if let Some(a) = hit {
                        self.do_action(a);
                    }
                }
                Ev::TrayMenu => self.tray_menu(),
                Ev::TrayOpen => {
                    if self.is_lead_pc {
                        restore_console_window();
                    } else {
                        open_settings_process();
                    }
                }
            }
        }
    }

    fn needs_animation(&self) -> bool {
        !self.items.is_empty() || self.status.is_some() || self.hidden_until.is_some()
    }

    fn update_timer(&mut self) {
        let want = self.needs_animation();
        if want != self.timer_on {
            self.timer_on = want;
            unsafe {
                if want {
                    SetTimer(self.tray_hwnd, TIMER_ANIM, 33, None);
                } else {
                    let _ = KillTimer(self.tray_hwnd, TIMER_ANIM);
                }
            }
        }
    }
}

fn render_item(pix: &mut Pixmap, item: &DrawItem, alpha: f32, now: Instant, font: Option<&Font>) {
    let (w, h) = (pix.width() as f32, pix.height() as f32);
    let map = |p: [f32; 2]| (p[0] * w, p[1] * h);
    let [r, g, b] = item.color;
    let mut paint = Paint::default();
    paint.anti_alias = true;
    let set = |paint: &mut Paint, a: f32| paint.set_color_rgba8(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8);
    let mut stroke = Stroke { line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    let last = *item.points.last().unwrap_or(&[0.0, 0.0]);
    match item.tool {
        Tool::Marker | Tool::MarkerSemi => {
            let semi = item.tool == Tool::MarkerSemi;
            stroke.width = if semi { 20.0 } else { 4.0 };
            set(&mut paint, alpha * if semi { 0.38 } else { 1.0 });
            let mut pb = PathBuilder::new();
            let (x, y) = map(item.points[0]);
            pb.move_to(x, y);
            if item.points.len() == 1 {
                pb.line_to(x + 0.1, y);
            }
            for p in &item.points[1..] {
                let (x, y) = map(*p);
                pb.line_to(x, y);
            }
            if let Some(path) = pb.finish() {
                pix.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        Tool::Rect => {
            if item.points.len() < 2 {
                return;
            }
            let (a, b2) = (map(item.points[0]), map(item.points[1]));
            let (x, y, rw, rh) = (a.0.min(b2.0), a.1.min(b2.1), (a.0 - b2.0).abs().max(1.0), (a.1 - b2.1).abs().max(1.0));
            stroke.width = 3.0;
            set(&mut paint, alpha);
            if let Some(rect) = Rect::from_xywh(x, y, rw, rh) {
                let path = PathBuilder::from_rect(rect);
                pix.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        Tool::Arrow => {
            if item.points.len() < 2 {
                return;
            }
            let (a, tip) = (map(item.points[0]), map(item.points[1]));
            stroke.width = 4.0;
            set(&mut paint, alpha);
            let angle = (tip.1 - a.1).atan2(tip.0 - a.0);
            let head = 18.0;
            let mut pb = PathBuilder::new();
            pb.move_to(a.0, a.1);
            pb.line_to(tip.0, tip.1);
            for off in [0.45f32, -0.45] {
                pb.move_to(tip.0, tip.1);
                pb.line_to(tip.0 - head * (angle + off).cos(), tip.1 - head * (angle + off).sin());
            }
            if let Some(path) = pb.finish() {
                pix.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        Tool::Laser => {
            stroke.width = 5.0;
            for i in 1..item.points.len() {
                let age = now.duration_since(item.stamps[i.min(item.stamps.len() - 1)]);
                if age >= LASER_LIFE {
                    continue;
                }
                set(&mut paint, 1.0 - age.as_secs_f32() / LASER_LIFE.as_secs_f32());
                let (a, b2) = (map(item.points[i - 1]), map(item.points[i]));
                let mut pb = PathBuilder::new();
                pb.move_to(a.0, a.1);
                pb.line_to(b2.0, b2.1);
                if let Some(path) = pb.finish() {
                    pix.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
            if item.ended.is_none() {
                let (x, y) = map(last);
                set(&mut paint, 1.0);
                if let Some(circle) = PathBuilder::from_circle(x, y, 7.0) {
                    pix.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
    }
    // Name tag, so the employee can see who is drawing.
    if let Some(font) = font {
        if alpha > 0.5 {
            let (x, y) = map(last);
            let tw = font.width(&item.name, 13.0);
            let (lx, ly) = ((x + 12.0).min(w - tw - 12.0), (y + 10.0).min(h - 24.0));
            set(&mut paint, 0.95);
            if let Some(tag) = Rect::from_xywh(lx, ly, tw + 10.0, 20.0) {
                pix.fill_rect(tag, &paint, Transform::identity(), None);
            }
            font.draw(pix, &item.name, lx + 5.0, ly + 15.0, 13.0, [255, 255, 255]);
        }
    }
}

/// Runs the employee interface. Never returns.
pub fn run(node: Arc<Node>, rx: Receiver<UiCmd>, handle: UiHandle, is_lead_pc: bool) -> ! {
    unsafe {
        handle.thread.store(GetCurrentThreadId(), Ordering::SeqCst);
        register_class(w!("TLSOverlay"), Some(overlay_proc));
        register_class(w!("TLSPanel"), Some(panel_proc));
        register_class(w!("TLSTray"), Some(tray_proc));
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let tray_hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("TLSTray"), w!("TLS"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, hinstance, None)
            .unwrap_or_default();
        let color = [122, 117, 116];
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: tray_hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: tray_icon(color),
            ..Default::default()
        };
        let tip = wide("TL Supporter");
        data.szTip[..tip.len()].copy_from_slice(&tip);
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
        let _ = RegisterHotKey(None, HOTKEY_HIDE, MOD_CONTROL | MOD_ALT, 'H' as u32);
        let panel_hwnd = create_layered(w!("TLSPanel"), 0, 0, PANEL_W, 40, false);
        let policy = node.state.read().directory.policy.clone();
        let mut ui = Ui {
            node,
            is_lead_pc,
            tray_hwnd,
            tray_data: data,
            tray_color: color,
            panel_hwnd,
            panel_dib: None,
            panel_visible: false,
            panel_dirty: true,
            actions: Vec::new(),
            overlays: Vec::new(),
            items: Vec::new(),
            hints: Vec::new(),
            activity: Vec::new(),
            policy,
            font: Font::load(),
            help_menu: false,
            help_waiting: false,
            status: None,
            hidden_until: None,
            active: false,
            leads_online: 0,
            timer_on: false,
        };
        ui.render_panel();
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                std::process::exit(0);
            }
            if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_HIDE {
                ui.toggle_hidden();
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            while let Ok(c) = rx.try_recv() {
                ui.on_cmd(c);
            }
            ui.drain_events();
            if ui.status.is_some() {
                ui.panel_dirty = true;
            }
            let was_empty = ui.items.is_empty();
            ui.render_overlays();
            if was_empty != ui.items.is_empty() {
                ui.panel_dirty = true;
            }
            ui.render_panel();
            ui.update_timer();
        }
    }
}
