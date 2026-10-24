//! Builds the index from a walk of the scope's folders, and adds a folder that
//! appears later. Platform-free: the caller lists a folder (`platform-win`
//! does it with `FindFirstFileExW`), this decides what to enter and keeps the
//! table.
//!
//! Listing is parallel; adding to the table is not. Worker threads take
//! folders from a queue and list them, and the calling thread adds what they
//! found and queues the subfolders, so ids are handed out in one place.

use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    index::{Builder, EntryId, Index, Meta},
    names::fold,
};

/// One name in a folder listing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listed {
    pub name: String,
    pub dir: bool,
    pub hidden: bool,
    pub system: bool,
    /// A junction or symbolic link: listed, never entered, so a link to a
    /// folder above it can't loop and a link elsewhere can't pull that in.
    pub link: bool,
    /// Unix seconds; 0 when unknown.
    pub modified: i64,
}

impl Listed {
    fn meta(&self) -> Meta<'_> {
        Meta {
            name: &self.name,
            dir: self.dir,
            hidden: self.hidden,
            system: self.system,
            modified: self.modified,
        }
    }

    /// Hidden or system by attribute, or by a leading dot.
    fn is_hidden(&self) -> bool {
        self.hidden || self.system || self.name.starts_with('.')
    }
}

/// What the walk leaves out. A rule with a path separator or a drive colon
/// is a path, excluded with everything under it; any other rule is a folder
/// name, excluded wherever it appears. Case doesn't matter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exclusions {
    names: Vec<String>,
    paths: Vec<String>,
    /// Leave out hidden and system files and folders, and names starting
    /// with a dot.
    pub skip_hidden: bool,
}

impl Exclusions {
    pub fn new<S: AsRef<str>>(rules: impl IntoIterator<Item = S>, skip_hidden: bool) -> Self {
        let mut exclusions = Self {
            skip_hidden,
            ..Self::default()
        };
        for rule in rules {
            let rule = rule.as_ref().trim().trim_end_matches(['\\', '/']);
            if rule.is_empty() {
                continue;
            }
            if rule.contains(['\\', '/', ':']) {
                exclusions.paths.push(fold(&rule.replace('/', "\\")));
            } else {
                exclusions.names.push(fold(rule));
            }
        }
        exclusions
    }

    /// Whether the folder or file at `path`, named `name`, is left out.
    pub fn excludes(&self, path: &Path, name: &str) -> bool {
        if !self.names.is_empty() {
            let name = fold(name);
            if self.names.contains(&name) {
                return true;
            }
        }
        if self.paths.is_empty() {
            return false;
        }
        let path = fold(&path.to_string_lossy());
        self.paths.iter().any(|excluded| {
            path.strip_prefix(excluded.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
        })
    }

    fn skips(&self, dir: &Path, listed: &Listed) -> bool {
        if self.skip_hidden && listed.is_hidden() {
            return true;
        }
        // Names and paths are rules for folders; files pass.
        listed.dir && self.excludes(&dir.join(&listed.name), &listed.name)
    }
}

#[derive(Default)]
pub struct ScanOptions<'a> {
    /// Listing threads; 0 picks from the number of cores.
    pub threads: usize,
    /// Set to stop: the scan returns `None`.
    pub cancel: Option<&'a AtomicBool>,
    /// Entries added so far, for "Indexing… 12,345 files".
    pub progress: Option<&'a AtomicUsize>,
}

/// Walks `roots` and builds an index of what they hold. `list` lists one
/// folder; a folder it can't list (no access) is kept, empty. Roots that
/// don't exist are kept too, empty, so they fill in once they appear.
pub fn scan<L>(
    roots: &[PathBuf],
    exclusions: &Exclusions,
    list: &L,
    options: &ScanOptions,
) -> Option<Index>
where
    L: Fn(&Path) -> io::Result<Vec<Listed>> + Sync,
{
    let mut builder = Builder::default();
    scan_into(&mut builder, roots, roots, exclusions, list, options).then(|| builder.finish())
}

