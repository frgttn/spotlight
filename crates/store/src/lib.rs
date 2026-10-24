//! SQLite persistence: settings, frecency, aliases, item hotkeys, the
//! calculator history, quicklinks, the clipboard history and snippets.
//!
//! The database is SQLCipher: encrypted with a 256-bit key the caller keeps
//! (see `spotlight_platform_win::vault`). Without a key it is plain SQLite.
//!
//! Calls are blocking. They are short (one indexed row), but callers on the UI
//! thread should still move writes to a background task.

use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};
pub use rusqlite::{Error, ErrorCode};
use spotlight_core::{
    clip::{self, ClipKind, Retention},
    frecency::Frecency,
    quicklink::{self, Quicklink},
    snippet::Snippet,
};

pub type Result<T> = rusqlite::Result<T>;

/// A raw SQLCipher key: used as it is, with no passphrase stretching.
pub type Key = [u8; 32];

/// The first bytes of every plain SQLite file; SQLCipher files look random.
const PLAIN_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Schema changes, applied in order. `PRAGMA user_version` records how many
/// have run, so only append to this list; never edit an entry that shipped.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE frecency (
        item_id   TEXT PRIMARY KEY,
        score     REAL NOT NULL,
        last_used INTEGER NOT NULL,
        count     INTEGER NOT NULL
    );",
    "CREATE TABLE aliases (
        item_id TEXT PRIMARY KEY,
        alias   TEXT NOT NULL
    );
    CREATE TABLE item_hotkeys (
        item_id TEXT PRIMARY KEY,
        hotkey  TEXT NOT NULL
    );",
    "CREATE TABLE calculations (
        id         INTEGER PRIMARY KEY,
        expression TEXT NOT NULL,
        answer     TEXT NOT NULL,
        used_at    INTEGER NOT NULL
    );
    CREATE INDEX calculations_used_at ON calculations (used_at);",
    // Seeded with `quicklink::DEFAULTS` when this runs; see `init`.
    "CREATE TABLE quicklinks (
        id        INTEGER PRIMARY KEY,
        name      TEXT NOT NULL,
        link      TEXT NOT NULL,
        keyword   TEXT,
        open_with TEXT
    );",
    // `content`: the text, the image file's path, or file paths one per line.
    // `hash` identifies the content, so copying it again moves the entry up.
    "CREATE TABLE clips (
        id        INTEGER PRIMARY KEY,
        kind      TEXT NOT NULL,
        content   TEXT NOT NULL,
        hash      TEXT NOT NULL UNIQUE,
        width     INTEGER,
        height    INTEGER,
        app       TEXT,
        app_path  TEXT,
        pinned    INTEGER NOT NULL DEFAULT 0,
        copied_at INTEGER NOT NULL
    );
    CREATE INDEX clips_copied_at ON clips (copied_at);",
    // One snippet per keyword; any number without one.
    "CREATE TABLE snippets (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL,
        text       TEXT NOT NULL,
        keyword    TEXT UNIQUE,
        created_at INTEGER NOT NULL
    );",
    // No schema change: swaps the first nine default quicklinks for the
    // current `quicklink::DEFAULTS`; see `refresh_default_quicklinks`.
    "SELECT 1;",
    // Icons already extracted from the shell, so a start doesn't extract them
    // all again. `name` is what the shell was asked for, `size` the pixel size.
    "CREATE TABLE icons (
        name     TEXT NOT NULL,
        size     INTEGER NOT NULL,
        width    INTEGER NOT NULL,
        height   INTEGER NOT NULL,
        bgra     BLOB NOT NULL,
        saved_at INTEGER NOT NULL,
        PRIMARY KEY (name, size)
    );",
];

/// An icon nobody asked for in this long is forgotten (about two months).
const ICON_KEEP_SECS: i64 = 60 * 24 * 3600;

/// A shell icon as saved: BGRA pixels with straight alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct CachedIcon {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    /// Unix seconds it was extracted.
    pub saved_at: i64,
}

/// The migration that creates `quicklinks`: databases created before it get
/// the default quicklinks once too, and deleting them later sticks.
const QUICKLINKS_MIGRATION: usize = 3;

/// The migration that replaces [`LEGACY_DEFAULTS`] with the current defaults.
const QUICKLINKS_REFRESH_MIGRATION: usize = 6;

/// The quicklinks the first databases were seeded with (name, keyword, link).
/// Removed, if still as they were saved, when the defaults were replaced.
const LEGACY_DEFAULTS: &[(&str, &str, &str)] = &[
    ("Search GitHub", "gh", "https://github.com/search?q={query}"),
    (
        "Search YouTube",
        "yt",
        "https://www.youtube.com/results?search_query={query}",
    ),
    (
        "Search Wikipedia",
        "w",
        "https://en.wikipedia.org/w/index.php?search={query}",
    ),
    (
        "Search Google Maps",
        "maps",
        "https://www.google.com/maps/search/{query}",
    ),
    (
        "Translate",
        "tr",
        "https://translate.google.com/?sl=auto&text={argument name=\"Text\"}&tl={argument name=\"To\" options=\"en,ru,uk,de,fr,es\"}",
    ),
    (
        "Search Stack Overflow",
        "so",
        "https://stackoverflow.com/search?q={query}",
    ),
    (
        "Search crates.io",
        "crates",
        "https://crates.io/search?q={query}",
    ),
    (
        "Search docs.rs",
        "docs",
        "https://docs.rs/releases/search?query={query}",
    ),
    (
        "Search MDN",
        "mdn",
        "https://developer.mozilla.org/en-US/search?q={query}",
    ),
];

