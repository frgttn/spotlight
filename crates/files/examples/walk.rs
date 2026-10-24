//! Indexes a real folder with a plain `std::fs` walk, then runs queries on it.
//! The real walker (parallel, `FindFirstFileExW`) comes with Search Files.
//!
//! `cargo run --release -p spotlight-files --example walk -- <folder> [query]…`

use std::{
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use spotlight_files::{Builder, EntryId, Meta, Query, SearchOptions};

fn walk(builder: &mut Builder, dir: &Path, id: EntryId) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        // Does not follow links: on Windows it comes from the directory listing.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let (hidden, system) = attributes(&meta);
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs() as i64);
        let child = builder.add(
            id,
            Meta {
                name: &name,
                dir: meta.is_dir(),
                hidden,
                system,
                modified,
            },
        );
        // Junctions and symlinks are listed but not followed.
        if meta.is_dir() && !meta.file_type().is_symlink() {
            walk(builder, &entry.path(), child);
        }
    }
}

#[cfg(windows)]
fn attributes(meta: &std::fs::Metadata) -> (bool, bool) {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    let attributes = meta.file_attributes();
    (attributes & HIDDEN != 0, attributes & SYSTEM != 0)
}

#[cfg(not(windows))]
fn attributes(_meta: &std::fs::Metadata) -> (bool, bool) {
    (false, false)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: walk <folder> [query]…"));
    let queries: Vec<String> = args.collect();

    let started = Instant::now();
    let mut builder = Builder::default();
    let id = builder.root(&root);
    walk(&mut builder, &root, id);
    let walked = started.elapsed();
    let index = builder.finish();
    println!(
        "{} entries: walk {:?}, build {:?}, {:.1} MB ({:.0} bytes per entry)",
        index.len(),
        walked,
        started.elapsed() - walked,
        index.memory_bytes() as f64 / 1_048_576.,
        index.memory_bytes() as f64 / index.len() as f64
    );

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let options = SearchOptions {
        now,
        limit: 8,
        ..SearchOptions::default()
    };
    for text in &queries {
        let started = Instant::now();
        let hits = index.search(&Query::parse(text), &options).unwrap();
        println!("\n{text:?}: {:?}", started.elapsed());
        for hit in hits {
            let path = index.path(hit.id);
            let shown = path.strip_prefix(&root).unwrap_or(&path);
            println!("  {:>4}  {}", hit.score, shown.display());
        }
    }
}
