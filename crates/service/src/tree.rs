//! One volume's files and folders, by MFT record: what the service keeps in
//! memory and in its snapshot.
//!
//! A node is 24 bytes, in a table indexed by record number (the low 48 bits
//! of a file reference number), with names in one arena. Paths are rebuilt
//! from parent links. Journal records update it in place, and each one says
//! what changed as paths, before and after, for the launchers watching.

use std::path::{Path, PathBuf};

use spotlight_files::Listed;
use spotlight_platform_win::{
    fswatch::Change,
    index_service::{Entry, ROOT},
    ntfs::{
        RECORD_MASK, Record, USN_REASON_BASIC_INFO_CHANGE, USN_REASON_CLOSE,
        USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE, USN_REASON_DATA_TRUNCATION,
        USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
    },
};

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// What changes a file's contents or times.
const CONTENT_CHANGE: u32 = USN_REASON_DATA_EXTEND
    | USN_REASON_DATA_OVERWRITE
    | USN_REASON_DATA_TRUNCATION
    | USN_REASON_BASIC_INFO_CHANGE;
/// Paths deeper than this are a broken parent chain, not a real path.
const MAX_DEPTH: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// The parent's file reference number; 0 for an empty slot.
    pub parent: u64,
    name_at: u32,
    name_len: u16,
    /// The high 16 bits of this node's file reference number.
    pub sequence: u16,
    pub attributes: u32,
    /// Last modification, unix seconds; 0 until read from the MFT record.
    pub modified: i64,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

pub struct Tree {
    /// The volume root's file reference number (record 5 on NTFS).
    pub root: u64,
    /// `C:\`.
    pub root_path: PathBuf,
    nodes: Vec<Node>,
    names: String,
}

impl Tree {
    pub fn new(root: u64, root_path: PathBuf) -> Self {
        Self {
            root,
            root_path,
            nodes: Vec::new(),
            names: String::new(),
        }
    }

