//! Removable drives: listing them and ejecting them the way Explorer's
//! "Eject" does, so Windows flushes and detaches the device properly.

use std::{
    mem::size_of,
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::CloseHandle,
        Storage::FileSystem::{
            BusTypeUsb, CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
        },
        System::{
            Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
            IO::DeviceIoControl,
            Ioctl::{
                IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
                STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
            },
            WindowsProgramming::{DRIVE_CDROM, DRIVE_FIXED, DRIVE_REMOVABLE},
        },
        UI::{
            Shell::{
                BHID_SFUIObject, CMF_NORMAL, CMINVOKECOMMANDINFO, IContextMenu, IShellItem,
                SHCreateItemFromParsingName,
            },
            WindowsAndMessaging::{CreatePopupMenu, DestroyMenu, SW_SHOWNORMAL},
        },
    },
    core::{HSTRING, s},
};

/// How long to wait for a drive letter to go away after asking to eject.
const EJECT_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    pub letter: char,
    /// The volume label, or what kind of drive it is.
    pub label: String,
    pub kind: DriveKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    /// USB sticks, card readers, external USB disks.
    Removable,
    /// CD/DVD/Blu-ray drives: ejecting opens the tray.
    Optical,
}

/// Drives that can be ejected: removable and optical drives, and fixed disks
/// on USB (external hard drives report themselves as fixed). Network drives
/// and internal disks are left out. Takes a few milliseconds.
pub fn removable() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| {
            let letter = char::from(b'A' + bit);
            let root = HSTRING::from(format!("{letter}:\\"));
            let kind = match unsafe { GetDriveTypeW(&root) } {
                DRIVE_REMOVABLE => DriveKind::Removable,
                DRIVE_CDROM => DriveKind::Optical,
                DRIVE_FIXED if on_usb(letter) => DriveKind::Removable,
                _ => return None,
            };
            let label = volume_label(&root).unwrap_or_else(|| {
                match kind {
                    DriveKind::Removable => "USB Drive",
                    DriveKind::Optical => "DVD Drive",
                }
                .to_owned()
            });
            Some(Drive {
                letter,
                label,
                kind,
            })
        })
        .collect()
}

fn volume_label(root: &HSTRING) -> Option<String> {
    let mut name = [0u16; 261];
    unsafe { GetVolumeInformationW(root, Some(&mut name), None, None, None, None) }.ok()?;
    let len = name.iter().position(|&ch| ch == 0).unwrap_or(name.len());
    let label = String::from_utf16_lossy(&name[..len]);
    (!label.trim().is_empty()).then_some(label)
}

/// Whether the disk behind a drive letter is attached over USB.
fn on_usb(letter: char) -> bool {
    unsafe {
        // No access rights needed just to ask the device about itself.
        let Ok(device) = CreateFileW(
            &HSTRING::from(format!("\\\\.\\{letter}:")),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) else {
            return false;
        };
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        // The descriptor is followed by variable-length strings; the fixed part is enough.
        let mut buffer = [0u8; 1024];
        let mut returned = 0u32;
        let asked = DeviceIoControl(
            device,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const _),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        );
        let _ = CloseHandle(device);
        if asked.is_err() || (returned as usize) < size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
            return false;
        }
        let descriptor = buffer
            .as_ptr()
            .cast::<STORAGE_DEVICE_DESCRIPTOR>()
            .read_unaligned();
        descriptor.BusType == BusTypeUsb
    }
}

/// Ejects drive `letter` and says how it went. Blocking: call it from a
/// thread of its own. Windows shows its own message if something keeps the
/// drive busy.
pub fn eject(letter: char) -> Result<String, String> {
    let letter = letter.to_ascii_uppercase();
    let Some(drive) = removable().into_iter().find(|drive| drive.letter == letter) else {
        return Err(format!("{letter}: isn't a removable drive"));
    };
    invoke_eject(letter)?;
    if drive.kind == DriveKind::Optical {
        // The letter stays; only the tray opens.
        return Ok(format!("Ejected {letter}:"));
    }
    let started = Instant::now();
    while started.elapsed() < EJECT_TIMEOUT {
        if !is_mounted(letter) {
            return Ok(format!("{letter}: can be removed"));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("{letter}: is still in use"))
}

/// Ejects every removable drive; reports which ones went and which didn't.
pub fn eject_all() -> Result<String, String> {
    let drives = removable();
    if drives.is_empty() {
        return Err("No removable drives".into());
    }
    let (mut ejected, mut busy) = (Vec::new(), Vec::new());
    for drive in drives {
        match eject(drive.letter) {
            Ok(_) => ejected.push(format!("{}:", drive.letter)),
            Err(_) => busy.push(format!("{}:", drive.letter)),
        }
    }
    match (ejected.is_empty(), busy.is_empty()) {
        (false, true) => Ok(format!("Ejected {}", ejected.join(", "))),
        (true, _) => Err(format!("{} still in use", busy.join(", "))),
        (false, false) => Err(format!(
            "Ejected {}; {} still in use",
            ejected.join(", "),
            busy.join(", ")
        )),
    }
}

fn is_mounted(letter: char) -> bool {
    let bit = u32::from(letter as u8 - b'A');
    unsafe { GetLogicalDrives() & (1 << bit) != 0 }
}

/// Runs the shell's "eject" verb on the drive, as its context menu in
/// Explorer would.
fn invoke_eject(letter: char) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let item: IShellItem =
            SHCreateItemFromParsingName(&HSTRING::from(format!("{letter}:\\")), None)
                .map_err(|err| err.message())?;
        let menu: IContextMenu = item
            .BindToHandler(None, &BHID_SFUIObject)
            .map_err(|err| err.message())?;
        // Some handlers only accept verbs after building their menu.
        let popup = CreatePopupMenu().map_err(|err| err.message())?;
        let _ = menu.QueryContextMenu(popup, 0, 1, 0x7FFF, CMF_NORMAL);
        let info = CMINVOKECOMMANDINFO {
            cbSize: size_of::<CMINVOKECOMMANDINFO>() as u32,
            lpVerb: s!("eject"),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let invoked = menu.InvokeCommand(&info);
        let _ = DestroyMenu(popup);
        invoked.map_err(|err| format!("Couldn't eject {letter}: ({})", err.message()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: lists this machine's drives, ejects nothing.
    #[test]
    fn lists_without_internal_disks() {
        let drives = removable();
        eprintln!("removable drives: {drives:?}");
        assert!(drives.iter().all(|drive| drive.letter != 'C'));
    }
}
