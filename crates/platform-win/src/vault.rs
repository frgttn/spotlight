//! Encryption at rest: the database key, kept in Windows Credential Manager
//! (as Raycast keeps its `main.db` key), and sealed files for what lives next
//! to the database (clipboard images).
//!
//! The key is random and never typed: Credential Manager ties it to the
//! Windows account, so copying the data folder elsewhere doesn't copy it.
//! Crypto is CNG (bcrypt.dll): AES-256-GCM for files.

use std::{fs, io, path::Path, sync::OnceLock};

use windows::{
    Win32::{
        Foundation::{ERROR_NOT_FOUND, NTSTATUS},
        Security::{
            Credentials::{
                CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
                CredWriteW,
            },
            Cryptography::{
                BCRYPT_AES_GCM_ALG_HANDLE, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO, BCRYPT_FLAGS,
                BCRYPT_HMAC_SHA256_ALG_HANDLE, BCRYPT_KEY_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                BCryptDecrypt, BCryptDestroyKey, BCryptEncrypt, BCryptGenRandom,
                BCryptGenerateSymmetricKey, BCryptHash,
            },
        },
    },
    core::{HSTRING, PWSTR},
};

pub type Key = [u8; 32];

/// The database key's entry in Credential Manager (Generic Credentials).
const CREDENTIAL: &str = "Spotlight/database";
/// Starts every sealed file; files without it are from before encryption.
const MAGIC: &[u8; 4] = b"SPV1";
const NONCE: usize = 12;
const TAG: usize = 16;
/// What the file key is derived for, so it is never the database key itself.
const FILES_CONTEXT: &[u8] = b"spotlight/files";
/// `BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION`, missing from the bindings.
const AUTH_MODE_INFO_VERSION: u32 = 1;

/// Set by [`unlock_files`]; until then files are read and written as they are.
static FILES_KEY: OnceLock<Key> = OnceLock::new();

/// The database key, created (and saved) on first use. `true` with a new
/// key: whatever was encrypted before can't be read any more.
pub fn database_key() -> windows::core::Result<(Key, bool)> {
    let target = HSTRING::from(CREDENTIAL);
    let mut found: *mut CREDENTIALW = std::ptr::null_mut();
    match unsafe { CredReadW(&target, CRED_TYPE_GENERIC, None, &mut found) } {
        Ok(()) => {
            let key = unsafe {
                let credential = &*found;
                let blob = std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                let key = Key::try_from(blob);
                CredFree(found as *const _);
                key
            };
            // Someone else's entry under this name, or a damaged one: a
            // key Spotlight didn't write can't open anything it wrote.
            key.map(|key| (key, false))
                .or_else(|_| Ok((save_new_key(&target)?, true)))
        }
        Err(err) if err.code() == ERROR_NOT_FOUND.to_hresult() => {
            Ok((save_new_key(&target)?, true))
        }
        Err(err) => Err(err),
    }
}

fn save_new_key(target: &HSTRING) -> windows::core::Result<Key> {
    let mut key = Key::default();
    random(&mut key)?;
    let user = HSTRING::from("Spotlight");
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_ptr() as *mut u16),
        UserName: PWSTR(user.as_ptr() as *mut u16),
        CredentialBlobSize: key.len() as u32,
        CredentialBlob: key.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    unsafe { CredWriteW(&credential, 0)? };
    Ok(key)
}

/// From now on [`write_file`] seals files and [`read_file`] opens them.
/// The files key is derived from the database key. Only the first call counts.
pub fn unlock_files(database_key: &Key) {
    let mut key = Key::default();
    let status = unsafe {
        BCryptHash(
            BCRYPT_HMAC_SHA256_ALG_HANDLE,
            Some(database_key),
            FILES_CONTEXT,
            &mut key,
        )
    };
    if check(status).is_ok() {
        let _ = FILES_KEY.set(key);
    }
}

/// Writes `bytes` to `path`, sealed once [`unlock_files`] ran.
pub fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match FILES_KEY.get() {
        Some(key) => fs::write(path, seal(key, bytes)?),
        None => fs::write(path, bytes),
    }
}

/// Reads a file [`write_file`] wrote, sealed or not.
pub fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    if !bytes.starts_with(MAGIC) {
        return Ok(bytes);
    }
    let key = FILES_KEY
        .get()
        .ok_or_else(|| io::Error::other("the file is encrypted and no key is unlocked"))?;
    unseal(key, &bytes)
}

