//! The `spotlight://` URL scheme, registered for this user under
//! `HKCU\Software\Classes`, so links start `spotlight.exe "<link>"`. A running
//! instance gets the link over the instance pipe.

use windows::{
    Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW},
    core::{HSTRING, PCWSTR},
};

use crate::registry;

/// Points the scheme at the running executable, unless it already does.
/// Whichever copy of Spotlight ran last owns the links.
pub fn register(scheme: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let exe = exe.display();
    let key = format!(r"Software\Classes\{scheme}");
    let command = format!("\"{exe}\" \"%1\"");
    let open = format!(r"{key}\shell\open\command");
    if registry::string(&format!(r"HKCU\{open}"), "").as_deref() == Some(command.as_str()) {
        return Ok(());
    }
    set(&key, "", "URL:Spotlight")?;
    // Its presence marks the key as a URL scheme; the value is unused.
    set(&key, "URL Protocol", "")?;
    set(&format!(r"{key}\DefaultIcon"), "", &format!("\"{exe}\",0"))?;
    set(&open, "", &command)
}

fn set(key: &str, name: &str, value: &str) -> Result<(), String> {
    let value = HSTRING::from(value);
    let bytes = (value.len() + 1) * size_of::<u16>();
    let name = HSTRING::from(name);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            if name.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(name.as_ptr())
            },
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            bytes as u32,
        )
    }
    .ok()
    .map_err(|err| format!("could not register {key}: {err}"))
}
