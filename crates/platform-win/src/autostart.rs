//! Start at login through the per-user `Run` registry key.
//!
//! The registry is the only state, so there is no separate setting that could
//! disagree with it. Task Manager's Startup tab disables an entry without
//! deleting it, by marking it in `StartupApproved\Run`; that mark counts as
//! off here, and turning start at login on clears it.

use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR},
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW,
            RegGetValueW, RegSetKeyValueW,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APPROVED_KEY: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");
const VALUE: PCWSTR = w!("Spotlight");

/// Whether Spotlight is registered to start at login (from any path) and not
/// disabled in Task Manager.
pub fn is_enabled() -> bool {
    let registered = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
        .is_ok()
    };
    registered && !disabled_in_task_manager()
}

/// Registers the running executable to start at login, or removes the entry.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        // Quoted, so a path with spaces isn't split into program and arguments.
        let command = HSTRING::from(format!("\"{}\"", exe.display()));
        let bytes = (command.len() + 1) * size_of::<u16>();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                bytes as u32,
            )
        }
        .ok()
        .map_err(|err| err.to_string())?;
    } else {
        delete(RUN_KEY)?;
    }
    // Either way a Task Manager "disabled" mark is stale now.
    delete(APPROVED_KEY)
}

/// The first byte of the `StartupApproved` value is even when enabled
/// (usually 02) and odd when disabled (usually 03). No value means enabled.
fn disabled_in_task_manager() -> bool {
    let mut data = [0u8; 12];
    let mut len = data.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            APPROVED_KEY,
            VALUE,
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    status.is_ok() && len > 0 && data[0] & 1 == 1
}

fn delete(key: PCWSTR) -> Result<(), String> {
    let status = match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, VALUE) } {
        // Already gone.
        status if status == ERROR_FILE_NOT_FOUND => WIN32_ERROR(0),
        status => status,
    };
    status.ok().map_err(|err| err.to_string())
}