/// Drops the legacy defaults the user hasn't changed, then adds every current
/// default whose keyword or name and link isn't already taken (so the user's
/// own quicklinks, and edited old ones, are left alone).
fn refresh_default_quicklinks(tx: &rusqlite::Transaction) -> Result<()> {
    for (name, keyword, link) in LEGACY_DEFAULTS {
        tx.execute(
            "DELETE FROM quicklinks
             WHERE name = ?1 AND link = ?2 AND keyword = ?3 AND open_with IS NULL",
            params![name, link, keyword],
        )?;
    }
    for (name, keyword, link) in quicklink::DEFAULTS {
        tx.execute(
            "INSERT INTO quicklinks (name, link, keyword)
             SELECT ?1, ?2, ?3
             WHERE NOT EXISTS (
                 SELECT 1 FROM quicklinks WHERE keyword = ?3 OR (name = ?1 AND link = ?2))",
            params![name, link, keyword],
        )?;
    }
    Ok(())
}

/// How many calculations the history keeps; older ones go.
const CALCULATIONS_KEPT: i64 = 200;

/// A calculation the user used, for the calculator history.
#[derive(Clone, Debug, PartialEq)]
pub struct Calculation {
    pub id: i64,
    pub expression: String,
    pub answer: String,
    /// Unix seconds.
    pub used_at: i64,
}

/// A clipboard history entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// 0 for one not saved yet.
    pub id: i64,
    pub kind: ClipKind,
    /// The text, the image file's path, or file paths one per line.
    pub content: String,
    /// Image size in pixels.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The app it was copied from: its name, and its program's path.
    pub app: Option<String>,
    pub app_path: Option<String>,
    pub pinned: bool,
    /// Unix seconds, of the latest copy.
    pub copied_at: i64,
}

impl Clip {
    /// Deletes the entry's image files, if it has any. For after it left
    /// the database.
    pub fn remove_files(&self) {
        if self.kind == ClipKind::Image {
            let _ = fs::remove_file(&self.content);
            let _ = fs::remove_file(clip::thumbnail_path(&self.content));
        }
    }
}

const CLIP_COLUMNS: &str = "id, kind, content, width, height, app, app_path, pinned, copied_at";

