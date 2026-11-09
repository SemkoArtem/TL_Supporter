//! What the employee sees: drawings over all windows, a small panel and the tray icon.
//! Both windows are hidden from screen capture and never take keyboard focus.

use crate::capture;
use crate::model::Policy;
use crate::node::Node;
use crate::proto::{DrawOp, LeadActivity, Tool};
use crate::util::wide;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    Consent { lead: String, name: String },
    Pointer { lead: String, name: String, color: [u8; 3], monitor: u8, x: f32, y: f32, on: bool, size: f32 },
    Tap { color: [u8; 3], monitor: u8, x: f32, y: f32 },
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
    ended: Option<Instant>,
    width: f32,
    alpha: f32,
}

#[derive(Clone)]
struct PointerMark {
    lead: String,
    name: String,
    color: [u8; 3],
    monitor: u8,
    x: f32,
    y: f32,
    size: f32,
    last: Instant,
}

#[derive(Clone)]
struct TapMark {
    color: [u8; 3],
    monitor: u8,
    x: f32,
    y: f32,
    start: Instant,
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
    /// 0 unread, 1 read, 2 accepted, 3 rejected, 4 ignored.
    state: u8,
    /// While set, the hint is shown as a card that appeared at this moment.
    toast_from: Option<Instant>,
}

#[derive(Clone)]
enum Action {
    Ack(usize, u8),
    OpenHistory,
    Consent(usize, bool),
    CopyHint(usize),
    Help,
    HelpReason(String),
    HelpCancel,
    HideDrawings,
}

const FADE: Duration = Duration::from_millis(1500);
const TAP_LIFE: Duration = Duration::from_millis(700);
const POINTER_LIFE: Duration = Duration::from_secs(2);
const HINT_SLIDE: Duration = Duration::from_millis(350);
const HISTORY_ROWS: usize = 6;

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

