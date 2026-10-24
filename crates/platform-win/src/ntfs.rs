//! An NTFS volume's file table and change journal, for the index service.
//!
//! Enumerating the MFT (`FSCTL_ENUM_USN_DATA`) lists every file and folder
//! on the volume, by file reference number, with its parent's number, name
//! and attributes, in a few seconds for a million files. The USN journal
//! (`FSCTL_READ_USN_JOURNAL`) then tells what changed since a given point,
//! across restarts. Both need administrator rights: they read the volume
//! itself, past every folder's access rules, which is why only the service
//! (running as LocalSystem) calls them.
//!
//! Enumeration gives no modification times; [`Volume::modified`] reads one
//! from the file's MFT record (`$STANDARD_INFORMATION`).

use std::{io, path::Path};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
            GetVolumeInformationW, OPEN_EXISTING,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_FILE_RECORD, FSCTL_QUERY_USN_JOURNAL,
                FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0, NTFS_FILE_RECORD_INPUT_BUFFER,
                READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
            },
        },
    },
    core::HSTRING,
};

use crate::fswalk::unix_seconds;

pub use windows::Win32::System::Ioctl::{
    USN_REASON_BASIC_INFO_CHANGE, USN_REASON_CLOSE, USN_REASON_DATA_EXTEND,
    USN_REASON_DATA_OVERWRITE, USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE,
    USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
};

/// The low 48 bits of a file reference number: the MFT record's index. The
/// high 16 are a sequence number that changes when the record is reused.
pub const RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// One file or folder, from the MFT or the journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub frn: u64,
    pub parent: u64,
    pub usn: i64,
    /// `USN_REASON_*` bits; 0 from an MFT enumeration.
    pub reason: u32,
    /// `FILE_ATTRIBUTE_*` bits.
    pub attributes: u32,
    /// When the journal wrote it, unix seconds; 0 from an enumeration.
    pub time: i64,
    pub name: String,
}

/// Where the journal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    /// Changes when the journal is deleted and created again: every USN
    /// remembered from before is meaningless then.
    pub id: u64,
    /// The oldest USN still in the journal.
    pub first: i64,
    /// The USN the next change will get.
    pub next: i64,
}

/// An open NTFS volume, by its root (`C:\`).
pub struct Volume {
    handle: HANDLE,
}

// The handle is only used for device I/O, which may come from any thread.
unsafe impl Send for Volume {}
unsafe impl Sync for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Volume {
    /// Opens the volume of drive `letter`. Needs administrator rights.
    pub fn open(letter: char) -> io::Result<Self> {
        let handle = unsafe {
            CreateFileW(
                &HSTRING::from(format!(r"\\.\{letter}:")),
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        }
        .map_err(to_io)?;
        Ok(Self { handle })
    }

    pub fn journal(&self) -> io::Result<Journal> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        self.control(FSCTL_QUERY_USN_JOURNAL, None, as_bytes_mut(&mut data))?;
        Ok(Journal {
            id: data.UsnJournalID,
            first: data.FirstUsn,
            next: data.NextUsn,
        })
    }

