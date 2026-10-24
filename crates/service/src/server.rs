//! The named pipe the launcher talks to, and what each connection may see.
//!
//! The service reads the volume past every folder's access rules, so it
//! guards what it hands out:
//! - The pipe is for SYSTEM and interactive users, local only.
//! - A caller must be one of the Spotlight executables the installer named.
//! - Every folder in a snapshot, and the folder of every change sent, is
//!   checked as the caller (with its token, impersonated): one the caller
//!   couldn't list itself is left out with everything in it.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    os::windows::io::FromRawHandle,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use spotlight_platform_win::{
    fswalk,
    fswatch::Change,
    index_service::{self, FromService, ToService},
    ntfs,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_IMPERSONATE,
            TOKEN_QUERY,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
            FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
        },
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
                ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
            },
            Threading::{
                GetCurrentThread, OpenProcess, OpenThreadToken, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, SetThreadToken,
            },
        },
    },
    core::{HSTRING, PWSTR},
};

use crate::{
    log,
    store::Store,
    volume::{Source, Volume},
};

/// SYSTEM gets all, interactive users read and write; nobody else gets in.
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)";
/// Entries per snapshot message.
const CHUNK: usize = 50_000;

/// The volumes launchers asked for, loaded on first use and followed after.
pub struct Volumes {
    open: Box<dyn Fn(char) -> io::Result<Box<dyn Source>> + Send + Sync>,
    store: Option<Arc<Store>>,
    loaded: Mutex<HashMap<char, Arc<Volume>>>,
}

impl Volumes {
    pub fn new(
        open: impl Fn(char) -> io::Result<Box<dyn Source>> + Send + Sync + 'static,
        store: Option<Arc<Store>>,
    ) -> Self {
        Self {
            open: Box::new(open),
            store,
            loaded: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, letter: char) -> io::Result<Arc<Volume>> {
        let mut loaded = self.loaded.lock().expect("volumes");
        if let Some(volume) = loaded.get(&letter) {
            return Ok(volume.clone());
        }
        let volume = Arc::new(Volume::load(
            letter,
            (self.open)(letter)?,
            self.store.as_deref(),
        )?);
        volume.follow(self.store.clone());
        loaded.insert(letter, volume.clone());
        Ok(volume)
    }

    /// Saves every volume and stops following them.
    pub fn shut_down(&self) {
        for volume in self.loaded.lock().expect("volumes").values() {
            volume.stop();
            if let Some(store) = &self.store {
                volume.save(store);
            }
        }
    }
}

pub struct Server {
    pipe: String,
    /// Executables allowed to connect, folded for comparison.
    clients: Vec<String>,
    /// Check folders as the caller. Only tests turn it off: their fake
    /// volumes have no real folders to check.
    check_access: bool,
    volumes: Arc<Volumes>,
    stop: AtomicBool,
}

impl Server {
    pub fn new(pipe: &str, clients: &[PathBuf], volumes: Arc<Volumes>) -> Self {
        Self {
            pipe: pipe.to_owned(),
            clients: clients.iter().map(|path| fold(path)).collect(),
            check_access: true,
            volumes,
            stop: AtomicBool::new(false),
        }
    }

    /// Accepts connections until [`Server::stop`], each on a thread.
    pub fn serve(self: &Arc<Self>) {
        while !self.stop.load(Ordering::Relaxed) {
            let pipe = match self.listen() {
                Ok(pipe) => pipe,
                Err(err) => {
                    log!("couldn't open the pipe: {err}");
                    thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
            };
            if self.stop.load(Ordering::Relaxed) {
                unsafe {
                    let _ = CloseHandle(pipe);
                }
                break;
            }
            let server = self.clone();
            let raw = pipe.0 as isize;
            let spawned = thread::Builder::new().name("client".into()).spawn(move || {
                if let Err(err) = server.handle(HANDLE(raw as _)) {
                    log!("a client left: {err}");
                }
            });
            if let Err(err) = spawned {
                log!("couldn't serve a client: {err}");
            }
        }
    }

    /// Stops accepting; connections under way end when their client goes.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the thread waiting in ConnectNamedPipe.
        if let Ok(handle) = unsafe {
            CreateFileW(
                &HSTRING::from(self.pipe.as_str()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        } {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }

    /// A pipe instance with a client connected.
    fn listen(&self) -> io::Result<HANDLE> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(PIPE_SDDL),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(to_io)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(self.pipe.as_str()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                Some(&attributes),
            )
        };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        }
        if pipe.is_invalid() {
            return Err(io::Error::last_os_error());
        }
        // ERROR_PIPE_CONNECTED (a client came before the wait) is a success too.
        if let Err(err) = unsafe { ConnectNamedPipe(pipe, None) }
            && err.code().0 & 0xFFFF != 535
        {
            unsafe {
                let _ = CloseHandle(pipe);
            }
            return Err(to_io(err));
        }
        Ok(pipe)
    }

