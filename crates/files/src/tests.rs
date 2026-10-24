//! Search behaviour on a small tree shaped like a real user profile.

use std::{path::Path, sync::atomic::AtomicBool};

use crate::{Builder, EntryId, Index, Meta, Query, SearchOptions};

const NOW: i64 = 1_760_000_000;
const DAY: i64 = 86_400;

/// `path` relative to the root, folders ending in `/`, with an age in days.
const TREE: &[(&str, i64)] = &[
    ("Desktop/", 300),
    ("Desktop/rea-test/", 300),
    ("Desktop/rea-test/notes/", 300),
    ("Desktop/rea-test/notes/03-rust-native-plan.md", 2),
    ("Desktop/rea-test/notes/04-file-search-plan.md", 0),
    ("Desktop/rea-test/launcher/", 300),
    ("Desktop/rea-test/launcher/crates/", 300),
    ("Desktop/rea-test/launcher/crates/ui/", 300),
    ("Desktop/rea-test/launcher/crates/ui/src/", 300),
    ("Desktop/rea-test/launcher/crates/ui/src/launcher.rs", 1),
    ("Desktop/rea-test/launcher/crates/ui/src/launcher/", 300),
    (
        "Desktop/rea-test/launcher/crates/ui/src/launcher/snippets.rs",
        1,
    ),
    ("Desktop/rea-test/launcher/crates/providers/", 300),
    ("Desktop/rea-test/launcher/crates/providers/src/", 300),
    (
        "Desktop/rea-test/launcher/crates/providers/src/spotlight.rs",
        3,
    ),
    ("Desktop/Spotlight.lnk", 40),
    ("Documents/", 300),
    ("Documents/Отчёт за 2024.docx", 90),
    ("Documents/Report final.pdf", 200),
    ("Documents/report-draft.pdf", 10),
    ("Documents/plan.md", 400),
    ("Documents/Taxes/", 300),
    ("Documents/Taxes/2024/", 300),
    ("Documents/Taxes/2024/receipt.pdf", 100),
    ("Pictures/", 300),
    ("Pictures/Screenshots/", 300),
    ("Pictures/Screenshots/Screenshot 2026-10-23.png", 1),
    ("Pictures/cat.jpg", 500),
    ("Downloads/", 300),
    ("Downloads/setup.exe", 20),
    ("Downloads/plan.zip", 20),
    ("AppData/", 300),
    ("AppData/launcher.log", 0),
];

fn build() -> (Index, EntryId) {
    let mut builder = Builder::default();
    let root = builder.root(Path::new(r"C:\Users\me"));
    let mut folders: Vec<(String, EntryId)> = Vec::new();
    for (path, age) in TREE {
        let dir = path.ends_with('/');
        let trimmed = path.trim_end_matches('/');
        let (parent, name) = match trimmed.rsplit_once('/') {
            Some((parent, name)) => (
                folders.iter().find(|(path, _)| path == parent).unwrap().1,
                name,
            ),
            None => (root, trimmed),
        };
        let id = builder.add(
            parent,
            Meta {
                name,
                dir,
                hidden: name == "AppData",
                modified: NOW - age * DAY,
                ..Meta::default()
            },
        );
        if dir {
            folders.push((trimmed.to_owned(), id));
        }
    }
    (builder.finish(), root)
}

fn options() -> SearchOptions<'static> {
    SearchOptions {
        now: NOW,
        ..SearchOptions::default()
    }
}

fn search(index: &Index, query: &str) -> Vec<String> {
    search_with(index, query, &options())
}