fn clip_row(row: &rusqlite::Row) -> Result<Clip> {
    let kind: String = row.get(1)?;
    Ok(Clip {
        id: row.get(0)?,
        kind: ClipKind::parse(&kind).unwrap_or(ClipKind::Text),
        content: row.get(2)?,
        width: row.get(3)?,
        height: row.get(4)?,
        app: row.get(5)?,
        app_path: row.get(6)?,
        pinned: row.get(7)?,
        copied_at: row.get(8)?,
    })
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, including its parent
    /// directory, and brings the schema up to date. With a `key` the database
    /// is encrypted, and a plain one from before is encrypted first; a wrong
    /// key fails with `SQLITE_NOTADB`.
    pub fn open(path: &Path, key: Option<&Key>) -> Result<Self> {
        if let Some(dir) = path.parent() {
            // A missing directory surfaces as SQLITE_CANTOPEN from `open` below.
            let _ = fs::create_dir_all(dir);
        }
        if let Some(key) = key
            && is_plain(path)
        {
            encrypt(path, key)?;
        }
        let conn = Connection::open(path)?;
        if let Some(key) = key {
            conn.pragma_update(None, "key", key_value(key))?;
        }
        // The first read of the file: with the wrong key, this is what fails.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        let applied: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let applied = applied.max(0) as usize;
        if applied < MIGRATIONS.len() {
            let tx = conn.transaction()?;
            for (ix, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
                tx.execute_batch(migration)?;
                if ix == QUICKLINKS_MIGRATION {
                    for (name, keyword, link) in quicklink::DEFAULTS {
                        tx.execute(
                            "INSERT INTO quicklinks (name, link, keyword) VALUES (?1, ?2, ?3)",
                            params![name, link, keyword],
                        )?;
                    }
                }
                if ix == QUICKLINKS_REFRESH_MIGRATION {
                    refresh_default_quicklinks(&tx)?;
                }
            }
            tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// The saved icons among `names` at pixel size `size`.
    pub fn icons(&self, names: &[String], size: u32) -> Result<Vec<CachedIcon>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT width, height, bgra, saved_at FROM icons WHERE name = ?1 AND size = ?2",
        )?;
        let mut found = Vec::new();
        for name in names {
            let icon = stmt
                .query_row(params![name, size], |row| {
                    Ok(CachedIcon {
                        name: name.clone(),
                        width: row.get(0)?,
                        height: row.get(1)?,
                        bgra: row.get(2)?,
                        saved_at: row.get(3)?,
                    })
                })
                .optional()?;
            found.extend(icon);
        }
        Ok(found)
    }

    /// Saves `icons` at pixel size `size`, replacing earlier ones, and drops
    /// icons nobody has asked for since `ICON_KEEP_SECS` before `now`.
    pub fn save_icons(&mut self, icons: &[CachedIcon], size: u32, now: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        for icon in icons {
            tx.execute(
                "INSERT OR REPLACE INTO icons (name, size, width, height, bgra, saved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![icon.name, size, icon.width, icon.height, icon.bgra, now],
            )?;
        }
        tx.execute(
            "DELETE FROM icons WHERE saved_at < ?1",
            [now - ICON_KEEP_SECS],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Every item that was ever used, keyed by item id.
    pub fn frecency(&self) -> Result<HashMap<String, Frecency>> {
        let mut stmt = self
            .conn
            .prepare("SELECT item_id, score, last_used, count FROM frecency")?;
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                Frecency {
                    score: row.get(1)?,
                    last_used: row.get(2)?,
                    count: row.get(3)?,
                },
            ))
        })?
        .collect()
    }

    /// Records one use of `item_id` at `now` (unix seconds) and returns the new state.
    pub fn record_visit(&mut self, item_id: &str, now: i64) -> Result<Frecency> {
        let tx = self.conn.transaction()?;
        let previous = tx
            .query_row(
                "SELECT score, last_used, count FROM frecency WHERE item_id = ?1",
                [item_id],
                |row| {
                    Ok(Frecency {
                        score: row.get(0)?,
                        last_used: row.get(1)?,
                        count: row.get(2)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default();
        let next = previous.visit(now);
        tx.execute(
            "INSERT INTO frecency (item_id, score, last_used, count) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (item_id) DO UPDATE SET
                score = excluded.score, last_used = excluded.last_used, count = excluded.count",
            params![item_id, next.score, next.last_used, next.count],
        )?;
        tx.commit()?;
        Ok(next)
    }

    /// Item id → alias.
    pub fn aliases(&self) -> Result<HashMap<String, String>> {
        self.pairs("SELECT item_id, alias FROM aliases")
    }

    /// Sets or (with `None`) removes the alias of `item_id`.
    pub fn set_alias(&self, item_id: &str, alias: Option<&str>) -> Result<()> {
        self.set_pair("aliases", "alias", item_id, alias)
    }

    /// Item id → hotkey, in `Hotkey`'s display form.
    pub fn item_hotkeys(&self) -> Result<HashMap<String, String>> {
        self.pairs("SELECT item_id, hotkey FROM item_hotkeys")
    }

    /// Sets or (with `None`) removes the hotkey of `item_id`.
    pub fn set_item_hotkey(&self, item_id: &str, hotkey: Option<&str>) -> Result<()> {
        self.set_pair("item_hotkeys", "hotkey", item_id, hotkey)
    }

    fn pairs(&self, sql: &str) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare(sql)?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }

    /// `table` and `column` are constants from this file, never user input.
    fn set_pair(
        &self,
        table: &str,
        column: &str,
        item_id: &str,
        value: Option<&str>,
    ) -> Result<()> {
        match value {
            Some(value) => self.conn.execute(
                &format!(
                    "INSERT INTO {table} (item_id, {column}) VALUES (?1, ?2)
                     ON CONFLICT (item_id) DO UPDATE SET {column} = excluded.{column}"
                ),
                params![item_id, value],
            )?,
            None => self.conn.execute(
                &format!("DELETE FROM {table} WHERE item_id = ?1"),
                [item_id],
            )?,
        };
        Ok(())
    }

    /// Adds a calculation to the history, or moves it to the top if the same
    /// expression is already there, and trims the history to its limit.
    pub fn add_calculation(&mut self, expression: &str, answer: &str, now: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM calculations WHERE expression = ?1",
            [expression],
        )?;
        tx.execute(
            "INSERT INTO calculations (expression, answer, used_at) VALUES (?1, ?2, ?3)",
            params![expression, answer, now],
        )?;
        tx.execute(
            "DELETE FROM calculations WHERE id NOT IN
                (SELECT id FROM calculations ORDER BY used_at DESC, id DESC LIMIT ?1)",
            [CALCULATIONS_KEPT],
        )?;
        tx.commit()
    }

    /// The calculator history, most recent first.
    pub fn calculations(&self) -> Result<Vec<Calculation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, expression, answer, used_at FROM calculations
             ORDER BY used_at DESC, id DESC",
        )?;
        stmt.query_map([], |row| {
            Ok(Calculation {
                id: row.get(0)?,
                expression: row.get(1)?,
                answer: row.get(2)?,
                used_at: row.get(3)?,
            })
        })?
        .collect()
    }

    pub fn delete_calculation(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM calculations WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn clear_calculations(&self) -> Result<()> {
        self.conn.execute("DELETE FROM calculations", [])?;
        Ok(())
    }

    /// Every quicklink, by name.
    pub fn quicklinks(&self) -> Result<Vec<Quicklink>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, link, keyword, open_with FROM quicklinks
             ORDER BY name COLLATE NOCASE, id",
        )?;
        stmt.query_map([], |row| {
            Ok(Quicklink {
                id: row.get(0)?,
                name: row.get(1)?,
                link: row.get(2)?,
                keyword: row.get(3)?,
                open_with: row.get(4)?,
            })
        })?
        .collect()
    }

    /// Adds `link` (id 0) or replaces the one with its id. Returns its id.
    pub fn save_quicklink(&self, link: &Quicklink) -> Result<i64> {
        let Quicklink {
            id,
            name,
            link,
            keyword,
            open_with,
        } = link;
        if *id == 0 {
            self.conn.execute(
                "INSERT INTO quicklinks (name, link, keyword, open_with) VALUES (?1, ?2, ?3, ?4)",
                params![name, link, keyword, open_with],
            )?;
            return Ok(self.conn.last_insert_rowid());
        }
        self.conn.execute(
            "UPDATE quicklinks SET name = ?2, link = ?3, keyword = ?4, open_with = ?5
             WHERE id = ?1",
            params![id, name, link, keyword, open_with],
        )?;
        Ok(*id)
    }

    pub fn delete_quicklink(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM quicklinks WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Every snippet, by name.
    pub fn snippets(&self) -> Result<Vec<Snippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, text, keyword FROM snippets
             ORDER BY name COLLATE NOCASE, id",
        )?;
        stmt.query_map([], |row| {
            Ok(Snippet {
                id: row.get(0)?,
                name: row.get(1)?,
                text: row.get(2)?,
                keyword: row.get(3)?,
            })
        })?
        .collect()
    }

    /// Adds `snippet` (id 0) or replaces the one with its id. Returns its id.
    /// Fails when another snippet has its keyword.
    pub fn save_snippet(&self, snippet: &Snippet, now: i64) -> Result<i64> {
        let Snippet {
            id,
            name,
            text,
            keyword,
        } = snippet;
        if *id == 0 {
            self.conn.execute(
                "INSERT INTO snippets (name, text, keyword, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![name, text, keyword, now],
            )?;
            return Ok(self.conn.last_insert_rowid());
        }
        self.conn.execute(
            "UPDATE snippets SET name = ?2, text = ?3, keyword = ?4 WHERE id = ?1",
            params![id, name, text, keyword],
        )?;
        Ok(*id)
    }

    /// Adds new snippets (their ids are ignored) all at once, or none if one
    /// fails. Returns how many were added.
    pub fn add_snippets(&mut self, snippets: &[Snippet], now: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        for snippet in snippets {
            tx.execute(
                "INSERT INTO snippets (name, text, keyword, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![snippet.name, snippet.text, snippet.keyword, now],
            )?;
        }
        tx.commit()?;
        Ok(snippets.len())
    }

    pub fn delete_snippet(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM snippets WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Adds `clip` to the clipboard history. When content with this `hash`
    /// is already there, that entry is replaced by a new one at the top
    /// (newer id, so it's first even within the same second), keeping its
    /// pin, and its app if `clip` has none (as when Spotlight itself put it
    /// back). Returns the new id and whether the content is new.
    pub fn add_clip(&mut self, clip: &Clip, hash: &str) -> Result<(i64, bool)> {
        let tx = self.conn.transaction()?;
        let existing = tx
            .query_row(
                "DELETE FROM clips WHERE hash = ?1 RETURNING pinned, app, app_path",
                [hash],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let (pinned, app, app_path) = match &existing {
            Some((pinned, app, app_path)) => (
                *pinned || clip.pinned,
                clip.app.as_ref().or(app.as_ref()),
                clip.app_path.as_ref().or(app_path.as_ref()),
            ),
            None => (clip.pinned, clip.app.as_ref(), clip.app_path.as_ref()),
        };
        tx.execute(
            "INSERT INTO clips
                (kind, content, hash, width, height, app, app_path, pinned, copied_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                clip.kind.as_str(),
                clip.content,
                hash,
                clip.width,
                clip.height,
                app,
                app_path,
                pinned,
                clip.copied_at
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok((id, existing.is_none()))
    }

    /// The clipboard history: pinned entries first, then the most recent.
    /// Unpinned entries copied before `cutoff` (unix seconds) are left out.
    pub fn clips(&self, cutoff: Option<i64>) -> Result<Vec<Clip>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLIP_COLUMNS} FROM clips WHERE pinned OR copied_at >= ?1
             ORDER BY pinned DESC, copied_at DESC, id DESC"
        ))?;
        stmt.query_map([cutoff.unwrap_or(i64::MIN)], clip_row)?
            .collect()
    }

    pub fn clip(&self, id: i64) -> Result<Option<Clip>> {
        self.conn
            .query_row(
                &format!("SELECT {CLIP_COLUMNS} FROM clips WHERE id = ?1"),
                [id],
                clip_row,
            )
            .optional()
    }

    pub fn pin_clip(&self, id: i64, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE clips SET pinned = ?2 WHERE id = ?1",
            params![id, pinned],
        )?;
        Ok(())
    }

    /// Deletes one entry and returns it, so its image file can go too.
    pub fn delete_clip(&self, id: i64) -> Result<Option<Clip>> {
        let clip = self.clip(id)?;
        self.conn.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        Ok(clip)
    }

    /// Deletes every unpinned entry and returns them.
    pub fn clear_clips(&mut self) -> Result<Vec<Clip>> {
        self.delete_clips_where("NOT pinned", [])
    }

    /// Deletes the unpinned entries `retention` no longer keeps at `now`
    /// (too old, or past the newest `max`) and returns them.
    pub fn trim_clips(&mut self, retention: Retention, now: i64) -> Result<Vec<Clip>> {
        let cutoff = retention.cutoff(now).unwrap_or(i64::MIN);
        self.delete_clips_where(
            "NOT pinned AND (copied_at < ?1 OR id NOT IN
                (SELECT id FROM clips WHERE NOT pinned
                 ORDER BY copied_at DESC, id DESC LIMIT ?2))",
            params![cutoff, retention.max],
        )
    }

    /// `condition` is a constant from this file, never user input.
    fn delete_clips_where(
        &mut self,
        condition: &str,
        params: impl rusqlite::Params + Clone,
    ) -> Result<Vec<Clip>> {
        let tx = self.conn.transaction()?;
        let removed = tx
            .prepare(&format!(
                "SELECT {CLIP_COLUMNS} FROM clips WHERE {condition}"
            ))?
            .query_map(params.clone(), clip_row)?
            .collect::<Result<Vec<_>>>()?;
        tx.execute(&format!("DELETE FROM clips WHERE {condition}"), params)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Carries the uses of `from` over to `to` (a file that was renamed),
    /// in place of any `to` had.
    pub fn rename_frecency(&self, from: &str, to: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE OR REPLACE frecency SET item_id = ?2 WHERE item_id = ?1",
            [from, to],
        )?;
        Ok(())
    }

    /// Forgets every use of `item_id`.
    pub fn forget(&self, item_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM frecency WHERE item_id = ?1", [item_id])?;
        Ok(())
    }
}

