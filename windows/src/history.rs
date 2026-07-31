// Transcription history — same location and schema as the macOS app:
// ~/.fastword/history.sqlite, table entries(id, text, created_at).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use uuid::Uuid;

pub struct HistoryStore {
    conn: Connection,
}

pub fn default_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fastword")
}

impl HistoryStore {
    pub fn open_default() -> Result<Self> {
        let dir = default_dir();
        std::fs::create_dir_all(&dir).context("creating ~/.fastword")?;
        Self::open(&dir.join("history.sqlite"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("opening history db")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries(
                id TEXT PRIMARY KEY,
                text TEXT NOT NULL,
                created_at REAL NOT NULL
            );",
        )
        .context("creating entries table")?;
        Ok(Self { conn })
    }

    pub fn insert(&self, text: &str) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.conn
            .execute(
                "INSERT INTO entries(id, text, created_at) VALUES(?1, ?2, ?3);",
                params![Uuid::new_v4().to_string().to_uppercase(), text, now],
            )
            .context("inserting history entry")?;
        Ok(())
    }

    /// Newest first, capped at 1000 like the macOS app. (No history UI on
    /// Windows yet — kept for tests and future use; the sqlite file itself
    /// is browsable with any client.)
    #[allow(dead_code)]
    pub fn load_all(&self) -> Result<Vec<(String, String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, created_at FROM entries ORDER BY created_at DESC LIMIT 1000;",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("fastword-win-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn insert_and_load_roundtrip() {
        let path = temp_db_path("roundtrip.sqlite");
        let store = HistoryStore::open(&path).unwrap();
        store.insert("привет мир").unwrap();
        store.insert("hello world").unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
        let texts: Vec<&str> = all.iter().map(|(_, t, _)| t.as_str()).collect();
        assert!(texts.contains(&"привет мир"));
        assert!(texts.contains(&"hello world"));
    }

    #[test]
    fn newest_entries_come_first() {
        let path = temp_db_path("order.sqlite");
        let store = HistoryStore::open(&path).unwrap();
        store.insert("first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        store.insert("second").unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all[0].1, "second");
        assert_eq!(all[1].1, "first");
    }

    #[test]
    fn reopen_persists_entries() {
        let path = temp_db_path("persist.sqlite");
        {
            let store = HistoryStore::open(&path).unwrap();
            store.insert("persisted").unwrap();
        }
        let store = HistoryStore::open(&path).unwrap();
        assert_eq!(store.load_all().unwrap().len(), 1);
    }
}
