//! The service's snapshots on disk: one file per volume, so a restart picks
//! up where the journal was instead of reading the whole MFT again.
//!
//! Snapshots name every file on a volume, so they are sealed (AES-256-GCM,
//! as clipboard images are) with a random key kept beside them protected by
//! DPAPI for the service's account, LocalSystem: copying the folder gives
//! nothing. The folder itself is for SYSTEM and Administrators only.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use spotlight_platform_win::vault::{self, Key};

use crate::tree::Tree;

const KEY_FILE: &str = "key.bin";

/// Where a volume's journal was when its snapshot was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub journal: u64,
    pub next: i64,
}

pub struct Store {
    dir: PathBuf,
    key: Key,
}

impl Store {
    /// Opens (or creates) the store in `dir`, with its key.
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let key_path = dir.join(KEY_FILE);
        let key = match fs::read(&key_path).and_then(|bytes| vault::unprotect_key(&bytes)) {
            Ok(key) => key,
            Err(_) => {
                // No key, or one another account protected: the snapshots
                // it sealed can't be read, so they go with it.
                for entry in fs::read_dir(dir)?.flatten() {
                    if entry.path().extension().is_some_and(|ext| ext == "snap") {
                        let _ = fs::remove_file(entry.path());
                    }
                }
                let key = vault::random_key()?;
                fs::write(&key_path, vault::protect_key(&key)?)?;
                key
            }
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
        })
    }

    pub fn save(&self, letter: char, position: Position, tree: &Tree) -> io::Result<()> {
        let mut plain = Vec::new();
        plain.extend_from_slice(&position.journal.to_le_bytes());
        plain.extend_from_slice(&position.next.to_le_bytes());
        plain.extend_from_slice(&tree.to_bytes());
        let sealed = vault::seal_with(&self.key, &plain)?;
        // Through a temporary file, so a crash keeps the previous snapshot.
        let path = self.path(letter);
        let partial = path.with_extension("partial");
        fs::write(&partial, sealed)?;
        fs::rename(&partial, &path)
    }

    /// The snapshot of drive `letter`, if there is one that opens. One that
    /// doesn't (damaged, or from another key) is deleted.
    pub fn load(&self, letter: char) -> Option<(Position, Tree)> {
        let path = self.path(letter);
        let sealed = fs::read(&path).ok()?;
        let loaded = vault::unseal_with(&self.key, &sealed)
            .ok()
            .and_then(|plain| {
                let journal = u64::from_le_bytes(plain.get(..8)?.try_into().ok()?);
                let next = i64::from_le_bytes(plain.get(8..16)?.try_into().ok()?);
                Some((Position { journal, next }, Tree::from_bytes(&plain[16..])?))
            });
        if loaded.is_none() {
            let _ = fs::remove_file(&path);
        }
        loaded
    }

    fn path(&self, letter: char) -> PathBuf {
        self.dir
            .join(format!("{}.snap", letter.to_ascii_uppercase()))
    }
}

#[cfg(test)]
mod tests {
    use spotlight_platform_win::ntfs::Record;

    use super::*;

    #[test]
    fn snapshots_round_trip_sealed() {
        let dir = std::env::temp_dir().join(format!("spotlight-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        let mut tree = Tree::new(5, PathBuf::from(r"D:\"));
        tree.insert(&Record {
            frn: 0x0001_0000_0000_0010,
            parent: 5,
            usn: 0,
            reason: 0,
            attributes: 0x20,
            time: 7,
            name: "секрет.txt".into(),
        });
        let position = Position {
            journal: 9,
            next: 1234,
        };
        store.save('d', position, &tree).unwrap();
        let raw = fs::read(dir.join("D.snap")).unwrap();
        assert!(
            !raw.windows(6)
                .any(|w| w == "секрет".as_bytes()[..6].as_ref())
        );

        // Opened again (the key comes from disk).
        let store = Store::open(&dir).unwrap();
        let (loaded, again) = store.load('D').unwrap();
        assert_eq!(loaded, position);
        assert_eq!(
            again.path(0x0001_0000_0000_0010),
            Some(PathBuf::from(r"D:\секрет.txt"))
        );

        // A damaged snapshot is dropped.
        fs::write(dir.join("D.snap"), b"SPV1 broken").unwrap();
        assert!(store.load('D').is_none());
        assert!(!dir.join("D.snap").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
