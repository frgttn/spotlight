//! A volume the service follows: its table, kept current from the journal,
//! and the launchers to tell about changes.
//!
//! Loading uses the snapshot when the journal still holds everything since
//! it; otherwise the MFT is read again. A journal that was reset or wrapped
//! past the snapshot's position means changes were lost, so the table is
//! read again and every launcher is told to ask anew ([`Change::Overflow`]).

use std::{
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use spotlight_platform_win::{
    fswatch::Change,
    index_service::Entry,
    ntfs::{self, Journal, Record},
};

use crate::{
    log,
    store::{Position, Store},
    tree::Tree,
};

/// How often the journal is read for new changes.
const POLL: Duration = Duration::from_millis(500);
/// A changed table is saved this often, and when the service stops.
const SAVE_EVERY: Duration = Duration::from_secs(300);

/// What the service reads a volume through: the real one needs
/// administrator rights, tests use a fake.
pub trait Source: Send + Sync {
    fn journal(&self) -> io::Result<Journal>;
    fn enumerate(&self, journal: &Journal, f: &mut dyn FnMut(Record)) -> io::Result<()>;
    fn read_changes(
        &self,
        journal: &Journal,
        from: i64,
        f: &mut dyn FnMut(Record),
    ) -> io::Result<i64>;
    /// The file reference number of the folder at `path`.
    fn frn_of(&self, path: &Path) -> io::Result<u64>;
    fn modified(&self, frn: u64) -> Option<i64>;
}

impl Source for ntfs::Volume {
    fn journal(&self) -> io::Result<Journal> {
        ntfs::Volume::journal(self)
    }

    fn enumerate(&self, journal: &Journal, f: &mut dyn FnMut(Record)) -> io::Result<()> {
        ntfs::Volume::enumerate(self, journal, f)
    }

    fn read_changes(
        &self,
        journal: &Journal,
        from: i64,
        f: &mut dyn FnMut(Record),
    ) -> io::Result<i64> {
        ntfs::Volume::read_changes(self, journal, from, f)
    }

    fn frn_of(&self, path: &Path) -> io::Result<u64> {
        ntfs::Volume::frn_of(path)
    }

    fn modified(&self, frn: u64) -> Option<i64> {
        ntfs::Volume::modified(self, frn)
    }
}

struct State {
    tree: Tree,
    journal: Journal,
    /// The USN to read from next.
    next: i64,
    /// Changed since the last save.
    dirty: bool,
}

pub struct Volume {
    pub letter: char,
    source: Box<dyn Source>,
    state: Mutex<State>,
    subscribers: Mutex<Vec<Sender<Arc<Vec<Change>>>>>,
    stop: AtomicBool,
}

impl Volume {
    /// Loads drive `letter`: from its snapshot in `store` when the journal
    /// still covers it, else by reading the MFT.
    pub fn load(letter: char, source: Box<dyn Source>, store: Option<&Store>) -> io::Result<Self> {
        let journal = source.journal()?;
        let started = Instant::now();
        let saved = store.and_then(|store| store.load(letter));
        let state = match saved {
            Some((position, tree))
                if position.journal == journal.id && position.next >= journal.first =>
            {
                let mut state = State {
                    tree,
                    journal,
                    next: position.next,
                    dirty: false,
                };
                // What changed while the service wasn't running.
                let mut caught_up = 0usize;
                let next = source.read_changes(&journal, state.next, &mut |record| {
                    state.tree.apply(&record);
                    caught_up += 1;
                })?;
                state.next = next;
                state.dirty = caught_up > 0;
                log!(
                    "{letter}: snapshot of {} entries, {caught_up} changes since, in {:.1} s",
                    state.tree.len(),
                    started.elapsed().as_secs_f64()
                );
                state
            }
            _ => {
                let state = read_table(letter, source.as_ref(), journal)?;
                log!(
                    "{letter}: MFT read, {} entries, {:.1} MB, in {:.1} s",
                    state.tree.len(),
                    state.tree.memory_bytes() as f64 / 1_048_576.,
                    started.elapsed().as_secs_f64()
                );
                state
            }
        };
        Ok(Self {
            letter,
            source,
            state: Mutex::new(state),
            subscribers: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        })
    }

    /// Batches of changes from now on, until the receiver is dropped.
    pub fn subscribe(&self) -> Receiver<Arc<Vec<Change>>> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().expect("subscribers").push(tx);
        rx
    }

    /// Everything under folder `top`, for a launcher; see [`Tree::subtree`].
    pub fn snapshot(&self, top: &Path, enter: impl FnMut(&Path) -> bool) -> io::Result<Vec<Entry>> {
        let frn = self.source.frn_of(top)?;
        let mut state = self.state.lock().expect("volume");
        if state.tree.get(frn).is_none() && frn != state.tree.root {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the folder isn't in the table",
            ));
        }
        let entries = state
            .tree
            .subtree(frn, enter, |frn| self.source.modified(frn));
        state.dirty = true;
        Ok(entries)
    }

    /// Reads the journal every [`POLL`] on a thread of its own and tells
    /// the subscribers; saves to `store` now and then.
    pub fn follow(self: &Arc<Self>, store: Option<Arc<Store>>) {
        let volume = self.clone();
        let spawned = thread::Builder::new()
            .name(format!("journal-{}", self.letter))
            .spawn(move || {
                let mut saved = Instant::now();
                while !volume.stop.load(Ordering::Relaxed) {
                    thread::sleep(POLL);
                    let changes = match volume.poll() {
                        Ok(changes) => changes,
                        Err(err) => {
                            log!(
                                "{}: journal unreadable ({err}); reading the MFT again",
                                volume.letter
                            );
                            match volume.reload() {
                                Ok(()) => vec![Change::Overflow],
                                Err(err) => {
                                    log!("{}: the MFT couldn't be read: {err}", volume.letter);
                                    vec![Change::Stopped(err.to_string())]
                                }
                            }
                        }
                    };
                    if !changes.is_empty() {
                        volume.broadcast(changes);
                    }
                    if let Some(store) = &store
                        && saved.elapsed() >= SAVE_EVERY
                    {
                        volume.save(store);
                        saved = Instant::now();
                    }
                }
            });
        if let Err(err) = spawned {
            log!("{}: couldn't follow the journal: {err}", self.letter);
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Saves the table if it changed since the last save.
    pub fn save(&self, store: &Store) {
        let mut state = self.state.lock().expect("volume");
        if !state.dirty {
            return;
        }
        let position = Position {
            journal: state.journal.id,
            next: state.next,
        };
        match store.save(self.letter, position, &state.tree) {
            Ok(()) => state.dirty = false,
            Err(err) => log!("{}: snapshot not saved: {err}", self.letter),
        }
    }

    /// New changes since the last read, as paths. An error means the
    /// journal no longer covers where this left off.
    fn poll(&self) -> io::Result<Vec<Change>> {
        let journal = self.source.journal()?;
        let mut state = self.state.lock().expect("volume");
        if journal.id != state.journal.id || journal.first > state.next {
            return Err(io::Error::other("the journal was reset or wrapped"));
        }
        let mut changes = Vec::new();
        let State { tree, next, .. } = &mut *state;
        let read = self.source.read_changes(&journal, *next, &mut |record| {
            changes.extend(tree.apply(&record));
        })?;
        state.next = read;
        if !changes.is_empty() {
            state.dirty = true;
        }
        Ok(changes)
    }

    fn reload(&self) -> io::Result<()> {
        let journal = self.source.journal()?;
        let fresh = read_table(self.letter, self.source.as_ref(), journal)?;
        *self.state.lock().expect("volume") = fresh;
        Ok(())
    }

    fn broadcast(&self, changes: Vec<Change>) {
        let batch = Arc::new(changes);
        self.subscribers
            .lock()
            .expect("subscribers")
            .retain(|subscriber| subscriber.send(batch.clone()).is_ok());
    }
}

fn read_table(letter: char, source: &dyn Source, journal: Journal) -> io::Result<State> {
    let root_path = format!("{letter}:\\");
    let root = source.frn_of(Path::new(&root_path))?;
    let mut tree = Tree::new(root, root_path.into());
    source.enumerate(&journal, &mut |record| tree.insert(&record))?;
    Ok(State {
        tree,
        journal,
        next: journal.next,
        dirty: true,
    })
}

#[cfg(test)]
pub mod fake {
    //! A volume in memory, with a journal tests append to.

    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    pub const ROOT: u64 = 0x0005_0000_0000_0005;

    pub struct Disk {
        pub journal: Journal,
        /// What enumerating finds.
        pub table: Vec<Record>,
        /// Journal records, by USN.
        pub changes: Vec<Record>,
        /// Folder paths to their references, for `frn_of`.
        pub folders: HashMap<PathBuf, u64>,
    }

    impl Default for Disk {
        fn default() -> Self {
            Self {
                journal: Journal {
                    id: 1,
                    first: 0,
                    next: 0,
                },
                table: Vec::new(),
                changes: Vec::new(),
                folders: HashMap::new(),
            }
        }
    }

    pub struct FakeVolume(pub Arc<Mutex<Disk>>);

    impl Source for FakeVolume {
        fn journal(&self) -> io::Result<Journal> {
            Ok(self.0.lock().unwrap().journal)
        }

        fn enumerate(&self, _: &Journal, f: &mut dyn FnMut(Record)) -> io::Result<()> {
            for record in self.0.lock().unwrap().table.clone() {
                f(record);
            }
            Ok(())
        }

        fn read_changes(
            &self,
            _: &Journal,
            from: i64,
            f: &mut dyn FnMut(Record),
        ) -> io::Result<i64> {
            let disk = self.0.lock().unwrap();
            for record in disk.changes.iter().filter(|record| record.usn >= from) {
                f(record.clone());
            }
            Ok(disk.journal.next)
        }

        fn frn_of(&self, path: &Path) -> io::Result<u64> {
            let disk = self.0.lock().unwrap();
            if path == Path::new(r"C:\") {
                return Ok(ROOT);
            }
            disk.folders
                .get(path)
                .copied()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such folder"))
        }

        fn modified(&self, frn: u64) -> Option<i64> {
            Some(1_000 + (frn & 0xFF) as i64)
        }
    }

    pub fn record(frn: u64, parent: u64, name: &str, dir: bool, reason: u32, usn: i64) -> Record {
        Record {
            frn,
            parent,
            usn,
            reason,
            attributes: if dir { 0x10 } else { 0x20 },
            time: 0,
            name: name.into(),
        }
    }

    /// `C:\Users\me\notes.txt` on a fake disk.
    pub fn disk() -> Arc<Mutex<Disk>> {
        let me = 0x0001_0000_0000_0021;
        let disk = Disk {
            journal: Journal {
                id: 1,
                first: 0,
                next: 10,
            },
            table: vec![
                record(0x0001_0000_0000_0020, ROOT, "Users", true, 0, 0),
                record(me, 0x0001_0000_0000_0020, "me", true, 0, 0),
                record(0x0001_0000_0000_0022, me, "notes.txt", false, 0, 0),
            ],
            changes: Vec::new(),
            folders: HashMap::from([(PathBuf::from(r"C:\Users\me"), me)]),
        };
        Arc::new(Mutex::new(disk))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use spotlight_platform_win::ntfs::{USN_REASON_CLOSE, USN_REASON_FILE_CREATE};

    use super::{fake::*, *};

    #[test]
    fn follows_the_journal_and_resumes_from_a_snapshot() {
        let dir = std::env::temp_dir().join(format!("spotlight-volume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        let disk = disk();

        let volume = Volume::load('C', Box::new(FakeVolume(disk.clone())), Some(&store)).unwrap();
        let entries = volume
            .snapshot(Path::new(r"C:\Users\me"), |_| true)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].listed.name, "notes.txt");
        // Its time was read from the "MFT record".
        assert_eq!(entries[0].listed.modified, 1_000 + 0x22);

        let changes = volume.subscribe();
        {
            let mut disk = disk.lock().unwrap();
            disk.changes.push(record(
                0x0001_0000_0000_0030,
                0x0001_0000_0000_0021,
                "plan.md",
                false,
                USN_REASON_FILE_CREATE | USN_REASON_CLOSE,
                10,
            ));
            disk.journal.next = 20;
        }
        let polled = volume.poll().unwrap();
        assert_eq!(polled, [Change::Added(r"C:\Users\me\plan.md".into())]);
        volume.broadcast(polled);
        assert_eq!(changes.recv().unwrap().len(), 1);
        volume.save(&store);

        // A change while "stopped", then a restart from the snapshot.
        {
            let mut disk = disk.lock().unwrap();
            disk.changes.push(record(
                0x0001_0000_0000_0031,
                0x0001_0000_0000_0021,
                "late.txt",
                false,
                USN_REASON_FILE_CREATE,
                20,
            ));
            disk.journal.next = 30;
            // Enumerating would no longer find anything: the snapshot must be used.
            disk.table.clear();
        }
        let again = Volume::load('C', Box::new(FakeVolume(disk.clone())), Some(&store)).unwrap();
        let mut names: Vec<String> = again
            .snapshot(Path::new(r"C:\Users\me"), |_| true)
            .unwrap()
            .into_iter()
            .map(|entry| entry.listed.name)
            .collect();
        names.sort();
        assert_eq!(names, ["late.txt", "notes.txt", "plan.md"]);

        // A reset journal: the snapshot is useless, the MFT is read.
        disk.lock().unwrap().journal.id = 2;
        assert!(again.poll().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        let _: PathBuf = dir;
    }
}
