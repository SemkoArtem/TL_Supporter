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
        let path = data_dir().join("tlsupporter.log");
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