fn search_with(index: &Index, query: &str, options: &SearchOptions) -> Vec<String> {
    let root = index.path(0);
    index
        .search(&Query::parse(query), options)
        .unwrap()
        .into_iter()
        .map(|hit| {
            index
                .path(hit.id)
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn finds_by_name() {
    let (index, _) = build();
    assert_eq!(
        search(&index, "launcher.rs")[0],
        "Desktop/rea-test/launcher/crates/ui/src/launcher.rs"
    );
    assert_eq!(
        search(&index, "snippets")[0],
        "Desktop/rea-test/launcher/crates/ui/src/launcher/snippets.rs"
    );
}

#[test]
fn tolerates_typos() {
    let (index, _) = build();
    let found = search(&index, "spotlgiht");
    assert!(
        found.iter().any(|path| path.ends_with("spotlight.rs")),
        "{found:?}"
    );
}

#[test]
fn exact_stem_ranks_first() {
    let (index, _) = build();
    let found = search(&index, "plan");
    // `plan.md` and `plan.zip` are named exactly that; the md file is older,
    // the zip is shallower in Downloads: both beat "…-plan.md".
    assert!(
        found[0].ends_with("plan.md") || found[0].ends_with("plan.zip"),
        "{found:?}"
    );
    assert!(
        found[1].ends_with("plan.md") || found[1].ends_with("plan.zip"),
        "{found:?}"
    );
    assert!(found.len() >= 4);
}

#[test]
fn folder_words_narrow_by_path() {
    let (index, _) = build();
    let found = search(&index, "rea plan");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found
            .iter()
            .all(|path| path.starts_with("Desktop/rea-test/notes/"))
    );
    // The newer file wins the tie.
    assert_eq!(found[0], "Desktop/rea-test/notes/04-file-search-plan.md");
}

#[test]
fn several_words_can_all_be_in_the_name() {
    let (index, _) = build();
    assert_eq!(
        search(&index, "report final")[0],
        "Documents/Report final.pdf"
    );
    assert_eq!(
        search(&index, "\"report final\""),
        ["Documents/Report final.pdf"]
    );
}

#[test]
fn cyrillic_names_ignore_case() {
    let (index, _) = build();
    assert_eq!(search(&index, "отчёт"), ["Documents/Отчёт за 2024.docx"]);
    assert_eq!(
        search(&index, "ОТЧЁТ 2024"),
        ["Documents/Отчёт за 2024.docx"]
    );
}

#[test]
fn short_words_match_name_starts() {
    let (index, _) = build();
    let found = search(&index, "ca");
    assert_eq!(found, ["Pictures/cat.jpg"]);
}

#[test]
fn extension_and_kind_filters() {
    let (index, _) = build();
    let pdfs = search(&index, "*.pdf");
    assert_eq!(pdfs.len(), 3);
    // Filters only: newest first.
    assert_eq!(pdfs[0], "Documents/report-draft.pdf");
    assert_eq!(search(&index, ".pdf"), pdfs);
    assert_eq!(search(&index, "ext:pdf"), pdfs);
    assert_eq!(search(&index, "report ext:pdf").len(), 2);
    assert_eq!(
        search(&index, "kind:image"),
        [
            "Pictures/Screenshots/Screenshot 2026-10-23.png",
            "Pictures/cat.jpg"
        ]
    );
    assert_eq!(search(&index, "plan kind:archive"), ["Downloads/plan.zip"]);
    assert!(
        search(&index, "kind:folder")
            .iter()
            .all(|path| !path.contains('.'))
    );
    assert!(search(&index, "*.nothing").is_empty());
}

#[test]
fn in_filter_and_exclusion() {
    let (index, _) = build();
    assert_eq!(
        search(&index, "pdf in:taxes"),
        ["Documents/Taxes/2024/receipt.pdf"]
    );
    let found = search(&index, "report !draft");
    assert_eq!(found, ["Documents/Report final.pdf"]);
}

#[test]
fn hidden_files_only_when_asked() {
    let (index, _) = build();
    assert!(
        !search(&index, "launcher")
            .iter()
            .any(|path| path.starts_with("AppData"))
    );
    let options = SearchOptions {
        include_hidden: true,
        ..options()
    };
    let found = search_with(&index, "launcher.log", &options);
    assert_eq!(found[0], "AppData/launcher.log");
}

#[test]
fn frecency_reorders_close_matches() {
    let (index, _) = build();
    let before = search(&index, "report");
    let draft = (0..index.len() as EntryId)
        .find(|&id| index.name(id) == "report-draft.pdf")
        .unwrap();
    let final_ = (0..index.len() as EntryId)
        .find(|&id| index.name(id) == "Report final.pdf")
        .unwrap();
    let favourite = if before[0].ends_with("report-draft.pdf") {
        final_
    } else {
        draft
    };
    let frecency = move |id: EntryId| if id == favourite { 5. } else { 0. };
    let options = SearchOptions {
        frecency: Some(&frecency),
        ..options()
    };
    let after = search_with(&index, "report", &options);
    assert_eq!(
        index.name(favourite),
        Path::new(&after[0]).file_name().unwrap().to_str().unwrap()
    );
}

#[test]
fn changes_show_up_without_compaction() {
    let (mut index, root) = build();
    let docs = (0..index.len() as EntryId)
        .find(|&id| index.name(id) == "Documents")
        .unwrap();
    let new = index.insert(
        docs,
        Meta {
            name: "квартальный бюджет.xlsx",
            modified: NOW,
            ..Meta::default()
        },
    );
    assert_eq!(
        search(&index, "бюджет"),
        ["Documents/квартальный бюджет.xlsx"]
    );
    // Short words go through the fst plus the added names.
    assert_eq!(search(&index, "кв"), ["Documents/квартальный бюджет.xlsx"]);

    index.rename(new, root, "budget.xlsx");
    assert_eq!(search(&index, "budget"), ["budget.xlsx"]);
    assert!(search(&index, "бюджет").is_empty());

    index.remove(docs);
    assert!(search(&index, "report").is_empty());
    assert!(
        search(&index, "*.pdf")
            .iter()
            .all(|path| !path.starts_with("Documents"))
    );

    let (compacted, _) = index.compact();
    assert_eq!(search(&compacted, "budget"), ["budget.xlsx"]);
    assert!(search(&compacted, "receipt").is_empty());
}

#[test]
fn cancelled_search_returns_none() {
    let (index, _) = build();
    let cancel = AtomicBool::new(true);
    let options = SearchOptions {
        cancel: Some(&cancel),
        ..options()
    };
    assert!(index.search(&Query::parse("plan"), &options).is_none());
}

#[test]
fn empty_query_finds_nothing() {
    let (index, _) = build();
    assert!(search(&index, "").is_empty());
    assert!(search(&index, "   ").is_empty());
}

#[test]
fn deep_inside_a_hidden_folder_is_hidden() {
    let mut builder = Builder::default();
    let root = builder.root(Path::new(r"C:\Users\me"));
    let app_data = builder.add(
        root,
        Meta {
            name: "AppData",
            dir: true,
            hidden: true,
            ..Meta::default()
        },
    );
    let local = builder.add(
        app_data,
        Meta {
            name: "Local",
            dir: true,
            ..Meta::default()
        },
    );
    let temp = builder.add(
        local,
        Meta {
            name: "Temp",
            dir: true,
            ..Meta::default()
        },
    );
    builder.add(
        temp,
        Meta {
            name: "chart.png",
            ..Meta::default()
        },
    );
    let index = builder.finish();
    assert!(search(&index, "chart").is_empty());
    assert!(search(&index, "*.png").is_empty());
    assert!(search(&index, "temp").is_empty());
}

#[test]
fn dot_names_count_as_hidden() {
    let mut builder = Builder::default();
    let root = builder.root(Path::new(r"C:\Users\me"));
    let vscode = builder.add(
        root,
        Meta {
            name: ".vscode",
            dir: true,
            ..Meta::default()
        },
    );
    builder.add(
        vscode,
        Meta {
            name: "settings.json",
            ..Meta::default()
        },
    );
    builder.add(
        root,
        Meta {
            name: ".gitignore",
            ..Meta::default()
        },
    );
    let index = builder.finish();
    assert!(search(&index, "settings").is_empty());
    assert!(search(&index, ".gitignore").is_empty());
    let options = SearchOptions {
        include_hidden: true,
        ..options()
    };
    assert_eq!(search_with(&index, ".gitignore", &options), [".gitignore"]);
    assert_eq!(
        search_with(&index, "settings", &options),
        [".vscode/settings.json"]
    );
}

#[test]
fn insert_updates_what_is_already_there() {
    let (mut index, root) = build();
    let before = index.len();
    let docs = index.child(root, "documents").unwrap();
    let report = index.child(docs, "REPORT FINAL.PDF").unwrap();
    let again = index.insert(
        docs,
        Meta {
            name: "Report final.pdf",
            modified: NOW,
            ..Meta::default()
        },
    );
    assert_eq!(again, report);
    assert_eq!(index.len(), before);
    assert_eq!(index.modified(report), NOW / 60 * 60);

    // A folder where the file was replaces it.
    let folder = index.insert(
        docs,
        Meta {
            name: "Report final.pdf",
            dir: true,
            ..Meta::default()
        },
    );
    assert_ne!(folder, report);
    assert!(!index.is_live(report));
    assert_eq!(index.child(docs, "report final.pdf"), Some(folder));
}

#[test]
fn rename_over_an_existing_name_replaces_it() {
    let (mut index, root) = build();
    let docs = index.child(root, "Documents").unwrap();
    let draft = index.child(docs, "report-draft.pdf").unwrap();
    let final_ = index.child(docs, "Report final.pdf").unwrap();
    index.rename(draft, docs, "Report final.pdf");
    assert!(!index.is_live(final_));
    assert_eq!(index.child(docs, "report final.pdf"), Some(draft));
    assert_eq!(index.child(docs, "report-draft.pdf"), None);
    assert_eq!(
        index.resolve(Path::new(r"C:\Users\me\Documents\Report final.pdf")),
        Some(draft)
    );
}

#[test]
fn recent_lists_files_newest_first() {
    let (index, _) = build();
    let recent: Vec<String> = index
        .recent(&options())
        .unwrap()
        .into_iter()
        .map(|hit| index.name(hit.id).to_owned())
        .collect();
    // Hidden AppData's log is left out, and folders are.
    assert_eq!(recent[0], "04-file-search-plan.md");
    assert!(
        !recent
            .iter()
            .any(|name| name == "launcher.log" || name == "Documents")
    );
}
