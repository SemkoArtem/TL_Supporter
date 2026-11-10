//! Windows helpers: input injection, clipboard, lock detection and sounds.

use crate::proto::InputEv;
use crate::util::wide;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::StationsAndDesktops::*;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// True while Windows is on the lock screen or a secure desktop.
pub fn is_desktop_locked() -> bool {
    unsafe {
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(0x0100)) {
            Ok(h) => {
                let switched = SwitchDesktop(h).is_ok();
                let _ = CloseDesktop(h);
                !switched
            }
            Err(_) => true,
        }
    }
}

thread_local! {
    static ATTACHED_DESKTOP: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// Move this thread onto whichever desktop currently receives input, including the
/// sign-in screen and administrator prompts. Those need a system-level process.
pub fn attach_input_desktop() -> bool {
    unsafe {
        const GENERIC_ALL: u32 = 0x1000_0000;
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(GENERIC_ALL)) else {
            return false;
        };
        if SetThreadDesktop(desktop).is_err() {
            let _ = CloseDesktop(desktop);
            return false;
        }
        // A desktop handle cannot be closed while a thread uses it, so release the previous one now.
        let previous = ATTACHED_DESKTOP.with(|c| c.replace(desktop.0 as isize));
        if previous != 0 {
            let _ = CloseDesktop(HDESK(previous as *mut _));
        }
        true
    }
}

/// Same as pressing Ctrl+Alt+Del. Works only from a system-level process.
pub fn send_sas() {
    type SendSasFn = unsafe extern "system" fn(i32);
    unsafe {
        let Ok(module) = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::w!("sas.dll")) else {
            crate::logi!("sas.dll is not available");
            return;
        };
        if let Some(f) = windows::Win32::System::LibraryLoader::GetProcAddress(module, windows::core::s!("SendSAS")) {
            let send: SendSasFn = std::mem::transmute(f);
            send(0);
        }
    }
}

pub fn idle_ms() -> u64 {
    unsafe {
        let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut info).as_bool() {
            GetTickCount().wrapping_sub(info.dwTime) as u64
        } else {
            0
        }
    }
}

/// Windows reads the sound from memory while it plays, so the buffer must stay alive.
static CHIME: parking_lot::Mutex<Vec<u8>> = parking_lot::const_mutex(Vec::new());