    /// Files and folders in use.
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|node| node.parent != 0).count()
    }

    pub fn memory_bytes(&self) -> usize {
        self.nodes.capacity() * size_of::<Node>() + self.names.capacity()
    }

    /// Adds or replaces the node of `record`. A modification time already
    /// read stays unless the record brings a newer one.
    pub fn insert(&mut self, record: &Record) {
        let index = (record.frn & RECORD_MASK) as usize;
        if index >= self.nodes.len() {
            self.nodes.resize(index + 1, Node::default());
        }
        let sequence = (record.frn >> 48) as u16;
        let old = self.nodes[index];
        let kept_time = if old.parent != 0 && old.sequence == sequence {
            old.modified
        } else {
            0
        };
        let (name_at, name_len) = if old.parent != 0 && self.name_of(&old) == record.name {
            (old.name_at, old.name_len)
        } else {
            let at = self.names.len() as u32;
            self.names.push_str(&record.name);
            (at, record.name.len() as u16)
        };
        self.nodes[index] = Node {
            parent: record.parent,
            name_at,
            name_len,
            sequence,
            attributes: record.attributes,
            modified: if record.time > 0 {
                record.time
            } else {
                kept_time
            },
        };
    }

    pub fn remove(&mut self, frn: u64) {
        if self.get(frn).is_some() {
            self.nodes[(frn & RECORD_MASK) as usize] = Node::default();
        }
    }

    /// The node of `frn`, if that record holds that file (not a newer one).
    pub fn get(&self, frn: u64) -> Option<&Node> {
        let node = self.nodes.get((frn & RECORD_MASK) as usize)?;
        (node.parent != 0 && node.sequence == (frn >> 48) as u16).then_some(node)
    }

    #[cfg(test)]
    pub fn set_modified(&mut self, frn: u64, modified: i64) {
        if self.get(frn).is_some() {
            self.nodes[(frn & RECORD_MASK) as usize].modified = modified;
        }
    }

    fn name_of(&self, node: &Node) -> &str {
        &self.names[node.name_at as usize..node.name_at as usize + node.name_len as usize]
    }

    /// The full path of `frn`, or `None` if its chain doesn't reach the root
    /// (it's in a folder being deleted, or in the volume's hidden metadata).
    pub fn path(&self, frn: u64) -> Option<PathBuf> {
        if frn & RECORD_MASK == self.root & RECORD_MASK {
            return Some(self.root_path.clone());
        }
        let mut names = Vec::new();
        let mut at = frn;
        for _ in 0..MAX_DEPTH {
            if at & RECORD_MASK == self.root & RECORD_MASK {
                let mut path = self.root_path.clone();
                for name in names.iter().rev() {
                    path.push(name);
                }
                return Some(path);
            }
            let node = self.get(at)?;
            names.push(self.name_of(node));
            at = node.parent;
        }
        None
    }

    /// Applies a journal record and says what it changed, as paths.
    pub fn apply(&mut self, record: &Record) -> Vec<Change> {
        let old = self.get(record.frn).copied();
        let old_path = old.and_then(|_| self.path(record.frn));
        if record.reason & USN_REASON_FILE_DELETE != 0 {
            self.remove(record.frn);
            return old_path.map(Change::Removed).into_iter().collect();
        }
        // The old name's record changes nothing yet: its new name follows.
        if record.reason & USN_REASON_RENAME_OLD_NAME != 0
            && record.reason & USN_REASON_RENAME_NEW_NAME == 0
        {
            return Vec::new();
        }
        self.insert(record);
        let Some(new_path) = self.path(record.frn) else {
            // Moved out of every known folder.
            return old_path.map(Change::Removed).into_iter().collect();
        };
        match old_path {
            None => vec![Change::Added(new_path)],
            Some(old_path) if old_path != new_path => vec![Change::Renamed {
                from: old_path,
                to: new_path,
            }],
            Some(_)
                if record.reason & CONTENT_CHANGE != 0 && record.reason & USN_REASON_CLOSE != 0 =>
            {
                vec![Change::Modified(new_path)]
            }
            Some(_) => Vec::new(),
        }
    }

    /// Everything under folder `top`, parents first, as a launcher's
    /// snapshot. A folder `enter` refuses (the launcher's user can't list
    /// it) is listed but not entered. `modified` reads the time of a file
    /// whose time isn't known yet; what it finds is kept.
    pub fn subtree(
        &mut self,
        top: u64,
        mut enter: impl FnMut(&Path) -> bool,
        mut modified: impl FnMut(u64) -> Option<i64>,
    ) -> Vec<Entry> {
        let children = self.children();
        let mut entries = Vec::new();
        // (record, its index among `entries`, its path)
        let mut stack: Vec<(usize, u32, PathBuf)> = vec![(
            (top & RECORD_MASK) as usize,
            ROOT,
            self.path(top).unwrap_or_default(),
        )];
        while let Some((record, entry, path)) = stack.pop() {
            for &child in children.of(record) {
                let child = child as usize;
                let node = self.nodes[child];
                let frn = (node.sequence as u64) << 48 | child as u64;
                let name = self.name_of(&node).to_owned();
                let mut time = node.modified;
                if time == 0 && !node.is_dir() {
                    time = modified(frn).unwrap_or(0);
                    self.nodes[child].modified = time;
                }
                let index = entries.len() as u32;
                let child_path = path.join(&name);
                let dir = node.is_dir();
                entries.push(Entry {
                    parent: entry,
                    listed: Listed {
                        name,
                        dir,
                        hidden: node.attributes & FILE_ATTRIBUTE_HIDDEN != 0,
                        system: node.attributes & FILE_ATTRIBUTE_SYSTEM != 0,
                        // Junctions have no children in the MFT, so walking
                        // the table never follows one.
                        link: false,
                        modified: time,
                    },
                });
                if dir && enter(&child_path) {
                    stack.push((child, index, child_path));
                }
            }
        }
        entries
    }

    /// Children of every record, from one pass over the table.
    fn children(&self) -> Children {
        let mut counts = vec![0u32; self.nodes.len() + 1];
        for node in &self.nodes {
            if node.parent != 0 {
                let parent = (node.parent & RECORD_MASK) as usize;
                if parent < self.nodes.len() {
                    counts[parent + 1] += 1;
                }
            }
        }
        for ix in 1..counts.len() {
            counts[ix] += counts[ix - 1];
        }
        let mut next = counts.clone();
        let mut members = vec![0u32; counts[self.nodes.len()] as usize];
        for (ix, node) in self.nodes.iter().enumerate() {
            if node.parent == 0 {
                continue;
            }
            let parent = (node.parent & RECORD_MASK) as usize;
            // A stale child of a reused record: its parent's sequence moved on.
            let parent_ok = self
                .nodes
                .get(parent)
                .is_some_and(|p| p.parent != 0 && p.sequence == (node.parent >> 48) as u16)
                || parent == (self.root & RECORD_MASK) as usize;
            if parent_ok && parent < self.nodes.len() && ix != parent {
                members[next[parent] as usize] = ix as u32;
                next[parent] += 1;
            }
        }
        Children {
            offsets: counts,
            members,
        }
    }

    /// The table and names as bytes, for the snapshot. Names are packed
    /// again, dropping those of renamed and deleted files.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.nodes.len() * 32 + self.names.len());
        out.extend_from_slice(&self.root.to_le_bytes());
        let root_path = self.root_path.to_string_lossy();
        out.extend_from_slice(&(root_path.len() as u32).to_le_bytes());
        out.extend_from_slice(root_path.as_bytes());
        out.extend_from_slice(&(self.nodes.len() as u64).to_le_bytes());
        for node in &self.nodes {
            out.extend_from_slice(&node.parent.to_le_bytes());
            if node.parent == 0 {
                continue;
            }
            out.extend_from_slice(&node.sequence.to_le_bytes());
            out.extend_from_slice(&node.attributes.to_le_bytes());
            out.extend_from_slice(&node.modified.to_le_bytes());
            let name = self.name_of(node);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut input = bytes;
        let mut take = |n: usize| -> Option<&[u8]> {
            if input.len() < n {
                return None;
            }
            let (taken, rest) = input.split_at(n);
            input = rest;
            Some(taken)
        };
        let root = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let root_path = PathBuf::from(std::str::from_utf8(take(len)?).ok()?);
        let count = u64::from_le_bytes(take(8)?.try_into().ok()?) as usize;
        let mut tree = Tree::new(root, root_path);
        tree.nodes.reserve(count);
        for _ in 0..count {
            let parent = u64::from_le_bytes(take(8)?.try_into().ok()?);
            if parent == 0 {
                tree.nodes.push(Node::default());
                continue;
            }
            let sequence = u16::from_le_bytes(take(2)?.try_into().ok()?);
            let attributes = u32::from_le_bytes(take(4)?.try_into().ok()?);
            let modified = i64::from_le_bytes(take(8)?.try_into().ok()?);
            let len = u16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
            let name = std::str::from_utf8(take(len)?).ok()?;
            let name_at = tree.names.len() as u32;
            tree.names.push_str(name);
            tree.nodes.push(Node {
                parent,
                name_at,
                name_len: len as u16,
                sequence,
                attributes,
                modified,
            });
        }
        Some(tree)
    }
}