    /// Calls `f` with every file and folder on the volume, in record order.
    pub fn enumerate(&self, journal: &Journal, mut f: impl FnMut(Record)) -> io::Result<()> {
        let mut input = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: journal.next,
        };
        let mut buffer = vec![0u64; BUFFER_BYTES / 8];
        loop {
            let read = match self.control(
                FSCTL_ENUM_USN_DATA,
                Some(as_bytes(&input)),
                as_bytes_mut(&mut buffer[..]),
            ) {
                Ok(read) => read,
                // ERROR_HANDLE_EOF: past the last record.
                Err(err) if err.raw_os_error() == Some(38) => return Ok(()),
                Err(err) => return Err(err),
            };
            if read <= 8 {
                return Ok(());
            }
            let bytes = &as_bytes(&buffer[..])[..read];
            // The first 8 bytes are where the next call starts.
            input.StartFileReferenceNumber = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            parse_records(&bytes[8..], &mut f);
        }
    }

    /// Calls `f` with every change from USN `from` on, and returns the USN to
    /// read from next time. Doesn't wait for new changes.
    pub fn read_changes(
        &self,
        journal: &Journal,
        from: i64,
        mut f: impl FnMut(Record),
    ) -> io::Result<i64> {
        let mut input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: from,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal.id,
        };
        let mut buffer = vec![0u64; BUFFER_BYTES / 8];
        loop {
            let read = self.control(
                FSCTL_READ_USN_JOURNAL,
                Some(as_bytes(&input)),
                as_bytes_mut(&mut buffer[..]),
            )?;
            let bytes = &as_bytes(&buffer[..])[..read];
            if read < 8 {
                return Ok(input.StartUsn);
            }
            let next = i64::from_le_bytes(bytes[..8].try_into().unwrap());
            if read == 8 || next == input.StartUsn {
                return Ok(next);
            }
            parse_records(&bytes[8..], &mut f);
            input.StartUsn = next;
        }
    }

    /// The file reference number of the folder at `path` on this volume.
    pub fn frn_of(path: &Path) -> io::Result<u64> {
        let handle = unsafe {
            CreateFileW(
                &HSTRING::from(crate::fswalk::long_path(path)),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(to_io)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let result = unsafe { GetFileInformationByHandle(handle, &mut info) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        result.map_err(to_io)?;
        Ok(((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64)
    }

    /// When file `frn` was last modified, unix seconds, from its MFT record;
    /// `None` when the record is gone or unreadable.
    pub fn modified(&self, frn: u64) -> Option<i64> {
        let input = NTFS_FILE_RECORD_INPUT_BUFFER {
            FileReferenceNumber: frn as i64,
        };
        // An output header (FRN, length) and a record of up to 4 KB.
        let mut buffer = vec![0u64; (12usize + 4096).div_ceil(8) + 1];
        let read = self
            .control(
                FSCTL_GET_NTFS_FILE_RECORD,
                Some(as_bytes(&input)),
                as_bytes_mut(&mut buffer[..]),
            )
            .ok()?;
        let bytes = as_bytes_mut(&mut buffer[..]);
        let returned = u64::from_le_bytes(bytes[..8].try_into().ok()?);
        // A record not in use gives the closest one below it instead.
        if returned & RECORD_MASK != frn & RECORD_MASK {
            return None;
        }
        let length = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        let record = bytes.get_mut(12..(12 + length).min(read))?;
        modified_from_record(record)
    }

    fn control(&self, code: u32, input: Option<&[u8]>, output: &mut [u8]) -> io::Result<usize> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.handle,
                code,
                input.map(|input| input.as_ptr().cast()),
                input.map_or(0, |input| input.len() as u32),
                Some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(to_io)?;
        Ok(returned as usize)
    }
}

/// Whether the drive holding `path` is NTFS. Needs no rights.
pub fn is_ntfs(path: &Path) -> bool {
    let Some(letter) = drive_letter(path) else {
        return false;
    };
    let mut name = [0u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            &HSTRING::from(format!(r"{letter}:\")),
            None,
            None,
            None,
            None,
            Some(&mut name),
        )
    };
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    ok.is_ok() && String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("NTFS")
}

/// `C` for `C:\Users\me`; `None` for a share or a relative path.
pub fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut chars = text.chars();
    let letter = chars.next()?;
    (letter.is_ascii_alphabetic() && chars.next() == Some(':')).then(|| letter.to_ascii_uppercase())
}

/// Bytes one enumeration or journal read asks for at a time.
const BUFFER_BYTES: usize = 1 << 20;

/// Calls `f` with each `USN_RECORD_V2` / `V3` in `bytes` (a chain of
/// records, each `RecordLength` long, at 8-byte boundaries).
pub fn parse_records(bytes: &[u8], f: &mut impl FnMut(Record)) {
    let mut offset = 0usize;
    while offset + 8 <= bytes.len() {
        let length = u32_at(bytes, offset) as usize;
        if length == 0 || offset + length > bytes.len() {
            break;
        }
        if let Some(record) = parse_record(&bytes[offset..offset + length]) {
            f(record);
        }
        offset += length;
    }
}

fn parse_record(bytes: &[u8]) -> Option<Record> {
    let major = u16::from_le_bytes(bytes.get(4..6)?.try_into().ok()?);
    // V2 has 64-bit references; V3 has 128-bit ones, of which NTFS uses the
    // low 64. The fields after them are the same, shifted.
    let (frn, parent, rest) = match major {
        2 => (u64_at(bytes, 8), u64_at(bytes, 16), 24),
        3 => (u64_at(bytes, 8), u64_at(bytes, 24), 40),
        _ => return None,
    };
    let usn = u64_at(bytes, rest) as i64;
    let timestamp = u64_at(bytes, rest + 8);
    let reason = u32_at(bytes, rest + 16);
    let attributes = u32_at(bytes, rest + 28);
    let name_len = u16::from_le_bytes(bytes.get(rest + 32..rest + 34)?.try_into().ok()?) as usize;
    let name_at = u16::from_le_bytes(bytes.get(rest + 34..rest + 36)?.try_into().ok()?) as usize;
    let name: Vec<u16> = bytes
        .get(name_at..name_at + name_len)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    Some(Record {
        frn,
        parent,
        usn,
        reason,
        attributes,
        time: if timestamp == 0 {
            0
        } else {
            unix_seconds(windows::Win32::Foundation::FILETIME {
                dwLowDateTime: timestamp as u32,
                dwHighDateTime: (timestamp >> 32) as u32,
            })
        },
        name: String::from_utf16_lossy(&name),
    })
}

/// The last modification time in an MFT `FILE` record, from its
/// `$STANDARD_INFORMATION` attribute. Applies the record's fixups first if
/// they haven't been (the last two bytes of every 512-byte sector hold the
/// update sequence number until then).
fn modified_from_record(record: &mut [u8]) -> Option<i64> {
    if record.get(..4)? != b"FILE" {
        return None;
    }
    let fixup_at = u16::from_le_bytes(record.get(4..6)?.try_into().ok()?) as usize;
    let fixups = u16::from_le_bytes(record.get(6..8)?.try_into().ok()?) as usize;
    if fixups > 1 {
        let check = [*record.get(fixup_at)?, *record.get(fixup_at + 1)?];
        for sector in 1..fixups {
            let end = sector * 512;
            if end > record.len() || record[end - 2..end] != check {
                break;
            }
            let original = fixup_at + sector * 2;
            let (a, b) = (*record.get(original)?, *record.get(original + 1)?);
            record[end - 2] = a;
            record[end - 1] = b;
        }
    }
    const STANDARD_INFORMATION: u32 = 0x10;
    const END: u32 = 0xFFFF_FFFF;
    let mut at = u16::from_le_bytes(record.get(0x14..0x16)?.try_into().ok()?) as usize;
    while at + 8 <= record.len() {
        let kind = u32_at(record, at);
        let length = u32_at(record, at + 4) as usize;
        if kind == END || length == 0 {
            return None;
        }
        // Resident (non-resident flag 0); content offset at 0x14.
        if kind == STANDARD_INFORMATION && record.get(at + 8) == Some(&0) {
            let content = at
                + u16::from_le_bytes(record.get(at + 0x14..at + 0x16)?.try_into().ok()?) as usize;
            let modified = u64_at(record, content + 8);
            return Some(unix_seconds(windows::Win32::Foundation::FILETIME {
                dwLowDateTime: modified as u32,
                dwHighDateTime: (modified >> 32) as u32,
            }));
        }
        at += length;
    }
    None
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// Plain data (structs of integers, `[u64]` buffers) as bytes.
fn as_bytes<T: ?Sized>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast(), size_of_val(value)) }
}

fn as_bytes_mut<T: ?Sized>(value: &mut T) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut((value as *mut T).cast(), size_of_val(value)) }
}

