//! Installed-application discovery through the shell's `AppsFolder`.
//!
//! `shell:AppsFolder` is what the Start menu's "All apps" list is built from, so
//! one enumeration covers classic Win32 shortcuts and MSIX/UWP packages alike.

use std::ffi::c_void;

use windows::{
    Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree,
        },
        UI::Shell::{
            BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT,
            SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
        },
    },
    core::Result,
};

#[derive(Clone, Debug)]
pub struct AppEntry {
    /// Name as shown in the Start menu.
    pub name: String,
    /// AppsFolder parsing name: an AUMID for packaged apps, a (known-folder relative) path otherwise.
    pub launch_id: String,
}

/// Enumerates all apps. Blocking and COM-heavy: call it from a dedicated thread.
pub fn discover() -> Result<Vec<AppEntry>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;

        let mut apps = Vec::new();
        loop {
            let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            for item in batch.iter().take(fetched as usize).flatten() {
                let (Ok(name), Ok(launch_id)) = (
                    display_name(item, SIGDN_NORMALDISPLAY),
                    display_name(item, SIGDN_PARENTRELATIVEPARSING),
                ) else {
                    continue;
                };
                if !name.is_empty() && !launch_id.is_empty() {
                    apps.push(AppEntry { name, launch_id });
                }
            }
            // `Next` maps S_FALSE (last, partial batch) to Ok, so a short batch marks the end.
            if (fetched as usize) < batch.len() {
                break;
            }
        }

        apps.sort_by_cached_key(|app| app.name.to_lowercase());
        apps.dedup_by(|a, b| a.launch_id == b.launch_id);
        Ok(apps)
    }
}

unsafe fn display_name(item: &IShellItem, sigdn: SIGDN) -> Result<String> {
    unsafe {
        let raw = item.GetDisplayName(sigdn)?;
        let text = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as *const c_void));
        Ok(text)
    }
}
