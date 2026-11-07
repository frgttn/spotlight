//! macOS counterpart of `loupe_platform_win::vault`, with its signatures.
//! A stub for now: there is no key in the Keychain yet, so files are never
//! unlocked and [`write_file`] and [`read_file`] work with plain files, as on
//! Windows before [`unlock_files`].

use std::{fs, io, path::Path};

use crate::UNSUPPORTED;

pub type Key = [u8; 32];

/// The database key, created (and saved) on first use. `true` with a new
/// key: whatever was encrypted before can't be read any more.
pub fn database_key() -> crate::Result<(Key, bool)> {
    Err(UNSUPPORTED.into())
}

/// From now on [`write_file`] seals files and [`read_file`] opens them.
/// The files key is derived from the database key. Only the first call counts.
pub fn unlock_files(_database_key: &Key) {}

/// Writes `bytes` to `path`, sealed once [`unlock_files`] ran.
pub fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)
}

/// Reads a file [`write_file`] wrote, sealed or not.
pub fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

/// Seals the files in `dir` that are still plain (written before
/// encryption), in place. Returns how many. Needs [`unlock_files`].
pub fn seal_folder(_dir: &Path) -> usize {
    0
}

/// Seals `plain` with `key`, as files are sealed: for data with a key of its
/// own (the index service's snapshots).
pub fn seal_with(_key: &Key, _plain: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::other(UNSUPPORTED))
}

/// Opens what [`seal_with`] sealed; fails on a wrong key or changed data.
pub fn unseal_with(_key: &Key, _sealed: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::other(UNSUPPORTED))
}

/// A new random key.
pub fn random_key() -> io::Result<Key> {
    Err(io::Error::other(UNSUPPORTED))
}

/// `key` protected with DPAPI for the current Windows account: only a
/// process running as that account on this machine can unprotect it. For
/// the service, that account is LocalSystem.
pub fn protect_key(_key: &Key) -> io::Result<Vec<u8>> {
    Err(io::Error::other(UNSUPPORTED))
}

pub fn unprotect_key(_protected: &[u8]) -> io::Result<Key> {
    Err(io::Error::other(UNSUPPORTED))
}