    fn handle(&self, pipe: HANDLE) -> io::Result<()> {
        // Owns the handle from here: it closes when the connection ends.
        let mut reader = unsafe { File::from_raw_handle(pipe.0) };
        let caller = caller_path(pipe)?;
        if !self.clients.contains(&fold(&caller)) {
            let why = format!(
                "{} isn't the Spotlight the service was installed for; install it again from this one",
                caller.display()
            );
            // Says why before closing; it names nothing but the caller.
            let _ = send(
                &Mutex::new(reader),
                &FromService::Unavailable {
                    root: index_service::ALL_ROOTS,
                    why: why.clone(),
                },
            );
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
        }
        let writer = Arc::new(Mutex::new(reader.try_clone()?));

        match index_service::decode_to(&index_service::read_frame(&mut reader)?)? {
            ToService::Hello { version } if version == index_service::VERSION => {}
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected Hello {}, got {other:?}", index_service::VERSION),
                ));
            }
        }
        // Only now: Windows lets a pipe server take its client's identity
        // once it has read something from it (ERROR_CANT_IMPERSONATE_PIPE).
        let gate = Arc::new(if self.check_access {
            Gate::of_caller(pipe)?
        } else {
            Gate::Open
        });
        let request = index_service::decode_to(&index_service::read_frame(&mut reader)?)?;
        let ToService::Watch { roots } = request else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected Watch"));
        };
        // Only writes from here on (see `ToService::Watch`); the threads
        // forwarding changes hold the pipe open until the client leaves.
        drop(reader);
        for (n, root) in roots.iter().enumerate() {
            self.watch(n as u32, root, &gate, &writer);
        }
        Ok(())
    }

    /// Sends root `n`'s snapshot, then forwards its changes on a thread.
    fn watch(&self, n: u32, root: &Path, gate: &Arc<Gate>, writer: &Arc<Mutex<File>>) {
        let unavailable = |why: String| {
            log!("{} not served: {why}", root.display());
            let _ = send(writer, &FromService::Unavailable { root: n, why });
        };
        let Some(letter) = ntfs::drive_letter(root) else {
            return unavailable("not on a drive".into());
        };
        if !gate.can_list(root) {
            return unavailable("the caller can't open it".into());
        }
        let volume = match self.volumes.get(letter) {
            Ok(volume) => volume,
            Err(err) => return unavailable(format!("drive {letter}: {err}")),
        };
        // Before the snapshot, so nothing between the two is missed.
        let changes = volume.subscribe();
        let entries = match volume.snapshot(root, |dir| gate.can_list(dir)) {
            Ok(entries) => entries,
            Err(err) => return unavailable(err.to_string()),
        };
        log!("{}: {} entries sent", root.display(), entries.len());
        for chunk in entries.chunks(CHUNK) {
            let message = FromService::Entries {
                root: n,
                entries: chunk.to_vec(),
            };
            if send(writer, &message).is_err() {
                return;
            }
        }
        if send(writer, &FromService::Ready { root: n }).is_err() {
            return;
        }

        let (gate, writer, root) = (gate.clone(), writer.clone(), root.to_path_buf());
        let spawned = thread::Builder::new()
            .name("client-changes".into())
            .spawn(move || {
                let prefix = format!("{}\\", fold(&root).trim_end_matches('\\'));
                let inside = |path: &Path| format!("{}\\", fold(path)).starts_with(&prefix);
                // Folders checked as the caller, by path.
                let mut allowed: HashMap<PathBuf, bool> = HashMap::new();
                let mut visible = |path: &Path| {
                    let Some(dir) = path.parent() else {
                        return false;
                    };
                    if allowed.len() > 10_000 {
                        allowed.clear();
                    }
                    *allowed
                        .entry(dir.to_path_buf())
                        .or_insert_with(|| gate.can_list(dir))
                };
                while let Ok(batch) = changes.recv() {
                    let mut shown = Vec::new();
                    for change in batch.iter() {
                        let keep = match change {
                            Change::Added(path)
                            | Change::Removed(path)
                            | Change::Modified(path) => inside(path) && visible(path),
                            Change::Renamed { from, to } => {
                                (inside(from) && visible(from)) || (inside(to) && visible(to))
                            }
                            Change::Overflow | Change::Stopped(_) => true,
                        };
                        if keep {
                            shown.push(change.clone());
                        }
                    }
                    if !shown.is_empty()
                        && send(
                            &writer,
                            &FromService::Changes {
                                root: n,
                                changes: shown,
                            },
                        )
                        .is_err()
                    {
                        return;
                    }
                }
            });
        if let Err(err) = spawned {
            log!("couldn't forward changes: {err}");
        }
    }
}

