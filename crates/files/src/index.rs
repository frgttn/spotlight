//! The entry table: one slot per file or folder, with changes applied in
//! place.
//!
//! An entry stores its parent's id, so a path is rebuilt only for the rows on
//! screen and a folder rename or move touches one entry. A scope root (a
//! folder the user indexes) is an entry whose name is its full path. Removing
//! an entry marks it deleted and leaves its descendants in place: search
//! skips anything under a deleted folder, and [`Index::compact`] drops them.
//!
//! Change notifications name paths, so the index also maps each (folder,
//! case-folded name) pair to its entry: [`Index::resolve`] walks a path down
//! from its scope root one name at a time.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Component, Path, PathBuf},
};

use crate::{
    kind::{ExtId, Exts},
    names::{NameId, Names, fold},
};

pub type EntryId = u32;

/// The parent of a scope root.
pub const NO_PARENT: EntryId = EntryId::MAX;

pub(crate) mod flag {
    pub const DIR: u16 = 1 << 0;
    pub const HIDDEN: u16 = 1 << 1;
    pub const SYSTEM: u16 = 1 << 2;
    pub const ROOT: u16 = 1 << 3;
    pub const DELETED: u16 = 1 << 4;
}

/// Seconds from the unix epoch to 2000-01-01, the origin of [`Entry::mtime`].
const MTIME_EPOCH: i64 = 946_684_800;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub parent: EntryId,
    pub name: NameId,
    /// Minutes since 2000-01-01 UTC; 0 when unknown.
    pub mtime: u32,
    pub ext: ExtId,
    pub flags: u16,
}

impl Entry {
    pub fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// What a walker or the NTFS journal knows about a file or folder.
#[derive(Clone, Copy, Debug, Default)]
pub struct Meta<'a> {
    pub name: &'a str,
    pub dir: bool,
    pub hidden: bool,
    pub system: bool,
    /// Last modification, unix seconds; 0 when unknown.
    pub modified: i64,
}

impl Meta<'_> {
    fn flags(&self) -> u16 {
        let mut flags = 0;
        if self.dir {
            flags |= flag::DIR;
        }
        // A leading dot hides a name on Unix, and tools keep that habit on
        // Windows too (`.vscode`, `.cargo`, `.gitignore`).
        if self.hidden || self.name.starts_with('.') {
            flags |= flag::HIDDEN;
        }
        if self.system {
            flags |= flag::SYSTEM;
        }
        flags
    }
}

fn to_mtime(unix: i64) -> u32 {
    if unix <= MTIME_EPOCH {
        return 0;
    }
    ((unix - MTIME_EPOCH) / 60).min(u32::MAX as i64) as u32
}

pub(crate) fn from_mtime(mtime: u32) -> i64 {
    if mtime == 0 {
        0
    } else {
        MTIME_EPOCH + mtime as i64 * 60
    }
}

/// Collects a full scan, then builds the [`Index`] with every name in the fst.
#[derive(Default)]
pub struct Builder {
    entries: Vec<Entry>,
    /// Name to its position in `order`, until `finish` assigns final ids.
    names: HashMap<Box<str>, NameId>,
    exts: Exts,
    children: HashMap<u64, EntryId>,
    roots: Vec<EntryId>,
}

impl Builder {
    pub fn with_capacity(entries: usize) -> Self {
        Self {
            entries: Vec::with_capacity(entries),
            names: HashMap::with_capacity(entries / 2),
            exts: Exts::default(),
            children: HashMap::with_capacity(entries),
            roots: Vec::new(),
        }
    }

    /// Adds a scope root: a folder whose name is its full path.
    pub fn root(&mut self, path: &Path) -> EntryId {
        let name = self.intern(&path.to_string_lossy());
        let id = push(
            &mut self.entries,
            Entry {
                parent: NO_PARENT,
                name,
                mtime: 0,
                ext: 0,
                flags: flag::DIR | flag::ROOT,
            },
        );
        self.roots.push(id);
        id
    }

