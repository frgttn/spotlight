//! SQLite persistence: settings and frecency.
//!
//! Calls are blocking. They are short (one indexed row), but callers on the UI
//! thread should still move writes to a background task.

use std::{collections::HashMap, fs, path::Path};

pub use rusqlite::Error;
use rusqlite::{Connection, OptionalExtension, params};
use spotlight_core::frecency::Frecency;

pub type Result<T> = rusqlite::Result<T>;

/// Schema changes, applied in order. `PRAGMA user_version` records how many
/// have run, so only append to this list; never edit an entry that shipped.
const MIGRATIONS: &[&str] = &["CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE frecency (
        item_id   TEXT PRIMARY KEY,
        score     REAL NOT NULL,
        last_used INTEGER NOT NULL,
        count     INTEGER NOT NULL
    );"];

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, including its parent
    /// directory, and brings the schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            // A missing directory surfaces as SQLITE_CANTOPEN from `open` below.
            let _ = fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
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
            for migration in &MIGRATIONS[applied..] {
                tx.execute_batch(migration)?;
            }
            tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
            tx.commit()?;
        }
        Ok(Self { conn })
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
    fn visits_accumulate_and_persist() {
        let dir = std::env::temp_dir().join(format!("spotlight-store-test-{}", std::process::id()));
        let path = dir.join("nested").join("test.db");
        let _ = fs::remove_dir_all(&dir);

        {
            let mut store = Store::open(&path).unwrap();
            store.record_visit("apps:calc", 100).unwrap();
            let second = store.record_visit("apps:calc", 100).unwrap();
            assert_eq!(second.count, 2);
            assert_eq!(second.score, 2.);
        }

        // Reopening runs no migrations twice and keeps the data.
        let store = Store::open(&path).unwrap();
        let all = store.frecency().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["apps:calc"], Frecency::default().visit(100).visit(100));
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }
}
