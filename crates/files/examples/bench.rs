//! Query timings on a synthetic tree of a million entries.
//!
//! `cargo run --release -p spotlight-files --example bench [entries]`

use std::{path::Path, time::Instant};

use spotlight_files::{Builder, EntryId, Index, Meta, Query, SearchOptions};

const WORDS: &[&str] = &[
    "report",
    "plan",
    "notes",
    "budget",
    "invoice",
    "photo",
    "screenshot",
    "project",
    "draft",
    "final",
    "backup",
    "config",
    "index",
    "main",
    "test",
    "data",
    "image",
    "video",
    "music",
    "readme",
    "setup",
    "launcher",
    "spotlight",
    "client",
    "server",
    "build",
    "release",
    "archive",
    "summary",
    "meeting",
    "agenda",
    "contract",
    "resume",
    "letter",
    "отчёт",
    "план",
    "заметки",
    "бюджет",
    "счёт",
    "фото",
    "проект",
    "договор",
    "резюме",
    "письмо",
    "итоги",
    "встреча",
];
const FOLDERS: &[&str] = &[
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "Desktop",
    "src",
    "assets",
    "node_modules",
    "lib",
    "target",
    "dist",
    "build",
    "docs",
    "tests",
    "vendor",
    "2023",
    "2024",
    "2025",
    "old",
    "work",
    "home",
    "Проекты",
    "Работа",
    "Архив",
];
const EXTS: &[&str] = &[
    "txt", "md", "pdf", "docx", "xlsx", "png", "jpg", "mp4", "mp3", "zip", "rs", "js", "ts",
    "json", "html", "css", "py", "exe", "dll", "log", "toml", "yaml", "svg", "lnk",
];
const NOW: i64 = 1_760_000_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, list: &[&'a str]) -> &'a str {
        list[self.below(list.len())]
    }
}

fn file_name(rng: &mut Rng) -> String {
    let mut name = String::new();
    for part in 0..1 + rng.below(3) {
        if part > 0 {
            name.push(['-', '_', ' ', '.'][rng.below(4)]);
        }
        name.push_str(rng.pick(WORDS));
    }
    if rng.below(3) == 0 {
        name.push_str(&format!(" {}", rng.below(10_000)));
    }
    name.push('.');
    name.push_str(rng.pick(EXTS));
    name
}

fn build(entries: usize) -> Index {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut builder = Builder::with_capacity(entries);
    let root = builder.root(Path::new(r"C:\Users\bench"));
    let mut folders: Vec<EntryId> = vec![root];
    while builder.len() < entries {
        let parent = folders[rng.below(folders.len())];
        let modified = NOW - rng.below(3 * 365 * 86_400) as i64;
        // About one entry in eight is a folder, as in a real profile.
        if rng.below(8) == 0 {
            let name = format!("{} {}", rng.pick(FOLDERS), rng.below(100));
            let id = builder.add(
                parent,
                Meta {
                    name: &name,
                    dir: true,
                    modified,
                    ..Meta::default()
                },
            );
            folders.push(id);
        } else {
            let name = file_name(&mut rng);
            builder.add(
                parent,
                Meta {
                    name: &name,
                    modified,
                    ..Meta::default()
                },
            );
        }
    }
    builder.finish()
}

fn main() {
    let entries = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(1_000_000);
    let started = Instant::now();
    let index = build(entries);
    println!(
        "built {} entries in {:?}, {:.1} MB ({:.0} bytes per entry)",
        index.len(),
        started.elapsed(),
        index.memory_bytes() as f64 / 1_048_576.,
        index.memory_bytes() as f64 / index.len() as f64
    );

    let options = SearchOptions {
        now: NOW,
        ..SearchOptions::default()
    };
    println!(
        "{:<24} {:>9} {:>9} {:>5}  best",
        "query", "median", "max", "hits"
    );
    for text in [
        "a",
        "re",
        "pla",
        "plan",
        "report",
        "report final",
        "spotlgiht",
        "launcher.rs",
        "work plan",
        "*.pdf",
        "kind:image photo",
        "отчёт",
        "бюджет 2024",
        "budget in:2024 ext:xlsx",
        "^readme$",
        "zzzzqqq",
    ] {
        let query = Query::parse(text);
        let mut times = Vec::new();
        let mut hits = Vec::new();
        for _ in 0..15 {
            let started = Instant::now();
            hits = index.search(&query, &options).unwrap();
            times.push(started.elapsed());
        }
        times.sort();
        let best = hits.first().map(|hit| index.name(hit.id)).unwrap_or("");
        println!(
            "{:<24} {:>7.2}ms {:>7.2}ms {:>5}  {}",
            text,
            times[times.len() / 2].as_secs_f64() * 1000.,
            times[times.len() - 1].as_secs_f64() * 1000.,
            hits.len(),
            best
        );
    }
}
