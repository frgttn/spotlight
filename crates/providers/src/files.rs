//! Search Files: the user's own file index (`spotlight-files`), kept up to
//! date while Spotlight runs.
//!
//! [`Files::start`] walks the scope (the user profile and folders added in
//! Settings) on a thread of its own, then watches each scope root for changes
//! and applies them as they come. Search Files is a live list: every
//! keystroke runs [`Provider::search_view`] on a background thread against
//! the index, under a read lock that changes take only briefly.

use std::{
    path::Path,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use spotlight_core::{
    clip,
    file_search::{Config, Status, profile, thousands},
    frecency,
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    view::{ListFilter, ListView, LiveRows, Preview, PreviewBody, View},
};
use spotlight_files::{
    EntryId, Exclusions, Hit, Index, Kind, Listed, Query, ScanOptions, SearchOptions, add_tree,
    is_excluded, scan,
};
use spotlight_platform_win::{
    fswalk,
    fswatch::{self, Change, Watcher},
};

use crate::glyph;

pub const ID: &str = "files";
const SEARCH_VIEW: &str = "search";
/// Rows a query shows.
const LIMIT: usize = 50;
/// Changes are applied in batches at most this often, so a burst (an unzip,
/// a build) takes the write lock a few times rather than once per file.
const APPLY_EVERY: Duration = Duration::from_millis(200);

fn rules(config: &Config) -> Exclusions {
    Exclusions::new(&config.exclusions, !config.include_hidden)
}

pub struct Files {
    shared: Arc<Shared>,
}

struct Shared {
    index: RwLock<Index>,
    config: Mutex<Config>,
    status: Mutex<Status>,
    /// Entries found by the scan under way.
    progress: AtomicUsize,
    /// Bumped by every start and stop; a scan or watcher from an older one
    /// sees it changed and lets go.
    generation: AtomicU64,
    /// The scope roots' watchers; dropping them stops them.
    watchers: Mutex<Vec<Watcher>>,
}

impl Files {
    pub fn new(config: Config) -> Self {
        Self {
            shared: Arc::new(Shared {
                index: RwLock::new(Index::default()),
                config: Mutex::new(config),
                status: Mutex::new(Status::Idle),
                progress: AtomicUsize::new(0),
                generation: AtomicU64::new(0),
                watchers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// (Re)builds the index in the background, then keeps it current.
    pub fn start(&self) {
        self.shared.launch();
    }

    /// Stops watching and lets go of the index's memory.
    pub fn stop(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.watchers.lock().expect("watchers").clear();
        *self.shared.index.write().expect("file index") = Index::default();
        *self.shared.status.lock().expect("status") = Status::Idle;
    }

    pub fn config(&self) -> Config {
        self.shared.config.lock().expect("config").clone()
    }

    /// Uses `config` from now on and indexes again if `running`.
    pub fn set_config(&self, config: Config, running: bool) {
        *self.shared.config.lock().expect("config") = config;
        if running {
            self.start();
        }
    }

    pub fn status(&self) -> Status {
        match &*self.shared.status.lock().expect("status") {
            Status::Indexing(_) => Status::Indexing(self.shared.progress.load(Ordering::Relaxed)),
            status => status.clone(),
        }
    }

    fn rows(&self, query: &str, filter: Option<&str>, cancel: &AtomicBool) -> Option<LiveRows> {
        let include_hidden = self.shared.config.lock().expect("config").include_hidden;
        let index = self.shared.index.read().expect("file index");
        let options = SearchOptions {
            limit: LIMIT,
            now: frecency::now(),
            include_hidden,
            frecency: None,
            cancel: Some(cancel),
        };
        let typed = match filter {
            Some(filter) => format!("{query} {filter}"),
            None => query.to_owned(),
        };
        let parsed = Query::parse(&typed);
        let hits: Vec<Hit> = if parsed.is_empty() {
            index.recent(&options)?
        } else {
            index.search(&parsed, &options)?
        };
        let items = hits.iter().map(|hit| file_item(&index, hit.id)).collect();
        drop(index);
        let empty = match self.status() {
            Status::Idle => Some("File search is off. Turn it on in Settings → Extensions.".into()),
            Status::Indexing(found) => Some(format!("Indexing… {} files so far", thousands(found))),
            Status::Ready { .. } if parsed.is_empty() => Some("No files yet".into()),
            Status::Ready { .. } => Some("No files match".into()),
        };
        Some(LiveRows { items, empty })
    }
}

impl Shared {
    fn current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// Starts a new scan, and with it a new generation: the scan or watchers
    /// of the one before let go.
    fn launch(self: &Arc<Self>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.watchers.lock().expect("watchers").clear();
        let shared = self.clone();
        let spawned = thread::Builder::new()
            .name("file-index".into())
            .spawn(move || shared.run(generation));
        if let Err(err) = spawned {
            eprintln!("[files] could not start indexing: {err}");
        }
    }

    /// Scans, swaps the new index in, then watches and applies changes until
    /// a newer start or a stop.
    fn run(self: Arc<Self>, generation: u64) {
        let config = self.config.lock().expect("config").clone();
        let roots = config.roots();
        let rules = rules(&config);
        *self.status.lock().expect("status") = Status::Indexing(0);
        self.progress.store(0, Ordering::Relaxed);

        // Watching starts before the walk, so nothing that changes during it
        // is missed; those changes wait in the channel until it ends.
        let (changes_tx, changes_rx) = async_channel::unbounded();
        let mut watchers = Vec::new();
        for root in &roots {
            match fswatch::watch(root, changes_tx.clone()) {
                Ok(watcher) => watchers.push(watcher),
                Err(err) => eprintln!("[files] {err}"),
            }
        }
        drop(changes_tx);

        let started = Instant::now();
        let stop = AtomicBool::new(false);
        let options = ScanOptions {
            threads: 0,
            cancel: Some(&stop),
            progress: Some(&self.progress),
        };
        // Checked between folders: a newer start makes this scan moot.
        let list = |dir: &Path| {
            if !self.current(generation) {
                stop.store(true, Ordering::Relaxed);
            }
            fswalk::list(dir)
        };
        let Some(index) = scan(&roots, &rules, &list, &options) else {
            return;
        };
        if !self.current(generation) {
            return;
        }
        let took = started.elapsed();
        let (entries, memory_bytes) = (index.len(), index.memory_bytes());
        eprintln!(
            "[files] indexed {entries} entries in {:.1} s, {:.1} MB",
            took.as_secs_f64(),
            memory_bytes as f64 / 1_048_576.
        );
        *self.index.write().expect("file index") = index;
        *self.watchers.lock().expect("watchers") = watchers;
        *self.status.lock().expect("status") = Status::Ready {
            entries,
            memory_bytes,
            took,
        };

        self.follow(generation, &rules, &changes_rx);
    }

    /// Applies changes in batches until the watchers stop.
    fn follow(
        self: &Arc<Self>,
        generation: u64,
        rules: &Exclusions,
        changes: &async_channel::Receiver<Vec<Change>>,
    ) {
        while let Ok(first) = changes.recv_blocking() {
            // Let a burst gather, then take everything that has arrived.
            thread::sleep(APPLY_EVERY);
            let mut batch = first;
            while let Ok(more) = changes.try_recv() {
                batch.extend(more);
            }
            if !self.current(generation) {
                return;
            }
            if batch
                .iter()
                .any(|change| matches!(change, Change::Overflow))
            {
                eprintln!("[files] changes were lost; indexing again");
                return self.launch();
            }
            let mut index = self.index.write().expect("file index");
            for change in batch {
                apply(&mut index, rules, change);
            }
            let compact = index.needs_compaction();
            drop(index);
            if compact {
                // Searches go on against the old one meanwhile; this thread
                // is the only writer, so nothing changes under it.
                let (compacted, _) = self.index.read().expect("file index").compact();
                *self.index.write().expect("file index") = compacted;
            }
            // Read first: the guard of a scrutinee lives through the `if let`.
            let took = match *self.status.lock().expect("status") {
                Status::Ready { took, .. } => Some(took),
                _ => None,
            };
            if let Some(took) = took {
                let (entries, memory_bytes) = {
                    let index = self.index.read().expect("file index");
                    (index.len(), index.memory_bytes())
                };
                *self.status.lock().expect("status") = Status::Ready {
                    entries,
                    memory_bytes,
                    took,
                };
            }
        }
    }
}

/// Applies one change to the index. Paths outside every scope root, or under
/// an excluded folder, resolve to nothing and are ignored.
fn apply(index: &mut Index, rules: &Exclusions, change: Change) {
    match change {
        Change::Added(path) | Change::Modified(path) => add_or_update(index, rules, &path),
        Change::Removed(path) => {
            if let Some(id) = index.resolve(&path) {
                index.remove(id);
            }
        }
        Change::Renamed { from, to } => match index.resolve(&from) {
            Some(id) => {
                let target = to.parent().and_then(|parent| index.resolve(parent));
                let name = to
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                match (target, name, fswalk::stat(&to)) {
                    (Some(parent), Some(name), Some(listed))
                        if !is_excluded(rules, to.parent().unwrap_or(&to), &listed) =>
                    {
                        index.rename(id, parent, &name);
                    }
                    _ => index.remove(id),
                }
            }
            None => add_or_update(index, rules, &to),
        },
        Change::Overflow => {}
        Change::Stopped(why) => eprintln!("[files] a watch stopped: {why}"),
    }
}

fn add_or_update(index: &mut Index, rules: &Exclusions, path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let Some(parent) = index.resolve(dir) else {
        return;
    };
    let Some(listed) = fswalk::stat(path) else {
        return;
    };
    if is_excluded(rules, dir, &listed) {
        return;
    }
    let existed = index.child(parent, &listed.name).is_some();
    let id = index.insert(parent, meta(&listed));
    // A folder that appeared brings its contents: one moved in from outside
    // the scope arrives as a single name.
    if listed.dir && !listed.link && !existed {
        add_tree(index, id, path, rules, &fswalk::list);
    }
}

fn meta(listed: &Listed) -> spotlight_files::Meta<'_> {
    spotlight_files::Meta {
        name: &listed.name,
        dir: listed.dir,
        hidden: listed.hidden,
        system: listed.system,
        modified: listed.modified,
    }
}

impl Provider for Files {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "File Search"
    }

    fn description(&self) -> &'static str {
        "Find files and folders in your user folder and the folders you add."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        vec![
            SearchItem::new(item_id(ID, SEARCH_VIEW), "Search Files")
                .subtitle("File Search")
                .keywords(&["find file", "open file", "everything", "explorer"])
                .icon(Icon::Glyph(glyph::FOLDER))
                .action(
                    "Search Files",
                    Command::OpenProviderView {
                        provider: ID.into(),
                        view: SEARCH_VIEW.into(),
                    },
                ),
        ]
    }

    fn view(&self, id: &str) -> Option<View> {
        if id != SEARCH_VIEW {
            return None;
        }
        let never = AtomicBool::new(false);
        let rows = self.rows("", None, &never).unwrap_or_default();
        let filter = |label: &str, keyword: Option<&str>| ListFilter::new(label, keyword);
        Some(View::List(ListView {
            title: "Search Files".into(),
            placeholder: Some("Search files and folders…".into()),
            items: rows.items,
            empty: rows.empty,
            filters: vec![
                filter("All", None),
                filter("Folders", Some("kind:folder")),
                filter("Documents", Some("kind:document")),
                filter("Images", Some("kind:image")),
                filter("Audio", Some("kind:audio")),
                filter("Videos", Some("kind:video")),
                filter("Archives", Some("kind:archive")),
                filter("Apps", Some("kind:app")),
            ],
            live: true,
        }))
    }

    fn search_view(
        &self,
        view: &str,
        query: &str,
        filter: Option<&str>,
        cancel: &AtomicBool,
    ) -> Option<LiveRows> {
        (view == SEARCH_VIEW)
            .then(|| self.rows(query, filter, cancel))
            .flatten()
    }
}

/// A result row: open, show, copy, or move to the Recycle Bin.
fn file_item(index: &Index, id: EntryId) -> SearchItem {
    let path = index.path(id);
    let text = path.to_string_lossy().into_owned();
    let name = index.name(id).to_owned();
    let dir = index.is_dir(id);
    let mut item = SearchItem::new(item_id(ID, &text), &name)
        .subtitle(path.parent().map(shorten).unwrap_or_default())
        .icon(Icon::Shell(text.clone()))
        .action("Open", Command::Open(text.clone()))
        .action("Show in Explorer", Command::ShowInFolder(text.clone()));
    if !dir {
        item = item.action("Open With…", Command::OpenWith(text.clone()));
    }
    item = item
        .action("Copy Path", Command::Copy(text.clone()))
        .action("Copy Name", Command::Copy(name))
        .action(
            if dir { "Copy Folder" } else { "Copy File" },
            Command::CopyFile(text.clone()),
        )
        .action("Move to Recycle Bin", Command::MoveToTrash(text.clone()));
    let kind = if dir {
        "Folder".to_owned()
    } else {
        match Kind::of_ext(index.extension(id)) {
            Kind::Other if index.extension(id).is_empty() => "File".to_owned(),
            Kind::Other => format!("{} file", index.extension(id).to_uppercase()),
            kind => format!("{kind:?}"),
        }
    };
    let mut metadata = vec![("Kind".to_owned(), kind)];
    if !dir && let Ok(meta) = std::fs::metadata(&path) {
        metadata.push(("Size".into(), size(meta.len())));
    }
    let modified = index.modified(id);
    if modified > 0 {
        metadata.push(("Modified".into(), clip::ago(modified, frecency::now())));
    }
    metadata.push((
        "Where".into(),
        path.parent().map(shorten).unwrap_or_default(),
    ));
    item.preview(Preview {
        body: PreviewBody::Thumbnail(text),
        metadata,
    })
}

/// `~\Documents` for a folder in the user profile, else the path.
fn shorten(path: &Path) -> String {
    if let Some(profile) = profile()
        && let Ok(rest) = path.strip_prefix(&profile)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~\\{}", rest.display())
        };
    }
    path.display().to_string()
}

