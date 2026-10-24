//! The service's log: a file in its data folder when it runs as a service
//! (it has no console then), stderr otherwise.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// A log past this size starts over.
const MAX_BYTES: u64 = 1 << 20;

static FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// From now on, lines go to `path` instead of stderr.
pub fn to_file(path: &Path) {
    if path.metadata().is_ok_and(|meta| meta.len() > MAX_BYTES) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = FILE.set(Mutex::new(file));
    }
}

pub fn line(text: &str) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    match FILE.get() {
        Some(file) => {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "{seconds} {text}");
            }
        }
        None => eprintln!("[service] {text}"),
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}
