//! Extensions and the kinds of file they mean, for filters (Images,
//! Documents…) and for ranking files nobody opens by hand lower.

use std::collections::HashMap;

pub type ExtId = u16;

/// What Search Files filters by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Folder,
    Document,
    Image,
    Audio,
    Video,
    Archive,
    App,
    Other,
}

impl Kind {
    /// The kind for a `kind:` query word, English or Russian.
    pub fn parse(word: &str) -> Option<Kind> {
        Some(match word.to_lowercase().as_str() {
            "folder" | "folders" | "dir" | "папка" | "папки" => Kind::Folder,
            "doc" | "docs" | "document" | "documents" | "документ" | "документы" => {
                Kind::Document
            }
            "image"
            | "images"
            | "img"
            | "picture"
            | "pictures"
            | "картинка"
            | "картинки"
            | "изображения" => Kind::Image,
            "audio" | "music" | "sound" | "аудио" | "музыка" => Kind::Audio,
            "video" | "videos" | "movie" | "видео" => Kind::Video,
            "archive" | "archives" | "zip" | "архив" | "архивы" => Kind::Archive,
            "app" | "apps" | "program" | "programs" | "приложение" | "приложения" | "программы" => {
                Kind::App
            }
            _ => return None,
        })
    }

    /// The kind of a file with this lowercase extension.
    pub fn of_ext(ext: &str) -> Kind {
        match ext {
            "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "xls" | "xlsx" | "ods"
            | "csv" | "ppt" | "pptx" | "odp" | "epub" | "djvu" | "fb2" | "pages" | "numbers"
            | "key" | "tex" => Kind::Document,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff"
            | "heic" | "heif" | "avif" | "raw" | "cr2" | "nef" | "arw" | "psd" | "xcf" => {
                Kind::Image
            }
            "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "wma" | "opus" | "aiff" | "mid"
            | "midi" => Kind::Audio,
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "flv" | "m4v" | "mpg" | "mpeg"
            | "3gp" | "ts" => Kind::Video,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" | "iso" => {
                Kind::Archive
            }
            "exe" | "lnk" | "msi" | "msix" | "appx" | "appref-ms" | "bat" | "cmd" | "ps1"
            | "url" => Kind::App,
            _ => Kind::Other,
        }
    }

    /// Files that tools write and people rarely look for: ranked lower.
    pub fn is_noise(ext: &str) -> bool {
        matches!(
            ext,
            "tmp"
                | "temp"
                | "log"
                | "bak"
                | "old"
                | "dll"
                | "sys"
                | "pyc"
                | "pyo"
                | "o"
                | "obj"
                | "pdb"
                | "lib"
                | "a"
                | "rlib"
                | "rmeta"
                | "d"
                | "lock"
                | "cache"
                | "dat"
                | "etl"
                | "mui"
                | "manifest"
                | "cat"
                | "db-wal"
                | "db-shm"
                | "ldb"
                | "idx"
                | "pack"
                | "map"
        )
    }
}

/// Interned lowercase extensions. Id 0 means none.
pub(crate) struct Exts {
    list: Vec<Box<str>>,
    kinds: Vec<Kind>,
    noise: Vec<bool>,
    ids: HashMap<Box<str>, ExtId>,
}

impl Default for Exts {
    fn default() -> Self {
        Self {
            list: vec!["".into()],
            kinds: vec![Kind::Other],
            noise: vec![false],
            ids: HashMap::new(),
        }
    }
}

impl Exts {
    /// The id of `name`'s extension, adding it if new. Ids run out after
    /// 65,535 distinct extensions; later ones are stored as none.
    pub fn intern(&mut self, name: &str) -> ExtId {
        let Some(ext) = extension(name) else {
            return 0;
        };
        let ext = ext.to_lowercase();
        if let Some(id) = self.ids.get(ext.as_str()) {
            return *id;
        }
        let Ok(id) = ExtId::try_from(self.list.len()) else {
            return 0;
        };
        self.kinds.push(Kind::of_ext(&ext));
        self.noise.push(Kind::is_noise(&ext));
        self.list.push(ext.as_str().into());
        self.ids.insert(ext.into(), id);
        id
    }

    pub fn find(&self, ext: &str) -> Option<ExtId> {
        self.ids.get(ext.to_lowercase().as_str()).copied()
    }

    pub fn get(&self, id: ExtId) -> &str {
        &self.list[id as usize]
    }

    pub fn kind(&self, id: ExtId) -> Kind {
        self.kinds[id as usize]
    }

    pub fn is_noise(&self, id: ExtId) -> bool {
        self.noise[id as usize]
    }

    pub fn memory_bytes(&self) -> usize {
        self.list.iter().map(|ext| ext.len() * 2 + 40).sum()
    }
}

/// The part after the last dot, if there is a name before it: `.gitignore`
/// has none, `archive.tar.gz` has `gz`.
pub(crate) fn extension(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 16 && !ext.contains(' ')).then_some(ext)
}

/// The name without its extension.
pub(crate) fn stem(name: &str) -> &str {
    match extension(name) {
        Some(ext) => &name[..name.len() - ext.len() - 1],
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_and_stems() {
        assert_eq!(extension("Report.PDF"), Some("PDF"));
        assert_eq!(extension(".gitignore"), None);
        assert_eq!(extension("archive.tar.gz"), Some("gz"));
        assert_eq!(extension("Version 2. Final"), None);
        assert_eq!(stem("plan.md"), "plan");
        assert_eq!(stem(".env"), ".env");
    }

    #[test]
    fn interns_lowercase() {
        let mut exts = Exts::default();
        let pdf = exts.intern("a.PDF");
        assert_eq!(exts.intern("b.pdf"), pdf);
        assert_eq!(exts.get(pdf), "pdf");
        assert_eq!(exts.kind(pdf), Kind::Document);
        assert_eq!(exts.intern("README"), 0);
        assert_eq!(exts.find("Pdf"), Some(pdf));
    }
}