/// `812 bytes`, `14 KB`, `3.2 MB`, `1.4 GB`.
fn size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} bytes")
    } else if value < KB * KB {
        format!("{:.0} KB", value / KB)
    } else if value < KB * KB * KB {
        format!("{:.1} MB", value / (KB * KB))
    } else {
        format!("{:.1} GB", value / (KB * KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    /// Polls `check` for up to five seconds.
    fn eventually(check: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Indexes a folder of its own (besides the profile), then follows a
    /// file being created, renamed, moved into a new folder and deleted.
    #[test]
    fn follows_changes_on_disk() {
        let dir = std::env::temp_dir().join(format!("spotlight-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("zqx-before.txt"), "x").unwrap();
        let files = Files::new(Config {
            folders: vec![dir.clone()],
            ..Config::default()
        });
        files.start();
        assert!(eventually(|| matches!(
            files.status(),
            Status::Ready { .. }
        )));

        let never = AtomicBool::new(false);
        let titles = |query: &str| -> Vec<String> {
            files
                .search_view(SEARCH_VIEW, query, None, &never)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.title)
                .collect()
        };
        assert_eq!(titles("'zqx-before"), ["zqx-before.txt"]);

        fs::write(dir.join("zqx-new.md"), "x").unwrap();
        assert!(eventually(|| titles("'zqx-new") == ["zqx-new.md"]));

        fs::rename(dir.join("zqx-new.md"), dir.join("zqx-renamed.md")).unwrap();
        assert!(eventually(
            || titles("'zqx-renamed") == ["zqx-renamed.md"] && titles("'zqx-new").is_empty()
        ));

        // A folder made elsewhere and moved in brings what it holds.
        let outside =
            std::env::temp_dir().join(format!("spotlight-files-out-{}", std::process::id()));
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(outside.join("inner")).unwrap();
        fs::write(outside.join("inner").join("zqx-deep.pdf"), "x").unwrap();
        fs::rename(&outside, dir.join("moved")).unwrap();
        assert!(eventually(|| titles("'zqx-deep") == ["zqx-deep.pdf"]));

        fs::remove_file(dir.join("zqx-renamed.md")).unwrap();
        assert!(eventually(|| titles("'zqx-renamed").is_empty()));

        files.stop();
        assert_eq!(files.status(), Status::Idle);
        fs::remove_dir_all(&dir).unwrap();
        let _: PathBuf = dir;
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(size(812), "812 bytes");
        assert_eq!(size(14 * 1024), "14 KB");
        assert_eq!(size(3_355_443), "3.2 MB");
    }
}
