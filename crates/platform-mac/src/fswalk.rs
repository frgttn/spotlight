//! macOS counterpart of `loupe_platform_win::fswalk`, with its signatures.
//! Plain `std::fs`: a folder listing needs nothing of macOS's own, besides
//! the Finder's hidden flag.

use loupe_files::Listed;
use std::{
    fs::{self, Metadata},
    io,
    path::Path,
    time::UNIX_EPOCH,
};

/// The names in folder `dir`, without `.` and `..`. An empty folder lists
/// nothing; one that can't be read is an error.
pub fn list(dir: &Path) -> io::Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            continue;
        };
        // Not followed: a link is listed as itself.
        if let Ok(meta) = entry.metadata() {
            listed.push(to_listed(
                entry.file_name().to_string_lossy().into_owned(),
                &meta,
            ));
        }
    }
    Ok(listed)
}

/// What a listing of its folder would say about `path`, or `None` if it is
/// gone: for a change notification, which names a path and nothing more.
pub fn stat(path: &Path) -> Option<Listed> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let meta = fs::symlink_metadata(path).ok()?;
    Some(to_listed(name, &meta))
}

/// `meta` without following links.
fn to_listed(name: String, meta: &Metadata) -> Listed {
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs() as i64);
    Listed {
        name,
        dir: meta.is_dir(),
        hidden: finder_hidden(meta),
        system: false,
        link: meta.file_type().is_symlink(),
        modified,
    }
}

/// Hidden from the Finder (`chflags hidden`); dot names count as hidden
/// on their own.
#[cfg(target_os = "macos")]
fn finder_hidden(meta: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const UF_HIDDEN: u32 = 0x8000;
    meta.st_flags() & UF_HIDDEN != 0
}

#[cfg(not(target_os = "macos"))]
fn finder_hidden(_meta: &Metadata) -> bool {
    false
}

/// `dir` with the `\\?\` prefix, so paths past 260 characters list too. A
/// drive keeps its backslash: `\\?\C:` would be the volume itself.
pub fn long_path(dir: &Path) -> String {
    // macOS has no path length limit to work around.
    dir.to_string_lossy().into_owned()
}