/// [`scan`] into a builder that may hold other roots already (ones the
/// index service filled in). `all_roots` are every root of the index: a
/// folder that is one of them is left to its own root. `false` when
/// cancelled.
pub fn scan_into<L>(
    builder: &mut Builder,
    roots: &[PathBuf],
    all_roots: &[PathBuf],
    exclusions: &Exclusions,
    list: &L,
    options: &ScanOptions,
) -> bool
where
    L: Fn(&Path) -> io::Result<Vec<Listed>> + Sync,
{
    let threads = match options.threads {
        0 => thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(2, 8),
        n => n,
    };
    let queue = Queue::default();
    let cancelled = || {
        options
            .cancel
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    };

    thread::scope(|scope| {
        let (found_tx, found_rx) = mpsc::channel::<(EntryId, PathBuf, Vec<Listed>)>();
        for _ in 0..threads {
            let found_tx = found_tx.clone();
            let queue = &queue;
            scope.spawn(move || {
                while let Some((path, id)) = queue.pop() {
                    let listed = list(&path).unwrap_or_default();
                    if found_tx.send((id, path, listed)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(found_tx);

        let mut pending = 0usize;
        for root in roots {
            let id = builder.root(root);
            queue.push(root.clone(), id);
            pending += 1;
        }
        // A root inside another is walked as itself, not again from outside.
        let is_root = root_check(all_roots);
        while pending > 0 {
            let Ok((id, dir, listed)) = found_rx.recv() else {
                break;
            };
            pending -= 1;
            if cancelled() {
                queue.close();
                return false;
            }
            for item in &listed {
                if exclusions.skips(&dir, item) || (item.dir && is_root(&dir.join(&item.name))) {
                    continue;
                }
                let child = builder.add(id, item.meta());
                if item.dir && !item.link {
                    queue.push(dir.join(&item.name), child);
                    pending += 1;
                }
            }
            if let Some(progress) = options.progress {
                progress.store(builder.len(), Ordering::Relaxed);
            }
        }
        queue.close();
        true
    })
}

/// Whether a folder's path is one of `roots`: such a folder is a root of
/// its own and isn't walked again from the root above it.
pub fn root_check(roots: &[PathBuf]) -> impl Fn(&Path) -> bool + Sync + use<> {
    let keys: Vec<String> = roots.iter().map(|root| path_key(root)).collect();
    move |path: &Path| keys.len() > 1 && keys.contains(&path_key(path))
}

/// Adds what folder `dir` (entry `id`, already in the index) holds, and so on
/// down: for a folder that was created or moved into the scope. One thread;
/// such folders are usually small. Returns the number of entries added.
pub fn add_tree<L>(
    index: &mut Index,
    id: EntryId,
    dir: &Path,
    exclusions: &Exclusions,
    list: &L,
) -> usize
where
    L: Fn(&Path) -> io::Result<Vec<Listed>>,
{
    let root_keys: Vec<String> = index
        .roots()
        .iter()
        .map(|&root| path_key(Path::new(index.name(root))))
        .collect();
    let mut added = 0;
    let mut stack = vec![(dir.to_path_buf(), id)];
    while let Some((dir, id)) = stack.pop() {
        for item in list(&dir).unwrap_or_default() {
            if exclusions.skips(&dir, &item)
                || (item.dir && root_keys.contains(&path_key(&dir.join(&item.name))))
            {
                continue;
            }
            let child = index.insert(id, item.meta());
            added += 1;
            if item.dir && !item.link {
                stack.push((dir.join(&item.name), child));
            }
        }
    }
    added
}

/// A path to compare with others: folded, backslashes, no trailing one.
fn path_key(path: &Path) -> String {
    fold(&path.to_string_lossy().replace('/', "\\"))
        .trim_end_matches('\\')
        .to_owned()
}

/// Whether a listed folder entry is left out: for change notifications,
/// which report paths the walk would never have entered.
pub fn is_excluded(exclusions: &Exclusions, dir: &Path, listed: &Listed) -> bool {
    exclusions.skips(dir, listed)
}

/// Folders waiting to be listed, shared by the listing threads.
#[derive(Default)]
struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct QueueState {
    jobs: VecDeque<(PathBuf, EntryId)>,
    closed: bool,
}

impl Queue {
    fn push(&self, path: PathBuf, id: EntryId) {
        self.state
            .lock()
            .expect("scan queue")
            .jobs
            .push_back((path, id));
        self.ready.notify_one();
    }

    /// The next folder, waiting for one; `None` once closed.
    fn pop(&self) -> Option<(PathBuf, EntryId)> {
        let mut state = self.state.lock().expect("scan queue");
        loop {
            if state.closed {
                return None;
            }
            // Depth first keeps the queue short.
            if let Some(job) = state.jobs.pop_back() {
                return Some(job);
            }
            state = self.ready.wait(state).expect("scan queue");
        }
    }

    fn close(&self) {
        self.state.lock().expect("scan queue").closed = true;
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{Query, SearchOptions};

    /// Like the app's defaults.
    const RULES: &[&str] = &["AppData", "node_modules", ".git", "target"];

    fn file(name: &str) -> Listed {
        Listed {
            name: name.into(),
            modified: 1_700_000_000,
            ..Listed::default()
        }
    }

    fn dir(name: &str) -> Listed {
        Listed {
            name: name.into(),
            dir: true,
            ..Listed::default()
        }
    }

    /// A fake disk: folder path to its listing.
    fn disk() -> HashMap<PathBuf, Vec<Listed>> {
        let mut disk = HashMap::new();
        disk.insert(
            PathBuf::from(r"C:\Users\me"),
            vec![
                dir("Documents"),
                dir("AppData"),
                dir("project"),
                Listed {
                    hidden: true,
                    ..file("desktop.ini")
                },
                Listed {
                    link: true,
                    ..dir("My Music")
                },
            ],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\Documents"),
            vec![file("report.pdf"), dir("Old")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\Documents\Old"),
            vec![file("draft.docx")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\AppData"),
            vec![file("cache.bin")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\project"),
            vec![file("main.rs"), dir("target"), dir(".git")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\project\target"),
            vec![file("main.exe")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\project\.git"),
            vec![file("HEAD")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\My Music"),
            vec![file("song.mp3")],
        );
        disk
    }

    fn names(index: &Index) -> Vec<String> {
        let mut names: Vec<String> = (0..index.len() as EntryId)
            .filter(|&id| index.is_live(id) && index.parent(id).is_some())
            .map(|id| index.name(id).to_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn walks_everything_but_exclusions_hidden_and_links() {
        let disk = disk();
        let list = |path: &Path| Ok(disk.get(path).cloned().unwrap_or_default());
        let exclusions = Exclusions::new(RULES, true);
        let index = scan(
            &[PathBuf::from(r"C:\Users\me")],
            &exclusions,
            &list,
            &ScanOptions::default(),
        )
        .unwrap();
        assert_eq!(
            names(&index),
            [
                "Documents",
                "My Music",
                "Old",
                "draft.docx",
                "main.rs",
                "project",
                "report.pdf"
            ]
        );
        let draft = index
            .resolve(Path::new(r"c:\users\ME\documents\old\DRAFT.docx"))
            .unwrap();
        assert_eq!(
            index.path(draft),
            Path::new(r"C:\Users\me\Documents\Old\draft.docx")
        );
        assert_eq!(index.resolve(Path::new(r"C:\Users\me\AppData")), None);
        assert_eq!(index.resolve(Path::new(r"D:\elsewhere")), None);
    }

    #[test]
    fn a_root_inside_another_is_walked_once() {
        let mut disk = disk();
        disk.insert(PathBuf::from(r"C:\Users\me\AppData"), vec![dir("Local")]);
        disk.insert(
            PathBuf::from(r"C:\Users\me\AppData\Local"),
            vec![file("notes.txt")],
        );
        let list = |path: &Path| Ok(disk.get(path).cloned().unwrap_or_default());
        let exclusions = Exclusions::new(RULES, true);
        let roots = [
            PathBuf::from(r"C:\Users\me"),
            PathBuf::from(r"C:\Users\me\AppData\Local"),
            PathBuf::from(r"C:\Users\me\Documents"),
        ];
        let index = scan(&roots, &exclusions, &list, &ScanOptions::default()).unwrap();
        // The excluded AppData's Local is a root of its own, and Documents
        // isn't listed twice.
        let notes = index
            .resolve(Path::new(r"C:\Users\me\AppData\Local\notes.txt"))
            .unwrap();
        assert_eq!(index.parent(notes), Some(index.roots()[1]));
        let reports = (0..index.len() as EntryId)
            .filter(|&id| index.name(id) == "report.pdf")
            .count();
        assert_eq!(reports, 1);
        let report = index
            .resolve(Path::new(r"C:\Users\me\Documents\report.pdf"))
            .unwrap();
        assert_eq!(index.parent(report), Some(index.roots()[2]));
    }

    #[test]
    fn hidden_files_stay_when_asked() {
        let disk = disk();
        let list = |path: &Path| Ok(disk.get(path).cloned().unwrap_or_default());
        let exclusions = Exclusions::new(["AppData", "target"], false);
        let index = scan(
            &[PathBuf::from(r"C:\Users\me")],
            &exclusions,
            &list,
            &ScanOptions::default(),
        )
        .unwrap();
        let names = names(&index);
        assert!(names.contains(&"desktop.ini".to_owned()));
        assert!(names.contains(&"HEAD".to_owned()));
    }

    #[test]
    fn path_rules_exclude_a_subtree() {
        let exclusions = Exclusions::new([r"C:\Users\me\Documents\Old\", "node_modules"], true);
        assert!(exclusions.excludes(Path::new(r"c:\users\me\documents\old"), "old"));
        assert!(exclusions.excludes(Path::new(r"C:\Users\me\Documents\Old\x"), "x"));
        assert!(!exclusions.excludes(Path::new(r"C:\Users\me\Documents\Older"), "Older"));
        assert!(exclusions.excludes(Path::new(r"D:\code\node_modules"), "node_modules"));
    }

    #[test]
    fn cancel_stops_the_scan() {
        let disk = disk();
        let list = |path: &Path| Ok(disk.get(path).cloned().unwrap_or_default());
        let cancel = AtomicBool::new(true);
        let options = ScanOptions {
            cancel: Some(&cancel),
            ..ScanOptions::default()
        };
        assert!(
            scan(
                &[PathBuf::from(r"C:\Users\me")],
                &Exclusions::default(),
                &list,
                &options
            )
            .is_none()
        );
    }

    #[test]
    fn added_folders_bring_their_contents() {
        let mut disk = disk();
        let list = |path: &Path| Ok(disk.get(path).cloned().unwrap_or_default());
        let exclusions = Exclusions::new(RULES, true);
        let mut index = scan(
            &[PathBuf::from(r"C:\Users\me")],
            &exclusions,
            &list,
            &ScanOptions::default(),
        )
        .unwrap();

        // A folder moved in from outside the scope arrives as one name.
        disk.insert(
            PathBuf::from(r"C:\Users\me\Photos"),
            vec![file("cat.jpg"), dir("2024"), dir("node_modules")],
        );
        disk.insert(
            PathBuf::from(r"C:\Users\me\Photos\2024"),
            vec![file("dog.jpg")],
        );
        let list = |path: &Path| Ok(disk.get(path).cloned().unwrap_or_default());
        let root = index.roots()[0];
        let photos = index.insert(root, dir("Photos").meta());
        let added = add_tree(
            &mut index,
            photos,
            Path::new(r"C:\Users\me\Photos"),
            &exclusions,
            &list,
        );
        assert_eq!(added, 3);
        let options = SearchOptions::default();
        let hits = index.search(&Query::parse("dog"), &options).unwrap();
        assert_eq!(
            index.path(hits[0].id),
            Path::new(r"C:\Users\me\Photos\2024\dog.jpg")
        );
    }
}