enum Ev {
    PanelClick(i32, i32),
    PanelWheel(i32),
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
        WM_MOUSEWHEEL => {
            push_event(Ev::PanelWheel(((w.0 >> 16) & 0xFFFF) as i16 as i32));
            LRESULT(0)
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

unsafe extern "system" fn tray_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m == WM_TRAY {
        let code = (l.0 & 0xFFFF) as u32;
        if code == WM_RBUTTONUP {
            push_event(Ev::TrayMenu);
        } else if code == WM_LBUTTONUP {
            push_event(Ev::TrayOpen);
        }
        // The shell delivers this without the message queue, so GetMessage would not return
        // and the click would wait for some later, unrelated message.
        let _ = PostMessageW(h, WM_WAKE, WPARAM(0), LPARAM(0));
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
    match own_console_window() {
        Some(h) => bring_up(h),
        None => restore_window(CONSOLE_TITLE),
    }
}

/// Show a window of this program and bring it to the front.
pub fn restore_window(title: &str) {
    let title = wide(title);
    unsafe {
        if let Ok(h) = FindWindowW(None, PCWSTR(title.as_ptr())) {
            bring_up(h);
        }
    }
}

fn bring_up(h: HWND) {
    unsafe {
        // SW_RESTORE would turn a maximized window that was only hidden into a smaller one.
        let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
        let _ = SetForegroundWindow(h);
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
    own_console_window().map(|h| unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() }).unwrap_or(false)
}

/// A taskbar button is shown on purpose while a help request is waiting to be noticed.
static ATTENTION: AtomicBool = AtomicBool::new(false);

/// Was the console just minimized, so that it should move to the tray?
pub fn console_wants_tray() -> bool {
    let Some(h) = own_console_window() else { return false };
    unsafe {
        let visible = IsWindowVisible(h).as_bool();
        if !IsIconic(h).as_bool() {
            if visible {
                ATTENTION.store(false, Ordering::SeqCst);
            }
            return false;
        }
        visible && !ATTENTION.load(Ordering::SeqCst)
    }
}

/// Blink the console button on the taskbar without taking the lead away from what they do.
pub fn flash_console_window() {
    let title = wide(CONSOLE_TITLE);
    unsafe {
        if let Ok(h) = FindWindowW(None, PCWSTR(title.as_ptr())) {
            // A console hidden in the tray has no taskbar button to blink, so give it one.
            if !IsWindowVisible(h).as_bool() {
                ATTENTION.store(true, Ordering::SeqCst);
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
    history_open: bool,
    history_top: usize,
    consent: Vec<(String, String)>,
    pointers: Vec<PointerMark>,
    taps: Vec<TapMark>,
    panel_rect: (i32, i32, i32, i32),
    /// Vertical position and height of each hint card inside the panel.
    toast_area: Vec<(f32, f32)>,
    last_refresh: Instant,
    lbutton_was: bool,
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
                self.pointers.retain(|p| p.lead != id);
                self.consent.retain(|c| c.0 != id);
                // The conversation stays readable after the lead has left.
                for h in self.hints.iter_mut().filter(|h| h.lead == id) {
                    h.toast_from = None;
                }
                self.panel_dirty = true;
            }
            UiCmd::Activity(a) => {
                self.activity = a;
                self.panel_dirty = true;
            }
            UiCmd::Hint { lead, name, color, id, text } => {
                let open = self.history_open;
                if open {
                    self.node.agent_hint_ack(&lead, id, 1);
                }
                self.hints.push(Hint { lead, name, color, id, text, state: u8::from(open), toast_from: (!open).then(Instant::now) });
                if self.hints.len() > 300 {
                    self.hints.remove(0);
                }
                self.panel_dirty = true;
            }
            UiCmd::Consent { lead, name } => {
                self.consent.retain(|c| c.0 != lead);
                self.consent.push((lead, name));
                self.panel_dirty = true;
            }
            UiCmd::Pointer { lead, name, color, monitor, x, y, on, size } => {
                self.pointers.retain(|p| p.lead != lead);
                if on {
                    self.pointers.push(PointerMark { lead, name, color, monitor, x, y, size, last: Instant::now() });
                }
            }
            UiCmd::Tap { color, monitor, x, y } => self.taps.push(TapMark { color, monitor, x, y, start: Instant::now() }),
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
            DrawOp::Begin { id, tool, monitor, x, y, width, alpha } => {
                self.items.push(DrawItem { lead, name, id, tool, monitor, color, points: vec![[x, y]], ended: None, width, alpha });
            }
            DrawOp::Point { id, x, y } => {
                if let Some(i) = self.items.iter_mut().rev().find(|i| i.lead == lead && i.id == id) {
                    if matches!(i.tool, Tool::Rect | Tool::Arrow) && i.points.len() == 2 {
                        i.points[1] = [x, y];
                    } else {
                        i.points.push([x, y]);
                    }
                }
            }
            DrawOp::End { id } => {
                if let Some(i) = self.items.iter_mut().rev().find(|i| i.lead == lead && i.id == id) {
                    i.ended = Some(now);
                }
            }
            DrawOp::Cancel { id } => self.items.retain(|i| !(i.lead == lead && i.id == id)),
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
        self.items.retain(|i| i.ended.map(|e| now.duration_since(e) < fade_total).unwrap_or(true));
        self.pointers.retain(|p| now.duration_since(p.last) < POINTER_LIFE);
        self.taps.retain(|t| now.duration_since(t.start) < TAP_LIFE);
        if let Some(until) = self.hidden_until {
            if now >= until {
                self.hidden_until = None;
                self.panel_dirty = true;
            }
        }
        if self.overlays.is_empty() && self.items.is_empty() && self.pointers.is_empty() && self.taps.is_empty() {
            return;
        }
        self.ensure_overlays();
        let hidden = self.hidden_until.is_some();
        let items = self.items.clone();
        let pointers = self.pointers.clone();
        let taps = self.taps.clone();
        let alphas: Vec<f32> = items.iter().map(|i| self.item_alpha(i, now)).collect();
        let font = self.font.take();
        for (index, o) in self.overlays.iter_mut().enumerate() {
            let mine: Vec<usize> =
                (0..items.len()).filter(|i| items[*i].monitor as usize == index && !hidden).collect();
            let my_pointers: Vec<&PointerMark> = pointers.iter().filter(|p| p.monitor as usize == index && !hidden).collect();
            let my_taps: Vec<&TapMark> = taps.iter().filter(|t| t.monitor as usize == index && !hidden).collect();
            if mine.is_empty() && my_pointers.is_empty() && my_taps.is_empty() {
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
                render_item(&mut o.pix, &items[i], alphas[i], font.as_ref());
            }
            for t in my_taps {
                render_tap(&mut o.pix, t, now);
            }
            for p in my_pointers {
                render_pointer(&mut o.pix, p, font.as_ref());
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
        let now = Instant::now();
        let hold = self.hint_hold();
        // Cards still on screen, with how far each has already slid away.
        let toasts: Vec<(usize, f32)> = if self.history_open {
            Vec::new()
        } else {
            self.hints
                .iter()
                .enumerate()
                .filter_map(|(i, h)| {
                    let e = now.duration_since(h.toast_from?);
                    let p = if e <= hold { 0.0 } else { (e - hold).as_secs_f32() / HINT_SLIDE.as_secs_f32() };
                    Some((i, p.min(1.0) * p.min(1.0) * PANEL_W as f32))
                })
                .collect()
        };
        let history_range = {
            let end = self.hints.len() - self.history_top.min(self.hints.len());
            end.saturating_sub(HISTORY_ROWS)..end
        };
        let row_h = |lines: usize, state: u8| 30.0 + lines as f32 * 19.0 + if state <= 1 { 26.0 } else { 0.0 } + 4.0;
        height += self.consent.len() as f32 * 72.0;
        for (i, _) in &toasts {
            height += 62.0 + hint_lines[*i].len() as f32 * 19.0 + 6.0;
        }
        if self.history_open {
            height += 30.0;
            for i in history_range.clone() {
                height += row_h(hint_lines[i].len(), self.hints[i].state);
            }
        }
        if !self.hints.is_empty() {
            height += 32.0;
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
        for (i, (_, name)) in self.consent.iter().enumerate() {
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 66.0).unwrap(), [31, 31, 31, 250]);
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, 5.0, 66.0).unwrap(), [0, 120, 215, 255]);
            font.draw(&mut pix, &format!("{name} хоче підключитись"), 15.0, y + 22.0, 13.0, [242, 242, 242]);
            font.draw(&mut pix, "Дозволити", 15.0, y + 52.0, 13.0, [108, 203, 95]);
            self.actions.push((Rect::from_xywh(10.0, y + 36.0, 90.0, 26.0).unwrap(), Action::Consent(i, true)));
            font.draw(&mut pix, "Відмовити", 120.0, y + 52.0, 13.0, [255, 120, 130]);
            self.actions.push((Rect::from_xywh(115.0, y + 36.0, 90.0, 26.0).unwrap(), Action::Consent(i, false)));
            y += 72.0;
        }
        self.toast_area.clear();
        for (i, offset) in &toasts {
            let (i, x0) = (*i, *offset);
            let hint = &self.hints[i];
            let lines = &hint_lines[i];
            let box_h = 62.0 + lines.len() as f32 * 19.0;
            self.toast_area.push((y, box_h));
            let bg = Rect::from_xywh(x0, y, PANEL_W as f32, box_h).unwrap();
            fill(&mut pix, &mut paint, bg, [31, 31, 31, 245]);
            let c = hint.color;
            fill(&mut pix, &mut paint, Rect::from_xywh(x0, y, 5.0, box_h).unwrap(), [c[0], c[1], c[2], 255]);
            font.draw(&mut pix, &hint.name, x0 + 15.0, y + 20.0, 13.0, c);
            for (n, line) in lines.iter().enumerate() {
                font.draw(&mut pix, line, x0 + 15.0, y + 40.0 + n as f32 * 19.0, body, [242, 242, 242]);
            }
            draw_ack_row(&font, &mut pix, &mut self.actions, x0, y + box_h - 24.0, i);
            y += box_h + 6.0;
        }
        if self.history_open {
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 30.0).unwrap(), [51, 51, 51, 250]);
            font.draw(&mut pix, "Історія", 12.0, y + 20.0, 13.0, [242, 242, 242]);
            if self.hints.len() > HISTORY_ROWS {
                font.draw(&mut pix, "коліщатком вгору: старіші", 118.0, y + 20.0, 11.0, [170, 170, 170]);
            }
            y += 30.0;
            for i in history_range {
                let hint = &self.hints[i];
                let lines = &hint_lines[i];
                let box_h = row_h(lines.len(), hint.state) - 4.0;
                fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, box_h).unwrap(), [31, 31, 31, 250]);
                let c = hint.color;
                fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, 5.0, box_h).unwrap(), [c[0], c[1], c[2], 255]);
                font.draw(&mut pix, &hint.name, 15.0, y + 20.0, 13.0, c);
                let (label, color) = match hint.state {
                    0 => ("Нове", [255, 120, 130]),
                    2 => ("Прийнято", [108, 203, 95]),
                    3 => ("Відхилено", [255, 120, 130]),
                    4 => ("Ігнор", [170, 170, 170]),
                    _ => ("Прочитано", [170, 170, 170]),
                };
                let tw = font.width(label, 12.0);
                font.draw(&mut pix, label, PANEL_W as f32 - tw - 10.0, y + 20.0, 12.0, color);
                for (n, line) in lines.iter().enumerate() {
                    font.draw(&mut pix, line, 15.0, y + 40.0 + n as f32 * 19.0, body, [242, 242, 242]);
                }
                if hint.state <= 1 {
                    draw_ack_row(&font, &mut pix, &mut self.actions, 0.0, y + box_h - 24.0, i);
                }
                y += box_h + 4.0;
            }
        }
        if let Some((text, _)) = &self.status {
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 32.0).unwrap(), [30, 30, 30, 235]);
            font.draw(&mut pix, text, 12.0, y + 21.0, 13.0, [255, 255, 255]);
            y += 36.0;
        }
        for a in &activity {
            let c = crate::util::color_for(&a.id);
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [31, 31, 31, 235]);
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, 5.0, 28.0).unwrap(), [c[0], c[1], c[2], 255]);
            let verb = if a.controlling {
                "керує"
            } else if a.speaking {
                "говорить"
            } else {
                "малює"
            };
            font.draw(&mut pix, &format!("{}: {verb}", a.name), 14.0, y + 19.0, 13.0, [242, 242, 242]);
            y += 30.0;
        }
        if !self.items.is_empty() && self.policy.allow_hide_key || self.hidden_until.is_some() {
            let label = if self.hidden_until.is_some() { "Показати малюнки" } else { "Сховати малюнки" };
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [51, 51, 51, 245]);
            font.draw(&mut pix, label, 12.0, y + 19.0, 13.0, [242, 242, 242]);
            self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), Action::HideDrawings));
            y += 32.0;
        }
        if self.help_menu {
            let mut labels: Vec<(String, String)> = vec![("Без причини".to_string(), String::new())];
            labels.extend(self.policy.help_reasons.iter().map(|r| (r.clone(), r.clone())));
            for (label, reason) in labels {
                fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 30.0).unwrap(), [31, 31, 31, 245]);
                font.draw(&mut pix, &label, 12.0, y + 20.0, 13.0, [242, 242, 242]);
                self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 30.0).unwrap(), Action::HelpReason(reason)));
                y += 32.0;
            }
        }
        if self.help_waiting {
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [51, 51, 51, 245]);
            font.draw(&mut pix, "Скасувати запит", 12.0, y + 19.0, 13.0, [242, 242, 242]);
            self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), Action::HelpCancel));
            y += 32.0;
        }
        if !self.hints.is_empty() {
            let unread = self.hints.iter().filter(|h| h.state == 0).count();
            fill(&mut pix, &mut paint, Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), [51, 51, 51, 245]);
            font.draw(&mut pix, "Історія", 12.0, y + 19.0, 13.0, [242, 242, 242]);
            if unread > 0 {
                paint.set_color_rgba8(232, 17, 35, 255);
                if let Some(c) = PathBuilder::from_circle(PANEL_W as f32 - 20.0, y + 14.0, 10.0) {
                    pix.fill_path(&c, &paint, FillRule::Winding, Transform::identity(), None);
                }
                let label = unread.min(99).to_string();
                let tw = font.width(&label, 12.0);
                font.draw(&mut pix, &label, PANEL_W as f32 - 20.0 - tw / 2.0, y + 18.0, 12.0, [255, 255, 255]);
            }
            self.actions.push((Rect::from_xywh(0.0, y, PANEL_W as f32, 28.0).unwrap(), Action::OpenHistory));
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
        self.panel_rect = (px, py, PANEL_W, h);
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
            Action::Ack(i, state) => {
                if let Some(h) = self.hints.get_mut(i) {
                    h.state = state;
                    h.toast_from = None;
                    self.node.agent_hint_ack(&h.lead, h.id, state);
                }
            }
            Action::OpenHistory => {
                self.history_open = !self.history_open;
                self.history_top = 0;
                if self.history_open {
                    for h in self.hints.iter_mut() {
                        h.toast_from = None;
                        if h.state == 0 {
                            h.state = 1;
                            self.node.agent_hint_ack(&h.lead, h.id, 1);
                        }
                    }
                }
            }
            Action::Consent(i, allow) => {
                if i < self.consent.len() {
                    let (lead, _) = self.consent.remove(i);
                    self.node.agent_consent(&lead, allow);
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

    fn hint_hold(&self) -> Duration {
        Duration::from_secs(self.policy.hint_show_sec.max(10) as u64)
    }

    /// Cards stay while the cursor is on them, then slide away. A click elsewhere closes the list.
    fn tick_hints(&mut self) {
        let now = Instant::now();
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let (x, y, w, h) = self.panel_rect;
        let over = self.panel_visible && p.x >= x && p.x < x + w && p.y >= y && p.y < y + h;
        // Only a cursor on the card itself keeps it, a mouse parked nearby must not.
        let over_card = over && self.toast_area.iter().any(|(top, height)| (p.y - y) as f32 >= *top && ((p.y - y) as f32) < top + height);
        let hold = self.hint_hold();
        let mut changed = false;
        for hint in self.hints.iter_mut() {
            let Some(t) = hint.toast_from else { continue };
            let e = now.duration_since(t);
            if e <= hold {
                continue;
            }
            if over_card {
                hint.toast_from = Some(now - hold);
            } else if e >= hold + HINT_SLIDE {
                hint.toast_from = None;
            }
            changed = true;
        }
        let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
        if down && !self.lbutton_was && !over && (self.history_open || self.help_menu) {
            self.history_open = false;
            self.help_menu = false;
            changed = true;
        }
        self.lbutton_was = down;
        // Redraw now and then, a picture left from a locked Windows must not stay on screen.
        if self.panel_visible && now.duration_since(self.last_refresh) >= Duration::from_secs(1) {
            self.last_refresh = now;
            changed = true;
        }
        if changed {
            self.panel_dirty = true;
        }
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
                let _ = AppendMenuW(menu, MF_STRING, 1, w!("Розгорнути"));
            } else {
                let _ = AppendMenuW(menu, MF_STRING, 2, w!("Потрібна допомога"));
            }
            let _ = AppendMenuW(menu, MF_STRING, 3, w!("Налаштування…"));
            if !self.is_lead_pc {
                let muted = crate::audio::load_voice_cfg().1;
                let _ = AppendMenuW(menu, MF_STRING, 6, if muted { w!("Увімкнути звук керівника") } else { w!("Вимкнути звук керівника") });
            }
            let _ = AppendMenuW(menu, MF_STRING, 5, w!("Перезапустити"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, 4, w!("Закрити програму…"));
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            // Without the foreground window a click elsewhere would not close the menu.
            let _ = SetForegroundWindow(self.tray_hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, self.tray_hwnd, None);
            let _ = PostMessageW(self.tray_hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            match cmd.0 {
                1 => restore_console_window(),
                2 => self.node.agent_request_help(String::new()),
                3 => open_settings_process(),
                4 => open_exit_prompt(),
                6 => {
                    let (volume, muted) = crate::audio::load_voice_cfg();
                    crate::audio::save_voice_cfg(volume, !muted);
                }
                5 => {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &self.tray_data);
                    crate::service::restart_program();
                }
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
                Ev::PanelWheel(delta) => {
                    let limit = self.hints.len().saturating_sub(HISTORY_ROWS) as i32;
                    self.history_top = (self.history_top as i32 + delta.signum()).clamp(0, limit.max(0)) as usize;
                    self.panel_dirty = true;
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
        !self.items.is_empty()
            || self.status.is_some()
            || self.hidden_until.is_some()
            || !self.pointers.is_empty()
            || !self.taps.is_empty()
            || self.history_open
            || self.help_menu
            || self.hints.iter().any(|h| h.toast_from.is_some())
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

/// The row of answers under a hint.
fn draw_ack_row(font: &Font, pix: &mut Pixmap, actions: &mut Vec<(Rect, Action)>, x0: f32, y: f32, i: usize) {
    for (label, state, color, dx, w) in [
        ("Прийняв", 2u8, [108, 203, 95], 15.0f32, 66.0f32),
        ("Відхилив", 3, [255, 120, 130], 88.0, 70.0),
        ("Ігнор", 4, [190, 190, 190], 166.0, 46.0),
    ] {
        font.draw(pix, label, x0 + dx, y + 15.0, 12.0, color);
        if let Some(r) = Rect::from_xywh(x0 + dx - 5.0, y - 2.0, w, 24.0) {
            actions.push((r, Action::Ack(i, state)));
        }
    }
    font.draw(pix, "Копія", x0 + 226.0, y + 15.0, 12.0, [96, 205, 255]);
    if let Some(r) = Rect::from_xywh(x0 + 221.0, y - 2.0, 55.0, 24.0) {
        actions.push((r, Action::CopyHint(i)));
    }
}

/// Short rings at the place the lead clicked.
fn render_tap(pix: &mut Pixmap, tap: &TapMark, now: Instant) {
    let (w, h) = (pix.width() as f32, pix.height() as f32);
    let (x, y) = (tap.x * w, tap.y * h);
    let progress = (now.duration_since(tap.start).as_secs_f32() / TAP_LIFE.as_secs_f32()).clamp(0.0, 1.0);
    let [r, g, b] = tap.color;
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color_rgba8(r, g, b, ((1.0 - progress) * 230.0) as u8);
    let stroke = Stroke { width: 2.5, ..Default::default() };
    for ring in 1..=3 {
        let radius = 6.0 + progress * 12.0 * ring as f32;
        if let Some(path) = PathBuilder::from_circle(x, y, radius) {
            pix.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// The glowing dot that shows where the lead is pointing. It leaves no trace.
fn render_pointer(pix: &mut Pixmap, p: &PointerMark, font: Option<&Font>) {
    let (w, h) = (pix.width() as f32, pix.height() as f32);
    let (x, y) = (p.x * w, p.y * h);
    let [r, g, b] = p.color;
    let mut paint = Paint::default();
    paint.anti_alias = true;
    let size = p.size.max(4.0);
    for (scale, alpha) in [(2.4f32, 50u8), (1.6, 110), (1.0, 255)] {
        paint.set_color_rgba8(r, g, b, alpha);
        if let Some(c) = PathBuilder::from_circle(x, y, size * scale) {
            pix.fill_path(&c, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
    if let Some(font) = font {
        let tw = font.width(&p.name, 13.0);
        let (lx, ly) = ((x + size * 2.0).min(w - tw - 12.0), (y + size * 2.0).min(h - 24.0));
        paint.set_color_rgba8(r, g, b, 240);
        if let Some(tag) = Rect::from_xywh(lx, ly, tw + 10.0, 20.0) {
            pix.fill_rect(tag, &paint, Transform::identity(), None);
        }
        font.draw(pix, &p.name, lx + 5.0, ly + 15.0, 13.0, [255, 255, 255]);
    }
}

fn render_item(pix: &mut Pixmap, item: &DrawItem, alpha: f32, font: Option<&Font>) {
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
            stroke.width = if item.width > 0.0 { item.width } else if semi { 20.0 } else { 4.0 };
            set(&mut paint, alpha * if item.alpha > 0.0 { item.alpha } else if semi { 0.38 } else { 1.0 });
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
            stroke.width = if item.width > 0.0 { item.width } else { 3.0 };
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
            stroke.width = if item.width > 0.0 { item.width } else { 4.0 };
            set(&mut paint, alpha);
            let angle = (tip.1 - a.1).atan2(tip.0 - a.0);
            let head = 14.0 + stroke.width * 1.5;
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
        Tool::Laser => {}
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
            history_open: false,
            history_top: 0,
            consent: Vec::new(),
            pointers: Vec::new(),
            taps: Vec::new(),
            panel_rect: (0, 0, 0, 0),
            toast_area: Vec::new(),
            last_refresh: Instant::now(),
            lbutton_was: false,
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
        // A slow tick keeps the panel honest even when nothing else happens.
        SetTimer(tray_hwnd, 2, 1000, None);
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
            ui.tick_hints();
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
