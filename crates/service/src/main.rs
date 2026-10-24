//! `spotlight-service.exe`: Spotlight's optional file index service. It reads
//! NTFS file tables and change journals as LocalSystem and serves the
//! launcher over a named pipe; see `notes/04-file-search-plan.md` §5.2.
//!
//! - `install --client <Spotlight.exe>` / `uninstall`: from an elevated
//!   process (Spotlight's Settings start one with a UAC prompt).
//! - `run`: what the Service Control Manager starts.
//! - `console`: serves in the foreground, for trying it from an elevated
//!   terminal; `scan <drive>`: reads a drive's table and follows its journal
//!   for a while, printing what it sees.

mod log;
mod scm;
mod server;
mod store;
mod tree;
mod volume;

use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use spotlight_platform_win::ntfs;

use crate::{tree::Tree, volume::Source};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => scm::run_as_service(),
        Some("install") => match args.iter().position(|arg| arg == "--client") {
            Some(ix) if ix + 1 < args.len() => scm::install(Path::new(&args[ix + 1])),
            _ => Err("install needs --client <path to Spotlight.exe>".into()),
        },
        Some("uninstall") => scm::uninstall(),
        Some("console") => scm::console(),
        Some("scan") => scan(args.get(1).and_then(|drive| drive.chars().next()).unwrap_or('C')),
        _ => Err("usage: spotlight-service install --client <Spotlight.exe> | uninstall | console | scan <drive>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// Reads drive `letter`'s table, then prints its changes for 30 seconds.
fn scan(letter: char) -> Result<(), String> {
    let volume =
        ntfs::Volume::open(letter).map_err(|err| format!("{letter}: {err} (run it elevated)"))?;
    let journal = volume.journal().map_err(|err| err.to_string())?;
    log!(
        "journal {:#x}, USNs {}..{}",
        journal.id,
        journal.first,
        journal.next
    );
    let root = Source::frn_of(&volume, Path::new(&format!("{letter}:\\")))
        .map_err(|err| err.to_string())?;
    let mut tree = Tree::new(root, PathBuf::from(format!("{letter}:\\")));
    let started = Instant::now();
    Source::enumerate(&volume, &journal, &mut |record| tree.insert(&record))
        .map_err(|err| err.to_string())?;
    log!(
        "{} entries in {:.2} s, {:.1} MB",
        tree.len(),
        started.elapsed().as_secs_f64(),
        tree.memory_bytes() as f64 / 1_048_576.
    );
    if let Some(profile) = env::var_os("USERPROFILE").map(PathBuf::from)
        && ntfs::drive_letter(&profile) == Some(letter)
    {
        let started = Instant::now();
        let frn = ntfs::Volume::frn_of(&profile).map_err(|err| err.to_string())?;
        let entries = tree.subtree(frn, |_| true, |frn| volume.modified(frn));
        log!(
            "{}: {} entries with times in {:.2} s",
            profile.display(),
            entries.len(),
            started.elapsed().as_secs_f64()
        );
    }
    log!("following the journal for 30 s…");
    let mut next = journal.next;
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        thread::sleep(Duration::from_millis(500));
        next = Source::read_changes(&volume, &journal, next, &mut |record| {
            for change in tree.apply(&record) {
                log!("{change:?}");
            }
        })
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}