/// Children by record, as offsets into one list.
struct Children {
    offsets: Vec<u32>,
    members: Vec<u32>,
}

impl Children {
    fn of(&self, record: usize) -> &[u32] {
        match (self.offsets.get(record), self.offsets.get(record + 1)) {
            (Some(&start), Some(&end)) => &self.members[start as usize..end as usize],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use spotlight_platform_win::ntfs::USN_REASON_FILE_CREATE;

    use super::*;

    const ROOT_FRN: u64 = 0x0005_0000_0000_0005;

    fn record(frn: u64, parent: u64, name: &str, dir: bool, reason: u32) -> Record {
        Record {
            frn,
            parent,
            usn: 0,
            reason,
            attributes: if dir { FILE_ATTRIBUTE_DIRECTORY } else { 0x20 },
            time: 0,
            name: name.into(),
        }
    }

    /// `C:\Users\me\notes.txt`, `C:\Users\me\Docs\a.pdf`, `C:\Windows`.
    fn tree() -> Tree {
        let mut tree = Tree::new(ROOT_FRN, PathBuf::from(r"C:\"));
        for r in [
            record(0x0001_0000_0000_0020, ROOT_FRN, "Users", true, 0),
            record(0x0001_0000_0000_0021, 0x0001_0000_0000_0020, "me", true, 0),
            record(
                0x0001_0000_0000_0022,
                0x0001_0000_0000_0021,
                "notes.txt",
                false,
                0,
            ),
            record(
                0x0001_0000_0000_0023,
                0x0001_0000_0000_0021,
                "Docs",
                true,
                0,
            ),
            record(
                0x0001_0000_0000_0024,
                0x0001_0000_0000_0023,
                "a.pdf",
                false,
                0,
            ),
            record(0x0001_0000_0000_0025, ROOT_FRN, "Windows", true, 0),
        ] {
            tree.insert(&r);
        }
        tree
    }

    #[test]
    fn paths_from_parent_links() {
        let tree = tree();
        assert_eq!(tree.len(), 6);
        assert_eq!(
            tree.path(0x0001_0000_0000_0024),
            Some(PathBuf::from(r"C:\Users\me\Docs\a.pdf"))
        );
        assert_eq!(tree.path(ROOT_FRN), Some(PathBuf::from(r"C:\")));
        // A reused record with a newer sequence is another file.
        assert_eq!(tree.get(0x0002_0000_0000_0024), None);
    }

    #[test]
    fn journal_records_become_path_changes() {
        let mut tree = tree();
        let me = 0x0001_0000_0000_0021;
        let created = record(
            0x0001_0000_0000_0030,
            me,
            "new.md",
            false,
            USN_REASON_FILE_CREATE,
        );
        assert_eq!(
            tree.apply(&created),
            [Change::Added(r"C:\Users\me\new.md".into())]
        );
        // Its data written and closed: modified.
        let written = Record {
            reason: USN_REASON_DATA_EXTEND | USN_REASON_CLOSE,
            time: 1_700_000_000,
            ..created.clone()
        };
        assert_eq!(
            tree.apply(&written),
            [Change::Modified(r"C:\Users\me\new.md".into())]
        );
        assert_eq!(tree.get(created.frn).unwrap().modified, 1_700_000_000);

        // Renamed into Docs: the old name's record waits for the new one.
        let old_name = Record {
            reason: USN_REASON_RENAME_OLD_NAME,
            ..created.clone()
        };
        assert!(tree.apply(&old_name).is_empty());
        let new_name = record(
            created.frn,
            0x0001_0000_0000_0023,
            "moved.md",
            false,
            USN_REASON_RENAME_NEW_NAME,
        );
        assert_eq!(
            tree.apply(&new_name),
            [Change::Renamed {
                from: r"C:\Users\me\new.md".into(),
                to: r"C:\Users\me\Docs\moved.md".into()
            }]
        );

        let deleted = Record {
            reason: USN_REASON_FILE_DELETE | USN_REASON_CLOSE,
            ..new_name
        };
        assert_eq!(
            tree.apply(&deleted),
            [Change::Removed(r"C:\Users\me\Docs\moved.md".into())]
        );
        assert!(tree.apply(&deleted).is_empty());
    }

    #[test]
    fn subtree_lists_parents_first_and_skips_refused_folders() {
        let mut tree = tree();
        let me = 0x0001_0000_0000_0021;
        let mut asked = Vec::new();
        let entries = tree.subtree(
            me,
            |path| {
                asked.push(path.to_path_buf());
                true
            },
            |frn| Some(frn as i64),
        );
        let names: Vec<&str> = entries
            .iter()
            .map(|entry| entry.listed.name.as_str())
            .collect();
        assert_eq!(names.len(), 3);
        for entry in &entries {
            if entry.parent != ROOT {
                assert!(entry.parent < entries.len() as u32);
                assert_eq!(entries[entry.parent as usize].listed.name, "Docs");
            }
        }
        assert_eq!(asked, [PathBuf::from(r"C:\Users\me\Docs")]);
        // Times read are kept.
        assert_eq!(
            tree.get(0x0001_0000_0000_0024).unwrap().modified,
            0x0001_0000_0000_0024
        );

        let refused = tree.subtree(me, |_| false, |_| None);
        assert_eq!(refused.len(), 2);
    }

    #[test]
    fn bytes_round_trip() {
        let mut tree = tree();
        tree.set_modified(0x0001_0000_0000_0022, 42);
        tree.remove(0x0001_0000_0000_0025);
        let again = Tree::from_bytes(&tree.to_bytes()).unwrap();
        assert_eq!(again.len(), 5);
        assert_eq!(
            again.path(0x0001_0000_0000_0024),
            tree.path(0x0001_0000_0000_0024)
        );
        assert_eq!(again.get(0x0001_0000_0000_0022).unwrap().modified, 42);
        assert!(Tree::from_bytes(&tree.to_bytes()[..10]).is_none());
    }
}