fn send(writer: &Mutex<File>, message: &FromService) -> io::Result<()> {
    let mut writer = writer.lock().expect("pipe writer");
    index_service::write_frame(&mut *writer, &index_service::encode_from(message))?;
    writer.flush()
}

/// Decides, as the caller, which folders it may see.
enum Gate {
    /// The caller's impersonation token.
    Token(isize),
    /// Everything: for tests.
    Open,
}

impl Gate {
    /// Takes the caller's token by impersonating it for a moment.
    fn of_caller(pipe: HANDLE) -> io::Result<Self> {
        unsafe {
            ImpersonateNamedPipeClient(pipe).map_err(to_io)?;
            let mut token = HANDLE::default();
            let opened = OpenThreadToken(
                GetCurrentThread(),
                TOKEN_QUERY | TOKEN_IMPERSONATE,
                true,
                &mut token,
            );
            let _ = RevertToSelf();
            opened.map_err(to_io)?;
            Ok(Gate::Token(token.0 as isize))
        }
    }

    fn can_list(&self, dir: &Path) -> bool {
        let Gate::Token(token) = self else {
            return true;
        };
        unsafe {
            if SetThreadToken(None, Some(HANDLE(*token as _))).is_err() {
                return false;
            }
            let opened = CreateFileW(
                &HSTRING::from(fswalk::long_path(dir)),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            );
            let _ = RevertToSelf();
            match opened {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    true
                }
                Err(_) => false,
            }
        }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        if let Gate::Token(token) = self {
            unsafe {
                let _ = CloseHandle(HANDLE(*token as _));
            }
        }
    }
}

// The token handle is only used through the calls above, from any thread.
unsafe impl Send for Gate {}
unsafe impl Sync for Gate {}

/// The executable of the process at the other end of `pipe`.
fn caller_path(pipe: HANDLE) -> io::Result<PathBuf> {
    unsafe {
        let mut pid = 0u32;
        GetNamedPipeClientProcessId(pipe, &mut pid).map_err(to_io)?;
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(to_io)?;
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        named.map_err(to_io)?;
        Ok(PathBuf::from(String::from_utf16_lossy(
            &buffer[..len as usize],
        )))
    }
}