/// Whether `path` is a plain SQLite database (so not encrypted yet). An empty
/// or missing file isn't one: it becomes whatever it is opened as.
pub fn is_plain(path: &Path) -> bool {
    let mut header = [0u8; 16];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok_and(|()| &header == PLAIN_HEADER)
}

/// `x'…'`: tells SQLCipher the key is raw bytes, not a passphrase.
fn key_value(key: &Key) -> String {
    let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("x'{hex}'")
}

/// Replaces the plain database at `path` with an encrypted copy. The copy is
/// made next to it and moved over it only when complete, so a failure leaves
/// the plain database as it was.
fn encrypt(path: &Path, key: &Key) -> Result<()> {
    let copy = with_suffix(path, ".encrypting");
    let _ = fs::remove_file(&copy);
    {
        let conn = Connection::open(path)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        conn.execute(
            "ATTACH DATABASE ?1 AS encrypted KEY ?2",
            params![copy.to_string_lossy(), key_value(key)],
        )?;
        conn.query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))?;
        conn.pragma_update(Some("encrypted"), "user_version", version)?;
        conn.execute("DETACH DATABASE encrypted", [])?;
        // Closing checkpoints the write-ahead log into the plain file and
        // deletes it, so none of it is left to replay into the new one.
        conn.close().map_err(|(_, err)| err)?;
    }
    for leftover in ["-wal", "-shm"] {
        let _ = fs::remove_file(with_suffix(path, leftover));
    }
    fs::rename(&copy, path).map_err(|err| {
        let _ = fs::remove_file(&copy);
        io_error(err)
    })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn io_error(err: std::io::Error) -> Error {
    Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
        Some(err.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("hotkey").unwrap(), None);
        store.set_setting("hotkey", "alt+space").unwrap();
        store.set_setting("hotkey", "ctrl+space").unwrap();
        assert_eq!(
            store.setting("hotkey").unwrap().as_deref(),
            Some("ctrl+space")
        );
    }

    #[test]
    fn aliases_and_item_hotkeys_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store.set_alias("apps:code", Some("vs")).unwrap();
        store.set_alias("apps:code", Some("code")).unwrap();
        store.set_alias("system:lock", Some("l")).unwrap();
        store.set_alias("system:lock", None).unwrap();
        assert_eq!(
            store.aliases().unwrap(),
            HashMap::from([("apps:code".to_owned(), "code".to_owned())])
        );

        store
            .set_item_hotkey("system:lock", Some("Ctrl+Alt+L"))
            .unwrap();
        assert_eq!(store.item_hotkeys().unwrap()["system:lock"], "Ctrl+Alt+L");
        store.set_item_hotkey("system:lock", None).unwrap();
        assert!(store.item_hotkeys().unwrap().is_empty());
    }

    #[test]
    fn calculator_history() {
        let mut store = Store::open_in_memory().unwrap();
        store.add_calculation("1+1", "2", 10).unwrap();
        store.add_calculation("2*3", "6", 20).unwrap();
        // Used again: moves to the top instead of appearing twice.
        store.add_calculation("1+1", "2", 30).unwrap();
        let history = store.calculations().unwrap();
        let expressions: Vec<_> = history.iter().map(|c| c.expression.as_str()).collect();
        assert_eq!(expressions, ["1+1", "2*3"]);

        store.delete_calculation(history[1].id).unwrap();
        assert_eq!(store.calculations().unwrap().len(), 1);
        store.clear_calculations().unwrap();
        assert!(store.calculations().unwrap().is_empty());
    }

    #[test]
    fn calculator_history_is_capped() {
        let mut store = Store::open_in_memory().unwrap();
        for ix in 0..CALCULATIONS_KEPT + 5 {
            store
                .add_calculation(&format!("{ix}+0"), &ix.to_string(), ix)
                .unwrap();
        }
        let history = store.calculations().unwrap();
        assert_eq!(history.len() as i64, CALCULATIONS_KEPT);
        assert_eq!(
            history[0].expression,
            format!("{}+0", CALCULATIONS_KEPT + 4)
        );
    }

    #[test]
    fn old_databases_swap_their_stock_quicklinks_for_the_new_defaults() {
        // A database from before the defaults were replaced.
        let conn = Connection::open_in_memory().unwrap();
        for migration in &MIGRATIONS[..QUICKLINKS_REFRESH_MIGRATION] {
            conn.execute_batch(migration).unwrap();
        }
        for (name, keyword, link) in LEGACY_DEFAULTS {
            conn.execute(
                "INSERT INTO quicklinks (name, link, keyword) VALUES (?1, ?2, ?3)",
                params![name, link, keyword],
            )
            .unwrap();
        }
        // The user edited one stock link, and has their own using a new default's keyword.
        conn.execute(
            "UPDATE quicklinks SET name = 'My YouTube' WHERE keyword = 'yt'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO quicklinks (name, link, keyword) VALUES ('Mine', 'https://x.com/{query}', 'g')",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", QUICKLINKS_REFRESH_MIGRATION as i64)
            .unwrap();

        let store = Store::init(conn).unwrap();
        let links = store.quicklinks().unwrap();
        let names: Vec<_> = links.iter().map(|link| link.name.as_str()).collect();
        // Stock ones gone, replaced by the new GitHub; edited and own ones kept.
        assert!(!names.contains(&"Search GitHub"));
        assert!(names.contains(&"GitHub"));
        assert!(names.contains(&"My YouTube"));
        assert!(!names.contains(&"YouTube"), "its keyword is taken");
        assert!(names.contains(&"Mine"));
        assert!(!names.contains(&"Google"), "its keyword is taken");
        let keywords: Vec<_> = links.iter().filter_map(|l| l.keyword.as_deref()).collect();
        let mut unique = keywords.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keywords.len());
        // Fresh databases end up with exactly the defaults.
        let fresh = Store::open_in_memory().unwrap().quicklinks().unwrap();
        assert_eq!(fresh.len(), quicklink::DEFAULTS.len());
    }

    #[test]
    fn quicklinks_start_with_the_defaults_and_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let links = store.quicklinks().unwrap();
        assert_eq!(links.len(), quicklink::DEFAULTS.len());
        assert!(
            links
                .iter()
                .any(|link| link.keyword.as_deref() == Some("gh"))
        );

        let mut link = Quicklink {
            id: 0,
            name: "Aardvark".into(),
            link: r"C:\Projects".into(),
            keyword: None,
            open_with: Some(r"C:\Browser\browser.exe".into()),
        };
        link.id = store.save_quicklink(&link).unwrap();
        assert_eq!(store.quicklinks().unwrap()[0], link);

        link.name = "Projects".into();
        link.keyword = Some("p".into());
        assert_eq!(store.save_quicklink(&link).unwrap(), link.id);
        let saved = store.quicklinks().unwrap();
        assert!(saved.contains(&link));
        assert_eq!(saved.len(), quicklink::DEFAULTS.len() + 1);

        store.delete_quicklink(link.id).unwrap();
        assert!(!store.quicklinks().unwrap().contains(&link));
    }

    fn text_clip(text: &str, copied_at: i64) -> Clip {
        Clip {
            id: 0,
            kind: ClipKind::Text,
            content: text.into(),
            width: None,
            height: None,
            app: Some("Notepad".into()),
            app_path: None,
            pinned: false,
            copied_at,
        }
    }

    fn contents(clips: Vec<Clip>) -> Vec<String> {
        clips.into_iter().map(|clip| clip.content).collect()
    }

    #[test]
    fn snippets_round_trip_with_unique_keywords() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.snippets().unwrap().is_empty());

        let mut address = Snippet {
            id: 0,
            name: "address".into(),
            text: "Main St 1\nSpringfield".into(),
            keyword: Some(";addr".into()),
        };
        address.id = store.save_snippet(&address, 1).unwrap();
        let mut sign = Snippet {
            id: 0,
            name: "Signature".into(),
            text: "— {cursor}".into(),
            keyword: None,
        };
        sign.id = store.save_snippet(&sign, 2).unwrap();
        // By name, ignoring case: "S" sorts before "a" in plain byte order.
        assert_eq!(store.snippets().unwrap(), [address.clone(), sign.clone()]);

        // Keywords are unique; snippets without one aren't limited.
        let taken = Snippet {
            id: 0,
            keyword: Some(";addr".into()),
            ..address.clone()
        };
        assert!(store.save_snippet(&taken, 3).is_err());
        let blank = Snippet {
            id: 0,
            ..sign.clone()
        };
        store.save_snippet(&blank, 3).unwrap();

        sign.keyword = Some(";sig".into());
        assert_eq!(store.save_snippet(&sign, 4).unwrap(), sign.id);
        assert!(store.snippets().unwrap().contains(&sign));

        store.delete_snippet(address.id).unwrap();
        assert!(!store.snippets().unwrap().contains(&address));
        assert_eq!(store.snippets().unwrap().len(), 2);
    }

    #[test]
    fn adding_snippets_is_all_or_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let snippet = |keyword: &str| Snippet {
            id: 0,
            name: keyword.into(),
            text: "text".into(),
            keyword: Some(keyword.into()),
        };
        assert_eq!(
            store.add_snippets(&[snippet(";a"), snippet(";b")], 1),
            Ok(2)
        );
        // ";b" is taken, so ";c" isn't added either.
        assert!(
            store
                .add_snippets(&[snippet(";c"), snippet(";b")], 2)
                .is_err()
        );
        let keywords: Vec<_> = store
            .snippets()
            .unwrap()
            .into_iter()
            .filter_map(|snippet| snippet.keyword)
            .collect();
        assert_eq!(keywords, [";a", ";b"]);
    }

    #[test]
    fn clipboard_history_moves_repeats_up_and_pins_first() {
        let mut store = Store::open_in_memory().unwrap();
        let (a, new) = store.add_clip(&text_clip("a", 10), "ha").unwrap();
        assert!(new);
        store.add_clip(&text_clip("b", 20), "hb").unwrap();
        // Copied again, by Spotlight itself (no app): moves up, keeps its app.
        let again = Clip {
            app: None,
            ..text_clip("a", 30)
        };
        let (again_id, new) = store.add_clip(&again, "ha").unwrap();
        assert!(!new && again_id != a);
        assert_eq!(store.clip(a).unwrap(), None);
        assert_eq!(contents(store.clips(None).unwrap()), ["a", "b"]);
        let a_clip = store.clip(again_id).unwrap().unwrap();
        assert_eq!(a_clip.app.as_deref(), Some("Notepad"));
        assert_eq!(a_clip.copied_at, 30);

        let b = store.clips(None).unwrap()[1].id;
        store.pin_clip(b, true).unwrap();
        assert_eq!(contents(store.clips(None).unwrap()), ["b", "a"]);
        // Pinned entries are listed however old, and survive clearing.
        assert_eq!(contents(store.clips(Some(25)).unwrap()), ["b", "a"]);
        assert_eq!(contents(store.clips(Some(35)).unwrap()), ["b"]);
        assert_eq!(contents(store.clear_clips().unwrap()), ["a"]);
        assert_eq!(contents(store.clips(None).unwrap()), ["b"]);
        assert_eq!(store.delete_clip(b).unwrap().unwrap().content, "b");
        assert!(store.clips(None).unwrap().is_empty());
    }

    #[test]
    fn trimming_keeps_the_newest_and_drops_the_old() {
        let mut store = Store::open_in_memory().unwrap();
        for ix in 0..6 {
            let text = ix.to_string();
            store.add_clip(&text_clip(&text, ix * 100), &text).unwrap();
        }
        let oldest = store.clips(None).unwrap().last().unwrap().id;
        store.pin_clip(oldest, true).unwrap();
        let three = Retention { days: None, max: 3 };
        assert_eq!(contents(store.trim_clips(three, 1000).unwrap()), ["1", "2"]);
        assert_eq!(contents(store.clips(None).unwrap()), ["0", "5", "4", "3"]);

        // A one-day retention two days later drops everything unpinned.
        let day = Retention {
            days: Some(1),
            max: 100,
        };
        assert_eq!(store.trim_clips(day, 2 * 86_400).unwrap().len(), 3);
        assert_eq!(contents(store.clips(None).unwrap()), ["0"]);
    }

    #[test]
    fn forget_removes_one_item() {
        let mut store = Store::open_in_memory().unwrap();
        store.record_visit("apps:a", 1).unwrap();
        store.record_visit("apps:b", 1).unwrap();
        store.forget("apps:a").unwrap();
        store.forget("apps:never-used").unwrap();
        let left: Vec<_> = store.frecency().unwrap().into_keys().collect();
        assert_eq!(left, ["apps:b"]);
    }

    #[test]
    fn rename_carries_uses_over() {
        let mut store = Store::open_in_memory().unwrap();
        store.record_visit("files:a", 1).unwrap();
        store.record_visit("files:a", 2).unwrap();
        store.record_visit("files:b", 1).unwrap();
        store.rename_frecency("files:a", "files:b").unwrap();
        store
            .rename_frecency("files:never-used", "files:c")
            .unwrap();
        let all = store.frecency().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["files:b"].count, 2);
    }

    #[test]
    fn visits_accumulate_and_persist() {
        let dir = std::env::temp_dir().join(format!("spotlight-store-test-{}", std::process::id()));
        let path = dir.join("nested").join("test.db");
        let _ = fs::remove_dir_all(&dir);

        {
            let mut store = Store::open(&path, None).unwrap();
            store.record_visit("apps:calc", 100).unwrap();
            let second = store.record_visit("apps:calc", 100).unwrap();
            assert_eq!(second.count, 2);
            assert_eq!(second.score, 2.);
        }

        // Reopening runs no migrations twice and keeps the data.
        let store = Store::open(&path, None).unwrap();
        let all = store.frecency().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["apps:calc"], Frecency::default().visit(100).visit(100));
        assert_eq!(store.quicklinks().unwrap().len(), quicklink::DEFAULTS.len());
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }

    fn not_a_database(result: Result<Store>) -> bool {
        matches!(
            result,
            Err(Error::SqliteFailure(err, _)) if err.code == rusqlite::ErrorCode::NotADatabase
        )
    }

    #[test]
    fn a_plain_database_is_encrypted_with_its_data() {
        let dir =
            std::env::temp_dir().join(format!("spotlight-cipher-test-{}", std::process::id()));
        let path = dir.join("test.db");
        let _ = fs::remove_dir_all(&dir);
        let key = [42u8; 32];

        {
            let store = Store::open(&path, None).unwrap();
            store.set_setting("hotkey", "alt+space").unwrap();
        }
        assert!(is_plain(&path));

        {
            let store = Store::open(&path, Some(&key)).unwrap();
            assert_eq!(
                store.setting("hotkey").unwrap().as_deref(),
                Some("alt+space")
            );
            // The schema version came along, so no migration ran again.
            assert_eq!(store.quicklinks().unwrap().len(), quicklink::DEFAULTS.len());
            store.set_setting("theme", "dark").unwrap();
        }
        assert!(!is_plain(&path));
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes.windows(9).any(|w| w == b"alt+space"));
        assert!(!fs::exists(with_suffix(&path, ".encrypting")).unwrap());

        // Reopening with the key reads it; without it, or with another, fails.
        let store = Store::open(&path, Some(&key)).unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("dark"));
        drop(store);
        assert!(not_a_database(Store::open(&path, Some(&[1u8; 32]))));
        assert!(not_a_database(Store::open(&path, None)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_database_is_encrypted_from_the_start() {
        let dir = std::env::temp_dir().join(format!("spotlight-cipher-new-{}", std::process::id()));
        let path = dir.join("test.db");
        let _ = fs::remove_dir_all(&dir);
        let key = [9u8; 32];
        Store::open(&path, Some(&key))
            .unwrap()
            .set_setting("a", "b")
            .unwrap();
        assert!(fs::metadata(&path).unwrap().len() > 0);
        assert!(!is_plain(&path));
        let store = Store::open(&path, Some(&key)).unwrap();
        assert_eq!(store.setting("a").unwrap().as_deref(), Some("b"));
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn icons_are_kept_per_size_and_old_ones_dropped() {
        let mut store = Store::open_in_memory().unwrap();
        let icon = |name: &str| CachedIcon {
            name: name.into(),
            width: 1,
            height: 1,
            bgra: vec![1, 2, 3, 4],
            saved_at: 0,
        };
        store.save_icons(&[icon("a"), icon("b")], 48, 1_000).unwrap();
        let names = ["a".to_owned(), "c".to_owned()];
        let found = store.icons(&names, 48).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bgra, [1, 2, 3, 4]);
        assert_eq!(found[0].saved_at, 1_000);
        assert!(store.icons(&names, 55).unwrap().is_empty());

        store
            .save_icons(&[icon("c")], 48, 1_000 + ICON_KEEP_SECS + 1)
            .unwrap();
        assert_eq!(store.icons(&names, 48).unwrap().len(), 1);
        assert_eq!(store.icons(&names, 48).unwrap()[0].name, "c");
    }
}