    /// Adds an entry under `parent`. A scan lists each name once per folder,
    /// so unlike [`Index::insert`] this doesn't look for one already there.
    pub fn add(&mut self, parent: EntryId, meta: Meta) -> EntryId {
        let ext = if meta.dir {
            0
        } else {
            self.exts.intern(meta.name)
        };
        let name = self.intern(meta.name);
        let id = push(
            &mut self.entries,
            Entry {
                parent,
                name,
                mtime: to_mtime(meta.modified),
                ext,
                flags: meta.flags(),
            },
        );
        self.children.insert(child_key(parent, meta.name), id);
        id
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn finish(mut self) -> Index {
        let mut unique: Vec<&str> = vec![""; self.names.len()];
        for (name, id) in &self.names {
            unique[*id as usize] = name;
        }
        let (names, ids) = Names::build(&unique);
        for entry in &mut self.entries {
            entry.name = ids[entry.name as usize];
        }
        Index {
            entries: self.entries,
            names,
            exts: self.exts,
            children: self.children,
            roots: self.roots,
            deleted: 0,
        }
    }

    fn intern(&mut self, name: &str) -> NameId {
        let next = self.names.len() as NameId;
        *self.names.entry(name.into()).or_insert(next)
    }
}

/// The key of `name` in folder `parent`, case-insensitive as NTFS is. A
/// collision of two 64-bit hashes is checked for on lookup, so at worst it
/// hides one of the two from [`Index::resolve`]; it never finds a wrong entry.
fn child_key(parent: EntryId, name: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    parent.hash(&mut hasher);
    fold(name).hash(&mut hasher);
    hasher.finish()
}

fn push(entries: &mut Vec<Entry>, entry: Entry) -> EntryId {
    let id = entries.len() as EntryId;
    assert!(id < NO_PARENT, "more entries than an index can hold");
    entries.push(entry);
    id
}

pub struct Index {
    pub(crate) entries: Vec<Entry>,
    pub(crate) names: Names,
    pub(crate) exts: Exts,
    /// [`child_key`] of each live entry, to its id.
    children: HashMap<u64, EntryId>,
    roots: Vec<EntryId>,
    /// Entries marked deleted, not counting their descendants.
    deleted: usize,
}

impl Default for Index {
    fn default() -> Self {
        Builder::default().finish()
    }
}

impl Index {
    /// Slots in the table, deleted ones included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn add_root(&mut self, path: &Path) -> EntryId {
        let name = self.names.intern(&path.to_string_lossy());
        let id = push(
            &mut self.entries,
            Entry {
                parent: NO_PARENT,
                name,
                mtime: 0,
                ext: 0,
                flags: flag::DIR | flag::ROOT,
            },
        );
        self.roots.push(id);
        id
    }

    /// Adds `meta` under `parent`, or updates the entry already there by that
    /// name: a scan and a change notification can both report a file. A file
    /// where a folder was, or the other way round, replaces it.
    pub fn insert(&mut self, parent: EntryId, meta: Meta) -> EntryId {
        if let Some(existing) = self.child(parent, meta.name) {
            if self.entries[existing as usize].has(flag::DIR) == meta.dir {
                self.update(existing, meta);
                // Only the case may differ ("readme" became "README").
                if self.name(existing) != meta.name {
                    self.rename(existing, parent, meta.name);
                }
                return existing;
            }
            self.remove(existing);
        }
        let ext = if meta.dir {
            0
        } else {
            self.exts.intern(meta.name)
        };
        let name = self.names.intern(meta.name);
        let id = push(
            &mut self.entries,
            Entry {
                parent,
                name,
                mtime: to_mtime(meta.modified),
                ext,
                flags: meta.flags(),
            },
        );
        self.children.insert(child_key(parent, meta.name), id);
        id
    }

    /// Marks the entry deleted, and with it everything under it.
    pub fn remove(&mut self, id: EntryId) {
        let entry = self.entries[id as usize];
        if entry.has(flag::DELETED) {
            return;
        }
        self.entries[id as usize].flags |= flag::DELETED;
        self.deleted += 1;
        let key = child_key(entry.parent, self.names.get(entry.name));
        if self.children.get(&key) == Some(&id) {
            self.children.remove(&key);
        }
    }

    /// Renames or moves the entry; a folder takes its contents along. An
    /// entry already at the new place by that name is replaced.
    pub fn rename(&mut self, id: EntryId, parent: EntryId, name: &str) {
        if let Some(existing) = self.child(parent, name)
            && existing != id
        {
            self.remove(existing);
        }
        let old = self.entries[id as usize];
        let old_key = child_key(old.parent, self.names.get(old.name));
        if self.children.get(&old_key) == Some(&id) {
            self.children.remove(&old_key);
        }
        let ext = if old.has(flag::DIR) {
            0
        } else {
            self.exts.intern(name)
        };
        let name_id = self.names.intern(name);
        let entry = &mut self.entries[id as usize];
        entry.parent = parent;
        entry.name = name_id;
        entry.ext = ext;
        if !entry.has(flag::DELETED) {
            self.children.insert(child_key(parent, name), id);
        }
    }

    /// The live entry named `name` (in any case) in folder `parent`.
    pub fn child(&self, parent: EntryId, name: &str) -> Option<EntryId> {
        let id = *self.children.get(&child_key(parent, name))?;
        let entry = &self.entries[id as usize];
        (entry.parent == parent
            && !entry.has(flag::DELETED)
            && fold(self.names.get(entry.name)) == fold(name))
        .then_some(id)
    }

    /// The scope roots, in the order they were added.
    pub fn roots(&self) -> &[EntryId] {
        &self.roots
    }