fn fold(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

fn to_io(err: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(err.code().0 & 0xFFFF)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use spotlight_platform_win::{
        index_service::{Client, ROOT},
        ntfs::{USN_REASON_CLOSE, USN_REASON_FILE_CREATE},
    };

    use super::*;
    use crate::volume::fake::{self, FakeVolume};

    fn server(pipe: &str, clients: Vec<PathBuf>) -> (Arc<Server>, Arc<Mutex<fake::Disk>>) {
        serve(pipe, clients, false)
    }

    fn serve(
        pipe: &str,
        clients: Vec<PathBuf>,
        check_access: bool,
    ) -> (Arc<Server>, Arc<Mutex<fake::Disk>>) {
        let disk = fake::disk();
        let source = disk.clone();
        let volumes = Arc::new(Volumes::new(
            move |_| Ok(Box::new(FakeVolume(source.clone())) as Box<dyn Source>),
            None,
        ));
        let mut server = Server::new(pipe, &clients, volumes);
        server.check_access = check_access;
        let server = Arc::new(server);
        let serving = server.clone();
        thread::spawn(move || serving.serve());
        (server, disk)
    }

    /// Connects once the server's thread has created the pipe.
    fn connect(pipe: &str) -> Client {
        for _ in 0..100 {
            if let Some(client) = Client::connect_to(pipe, Duration::from_millis(20)) {
                return client;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("couldn't connect to {pipe}");
    }

    #[test]
    fn serves_a_snapshot_then_changes() {
        let pipe = format!(r"\\.\pipe\spotlight-index-test-{}", std::process::id());
        let (server, disk) = server(&pipe, vec![std::env::current_exe().unwrap()]);
        let mut client = connect(&pipe);
        client
            .send(&ToService::Watch {
                roots: vec![
                    PathBuf::from(r"C:\Users\me"),
                    PathBuf::from(r"\\server\share"),
                ],
            })
            .unwrap();
        let FromService::Entries { root: 0, entries } = client.recv().unwrap() else {
            panic!("no entries");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].parent, ROOT);
        assert_eq!(entries[0].listed.name, "notes.txt");
        assert_eq!(client.recv().unwrap(), FromService::Ready { root: 0 });
        // A root the service can't serve.
        assert!(matches!(
            client.recv().unwrap(),
            FromService::Unavailable { root: 1, .. }
        ));

        {
            let mut disk = disk.lock().unwrap();
            disk.changes.push(fake::record(
                0x0001_0000_0000_0030,
                0x0001_0000_0000_0021,
                "plan.md",
                false,
                USN_REASON_FILE_CREATE | USN_REASON_CLOSE,
                10,
            ));
            // Outside the root: not sent.
            disk.changes.push(fake::record(
                0x0001_0000_0000_0031,
                0x0001_0000_0000_0020,
                "other.txt",
                false,
                USN_REASON_FILE_CREATE,
                11,
            ));
            disk.journal.next = 20;
        }
        assert_eq!(
            client.recv().unwrap(),
            FromService::Changes {
                root: 0,
                changes: vec![Change::Added(r"C:\Users\me\plan.md".into())]
            }
        );
        server.stop();
    }

    /// With the caller's rights checked, as in the installed service: the
    /// service takes the caller's identity (after reading from it, or Windows
    /// refuses) and checks each root as the caller.
    #[test]
    fn checks_roots_as_the_caller() {
        let pipe = format!(r"\\.\pipe\spotlight-index-test-gate-{}", std::process::id());
        let (server, _) = serve(&pipe, vec![std::env::current_exe().unwrap()], true);
        let mut client = connect(&pipe);
        let readable = std::env::temp_dir();
        client
            .send(&ToService::Watch {
                roots: vec![PathBuf::from(r"C:\no such folder\zq"), readable],
            })
            .unwrap();
        // Can't be opened as the caller: never reaches the drive.
        let FromService::Unavailable { root: 0, why } = client.recv().unwrap() else {
            panic!("root 0 answered otherwise");
        };
        assert_eq!(why, "the caller can't open it");
        // Opened as the caller; the fake drive just doesn't have it.
        let FromService::Unavailable { root: 1, why } = client.recv().unwrap() else {
            panic!("root 1 answered otherwise");
        };
        assert_eq!(why, "no such folder");
        server.stop();
    }

    #[test]
    fn refuses_other_programs() {
        let pipe = format!(
            r"\\.\pipe\spotlight-index-test-refuse-{}",
            std::process::id()
        );
        let (server, _) = server(
            &pipe,
            vec![PathBuf::from(r"C:\Program Files\Spotlight\Spotlight.exe")],
        );
        let mut client = connect(&pipe);
        let _ = client.send(&ToService::Watch {
            roots: vec![PathBuf::from(r"C:\Users\me")],
        });
        // Told why, then nothing more.
        assert!(matches!(
            client.recv().unwrap(),
            FromService::Unavailable {
                root: index_service::ALL_ROOTS,
                ..
            }
        ));
        assert!(client.recv().is_err());
        server.stop();
    }
}
