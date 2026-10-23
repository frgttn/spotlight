//! Reading the registry, for what other programs record about themselves
//! (where Steam is, which GOG and Ubisoft games are installed).

use std::{ffi::c_void, mem::size_of};

use windows::{
    Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey,
            RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
        },
    },
    core::{HSTRING, PWSTR},
};

/// `HKCU\…` or `HKLM\…` and the rest of the path.
fn split(path: &str) -> Option<(HKEY, &str)> {
    let (hive, rest) = path.split_once('\\')?;
    let hive = match hive {
        "HKCU" | "HKEY_CURRENT_USER" => HKEY_CURRENT_USER,
        "HKLM" | "HKEY_LOCAL_MACHINE" => HKEY_LOCAL_MACHINE,
        _ => return None,
    };
    Some((hive, rest))
}

/// The string value `name` of key `path` (`"HKCU\\Software\\Valve\\Steam"`),
/// if both exist.
pub fn string(path: &str, name: &str) -> Option<String> {
    let (hive, subkey) = split(path)?;
    let (subkey, name) = (HSTRING::from(subkey), HSTRING::from(name));
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(
            hive,
            &subkey,
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(size_of::<u16>())];
        if RegGetValueW(
            hive,
            &subkey,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = buffer
            .iter()
            .position(|&ch| ch == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

/// The names of the subkeys of `path`; empty if it doesn't exist.
pub fn subkeys(path: &str) -> Vec<String> {
    let Some((hive, subkey)) = split(path) else {
        return Vec::new();
    };
    let mut key = HKEY::default();
    unsafe {
        if RegOpenKeyExW(hive, &HSTRING::from(subkey), None, KEY_READ, &mut key) != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut names = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let status = RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            );
            if status != ERROR_SUCCESS {
                break;
            }
            names.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        let _ = RegCloseKey(key);
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_values_and_subkeys() {
        let product = string(
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "ProductName",
        );
        assert!(product.is_some_and(|name| name.starts_with("Windows")));
        assert!(!subkeys(r"HKLM\SOFTWARE\Microsoft").is_empty());
        assert_eq!(string(r"HKCU\No\Such\Key", "x"), None);
        assert!(subkeys(r"HKCU\No\Such\Key").is_empty());
        assert_eq!(string(r"XX\Software", "x"), None);
    }
}