    /// The live entry at `path`, found from the scope root it is under (the
    /// innermost one, when roots nest). Case-insensitive.
    pub fn resolve(&self, path: &Path) -> Option<EntryId> {
        let (root, rest) = self
            .roots
            .iter()
            .filter(|&&root| !self.entries[root as usize].has(flag::DELETED))
            .filter_map(|&root| {
                Some((root, strip_prefix_folded(path, Path::new(self.name(root)))?))
            })
            .min_by_key(|(_, rest)| rest.components().count())?;
        let mut at = root;
        for component in rest.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            at = self.child(at, &name.to_string_lossy())?;
        }
        Some(at)
    }

    /// Updates what changes in place: modification time and attributes. The
    /// name in `meta` is ignored; use [`Index::rename`].
    pub fn update(&mut self, id: EntryId, meta: Meta) {
        let entry = &mut self.entries[id as usize];
        entry.mtime = to_mtime(meta.modified);
        let kept = entry.flags & (flag::ROOT | flag::DELETED | flag::DIR);
        entry.flags = kept | (meta.flags() & !flag::DIR);
    }

    /// Whether the entry and every folder above it still exist.
    pub fn is_live(&self, mut id: EntryId) -> bool {
        while id != NO_PARENT {
            let entry = &self.entries[id as usize];
            if entry.has(flag::DELETED) {
                return false;
            }
            id = entry.parent;
        }
        true
    }

    pub fn name(&self, id: EntryId) -> &str {
        self.names.get(self.entries[id as usize].name)
    }

    pub fn parent(&self, id: EntryId) -> Option<EntryId> {
        let parent = self.entries[id as usize].parent;
        (parent != NO_PARENT).then_some(parent)
    }

    pub fn is_dir(&self, id: EntryId) -> bool {
        self.entries[id as usize].has(flag::DIR)
    }

    /// Unix seconds, to the minute; 0 when unknown.
    pub fn modified(&self, id: EntryId) -> i64 {
        from_mtime(self.entries[id as usize].mtime)
    }

    /// The lowercase extension, without the dot; empty for folders and
    /// files without one.
    pub fn extension(&self, id: EntryId) -> &str {
        self.exts.get(self.entries[id as usize].ext)
    }

    /// The full path: the scope root's path joined with the names below it.
    pub fn path(&self, id: EntryId) -> PathBuf {
        let mut chain = Vec::new();
        let mut at = id;
        while at != NO_PARENT {
            chain.push(at);
            at = self.entries[at as usize].parent;
        }
        let mut path = PathBuf::new();
        for id in chain.into_iter().rev() {
            path.push(self.name(id));
        }
        path
    }

    /// Whether enough has changed since the last build that compacting pays
    /// off: names outside the fst, or slots held by deleted entries.
    pub fn needs_compaction(&self) -> bool {
        let base = self.names.len().saturating_sub(self.names.added_len());
        self.names.added_len() > 50_000.max(base / 20)
            || self.deleted > 10_000.max(self.entries.len() / 10)
    }

    /// A fresh index with every live entry, all names in the fst, and no
    /// deleted slots. Returns it with the new id of each old entry
    /// ([`NO_PARENT`] for dropped ones), for maps that hold entry ids.
    pub fn compact(&self) -> (Index, Vec<EntryId>) {
        let mut builder = Builder::with_capacity(self.entries.len());
        let mut remap = vec![NO_PARENT; self.entries.len()];
        let mut live = vec![None; self.entries.len()];
        let mut chain = Vec::new();
        for id in 0..self.entries.len() as EntryId {
            if remap[id as usize] != NO_PARENT || !self.live_cached(id, &mut live, &mut chain) {
                continue;
            }
            // Parents first: a moved entry can sit before its new parent.
            chain.clear();
            let mut at = id;
            while at != NO_PARENT && remap[at as usize] == NO_PARENT {
                chain.push(at);
                at = self.entries[at as usize].parent;
            }
            for &id in chain.iter().rev() {
                let entry = self.entries[id as usize];
                let new = if entry.has(flag::ROOT) {
                    builder.root(Path::new(self.name(id)))
                } else {
                    let parent = remap[entry.parent as usize];
                    let new = builder.add(
                        parent,
                        Meta {
                            name: self.name(id),
                            dir: entry.has(flag::DIR),
                            ..Meta::default()
                        },
                    );
                    // Copied as is: flags and mtime need no conversion.
                    let copied = &mut builder.entries[new as usize];
                    copied.mtime = entry.mtime;
                    copied.flags = entry.flags;
                    new
                };
                remap[id as usize] = new;
            }
        }
        (builder.finish(), remap)
    }

    /// Approximate heap use, for Settings and the memory budget.
    pub fn memory_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>()
            // A key, an id and a control byte per slot.
            + self.children.capacity() * (size_of::<u64>() + size_of::<EntryId>() + 1)
            + self.names.memory_bytes()
            + self.exts.memory_bytes()
    }

    /// [`Index::is_live`] with answers remembered per entry, so a whole-table
    /// pass stays linear.
    fn live_cached(
        &self,
        id: EntryId,
        live: &mut [Option<bool>],
        chain: &mut Vec<EntryId>,
    ) -> bool {
        chain.clear();
        let mut at = id;
        let mut answer = true;
        while at != NO_PARENT {
            if let Some(known) = live[at as usize] {
                answer = known;
                break;
            }
            let entry = &self.entries[at as usize];
            chain.push(at);
            if entry.has(flag::DELETED) {
                answer = false;
                break;
            }
            at = entry.parent;
        }
        for &id in chain.iter() {
            live[id as usize] = Some(answer);
        }
        answer
    }
}

