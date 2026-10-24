//! Talking to `spotlight-service.exe`, the optional file index service.
//!
//! The service reads NTFS volumes' file tables and change journals, which
//! takes administrator rights, and hands the launcher what is under its scope
//! roots over a named pipe: first a snapshot of each root (entries, parents
//! first), then the changes as they happen, as paths. The launcher stats
//! changed paths itself, as the user, so the service never has to say more
//! about a file than its name.
//!
//! Messages are length-prefixed frames of little-endian fields; strings are
//! a `u32` byte count and UTF-8.

use std::{
    fs::File,
    io::{self, Read, Write},
    os::windows::io::FromRawHandle,
    path::{Path, PathBuf},
    time::Duration,
};

use spotlight_files::Listed;
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE},
        Storage::FileSystem::{CreateFileW, FILE_SHARE_NONE, OPEN_EXISTING},
        System::{
            Pipes::WaitNamedPipeW,
            Services::{
                CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
                SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
            },
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        },
        UI::{
            Shell::{
                SEE_MASK_NO_CONSOLE, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{HSTRING, PCWSTR},
};

use crate::fswatch::Change;

/// The pipe the service listens on; one per machine.
pub const PIPE: &str = r"\\.\pipe\spotlight-index";
/// The service's name in the Service Control Manager.
pub const SERVICE: &str = "SpotlightIndex";
/// Bumped when the messages change; the service refuses other versions.
pub const VERSION: u32 = 1;
/// A frame larger than this is a broken stream, not a message.
const MAX_FRAME: usize = 64 << 20;

/// One entry of a root's snapshot. Entries come parents first.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The index of the parent among this root's entries; [`ROOT`] for an
    /// entry directly in the root.
    pub parent: u32,
    pub listed: Listed,
}

/// The parent of an entry directly in a root.
pub const ROOT: u32 = u32::MAX;

#[derive(Clone, Debug, PartialEq)]
pub enum ToService {
    /// Sent first.
    Hello { version: u32 },
    /// Asks for these roots (numbered from 0 in this order): each one's
    /// snapshot, then their changes. Sent once, after `Hello`: from then on
    /// only the service writes, since a pipe opened for plain blocking I/O
    /// can't read and write at the same time.
    Watch { roots: Vec<PathBuf> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum FromService {
    /// Part of a root's snapshot.
    Entries {
        root: u32,
        entries: Vec<Entry>,
    },
    /// The root's snapshot is complete; changes follow.
    Ready {
        root: u32,
    },
    Changes {
        root: u32,
        changes: Vec<Change>,
    },
    /// The service can't serve this root (not NTFS, no access); the
    /// launcher walks it itself.
    Unavailable {
        root: u32,
        why: String,
    },
    /// The service lost track (the journal was reset): ask again.
    Resync {
        root: u32,
    },
}

/// Whether the service is installed, and running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    Stopped,
    Running,
}

/// Asks the Service Control Manager; needs no rights.
pub fn state() -> State {
    unsafe {
        let Ok(manager) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return State::NotInstalled;
        };
        let state = match OpenServiceW(manager, &HSTRING::from(SERVICE), SERVICE_QUERY_STATUS) {
            Ok(service) => {
                let mut status = SERVICE_STATUS::default();
                let running = QueryServiceStatus(service, &mut status).is_ok()
                    && status.dwCurrentState == SERVICE_RUNNING;
                let _ = CloseServiceHandle(service);
                if running {
                    State::Running
                } else {
                    State::Stopped
                }
            }
            Err(_) => State::NotInstalled,
        };
        let _ = CloseServiceHandle(manager);
        state
    }
}

/// `spotlight-service.exe` next to this executable, as a build puts it.
pub fn service_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let service = exe.parent()?.join("spotlight-service.exe");
    service.exists().then_some(service)
}

/// Installs and starts the service, allowing this executable to connect.
/// Shows the UAC prompt and waits for the elevated installer.
pub fn install() -> Result<(), String> {
    let client = std::env::current_exe().map_err(|err| err.to_string())?;
    run_elevated(&format!("install --client \"{}\"", client.display()))
}

/// Stops and removes the service and its data, after the UAC prompt.
pub fn uninstall() -> Result<(), String> {
    run_elevated("uninstall")
}

