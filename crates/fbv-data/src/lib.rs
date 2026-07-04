//! Library database (SQLite + FTS5) and the parsed-board blob cache.
//!
//! Two stores by design:
//! * `library.db` — everything searchable and user-owned (board metadata,
//!   parts, nets, tags, conditions, quarantine). Survives cache deletion.
//! * `cache/{sha256}.fbb` — the parsed `BoardModel`, serialized; disposable,
//!   rebuilt by re-parsing. Gives the <300 ms open-from-search.
//!
//! Concurrency model: WAL. The indexer owns one writer connection on its own
//! thread; the UI owns reader connections. SQLite serializes the rest.

pub mod cache;

use anyhow::{Context, Result};
use fbv_core::{netclass, BoardModel};
use rusqlite::{params, Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA_VERSION: i64 = 2;

/// Index schema generation: bump to force re-index of already-imported
/// boards after extraction logic changes.
pub const INDEX_VERSION: i64 = 1;

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct BoardRow {
    pub board_id: i64,
    pub sha256: String,
    pub display_name: String,
    pub model: Option<String>,
    pub oem_code: Option<String>,
    pub format: String,
    pub part_count: i64,
    pub net_count: i64,
    pub pin_count: i64,
    pub condition: String,
    pub favorite: bool,
    pub path: Option<String>,
    pub missing: bool,
}

#[derive(Debug, Clone)]
pub struct QuarantineRow {
    pub path: String,
    pub reason: String,
}

pub struct NewBoard<'a> {
    pub sha256: &'a str,
    pub display_name: &'a str,
    pub oem_code: Option<&'a str>,
    pub format: &'a str,
    pub model: &'a BoardModel,
    pub path: &'a Path,
    pub size: i64,
    pub mtime: i64,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening library db at {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        if version == 1 {
            // v2: quarantine entries remember which parser generation failed
            // on them, so parser upgrades automatically retry old failures.
            self.conn.execute_batch(
                "ALTER TABLE quarantine ADD COLUMN parser_gen INTEGER NOT NULL DEFAULT 0;",
            )?;
            self.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
            return Ok(());
        }
        self.conn.execute_batch(
            r#"
            BEGIN;
            CREATE TABLE IF NOT EXISTS boards(
                board_id      INTEGER PRIMARY KEY,
                sha256        TEXT NOT NULL UNIQUE,
                display_name  TEXT NOT NULL,
                model         TEXT,
                oem_code      TEXT,
                format        TEXT NOT NULL,
                part_count    INTEGER NOT NULL,
                net_count     INTEGER NOT NULL,
                pin_count     INTEGER NOT NULL,
                condition     TEXT NOT NULL DEFAULT 'unknown',
                favorite      INTEGER NOT NULL DEFAULT 0,
                imported_at   INTEGER NOT NULL,
                index_version INTEGER NOT NULL,
                notes         TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS board_files(
                file_id  INTEGER PRIMARY KEY,
                board_id INTEGER NOT NULL REFERENCES boards(board_id) ON DELETE CASCADE,
                abs_path TEXT NOT NULL UNIQUE,
                size     INTEGER NOT NULL,
                mtime    INTEGER NOT NULL,
                missing  INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS board_files_board ON board_files(board_id);
            CREATE TABLE IF NOT EXISTS parts(
                part_id     INTEGER PRIMARY KEY,
                board_id    INTEGER NOT NULL REFERENCES boards(board_id) ON DELETE CASCADE,
                part_idx    INTEGER NOT NULL,
                refdes      TEXT NOT NULL,
                side        TEXT NOT NULL,
                x           REAL NOT NULL,
                y           REAL NOT NULL,
                pin_count   INTEGER NOT NULL,
                value       TEXT,
                part_number TEXT,
                package     TEXT,
                harvested   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS parts_board ON parts(board_id);
            CREATE TABLE IF NOT EXISTS nets(
                net_id    INTEGER PRIMARY KEY,
                board_id  INTEGER NOT NULL REFERENCES boards(board_id) ON DELETE CASCADE,
                net_idx   INTEGER NOT NULL,
                name      TEXT NOT NULL,
                pin_count INTEGER NOT NULL,
                is_power  INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS nets_board ON nets(board_id);
            CREATE TABLE IF NOT EXISTS quarantine(
                path       TEXT PRIMARY KEY,
                reason     TEXT NOT NULL,
                size       INTEGER NOT NULL,
                mtime      INTEGER NOT NULL,
                seen_at    INTEGER NOT NULL,
                parser_gen INTEGER NOT NULL DEFAULT 0
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
                kind UNINDEXED,
                board_id UNINDEXED,
                entity_idx UNINDEXED,
                refdes, value, part_number, package, net, board_name, model,
                tokenize = "unicode61 tokenchars '_-.+'",
                prefix = '2 3 4'
            );
            COMMIT;
            "#,
        )?;
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// Returns the board id for a content hash, if already imported.
    pub fn board_id_for_hash(&self, sha256: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT board_id FROM boards WHERE sha256 = ?1")?;
        let id = stmt
            .query_row([sha256], |r| r.get::<_, i64>(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(id)
    }

    /// Registers (or refreshes) a path -> board mapping.
    pub fn upsert_file(&self, board_id: i64, path: &Path, size: i64, mtime: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO board_files(board_id, abs_path, size, mtime, missing)
             VALUES (?1, ?2, ?3, ?4, 0)
             ON CONFLICT(abs_path) DO UPDATE
             SET board_id = ?1, size = ?3, mtime = ?4, missing = 0",
            params![board_id, path.to_string_lossy(), size, mtime],
        )?;
        Ok(())
    }

    /// Inserts a fully parsed board with its parts, nets and search rows.
    /// One transaction per board keeps the UI reader unblocked.
    pub fn insert_board(&mut self, nb: &NewBoard) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO boards(sha256, display_name, model, oem_code, format,
                                part_count, net_count, pin_count, imported_at, index_version)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                nb.sha256,
                nb.display_name,
                nb.oem_code,
                nb.format,
                nb.model.parts.iter().filter(|p| !p.is_dummy).count() as i64,
                nb.model.nets.len() as i64,
                nb.model.pins.len() as i64,
                now(),
                INDEX_VERSION,
            ],
        )?;
        let board_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO board_files(board_id, abs_path, size, mtime, missing)
             VALUES (?1, ?2, ?3, ?4, 0)
             ON CONFLICT(abs_path) DO UPDATE
             SET board_id = ?1, size = ?3, mtime = ?4, missing = 0",
            params![board_id, nb.path.to_string_lossy(), nb.size, nb.mtime],
        )?;

        {
            let mut part_stmt = tx.prepare_cached(
                "INSERT INTO parts(board_id, part_idx, refdes, side, x, y, pin_count,
                                   value, part_number, package)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            let mut fts_stmt = tx.prepare_cached(
                "INSERT INTO search_fts(kind, board_id, entity_idx, refdes, value,
                                        part_number, package, net, board_name, model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for (idx, part) in nb.model.parts.iter().enumerate() {
                if part.is_dummy {
                    continue;
                }
                let center = nb
                    .model
                    .part_bounds(idx)
                    .map(|(min, max)| ((min.x + max.x) / 2.0, (min.y + max.y) / 2.0))
                    .unwrap_or((0.0, 0.0));
                part_stmt.execute(params![
                    board_id,
                    idx as i64,
                    part.refdes,
                    part.side.label(),
                    center.0 as f64,
                    center.1 as f64,
                    part.pins.len() as i64,
                    part.value,
                    part.mfg_code,
                    part.package,
                ])?;
                fts_stmt.execute(params![
                    "part",
                    board_id,
                    idx as i64,
                    part.refdes,
                    part.value.as_deref().unwrap_or(""),
                    part.mfg_code.as_deref().unwrap_or(""),
                    part.package.as_deref().unwrap_or(""),
                    "",
                    nb.display_name,
                    nb.oem_code.unwrap_or(""),
                ])?;
            }
            let mut net_stmt = tx.prepare_cached(
                "INSERT INTO nets(board_id, net_idx, name, pin_count, is_power)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (idx, net) in nb.model.nets.iter().enumerate() {
                net_stmt.execute(params![
                    board_id,
                    idx as i64,
                    net.name,
                    net.pins.len() as i64,
                    netclass::is_power_hint(&net.name) as i64,
                ])?;
                fts_stmt.execute(params![
                    "net",
                    board_id,
                    idx as i64,
                    "",
                    "",
                    "",
                    "",
                    net.name,
                    nb.display_name,
                    nb.oem_code.unwrap_or(""),
                ])?;
            }
        }
        tx.commit()?;
        Ok(board_id)
    }

    /// Removes a board and everything hanging off it.
    pub fn delete_board(&self, board_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM boards WHERE board_id = ?1", [board_id])?;
        self.conn
            .execute("DELETE FROM search_fts WHERE board_id = ?1", [board_id])?;
        Ok(())
    }

    pub fn list_boards(&self) -> Result<Vec<BoardRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT b.board_id, b.sha256, b.display_name, b.model, b.oem_code, b.format,
                    b.part_count, b.net_count, b.pin_count, b.condition, b.favorite,
                    (SELECT abs_path FROM board_files f
                      WHERE f.board_id = b.board_id ORDER BY f.missing, f.file_id LIMIT 1),
                    (SELECT MIN(missing) FROM board_files f WHERE f.board_id = b.board_id)
             FROM boards b
             ORDER BY b.favorite DESC, b.display_name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BoardRow {
                    board_id: r.get(0)?,
                    sha256: r.get(1)?,
                    display_name: r.get(2)?,
                    model: r.get(3)?,
                    oem_code: r.get(4)?,
                    format: r.get(5)?,
                    part_count: r.get(6)?,
                    net_count: r.get(7)?,
                    pin_count: r.get(8)?,
                    condition: r.get(9)?,
                    favorite: r.get::<_, i64>(10)? != 0,
                    path: r.get(11)?,
                    missing: r.get::<_, Option<i64>>(12)?.unwrap_or(1) != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn board_row(&self, board_id: i64) -> Result<Option<BoardRow>> {
        Ok(self
            .list_boards()?
            .into_iter()
            .find(|b| b.board_id == board_id))
    }

    pub fn set_condition(&self, board_id: i64, condition: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE boards SET condition = ?2 WHERE board_id = ?1",
            params![board_id, condition],
        )?;
        Ok(())
    }

    pub fn set_favorite(&self, board_id: i64, fav: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE boards SET favorite = ?2 WHERE board_id = ?1",
            params![board_id, fav as i64],
        )?;
        Ok(())
    }

    pub fn set_display_name(&self, board_id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE boards SET display_name = ?2 WHERE board_id = ?1",
            params![board_id, name],
        )?;
        self.conn.execute(
            "UPDATE search_fts SET board_name = ?2 WHERE board_id = ?1",
            params![board_id, name],
        )?;
        Ok(())
    }

    /// Sets the OEM code only when none is known yet (e.g. a duplicate file
    /// imported later under a better-named path).
    pub fn fill_missing_oem_code(&self, board_id: i64, oem_code: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE boards SET oem_code = ?2
             WHERE board_id = ?1 AND (oem_code IS NULL OR oem_code = '')",
            params![board_id, oem_code],
        )?;
        if changed > 0 {
            self.conn.execute(
                "UPDATE search_fts SET model = ?2 WHERE board_id = ?1",
                params![board_id, oem_code],
            )?;
        }
        Ok(())
    }

    pub fn set_harvested(&self, board_id: i64, part_idx: i64, harvested: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE parts SET harvested = ?3 WHERE board_id = ?1 AND part_idx = ?2",
            params![board_id, part_idx, harvested as i64],
        )?;
        Ok(())
    }

    pub fn harvested_parts(&self, board_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT part_idx FROM parts WHERE board_id = ?1 AND harvested = 1",
        )?;
        let rows = stmt
            .query_map([board_id], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn quarantine(
        &self,
        path: &Path,
        reason: &str,
        size: i64,
        mtime: i64,
        parser_gen: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO quarantine(path, reason, size, mtime, seen_at, parser_gen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE
             SET reason = ?2, size = ?3, mtime = ?4, seen_at = ?5, parser_gen = ?6",
            params![path.to_string_lossy(), reason, size, mtime, now(), parser_gen],
        )?;
        Ok(())
    }

    /// True when this exact file (path+size+mtime) already failed under the
    /// *current* parser generation — skip it instead of re-parsing on every
    /// scan. Files that failed under an older generation are retried.
    pub fn is_quarantined(
        &self,
        path: &Path,
        size: i64,
        mtime: i64,
        parser_gen: i64,
    ) -> Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT 1 FROM quarantine
             WHERE path = ?1 AND size = ?2 AND mtime = ?3 AND parser_gen = ?4",
        )?;
        Ok(stmt.exists(params![path.to_string_lossy(), size, mtime, parser_gen])?)
    }

    /// Drops a quarantine entry (the file imported successfully after all).
    pub fn unquarantine(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "DELETE FROM quarantine WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn list_quarantine(&self) -> Result<Vec<QuarantineRow>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, reason FROM quarantine ORDER BY seen_at DESC")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(QuarantineRow {
                    path: r.get(0)?,
                    reason: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Path of the best (non-missing) file for a board.
    pub fn board_path(&self, board_id: i64) -> Result<Option<PathBuf>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT abs_path FROM board_files
             WHERE board_id = ?1 ORDER BY missing, file_id LIMIT 1",
        )?;
        let p = stmt
            .query_row([board_id], |r| r.get::<_, String>(0))
            .map(|s| Some(PathBuf::from(s)))
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(p)
    }

    pub fn stats(&self) -> Result<(i64, i64, i64)> {
        let boards: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
        let parts: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM parts", [], |r| r.get(0))?;
        let nets: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nets", [], |r| r.get(0))?;
        Ok((boards, parts, nets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbv_core::{BoardBuilder, BoardFormat, Part, Pin, Point, Side};

    fn tiny_model() -> BoardModel {
        let mut b = BoardBuilder::new();
        let part = b.add_part(Part {
            refdes: "U5300".into(),
            mfg_code: Some("ISL9239".into()),
            value: Some("charger".into()),
            package: None,
            side: Side::Top,
            pins: vec![],
            is_dummy: false,
        });
        let net = b.net_id("PPBUS_G3H");
        b.add_pin(Pin {
            part,
            number: "1".into(),
            name: String::new(),
            pos: Point::new(10.0, 10.0),
            radius: 1.0,
            side: Side::Top,
            net,
            is_test_pad: false,
        });
        b.finish(BoardFormat::Brd)
    }

    #[test]
    fn fts5_is_available_and_roundtrips() {
        let mut db = Database::open_in_memory().unwrap();
        let model = tiny_model();
        let id = db
            .insert_board(&NewBoard {
                sha256: "abc123",
                display_name: "820-00281 MLB",
                oem_code: Some("820-00281"),
                format: "BRD (Test_Link)",
                model: &model,
                path: Path::new("/lib/820-00281.brd"),
                size: 1234,
                mtime: 111,
            })
            .unwrap();
        assert_eq!(db.board_id_for_hash("abc123").unwrap(), Some(id));

        // FTS search works, including prefix.
        let hits: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM search_fts WHERE search_fts MATCH 'ISL92*'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
        let hits: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM search_fts WHERE search_fts MATCH 'net:PPBUS_G3H'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        // Board metadata + condition/harvested flags.
        db.set_condition(id, "donor").unwrap();
        db.set_harvested(id, 0, true).unwrap();
        assert_eq!(db.harvested_parts(id).unwrap(), vec![0]);
        let rows = db.list_boards().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].condition, "donor");
        assert_eq!(rows[0].part_count, 1);

        // Delete cleans FTS too.
        db.delete_board(id).unwrap();
        let hits: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM search_fts WHERE search_fts MATCH 'ISL92*'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0);
    }

    #[test]
    fn quarantine_dedup_and_parser_generation_retry() {
        let db = Database::open_in_memory().unwrap();
        let p = Path::new("/lib/junk.brd");
        db.quarantine(p, "unrecognized", 10, 1, 1).unwrap();
        assert!(db.is_quarantined(p, 10, 1, 1).unwrap());
        assert!(!db.is_quarantined(p, 10, 2, 1).unwrap()); // changed file: retry
        // Newer parser generation: same file gets retried automatically.
        assert!(!db.is_quarantined(p, 10, 1, 2).unwrap());
        assert_eq!(db.list_quarantine().unwrap().len(), 1);
        db.unquarantine(p).unwrap();
        assert_eq!(db.list_quarantine().unwrap().len(), 0);
    }
}