/// `path` without `prefix`, comparing components without case.
fn strip_prefix_folded<'a>(path: &'a Path, prefix: &Path) -> Option<&'a Path> {
    let mut rest = path.components();
    for want in prefix.components() {
        let got = rest.next()?;
        if fold(&got.as_os_str().to_string_lossy()) != fold(&want.as_os_str().to_string_lossy()) {
            return None;
        }
    }
    Some(rest.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> Meta<'_> {
        Meta {
            name,
            modified: 1_700_000_000,
            ..Meta::default()
        }
    }

    fn dir(name: &str) -> Meta<'_> {
        Meta {
            name,
            dir: true,
            ..Meta::default()
        }
    }

    fn sample() -> (Index, [EntryId; 4]) {
        let mut builder = Builder::default();
        let root = builder.root(Path::new(r"C:\Users\me"));
        let docs = builder.add(root, dir("Documents"));
        let report = builder.add(docs, file("Report.PDF"));
        let notes = builder.add(root, file("notes.txt"));
        (builder.finish(), [root, docs, report, notes])
    }

    #[test]
    fn rebuilds_paths_and_reads_fields() {
        let (index, [root, docs, report, _]) = sample();
        assert_eq!(
            index.path(report),
            Path::new(r"C:\Users\me\Documents\Report.PDF")
        );
        assert_eq!(index.path(root), Path::new(r"C:\Users\me"));
        assert_eq!(index.parent(report), Some(docs));
        assert_eq!(index.parent(root), None);
        assert_eq!(index.extension(report), "pdf");
        assert_eq!(index.extension(docs), "");
        assert!(index.is_dir(docs));
        assert_eq!(index.modified(report), 1_700_000_000 / 60 * 60);
    }

    #[test]
    fn removing_a_folder_hides_its_contents() {
        let (mut index, [_, docs, report, notes]) = sample();
        index.remove(docs);
        assert!(!index.is_live(report));
        assert!(index.is_live(notes));
    }

    #[test]
    fn rename_moves_a_folder_with_its_contents() {
        let (mut index, [root, docs, report, notes]) = sample();
        let archive = index.insert(root, dir("Archive"));
        index.rename(docs, archive, "Old documents");
        assert_eq!(
            index.path(report),
            Path::new(r"C:\Users\me\Archive\Old documents\Report.PDF")
        );
        index.rename(notes, docs, "notes.md");
        assert_eq!(index.extension(notes), "md");
    }

    #[test]
    fn compact_drops_deleted_and_keeps_parents_first() {
        let (mut index, [root, docs, report, notes]) = sample();
        let archive = index.insert(root, dir("Archive"));
        // `docs` now sits before its new parent in the table.
        index.rename(docs, archive, "Documents");
        let new_file = index.insert(archive, file("new.txt"));
        index.remove(notes);

        let (compacted, remap) = index.compact();
        assert_eq!(remap[notes as usize], NO_PARENT);
        assert_eq!(compacted.len(), 5);
        assert_eq!(compacted.names.added_len(), 0);
        let report = remap[report as usize];
        assert_eq!(
            compacted.path(report),
            Path::new(r"C:\Users\me\Archive\Documents\Report.PDF")
        );
        assert_eq!(
            compacted.modified(report),
            index.modified(remap.iter().position(|&n| n == report).unwrap() as EntryId)
        );
        assert_eq!(compacted.name(remap[new_file as usize]), "new.txt");
        for id in 0..compacted.len() as EntryId {
            if let Some(parent) = compacted.parent(id) {
                assert!(parent < id, "parent after child");
            }
        }
    }

    #[test]
    fn mtime_round_trips_to_the_minute() {
        assert_eq!(from_mtime(to_mtime(0)), 0);
        assert_eq!(
            from_mtime(to_mtime(MTIME_EPOCH + 3_659)),
            MTIME_EPOCH + 3_600
        );
    }
}
