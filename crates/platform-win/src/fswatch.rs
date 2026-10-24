//! Watches a folder tree for the file index with `ReadDirectoryChangesW`:
//! names created, deleted, renamed or written to, anywhere below the folder.
//!
//! One thread per watched folder waits on its overlapped read and on a stop
//! event, so dropping the [`Watcher`] ends it at once. When changes come
//! faster than the buffer holds, Windows drops them and says so; that is
//! reported as [`Change::Overflow`], and the index rescans the folder.

use std::{
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Storage::FileSystem::{
            CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
            FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE,
            FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_INFORMATION,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            ReadDirectoryChangesW,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects},
        },
    },
    core::HSTRING,
};

/// What changed, by full path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added(PathBuf),
    Removed(PathBuf),
    /// Written to, or its attributes changed.
    Modified(PathBuf),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    /// Changes were lost: the whole folder needs a rescan.
    Overflow,
    /// The watch ended (the folder was deleted, or the drive went away).
    Stopped(String),
}

/// Bytes of change records one read can hold. 64 KB is the most a network
/// share accepts, and plenty locally between two reads.
const BUFFER_BYTES: usize = 64 * 1024;

/// Watches until dropped.
pub struct Watcher {
    /// The stop event, as a raw handle so the watcher can move between threads.
    stop: isize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as _));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.stop as _));
        }
    }
}

