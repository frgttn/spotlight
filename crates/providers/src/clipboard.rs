//! Clipboard history: keeps what is copied (`ClipboardHistory::record`, fed
//! by the clipboard listener) and lists it in a view of its own, with a
//! preview of the selected entry.
//!
//! Text and file lists live in the database. Images are PNG files in the
//! `clipboard` folder of the data folder, with a smaller copy for the preview.

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use image::{ImageFormat, imageops::FilterType};
use spotlight_core::{
    clip::{self, ClipKind, Retention},
    frecency,
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    view::{ListView, Preview, PreviewBody, View},
};
use spotlight_platform_win::clipboard::{Content, Copied};
use spotlight_store::{Clip, Store};

use crate::{glyph, web};

const ID: &str = "clipboard";
const HISTORY_VIEW: &str = "history";
/// Preview images are scaled to fit this square (pixels), to keep what the
/// UI decodes small.
const THUMBNAIL_SIZE: u32 = 640;
/// How much of an entry's text search looks at, and the preview shows.
const SEARCHED_CHARS: usize = 500;
const PREVIEW_CHARS: usize = 20_000;

pub struct ClipboardHistory {
    store: Option<Arc<Mutex<Store>>>,
    /// Where images go; `None` keeps text and files only.
    images: Option<PathBuf>,
    recording: AtomicBool,
    retention: Mutex<Retention>,
}

impl ClipboardHistory {
    /// `data_dir` is the data folder; images go in its `clipboard` folder.
    pub fn new(
        store: Option<Arc<Mutex<Store>>>,
        data_dir: Option<PathBuf>,
        retention: Retention,
    ) -> Self {
        Self {
            store,
            images: data_dir.map(|dir| dir.join("clipboard")),
            recording: AtomicBool::new(true),
            retention: Mutex::new(retention),
        }
    }

    /// Stops or resumes keeping what is copied (the source's switch in Settings).
    pub fn set_recording(&self, on: bool) {
        self.recording.store(on, Ordering::Relaxed);
    }