/// Two short notes as a WAV file in memory, so that the volume can be chosen.
fn chime_wav(volume: f32) -> Vec<u8> {
    const RATE: u32 = 22_050;
    let mut samples: Vec<i16> = Vec::new();
    for (freq, ms) in [(660.0f32, 110u32), (880.0, 150)] {
        let count = RATE * ms / 1000;
        for i in 0..count {
            let t = i as f32 / RATE as f32;
            let fade = 1.0 - i as f32 / count as f32;
            samples.push(((t * freq * std::f32::consts::TAU).sin() * fade * volume * 0.35 * 32767.0) as i16);
        }
    }
    let bytes = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&bytes.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

pub fn play_connect_sound(volume: f32) {
    if volume <= 0.01 {
        return;
    }
    let mut buffer = CHIME.lock();
    unsafe {
        // Stops anything still playing from the old buffer.
        let _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
        *buffer = chime_wav(volume);
        let _ = PlaySoundW(PCWSTR(buffer.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

pub fn key_is_down(vk: u32) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn clipboard_get() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let global = HGLOBAL(h.0);
            let p = GlobalLock(global) as *const u16;
            if !p.is_null() {
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                let _ = GlobalUnlock(global);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

pub fn clipboard_set(text: &str) {
    unsafe {
        // Windows refuses to store data when the clipboard has no owner window.
        let owner = CreateWindowExW(WINDOW_EX_STYLE(0), windows::core::w!("STATIC"), PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, HWND_MESSAGE, None, None, None).unwrap_or_default();
        // Another program may be holding the clipboard for a moment.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(owner).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            crate::logi!("clipboard is busy, text not stored");
            let _ = DestroyWindow(owner);
            return;
        }
        let _ = EmptyClipboard();
        let w = wide(text);
        if let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(global) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(global);
                if let Err(e) = SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(global.0)) {
                    crate::logi!("clipboard write failed: {e}");
                }
            }
        }
        let _ = CloseClipboard();
        let _ = DestroyWindow(owner);
    }
}

#[cfg(test)]
mod clipboard_tests {
    /// Touches the real clipboard, so it runs only on request and puts the old text back.
    #[test]
    #[ignore]
    fn text_written_can_be_read_back() {
        let saved = super::clipboard_get();
        super::clipboard_set("TL Supporter перевірка буфера");
        let read = super::clipboard_get();
        if let Some(old) = saved {
            super::clipboard_set(&old);
        }
        assert_eq!(read.as_deref(), Some("TL Supporter перевірка буфера"));
    }
}

/// Keys and mouse buttons a lead is holding down on this PC. If the lead goes away they must
/// be let go, otherwise the employee is left with a stuck key.
#[derive(Default)]
pub struct HeldInput {
    keys: std::collections::HashSet<u16>,
    buttons: std::collections::HashSet<u8>,
}

impl HeldInput {
    pub fn note(&mut self, ev: InputEv) {
        match ev {
            InputEv::Key { vk, down: true } => {
                self.keys.insert(vk);
            }
            InputEv::Key { vk, down: false } => {
                self.keys.remove(&vk);
            }
            InputEv::Button { button, down: true } => {
                self.buttons.insert(button);
            }
            InputEv::Button { button, down: false } => {
                self.buttons.remove(&button);
            }
            _ => {}
        }
    }

    pub fn release_all(&mut self) {
        for vk in self.keys.drain() {
            inject(InputEv::Key { vk, down: false }, (0, 0, 1, 1));
        }
        for button in self.buttons.drain() {
            inject(InputEv::Button { button, down: false }, (0, 0, 1, 1));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Convert a position on a monitor to SendInput's 0..65535 virtual desktop range.
pub fn to_absolute(rect: (i32, i32, i32, i32), virt: (i32, i32, i32, i32), x: f32, y: f32) -> (i32, i32) {
    let px = rect.0 as f32 + x.clamp(0.0, 1.0) * (rect.2 - 1).max(1) as f32;
    let py = rect.1 as f32 + y.clamp(0.0, 1.0) * (rect.3 - 1).max(1) as f32;
    let ax = (px - virt.0 as f32) * 65535.0 / (virt.2 - 1).max(1) as f32;
    let ay = (py - virt.1 as f32) * 65535.0 / (virt.3 - 1).max(1) as f32;
    (ax.round() as i32, ay.round() as i32)
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xA3 | 0xA5 | 0x6F | 0x90)
}

/// Apply one input event on this PC. Keys are sent as scan codes so they also reach the
/// remote desktop window.
pub fn inject(ev: InputEv, rect: (i32, i32, i32, i32)) {
    match ev {
        InputEv::Move { x, y } => {
            let (ax, ay) = to_absolute(rect, virtual_screen(), x, y);
            send(&[mouse_input(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK)]);
        }
        InputEv::Button { button, down } => {
            let flags = match (button, down) {
                (0, true) => MOUSEEVENTF_LEFTDOWN,
                (0, false) => MOUSEEVENTF_LEFTUP,
                (1, true) => MOUSEEVENTF_RIGHTDOWN,
                (1, false) => MOUSEEVENTF_RIGHTUP,
                (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                _ => MOUSEEVENTF_MIDDLEUP,
            };
            send(&[mouse_input(0, 0, 0, flags)]);
        }
        InputEv::Wheel { dy } => send(&[mouse_input(0, 0, (dy * 120.0) as i32, MOUSEEVENTF_WHEEL)]),
        InputEv::Key { vk, down } => {
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
            let mut flags = KEYEVENTF_SCANCODE;
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            if is_extended(vk) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            send(&[key_input(0, scan, flags)]);
        }
        InputEv::Text { ch } => {
            let Some(c) = char::from_u32(ch) else { return };
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                send(&[
                    key_input(0, *u, KEYEVENTF_UNICODE),
                    key_input(0, *u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                ]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_input_follows_presses_and_releases() {
        let mut held = HeldInput::default();
        held.note(InputEv::Key { vk: 0x41, down: true });
        held.note(InputEv::Button { button: 0, down: true });
        assert!(!held.is_empty());
        held.note(InputEv::Key { vk: 0x41, down: false });
        held.note(InputEv::Button { button: 0, down: false });
        assert!(held.is_empty());
    }

    #[test]
    fn absolute_coordinates_cover_the_desktop() {
        let virt = (0, 0, 1920, 1080);
        let rect = (0, 0, 1920, 1080);
        assert_eq!(to_absolute(rect, virt, 0.0, 0.0), (0, 0));
        assert_eq!(to_absolute(rect, virt, 1.0, 1.0), (65535, 65535));
    }

    #[test]
    fn second_monitor_is_offset() {
        let virt = (0, 0, 3840, 1080);
        let rect = (1920, 0, 1920, 1080);
        let (x, _) = to_absolute(rect, virt, 0.0, 0.5);
        assert!((32000..33000).contains(&x));
    }

    #[test]
    fn out_of_range_is_clamped() {
        let virt = (0, 0, 100, 100);
        assert_eq!(to_absolute(virt, virt, 5.0, -1.0), (65535, 0));
    }
}
