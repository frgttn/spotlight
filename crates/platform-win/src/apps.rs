//! Installed-application discovery through the shell's `AppsFolder`, and
//! the Control Panel's items the same way.
//!
//! `shell:AppsFolder` is what the Start menu's "All apps" list is built from, so
//! one enumeration covers classic Win32 shortcuts and MSIX/UWP packages alike.

use std::{ffi::c_void, path::Path};

use windows::{
    Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree,
        },
        UI::Shell::{
            BHID_EnumItems, FOLDERID_AppsFolder, FOLDERID_ControlPanelFolder, IEnumShellItems,
            IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem, SHGetKnownFolderPath, SIGDN,
            SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
        },
    },
    core::{GUID, Result},
};

#[derive(Clone, Debug)]
pub struct AppEntry {
    /// Name as shown in the Start menu.
    pub name: String,
    /// AppsFolder parsing name: an AUMID for packaged apps, a (known-folder relative) path otherwise.
    pub launch_id: String,
    /// The program or shortcut on disk, for Win32 apps whose parsing name is a
    /// path. `None` for packaged apps and apps known only by an AUMID.
    pub path: Option<String>,
}

/// Enumerates all apps. Blocking and COM-heavy: call it from a dedicated thread.
pub fn discover() -> Result<Vec<AppEntry>> {
    let mut apps: Vec<AppEntry> = enumerate(&FOLDERID_AppsFolder, SIGDN_PARENTRELATIVEPARSING)?
        .into_iter()
        .map(|(name, launch_id)| AppEntry {
            path: file_path(&launch_id),
            name,
            launch_id,
        })
        .collect();
    apps.sort_by_cached_key(|app| app.name.to_lowercase());
    apps.dedup_by(|a, b| a.launch_id == b.launch_id);
    Ok(apps)
}

/// An item of "All Control Panel Items" (Mouse, Device Manager, …).
#[derive(Clone, Debug)]
pub struct ControlPanelItem {
    pub name: String,
    /// Absolute shell parsing name (`::{26EE…}\0\::{GUID}`): opens the
    /// item, and is where its icon comes from.
    pub parsing_name: String,
}

/// Enumerates the Control Panel's items, as its "All items" view lists them.
/// Blocking and COM-heavy, like [`discover`].
pub fn control_panel() -> Result<Vec<ControlPanelItem>> {
    let mut items: Vec<ControlPanelItem> =
        enumerate(&FOLDERID_ControlPanelFolder, SIGDN_DESKTOPABSOLUTEPARSING)?
            .into_iter()
            .map(|(name, parsing_name)| ControlPanelItem { name, parsing_name })
            .collect();
    items.sort_by_cached_key(|item| item.name.to_lowercase());
    items.dedup_by(|a, b| a.parsing_name == b.parsing_name);
    Ok(items)
}

/// Every item of a shell folder: its display name and the name `id` asks for.
fn enumerate(folder: &GUID, id: SIGDN) -> Result<Vec<(String, String)>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let folder: IShellItem = SHGetKnownFolderItem(folder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;

        let mut entries = Vec::new();
        loop {
            let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            for item in batch.iter().take(fetched as usize).flatten() {
                if let (Ok(name), Ok(id)) = (
                    display_name(item, SIGDN_NORMALDISPLAY),
                    display_name(item, id),
                ) && !name.is_empty()
                    && !id.is_empty()
                {
                    entries.push((name, id));
                }
            }
            // `Next` maps S_FALSE (last, partial batch) to Ok, so a short batch marks the end.
            if (fetched as usize) < batch.len() {
                break;
            }
        }
        Ok(entries)
    }
}

/// The parsing name as a file path, when it is one: absolute
/// (`C:\Tools\app.exe`) or relative to a known folder
/// (`{6D809377-6AF0-444B-8957-A3773F02200E}\App\app.exe`, i.e. Program Files).
/// Only paths to existing files are returned.
fn file_path(launch_id: &str) -> Option<String> {
    let path = match launch_id.strip_prefix('{') {
        Some(rest) => {
            let (guid, relative) = rest.split_once("}\\")?;
            let folder = GUID::try_from(guid).ok()?;
            let base = unsafe {
                let raw = SHGetKnownFolderPath(&folder, KF_FLAG_DEFAULT, None).ok()?;
                let base = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const c_void));
                base?
            };
            format!("{base}\\{relative}")
        }
        None => launch_id.to_owned(),
    };
    let file = Path::new(&path);
    (file.is_absolute() && file.is_file()).then_some(path)
}

unsafe fn display_name(item: &IShellItem, sigdn: SIGDN) -> Result<String> {
    unsafe {
        let raw = item.GetDisplayName(sigdn)?;
        let text = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as *const c_void));
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: lists this machine's Control Panel items.
    #[test]
    fn lists_control_panel_items() {
        let items = control_panel().unwrap();
        for item in items.iter().take(8) {
            eprintln!("{:<40} {}", item.name, item.parsing_name);
        }
        eprintln!("... {} items", items.len());
        assert!(items.len() > 10);
    }
}
