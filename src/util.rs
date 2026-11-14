//! Small shared helpers: paths, time and a file logger.

use parking_lot::Mutex;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A test setting from the environment. The copy started by the service ignores them,
/// otherwise an employee could point the running program at a folder of their own.
pub fn test_env(name: &str) -> Option<String> {
    static FROM_SERVICE: OnceLock<bool> = OnceLock::new();
    let from_service = *FROM_SERVICE.get_or_init(|| std::env::args().any(|a| a == "--from-service" || a == "--service"));
    if from_service {
        return None;
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Folder beside the installed program, where ordinary users cannot write.
pub fn secure_dir() -> Option<PathBuf> {
    if test_env("TLS_DATA_DIR").is_some() {
        return Some(data_dir());
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.join("TLSupporterData"))
}

/// The data folder is open to every user, so a process running as SYSTEM must not write there:
/// a planted link would turn that write or delete against any file on the PC.
fn log_path() -> Option<PathBuf> {
    if !std::env::args().any(|a| a == "--service" || a == "--system-agent") {
        return Some(data_dir().join("tlsupporter.log"));
    }
    let dir = secure_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("system.log"))
}

/// Data folder. `TLS_DATA_DIR` lets two copies run side by side on one PC for testing.
pub fn data_dir() -> PathBuf {
    let dir = match test_env("TLS_DATA_DIR") {
        Some(v) => PathBuf::from(v),
        _ => {
            let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
            PathBuf::from(base).join("TLSupporter")
        }
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn format_ts(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms).single() {
        Some(t) => t.format("%d.%m.%Y %H:%M:%S").to_string(),
        None => String::new(),
    }
}

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

pub fn log_line(text: &str) {
    let slot = LOG.get_or_init(|| {
        let Some(path) = log_path() else { return Mutex::new(None) };
        // Keep the log small: start over once it passes 2 MB.
        if std::fs::metadata(&path).map(|m| m.len() > 2_000_000).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
        }
        Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(path).ok())
    });
    let line = format!("{} [{}] {}\n", format_ts(now_ms()), std::process::id(), text);
    if let Some(f) = slot.lock().as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
    if crate::util::test_env("TLS_CONSOLE").is_some() {
        eprint!("{line}");
    }
}

#[macro_export]
macro_rules! logi {
    ($($arg:tt)*) => { $crate::util::log_line(&format!($($arg)*)) };
}

/// The user's Documents folder.
pub fn documents_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        path.map(PathBuf::from)
    }
}

/// Shows a file in Explorer.
pub fn show_file(path: &std::path::Path) {
    let _ = std::process::Command::new(windows_dir().join("explorer.exe")).arg(format!("/select,{}", path.display())).spawn();
}
/// The Windows folder, asked from the system rather than taken from the environment.
pub fn windows_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return PathBuf::from(r"C:\Windows");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

/// A Windows tool by its full path. A short name would first be looked for beside this
/// program, where someone could have put a file of the same name.
pub fn system_tool(name: &str) -> PathBuf {
    windows_dir().join("System32").join(format!("{name}.exe"))
}

/// Shows the log file in Explorer, so that it can be sent along with a report.
pub fn open_log() {
    let log = data_dir().join("tlsupporter.log");
    let explorer = windows_dir().join("explorer.exe");
    let _ = if log.exists() {
        std::process::Command::new(explorer).arg(format!("/select,{}", log.display())).spawn()
    } else {
        std::process::Command::new(explorer).arg(data_dir()).spawn()
    };
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Stable colour for a lead, derived from the device id.
pub fn color_for(id: &str) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 8] = [
        [232, 17, 35],
        [0, 120, 215],
        [16, 137, 62],
        [202, 80, 16],
        [136, 23, 152],
        [0, 153, 188],
        [194, 57, 179],
        [122, 117, 116],
    ];
    let h: usize = id.bytes().map(|b| b as usize).sum();
    PALETTE[h % PALETTE.len()]
}