/// Starts watching `root` and everything below it. Each batch of changes
/// Windows reports arrives on `changes`, in order.
pub fn watch(root: &Path, changes: async_channel::Sender<Vec<Change>>) -> Result<Watcher, String> {
    let directory = unsafe {
        CreateFileW(
            &HSTRING::from(crate::fswalk::long_path(root)),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|err| format!("Couldn't watch {}: {err}", root.display()))?;
    let stop = unsafe { CreateEventW(None, true, false, None) }.map_err(|err| {
        unsafe {
            let _ = CloseHandle(directory);
        }
        format!("Couldn't watch {}: {err}", root.display())
    })?;
    let (directory_raw, stop_raw) = (directory.0 as isize, stop.0 as isize);
    let root = root.to_path_buf();
    // Windows keeps changes only from the first read on, so this returns once
    // that read is under way: nothing made right after it is missed.
    let (watching_tx, watching_rx) = std::sync::mpsc::channel();
    let thread = thread::Builder::new()
        .name("file-watch".into())
        .spawn(move || {
            let directory = HANDLE(directory_raw as _);
            run(
                directory,
                HANDLE(stop_raw as _),
                &root,
                &changes,
                watching_tx,
            );
            unsafe {
                let _ = CloseHandle(directory);
            }
        })
        .map_err(|err| format!("Couldn't start watching: {err}"))?;
    let _ = watching_rx.recv();
    Ok(Watcher {
        stop: stop_raw,
        thread: Some(thread),
    })
}

fn run(
    directory: HANDLE,
    stop: HANDLE,
    root: &Path,
    changes: &async_channel::Sender<Vec<Change>>,
    watching: std::sync::mpsc::Sender<()>,
) {
    let Ok(done) = (unsafe { CreateEventW(None, true, false, None) }) else {
        let _ = changes.send_blocking(vec![Change::Stopped("Couldn't create an event".into())]);
        return;
    };
    // `FILE_NOTIFY_INFORMATION` records are DWORD-aligned.
    let mut buffer = vec![0u32; BUFFER_BYTES / 4];
    let filter: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_ATTRIBUTES;
    loop {
        let mut overlapped = OVERLAPPED {
            hEvent: done,
            ..Default::default()
        };
        let started = unsafe {
            ReadDirectoryChangesW(
                directory,
                buffer.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
                true,
                filter,
                None,
                Some(&mut overlapped),
                None,
            )
        };
        // `watch` waits for the first of these; later ones go nowhere.
        let _ = watching.send(());
        if let Err(err) = started {
            let _ = changes.send_blocking(vec![Change::Stopped(err.to_string())]);
            break;
        }
        let woke = unsafe { WaitForMultipleObjects(&[done, stop], false, INFINITE) };
        if woke != WAIT_OBJECT_0 {
            // Stopped: the read must end before its buffer and OVERLAPPED go.
            unsafe {
                let _ = CancelIoEx(directory, Some(&overlapped));
                let mut ignored = 0;
                let _ = GetOverlappedResult(directory, &overlapped, &mut ignored, true);
            }
            break;
        }
        let mut bytes = 0u32;
        if let Err(err) = unsafe { GetOverlappedResult(directory, &overlapped, &mut bytes, false) }
        {
            let _ = changes.send_blocking(vec![Change::Stopped(err.to_string())]);
            break;
        }
        let batch = if bytes == 0 {
            vec![Change::Overflow]
        } else {
            parse(&buffer, bytes as usize, root)
        };
        if changes.send_blocking(batch).is_err() {
            break;
        }
    }
    unsafe {
        let _ = CloseHandle(done);
    }
}

/// The records in the first `bytes` of `buffer`, as changes under `root`.
fn parse(buffer: &[u32], bytes: usize, root: &Path) -> Vec<Change> {
    let base = buffer.as_ptr().cast::<u8>();
    let mut batch = Vec::new();
    let mut renamed_from: Option<PathBuf> = None;
    let mut offset = 0usize;
    while offset + size_of::<FILE_NOTIFY_INFORMATION>() <= bytes {
        // Windows wrote a chain of records here, each at a DWORD boundary.
        let record = unsafe { &*base.add(offset).cast::<FILE_NOTIFY_INFORMATION>() };
        let name_len = record.FileNameLength as usize / 2;
        let name = unsafe { std::slice::from_raw_parts(record.FileName.as_ptr(), name_len) };
        let path = root.join(String::from_utf16_lossy(name));
        match record.Action {
            FILE_ACTION_ADDED => batch.push(Change::Added(path)),
            FILE_ACTION_REMOVED => batch.push(Change::Removed(path)),
            FILE_ACTION_MODIFIED => batch.push(Change::Modified(path)),
            FILE_ACTION_RENAMED_OLD_NAME => renamed_from = Some(path),
            FILE_ACTION_RENAMED_NEW_NAME => batch.push(match renamed_from.take() {
                Some(from) => Change::Renamed { from, to: path },
                // The old name was outside what this read saw.
                None => Change::Added(path),
            }),
            _ => {}
        }
        if record.NextEntryOffset == 0 {
            break;
        }
        offset += record.NextEntryOffset as usize;
    }
    // A rename whose new name never came moved the file out of the tree.
    if let Some(from) = renamed_from {
        batch.push(Change::Removed(from));
    }
    batch
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use super::*;

    /// Collects changes until `want` matches what arrived, or times out.
    fn wait_for(
        rx: &async_channel::Receiver<Vec<Change>>,
        want: impl Fn(&[Change]) -> bool,
    ) -> Vec<Change> {
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            while let Ok(batch) = rx.try_recv() {
                seen.extend(batch);
            }
            if want(&seen) {
                return seen;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("changes never arrived; saw {seen:?}");
    }

    #[test]
    fn reports_creates_renames_and_deletes() {
        let dir = std::env::temp_dir().join(format!("spotlight-fswatch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        let (tx, rx) = async_channel::unbounded();
        let watcher = watch(&dir, tx).unwrap();

        fs::write(dir.join("sub").join("a.txt"), "x").unwrap();
        let a = dir.join("sub").join("a.txt");
        wait_for(&rx, |seen| seen.contains(&Change::Added(a.clone())));

        // Renamed in place: one change with both names.
        let renamed = dir.join("sub").join("renamed.txt");
        fs::rename(&a, &renamed).unwrap();
        wait_for(&rx, |seen| {
            seen.contains(&Change::Renamed {
                from: a.clone(),
                to: renamed.clone(),
            })
        });

        // Moved to another folder: Windows reports a removal and an addition.
        let b = dir.join("b.txt");
        fs::rename(&renamed, &b).unwrap();
        wait_for(&rx, |seen| {
            seen.contains(&Change::Removed(renamed.clone()))
                && seen.contains(&Change::Added(b.clone()))
        });

        fs::remove_file(&b).unwrap();
        wait_for(&rx, |seen| seen.contains(&Change::Removed(b.clone())));

        drop(watcher);
        fs::remove_dir_all(&dir).unwrap();
    }
}