fn run_elevated(arguments: &str) -> Result<(), String> {
    let service = service_exe().ok_or("spotlight-service.exe isn't next to Spotlight")?;
    let file = HSTRING::from(service.as_os_str());
    let parameters = HSTRING::from(arguments);
    let verb = HSTRING::from("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        // Declining the prompt fails here (ERROR_CANCELLED).
        ShellExecuteExW(&mut info)
            .map_err(|_| "Not allowed: the administrator prompt was declined".to_owned())?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if code == 0 {
            Ok(())
        } else {
            Err(format!(
                "The service's installer failed (exit code {code}); see {}",
                PathBuf::from(std::env::var_os("ProgramData").unwrap_or_default())
                    .join(r"Spotlight\service.log")
                    .display()
            ))
        }
    }
}

/// A connection to the service.
pub struct Client {
    pipe: File,
}

impl Client {
    /// Connects, waiting up to `wait` for a free pipe instance. `None` when
    /// the service isn't running.
    pub fn connect(wait: Duration) -> Option<Self> {
        Self::connect_to(PIPE, wait)
    }

    /// [`Client::connect`] to another pipe name, for tests.
    pub fn connect_to(pipe: &str, wait: Duration) -> Option<Self> {
        let name = HSTRING::from(pipe);
        unsafe {
            let _ = WaitNamedPipeW(&name, wait.as_millis() as u32);
        }
        let handle = unsafe {
            CreateFileW(
                &name,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        }
        .ok()?;
        let mut client = Self {
            pipe: unsafe { File::from_raw_handle(handle.0) },
        };
        client.send(&ToService::Hello { version: VERSION }).ok()?;
        Some(client)
    }

    pub fn send(&mut self, message: &ToService) -> io::Result<()> {
        write_frame(&mut self.pipe, &encode_to(message))
    }

    /// The next message; blocks.
    pub fn recv(&mut self) -> io::Result<FromService> {
        decode_from(&read_frame(&mut self.pipe)?)
    }
}

pub fn write_frame(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    out.write_all(&frame)?;
    out.flush()
}

pub fn read_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn encode_to(message: &ToService) -> Vec<u8> {
    let mut out = Writer::default();
    match message {
        ToService::Hello { version } => {
            out.u8(1);
            out.u32(*version);
        }
        ToService::Watch { roots } => {
            out.u8(2);
            out.u32(roots.len() as u32);
            for root in roots {
                out.path(root);
            }
        }
    }
    out.0
}

pub fn decode_to(bytes: &[u8]) -> io::Result<ToService> {
    let mut input = Reader(bytes);
    let message = match input.u8()? {
        1 => ToService::Hello {
            version: input.u32()?,
        },
        2 => {
            let count = input.u32()? as usize;
            let mut roots = Vec::with_capacity(count.min(256));
            for _ in 0..count {
                roots.push(input.path()?);
            }
            ToService::Watch { roots }
        }
        tag => return Err(bad(&format!("unknown request {tag}"))),
    };
    input.end()?;
    Ok(message)
}

pub fn encode_from(message: &FromService) -> Vec<u8> {
    let mut out = Writer::default();
    match message {
        FromService::Entries { root, entries } => {
            out.u8(1);
            out.u32(*root);
            out.u32(entries.len() as u32);
            for entry in entries {
                out.u32(entry.parent);
                let listed = &entry.listed;
                out.string(&listed.name);
                out.u8(listed.dir as u8
                    | (listed.hidden as u8) << 1
                    | (listed.system as u8) << 2
                    | (listed.link as u8) << 3);
                out.i64(listed.modified);
            }
        }
        FromService::Ready { root } => {
            out.u8(2);
            out.u32(*root);
        }
        FromService::Changes { root, changes } => {
            out.u8(3);
            out.u32(*root);
            out.u32(changes.len() as u32);
            for change in changes {
                match change {
                    Change::Added(path) => {
                        out.u8(1);
                        out.path(path);
                    }
                    Change::Removed(path) => {
                        out.u8(2);
                        out.path(path);
                    }
                    Change::Modified(path) => {
                        out.u8(3);
                        out.path(path);
                    }
                    Change::Renamed { from, to } => {
                        out.u8(4);
                        out.path(from);
                        out.path(to);
                    }
                    Change::Overflow => out.u8(5),
                    Change::Stopped(why) => {
                        out.u8(6);
                        out.string(why);
                    }
                }
            }
        }
        FromService::Unavailable { root, why } => {
            out.u8(4);
            out.u32(*root);
            out.string(why);
        }
        FromService::Resync { root } => {
            out.u8(5);
            out.u32(*root);
        }
    }
    out.0
}

pub fn decode_from(bytes: &[u8]) -> io::Result<FromService> {
    let mut input = Reader(bytes);
    let message = match input.u8()? {
        1 => {
            let root = input.u32()?;
            let count = input.u32()? as usize;
            let mut entries = Vec::with_capacity(count.min(1 << 20));
            for _ in 0..count {
                let parent = input.u32()?;
                let name = input.string()?;
                let flags = input.u8()?;
                let modified = input.i64()?;
                entries.push(Entry {
                    parent,
                    listed: Listed {
                        name,
                        dir: flags & 1 != 0,
                        hidden: flags & 2 != 0,
                        system: flags & 4 != 0,
                        link: flags & 8 != 0,
                        modified,
                    },
                });
            }
            FromService::Entries { root, entries }
        }
        2 => FromService::Ready { root: input.u32()? },
        3 => {
            let root = input.u32()?;
            let count = input.u32()? as usize;
            let mut changes = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                changes.push(match input.u8()? {
                    1 => Change::Added(input.path()?),
                    2 => Change::Removed(input.path()?),
                    3 => Change::Modified(input.path()?),
                    4 => Change::Renamed {
                        from: input.path()?,
                        to: input.path()?,
                    },
                    5 => Change::Overflow,
                    6 => Change::Stopped(input.string()?),
                    tag => return Err(bad(&format!("unknown change {tag}"))),
                });
            }
            FromService::Changes { root, changes }
        }
        4 => FromService::Unavailable {
            root: input.u32()?,
            why: input.string()?,
        },
        5 => FromService::Resync { root: input.u32()? },
        tag => return Err(bad(&format!("unknown message {tag}"))),
    };
    input.end()?;
    Ok(message)
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, text: &str) {
        self.u32(text.len() as u32);
        self.0.extend_from_slice(text.as_bytes());
    }

    fn path(&mut self, path: &Path) {
        self.string(&path.to_string_lossy());
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(bad("message cut short"));
        }
        let (taken, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(taken)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> io::Result<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| bad("text isn't UTF-8"))
    }

    fn path(&mut self) -> io::Result<PathBuf> {
        Ok(PathBuf::from(self.string()?))
    }

    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(bad("bytes after the message"))
        }
    }
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let to = [
            ToService::Hello { version: VERSION },
            ToService::Watch {
                roots: vec![PathBuf::from(r"C:\Users\мне"), PathBuf::from(r"D:\")],
            },
        ];
        for message in to {
            assert_eq!(decode_to(&encode_to(&message)).unwrap(), message);
        }
        let from = [
            FromService::Entries {
                root: 1,
                entries: vec![
                    Entry {
                        parent: ROOT,
                        listed: Listed {
                            name: "Документы".into(),
                            dir: true,
                            hidden: false,
                            system: true,
                            link: false,
                            modified: 1_700_000_000,
                        },
                    },
                    Entry {
                        parent: 0,
                        listed: Listed {
                            name: "a.txt".into(),
                            link: true,
                            ..Listed::default()
                        },
                    },
                ],
            },
            FromService::Ready { root: 0 },
            FromService::Changes {
                root: 2,
                changes: vec![
                    Change::Added(r"C:\a".into()),
                    Change::Removed(r"C:\b".into()),
                    Change::Modified(r"C:\c".into()),
                    Change::Renamed {
                        from: r"C:\d".into(),
                        to: r"C:\e".into(),
                    },
                    Change::Overflow,
                    Change::Stopped("gone".into()),
                ],
            },
            FromService::Unavailable {
                root: 3,
                why: "not NTFS".into(),
            },
            FromService::Resync { root: 4 },
        ];
        for message in from {
            assert_eq!(decode_from(&encode_from(&message)).unwrap(), message);
        }
    }

    #[test]
    fn refuses_broken_messages() {
        assert!(decode_from(&[]).is_err());
        assert!(decode_from(&[9]).is_err());
        let mut bytes = encode_from(&FromService::Ready { root: 1 });
        bytes.push(0);
        assert!(decode_from(&bytes).is_err());
        assert!(decode_to(&encode_to(&ToService::Hello { version: 1 })[..3]).is_err());
    }

    #[test]
    fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"abc").unwrap();
        write_frame(&mut buffer, b"").unwrap();
        let mut input = &buffer[..];
        assert_eq!(read_frame(&mut input).unwrap(), b"abc");
        assert_eq!(read_frame(&mut input).unwrap(), b"");
        assert!(read_frame(&mut input).is_err());
    }

    #[test]
    fn no_service_means_no_client() {
        if state() == State::NotInstalled {
            assert!(Client::connect(Duration::from_millis(10)).is_none());
        }
    }
}
