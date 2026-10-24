//! Lists a folder for the file index: one `FindFirstFileExW` pass that gives
//! each name with its attributes and modification time, with no `stat` per
//! file (`FindExInfoBasic` skips the 8.3 short names, `LARGE_FETCH` asks for
//! bigger batches).

use std::{io, path::Path};

use spotlight_files::Listed;
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, FILETIME},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_SYSTEM, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
            FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
        },
    },
    core::HSTRING,
};

/// Reparse tags of junctions and symbolic links, which the walk doesn't
/// enter. Other reparse points (OneDrive's cloud placeholders) are ordinary
/// folders and files to it.
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// 100 ns intervals between 1601-01-01 and the unix epoch.
const FILETIME_UNIX_OFFSET: u64 = 116_444_736_000_000_000;

/// The names in folder `dir`, without `.` and `..`. An empty folder lists
/// nothing; one that can't be read is an error.
pub fn list(dir: &Path) -> io::Result<Vec<Listed>> {
    let pattern = HSTRING::from(long_path(dir) + "\\*");
    let mut data = WIN32_FIND_DATAW::default();
    let found = unsafe {
        FindFirstFileExW(
            &pattern,
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match found {
        Ok(handle) => handle,
        Err(err) if err.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(Vec::new()),
        Err(err) => return Err(io::Error::from_raw_os_error(err.code().0 & 0xFFFF)),
    };
    let mut listed = Vec::new();
    loop {
        if let Some(item) = to_listed(&data) {
            listed.push(item);
        }
        if let Err(err) = unsafe { FindNextFileW(handle, &mut data) } {
            if err.code() != ERROR_NO_MORE_FILES.to_hresult() {
                eprintln!("[fswalk] listing {} stopped early: {err}", dir.display());
            }
            break;
        }
    }
    unsafe {
        let _ = FindClose(handle);
    }
    Ok(listed)
}

/// What a listing of its folder would say about `path`, or `None` if it is
/// gone: for a change notification, which names a path and nothing more.
pub fn stat(path: &Path) -> Option<Listed> {
    // Without a wildcard, the search finds the item itself.
    let mut data = WIN32_FIND_DATAW::default();
    let handle = unsafe {
        FindFirstFileExW(
            &HSTRING::from(long_path(path)),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            Default::default(),
        )
    }
    .ok()?;
    unsafe {
        let _ = FindClose(handle);
    }
    to_listed(&data)
}

fn to_listed(data: &WIN32_FIND_DATAW) -> Option<Listed> {
    let len = data
        .cFileName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(data.cFileName.len());
    let name = String::from_utf16_lossy(&data.cFileName[..len]);
    if name == "." || name == ".." || name.is_empty() {
        return None;
    }
    let attributes = data.dwFileAttributes;
    let has = |attribute: u32| attributes & attribute != 0;
    let reparse = has(FILE_ATTRIBUTE_REPARSE_POINT.0);
    // With a reparse point, dwReserved0 holds its tag.
    let link = reparse
        && matches!(
            data.dwReserved0,
            IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK
        );
    Some(Listed {
        name,
        dir: has(FILE_ATTRIBUTE_DIRECTORY.0),
        hidden: has(FILE_ATTRIBUTE_HIDDEN.0),
        system: has(FILE_ATTRIBUTE_SYSTEM.0),
        link,
        modified: unix_seconds(data.ftLastWriteTime),
    })
}

pub(crate) fn unix_seconds(time: FILETIME) -> i64 {
    let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
    ticks
        .checked_sub(FILETIME_UNIX_OFFSET)
        .map_or(0, |since| (since / 10_000_000) as i64)
}

/// `dir` with the `\\?\` prefix, so paths past 260 characters list too.
pub(crate) fn long_path(dir: &Path) -> String {
    let text = dir.to_string_lossy().replace('/', "\\");
    let text = text.trim_end_matches('\\');
    if text.starts_with(r"\\?\") {
        text.to_owned()
    } else if let Some(share) = text.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{share}")
    } else {
        format!(r"\\?\{text}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn lists_names_kinds_and_times() {
        let dir = std::env::temp_dir().join(format!("spotlight-fswalk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "x").unwrap();
        fs::write(dir.join("Отчёт.docx"), "x").unwrap();

        let mut listed = list(&dir).unwrap();
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<(&str, bool)> = listed
            .iter()
            .map(|item| (item.name.as_str(), item.dir))
            .collect();
        assert_eq!(
            names,
            [("a.txt", false), ("sub", true), ("Отчёт.docx", false)]
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            (now - listed[0].modified).abs() < 120,
            "{}",
            listed[0].modified
        );
        assert!(list(&dir.join("sub")).unwrap().is_empty());
        assert!(list(&dir.join("missing")).is_err());
        let sub = stat(&dir.join("sub")).unwrap();
        assert!(sub.dir);
        assert_eq!(sub.name, "sub");
        assert_eq!(stat(&dir.join("a.txt")).unwrap().name, "a.txt");
        assert_eq!(stat(&dir.join("missing")), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn long_paths_get_the_prefix() {
        assert_eq!(long_path(Path::new(r"C:\Users\me\")), r"\\?\C:\Users\me");
        assert_eq!(
            long_path(Path::new(r"\\server\share\x")),
            r"\\?\UNC\server\share\x"
        );
        assert_eq!(long_path(Path::new(r"\\?\C:\x")), r"\\?\C:\x");
    }
}