/// Seals the files in `dir` that are still plain (written before
/// encryption), in place. Returns how many. Needs [`unlock_files`].
pub fn seal_folder(dir: &Path) -> usize {
    let (Some(key), Ok(entries)) = (FILES_KEY.get(), fs::read_dir(dir)) else {
        return 0;
    };
    let mut sealed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.starts_with(MAGIC) {
            continue;
        }
        // Through a temporary file, so a crash leaves the plain or the
        // sealed copy, never half of one.
        let partial = path.with_extension("sealing");
        let done = seal(key, &bytes)
            .and_then(|data| fs::write(&partial, data))
            .and_then(|()| fs::rename(&partial, &path));
        match done {
            Ok(()) => sealed += 1,
            Err(err) => {
                eprintln!("[vault] could not encrypt {}: {err}", path.display());
                let _ = fs::remove_file(&partial);
            }
        }
    }
    sealed
}

/// `MAGIC`, a random nonce, the AES-256-GCM ciphertext and its tag.
fn seal(key: &Key, plain: &[u8]) -> io::Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE];
    random(&mut nonce)?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE + plain.len() + TAG);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.resize(MAGIC.len() + NONCE + plain.len(), 0);
    let mut tag = [0u8; TAG];
    let mut info = auth_info(&mut nonce, &mut tag);
    with_key(key, |handle| {
        let mut written = 0;
        check(unsafe {
            BCryptEncrypt(
                handle,
                Some(plain),
                Some(&mut info as *mut _ as *const _),
                None,
                Some(&mut out[MAGIC.len() + NONCE..]),
                &mut written,
                BCRYPT_FLAGS(0),
            )
        })
    })?;
    out.extend_from_slice(&tag);
    Ok(out)
}

fn unseal(key: &Key, data: &[u8]) -> io::Result<Vec<u8>> {
    let header = MAGIC.len() + NONCE;
    if data.len() < header + TAG {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated file"));
    }
    let mut nonce: [u8; NONCE] = data[MAGIC.len()..header].try_into().unwrap();
    let (cipher, tag) = data[header..].split_at(data.len() - header - TAG);
    let mut tag: [u8; TAG] = tag.try_into().unwrap();
    let mut info = auth_info(&mut nonce, &mut tag);
    let mut plain = vec![0u8; cipher.len()];
    with_key(key, |handle| {
        let mut written = 0;
        check(unsafe {
            BCryptDecrypt(
                handle,
                Some(cipher),
                Some(&mut info as *mut _ as *const _),
                None,
                Some(&mut plain),
                &mut written,
                BCRYPT_FLAGS(0),
            )
        })
    })
    // A wrong key or a changed file fails the tag check.
    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(plain)
}

fn auth_info(
    nonce: &mut [u8; NONCE],
    tag: &mut [u8; TAG],
) -> BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
    BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: AUTH_MODE_INFO_VERSION,
        pbNonce: nonce.as_mut_ptr(),
        cbNonce: NONCE as u32,
        pbTag: tag.as_mut_ptr(),
        cbTag: TAG as u32,
        ..Default::default()
    }
}

/// Runs `f` with an AES-GCM key handle for `key`.
fn with_key(key: &Key, f: impl FnOnce(BCRYPT_KEY_HANDLE) -> io::Result<()>) -> io::Result<()> {
    let mut handle = BCRYPT_KEY_HANDLE::default();
    check(unsafe {
        BCryptGenerateSymmetricKey(BCRYPT_AES_GCM_ALG_HANDLE, &mut handle, None, key, 0)
    })?;
    let result = f(handle);
    unsafe {
        let _ = BCryptDestroyKey(handle);
    }
    result
}

fn random(buffer: &mut [u8]) -> io::Result<()> {
    check(unsafe { BCryptGenRandom(None, buffer, BCRYPT_USE_SYSTEM_PREFERRED_RNG) })
}

fn check(status: NTSTATUS) -> io::Result<()> {
    status.ok().map_err(|err| io::Error::other(err.message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealing_round_trips_and_detects_changes() {
        let key = [7u8; 32];
        let plain = b"\x89PNG not really an image";
        let sealed = seal(&key, plain).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert_eq!(sealed.len(), MAGIC.len() + NONCE + plain.len() + TAG);
        assert!(!sealed.windows(plain.len()).any(|w| w == plain));
        assert_eq!(unseal(&key, &sealed).unwrap(), plain);
        // A random nonce each time.
        assert_ne!(seal(&key, plain).unwrap(), sealed);

        assert!(unseal(&[8u8; 32], &sealed).is_err());
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(unseal(&key, &changed).is_err());
        assert!(unseal(&key, &sealed[..10]).is_err());
        assert_eq!(unseal(&key, &seal(&key, b"").unwrap()).unwrap(), b"");
    }
}