    pub fn retention(&self) -> Retention {
        *self.retention.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Keeps less (or more) from now on, and drops what no longer fits.
    pub fn set_retention(&self, retention: Retention) {
        *self.retention.lock().unwrap_or_else(|err| err.into_inner()) = retention;
        self.trim();
    }

    /// Adds what was copied to the history. Blocking (writes an image file
    /// and the database), so it runs on the listener's side, off the UI.
    pub fn record(&self, copied: Copied) {
        if !self.recording.load(Ordering::Relaxed) {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        let (name, path) = copied.app.unzip();
        let mut clip = Clip {
            id: 0,
            kind: ClipKind::Text,
            content: String::new(),
            width: None,
            height: None,
            app: name,
            app_path: path,
            pinned: false,
            copied_at: frecency::now(),
        };
        let hash = match copied.content {
            Content::Text(text) => {
                let hash = hash(b'T', text.as_bytes());
                clip.content = text;
                hash
            }
            Content::Files(paths) => {
                clip.kind = ClipKind::Files;
                clip.content = paths.join("\n");
                hash(b'F', clip.content.as_bytes())
            }
            Content::Image(image) => {
                let hash = hash(b'I', &image.png);
                let Some(path) = self.save_image(&hash, &image.png) else {
                    return;
                };
                clip.kind = ClipKind::Image;
                clip.content = path;
                clip.width = Some(image.width);
                clip.height = Some(image.height);
                hash
            }
        };
        let added = store
            .lock()
            .map_err(|_| "store lock poisoned".to_owned())
            .and_then(|mut store| store.add_clip(&clip, &hash).map_err(|err| err.to_string()));
        if let Err(err) = added {
            eprintln!("[clipboard] could not save a clipboard entry: {err}");
        }
        self.trim();
    }

    /// Drops the entries the retention no longer keeps, with their images.
    pub fn trim(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let retention = self.retention();
        let removed = store
            .lock()
            .map_err(|_| "store lock poisoned".to_owned())
            .and_then(|mut store| {
                store
                    .trim_clips(retention, frecency::now())
                    .map_err(|err| err.to_string())
            });
        match removed {
            Ok(removed) => removed.iter().for_each(Clip::remove_files),
            Err(err) => eprintln!("[clipboard] could not trim the history: {err}"),
        }
    }

    /// Writes the image and its preview-sized copy (once per content), and
    /// returns the image's path.
    fn save_image(&self, hash: &str, png: &[u8]) -> Option<String> {
        let dir = self.images.as_ref()?;
        let path = dir.join(format!("{hash}.png"));
        let path_text = path.to_string_lossy().into_owned();
        if path.exists() {
            return Some(path_text);
        }
        let saved = fs::create_dir_all(dir)
            .and_then(|()| fs::write(&path, png))
            .map_err(|err| err.to_string())
            .and_then(|()| {
                let image = image::load_from_memory_with_format(png, ImageFormat::Png)
                    .map_err(|err| err.to_string())?;
                if image.width().max(image.height()) <= THUMBNAIL_SIZE {
                    return Ok(());
                }
                image
                    .resize(THUMBNAIL_SIZE, THUMBNAIL_SIZE, FilterType::Triangle)
                    .save_with_format(clip::thumbnail_path(&path_text), ImageFormat::Png)
                    .map_err(|err| err.to_string())
            });
        match saved {
            Ok(()) => Some(path_text),
            Err(err) => {
                eprintln!("[clipboard] could not save an image: {err}");
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    fn history(&self) -> ListView {
        let cutoff = self.retention().cutoff(frecency::now());
        let clips = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| {
                store.clips(cutoff).unwrap_or_else(|err| {
                    eprintln!("[clipboard] could not read the history: {err}");
                    Vec::new()
                })
            })
            .unwrap_or_default();
        let now = frecency::now();
        let empty = if self.recording.load(Ordering::Relaxed) {
            "Nothing copied yet. Text, images and files you copy show up here."
        } else {
            "Clipboard History is turned off in Settings, so nothing new is kept."
        };
        ListView {
            title: "Clipboard History".into(),
            placeholder: Some("Search clipboard history…".into()),
            items: clips.iter().map(|clip| item(clip, now)).collect(),
            empty: Some(empty.into()),
        }
    }
}

impl Provider for ClipboardHistory {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Clipboard History"
    }

    fn description(&self) -> &'static str {
        "Keeps what you copy, to paste it again. Off stops keeping new copies too."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        vec![
            SearchItem::new(item_id(ID, HISTORY_VIEW), "Clipboard History")
                .subtitle("Command")
                .keywords(&["clipboard", "paste", "copy", "history", "pasteboard"])
                .icon(Icon::Glyph(glyph::PASTE))
                .action(
                    "Open Clipboard History",
                    Command::OpenProviderView {
                        provider: ID.into(),
                        view: HISTORY_VIEW.into(),
                    },
                ),
        ]
    }

    fn view(&self, id: &str) -> Option<View> {
        (id == HISTORY_VIEW).then(|| View::List(self.history()))
    }
}

/// What kind of text it is, for the tag and the icon.
#[derive(Clone, Copy, PartialEq)]
enum Tag {
    Text,
    Link,
    Color,
    Image,
    File,
}

impl Tag {
    fn of(clip: &Clip) -> Self {
        match clip.kind {
            ClipKind::Image => Tag::Image,
            ClipKind::Files => Tag::File,
            ClipKind::Text if clip::is_color(&clip.content) => Tag::Color,
            ClipKind::Text if is_link(&clip.content) => Tag::Link,
            ClipKind::Text => Tag::Text,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tag::Text => "Text",
            Tag::Link => "Link",
            Tag::Color => "Color",
            Tag::Image => "Image",
            Tag::File => "File",
        }
    }

    fn glyph(self) -> char {
        match self {
            Tag::Text => glyph::TEXT,
            Tag::Link => glyph::LINK,
            Tag::Color => glyph::COLOR,
            Tag::Image => glyph::IMAGE,
            Tag::File => glyph::FOLDER,
        }
    }
}

/// One URL and nothing else.
fn is_link(text: &str) -> bool {
    web::as_url(text).is_some()
}

/// A row of the history: the text's first line, the image's size or the
/// file names; its preview; and Paste, Copy, Pin and Delete.
fn item(clip: &Clip, now: i64) -> SearchItem {
    let tag = Tag::of(clip);
    let paths: Vec<&str> = match clip.kind {
        ClipKind::Files => clip.content.lines().collect(),
        _ => Vec::new(),
    };
    let title = match clip.kind {
        ClipKind::Text => clip::first_line(&clip.content, 80),
        ClipKind::Image => match (clip.width, clip.height) {
            (Some(width), Some(height)) => format!("Image ({width}×{height})"),
            _ => "Image".into(),
        },
        ClipKind::Files => match paths.as_slice() {
            [path] => file_name(path),
            paths => format!("{} files: {}", paths.len(), {
                let names: Vec<String> = paths.iter().take(3).map(|path| file_name(path)).collect();
                names.join(", ")
            }),
        },
    };
    let subtitle = if clip.pinned {
        "Pinned".to_owned()
    } else {
        clip::ago(clip.copied_at, now)
    };

    let mut keywords = vec![tag.name().to_lowercase()];
    if clip.kind != ClipKind::Image {
        keywords.push(clip.content.chars().take(SEARCHED_CHARS).collect());
    }
    keywords.extend(clip.app.clone());

    let mut item = SearchItem::new(item_id(ID, &clip.id.to_string()), title)
        .subtitle(subtitle)
        .icon(Icon::Glyph(tag.glyph()))
        .action(
            "Paste",
            Command::RestoreClip {
                id: clip.id,
                paste: true,
            },
        )
        .action(
            "Copy to Clipboard",
            Command::RestoreClip {
                id: clip.id,
                paste: false,
            },
        );
    item.keywords = keywords;
    match (tag, paths.as_slice()) {
        (Tag::Link, _) => {
            if let Some(url) = web::as_url(&clip.content) {
                item = item.action("Open Link", Command::OpenUrl(url));
            }
        }
        (Tag::File, [path]) => {
            item = item
                .action("Open", Command::Open((*path).into()))
                .action("Show in Folder", Command::ShowInFolder((*path).into()));
        }
        _ => {}
    }
    let pin = if clip.pinned { "Unpin" } else { "Pin" };
    item = item
        .action(
            pin,
            Command::PinClip {
                id: clip.id,
                pinned: !clip.pinned,
            },
        )
        .action("Delete Entry", Command::DeleteClip(clip.id))
        .action(
            "Delete All Unpinned Entries",
            Command::ClearClipboardHistory,
        );
    item.preview(preview(clip, tag, &paths, now))
}

fn preview(clip: &Clip, tag: Tag, paths: &[&str], now: i64) -> Preview {
    let body = match clip.kind {
        ClipKind::Text => PreviewBody::Text(clip.content.chars().take(PREVIEW_CHARS).collect()),
        ClipKind::Image => {
            let thumbnail = clip::thumbnail_path(&clip.content);
            PreviewBody::Image(if fs::exists(&thumbnail).unwrap_or(false) {
                thumbnail
            } else {
                clip.content.clone()
            })
        }
        ClipKind::Files => PreviewBody::Files(paths.iter().map(|&path| path.to_owned()).collect()),
    };
    let mut metadata = vec![("Content".to_owned(), tag.name().to_owned())];
    if let Some(app) = &clip.app {
        metadata.push(("Application".into(), app.clone()));
    }
    match clip.kind {
        ClipKind::Text => {
            let text = &clip.content;
            metadata.push(("Characters".into(), text.chars().count().to_string()));
            metadata.push(("Words".into(), text.split_whitespace().count().to_string()));
            metadata.push(("Lines".into(), text.lines().count().max(1).to_string()));
        }
        ClipKind::Image => {
            if let (Some(width), Some(height)) = (clip.width, clip.height) {
                metadata.push(("Dimensions".into(), format!("{width} × {height}")));
            }
        }
        ClipKind::Files => metadata.push(("Files".into(), paths.len().to_string())),
    }
    metadata.push(("Copied".into(), clip::ago(clip.copied_at, now)));
    if clip.pinned {
        metadata.push(("Pinned".into(), "Yes".into()));
    }
    Preview { body, metadata }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// FNV-1a over a kind byte and the content: stable across runs and Rust
/// versions (unlike `DefaultHasher`), since the database keeps it.
fn hash(kind: u8, bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in std::iter::once(&kind).chain(bytes) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use spotlight_platform_win::clipboard::Image;

    use super::*;

    fn history() -> ClipboardHistory {
        let store = Store::open_in_memory().unwrap();
        ClipboardHistory::new(
            Some(Arc::new(Mutex::new(store))),
            None,
            Retention::default(),
        )
    }

    fn copied(content: Content) -> Copied {
        Copied {
            content,
            app: Some(("Notepad".into(), r"C:\Windows\notepad.exe".into())),
        }
    }

    fn titles(history: &ClipboardHistory) -> Vec<String> {
        history
            .history()
            .items
            .into_iter()
            .map(|item| item.title)
            .collect()
    }

    #[test]
    fn records_text_and_files_newest_first() {
        let history = history();
        history.record(copied(Content::Text("\n  hello\nworld".into())));
        history.record(copied(Content::Files(vec![
            r"C:\a\report.pdf".into(),
            r"C:\b\photos\".into(),
        ])));
        history.record(copied(Content::Text("https://example.com/x".into())));
        assert_eq!(
            titles(&history),
            [
                "https://example.com/x",
                "2 files: report.pdf, photos",
                "hello"
            ]
        );

        let list = history.history();
        let link = &list.items[0];
        assert_eq!(link.actions[0].title, "Paste");
        assert!(
            link.actions
                .iter()
                .any(|action| action.title == "Open Link")
        );
        let preview = list.items[2].preview.as_ref().unwrap();
        assert_eq!(preview.body, PreviewBody::Text("\n  hello\nworld".into()));
        assert!(
            preview
                .metadata
                .contains(&("Application".into(), "Notepad".into()))
        );
        assert!(preview.metadata.contains(&("Words".into(), "2".into())));
    }

    #[test]
    fn copying_again_moves_an_entry_up() {
        let history = history();
        history.record(copied(Content::Text("a".into())));
        history.record(copied(Content::Text("b".into())));
        history.record(copied(Content::Text("a".into())));
        assert_eq!(titles(&history), ["a", "b"]);
    }

    #[test]
    fn nothing_is_kept_while_turned_off() {
        let history = history();
        history.set_recording(false);
        history.record(copied(Content::Text("secret".into())));
        assert!(titles(&history).is_empty());
        assert!(history.history().empty.unwrap().contains("turned off"));
    }

    #[test]
    fn retention_caps_the_history() {
        let history = history();
        for ix in 0..5 {
            history.record(copied(Content::Text(ix.to_string())));
        }
        history.set_retention(Retention { days: None, max: 2 });
        assert_eq!(titles(&history).len(), 2);
    }

    #[test]
    fn images_need_a_folder() {
        let history = history();
        history.record(copied(Content::Image(Image {
            png: vec![1, 2, 3],
            width: 1,
            height: 1,
        })));
        assert!(titles(&history).is_empty());
    }

    #[test]
    fn images_are_saved_with_a_preview_copy() {
        let dir = std::env::temp_dir().join(format!("spotlight-clips-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let history =
            ClipboardHistory::new(Some(store.clone()), Some(dir.clone()), Retention::default());
        let big = image::RgbaImage::from_pixel(1000, 500, image::Rgba([9, 9, 9, 255]));
        let mut png = Vec::new();
        big.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        history.record(copied(Content::Image(Image {
            png,
            width: 1000,
            height: 500,
        })));

        let list = history.history();
        assert_eq!(list.items[0].title, "Image (1000×500)");
        let Some(PreviewBody::Image(preview)) = list.items[0].preview.as_ref().map(|p| &p.body)
        else {
            panic!("expected an image preview");
        };
        assert!(preview.ends_with(".thumb.png"));
        let (width, height) = image::image_dimensions(preview).unwrap();
        assert_eq!((width, height), (640, 320));

        let clip = store.lock().unwrap().clips(None).unwrap().remove(0);
        clip.remove_files();
        assert!(!fs::exists(&clip.content).unwrap());
        assert!(!fs::exists(preview).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hashes_are_stable() {
        assert_eq!(hash(b'T', b"hello"), hash(b'T', b"hello"));
        assert_ne!(hash(b'T', b"hello"), hash(b'F', b"hello"));
        assert_eq!(hash(b'T', b""), "af64094c86023903");
    }
}