fn to_io(err: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(err.code().0 & 0xFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `USN_RECORD_V2` as Windows lays it out.
    fn v2(frn: u64, parent: u64, reason: u32, attributes: u32, name: &str) -> Vec<u8> {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let length = (60 + name.len()).div_ceil(8) * 8;
        let mut out = vec![0u8; length];
        out[0..4].copy_from_slice(&(length as u32).to_le_bytes());
        out[4..6].copy_from_slice(&2u16.to_le_bytes());
        out[8..16].copy_from_slice(&frn.to_le_bytes());
        out[16..24].copy_from_slice(&parent.to_le_bytes());
        out[24..32].copy_from_slice(&77i64.to_le_bytes());
        // 2024-01-01 as a FILETIME.
        out[32..40].copy_from_slice(&133_485_408_000_000_000u64.to_le_bytes());
        out[40..44].copy_from_slice(&reason.to_le_bytes());
        out[52..56].copy_from_slice(&attributes.to_le_bytes());
        out[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        out[58..60].copy_from_slice(&60u16.to_le_bytes());
        out[60..60 + name.len()].copy_from_slice(&name);
        out
    }

    #[test]
    fn parses_a_chain_of_records() {
        let mut bytes = v2(
            0x0001_0000_0000_0010,
            5,
            USN_REASON_FILE_CREATE,
            0x10,
            "Отчёты",
        );
        bytes.extend(v2(
            0x0002_0000_0000_0011,
            0x0001_0000_0000_0010,
            0,
            0x20,
            "a.txt",
        ));
        let mut records = Vec::new();
        parse_records(&bytes, &mut |record| records.push(record));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "Отчёты");
        assert_eq!(records[0].frn & RECORD_MASK, 0x10);
        assert_eq!(records[0].reason, USN_REASON_FILE_CREATE);
        assert_eq!(records[0].time, 1_704_067_200);
        assert_eq!(records[1].parent, records[0].frn);
        assert_eq!(records[1].attributes, 0x20);
        assert_eq!(records[1].usn, 77);
    }

    #[test]
    fn stops_at_a_truncated_record() {
        let mut bytes = v2(16, 5, 0, 0, "x");
        bytes.extend_from_slice(&[200, 0, 0, 0, 2, 0]);
        let mut count = 0;
        parse_records(&bytes, &mut |_| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn reads_the_modification_time_of_a_file_record() {
        // A 1 KB record: header, $STANDARD_INFORMATION at 0x38, then the end.
        let mut record = vec![0u8; 1024];
        record[..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        record[6..8].copy_from_slice(&3u16.to_le_bytes());
        record[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        let attribute = 0x38;
        record[attribute..attribute + 4].copy_from_slice(&0x10u32.to_le_bytes());
        record[attribute + 4..attribute + 8].copy_from_slice(&0x60u32.to_le_bytes());
        record[attribute + 0x14..attribute + 0x16].copy_from_slice(&0x18u16.to_le_bytes());
        let content = attribute + 0x18;
        record[content + 8..content + 16]
            .copy_from_slice(&133_485_408_000_000_000u64.to_le_bytes());
        record[attribute + 0x60..attribute + 0x64].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        // Fixups not applied yet: sector ends hold the sequence number.
        record[0x30..0x32].copy_from_slice(&[7, 0]);
        record[0x32..0x34].copy_from_slice(&[0xAB, 0xCD]);
        record[0x34..0x36].copy_from_slice(&[0xEF, 0x01]);
        record[510..512].copy_from_slice(&[7, 0]);
        record[1022..1024].copy_from_slice(&[7, 0]);
        assert_eq!(modified_from_record(&mut record), Some(1_704_067_200));
        assert_eq!(record[510..512], [0xAB, 0xCD]);
        assert_eq!(modified_from_record(&mut [0u8; 64]), None);
    }

    #[test]
    fn finds_drive_letters_and_ntfs() {
        assert_eq!(drive_letter(Path::new(r"c:\Users")), Some('C'));
        assert_eq!(drive_letter(Path::new(r"\\?\D:\x")), Some('D'));
        assert_eq!(drive_letter(Path::new(r"\\server\share")), None);
        let windows = std::env::var("SystemRoot").unwrap();
        assert!(is_ntfs(Path::new(&windows)));
        // The volume itself needs administrator rights.
        if Volume::open('C').is_err() {
            assert!(Volume::frn_of(Path::new(&windows)).is_ok());
        }
    }
}
