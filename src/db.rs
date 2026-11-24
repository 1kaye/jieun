use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS tracked_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE,
    added_at TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    message TEXT NOT NULL,
    is_auto_backup INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS revision_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    revision_id INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    prev_hash TEXT,
    new_hash TEXT NOT NULL,
    file_mode INTEGER NOT NULL,
    is_symlink INTEGER NOT NULL DEFAULT 0,
    symlink_target TEXT,
    FOREIGN KEY (revision_id) REFERENCES revisions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_revision_files_path ON revision_files(file_path);
CREATE INDEX IF NOT EXISTS idx_revision_files_rev ON revision_files(revision_id);
"#;

#[derive(Debug, Clone, Serialize)]
pub struct TrackedFile {
    pub id: i64,
    pub path: String,
    pub added_at: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub id: i64,
    pub timestamp: String,
    pub message: String,
    pub is_auto_backup: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevisionFile {
    pub id: i64,
    pub revision_id: i64,
    pub file_path: String,
    pub prev_hash: Option<String>,
    pub new_hash: String,
    pub file_mode: u32,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewRevisionFile {
    pub file_path: String,
    pub prev_hash: Option<String>,
    pub new_hash: String,
    pub file_mode: u32,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn init(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open SQLite database at {}", db_path.display()))?;
        configure_and_migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn open(db_path: &Path) -> Result<Self> {
        if !db_path.exists() {
            bail!(
                "jieun is not initialized (missing {}). Run 'jieun init' first.",
                db_path.display()
            );
        }
        Self::init(db_path)
    }

    pub fn get_tracked_file(&self, path: &str) -> Result<Option<TrackedFile>> {
        self.conn
            .query_row(
                "SELECT id, path, added_at, active FROM tracked_files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(TrackedFile {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        added_at: row.get(2)?,
                        active: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .context("failed to query tracked_files")
    }

    pub fn upsert_tracked_file(&self, path: &str, added_at: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO tracked_files (path, added_at, active)
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(path) DO UPDATE SET active = 1",
                params![path, added_at],
            )
            .context("failed to register tracked file")?;
        Ok(())
    }

    pub fn list_active_tracked_files(&self) -> Result<Vec<TrackedFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, added_at, active
             FROM tracked_files
             WHERE active = 1
             ORDER BY path ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TrackedFile {
                id: row.get(0)?,
                path: row.get(1)?,
                added_at: row.get(2)?,
                active: row.get::<_, i64>(3)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list tracked files")
    }

    pub fn set_tracked_inactive(&self, path: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "UPDATE tracked_files SET active = 0 WHERE path = ?1 AND active = 1",
                params![path],
            )
            .context("failed to untrack file")?;
        Ok(changed > 0)
    }

    pub fn latest_revision_file(&self, file_path: &str) -> Result<Option<RevisionFile>> {
        self.conn
            .query_row(
                "SELECT id, revision_id, file_path, prev_hash, new_hash, file_mode, is_symlink, symlink_target
                 FROM revision_files
                 WHERE file_path = ?1
                 ORDER BY revision_id DESC, id DESC
                 LIMIT 1",
                params![file_path],
                row_to_revision_file,
            )
            .optional()
            .context("failed to query latest file revision")
    }

    /// Find the metadata (mode, symlink info) associated with a file's prior state before `before_rev_id`.
    pub fn previous_file_state(
        &self,
        file_path: &str,
        before_rev_id: i64,
        expected_hash: &str,
    ) -> Result<Option<RevisionFile>> {
        let exact = self
            .conn
            .query_row(
                "SELECT id, revision_id, file_path, prev_hash, new_hash, file_mode, is_symlink, symlink_target
                 FROM revision_files
                 WHERE file_path = ?1 AND revision_id < ?2 AND new_hash = ?3
                 ORDER BY revision_id DESC, id DESC
                 LIMIT 1",
                params![file_path, before_rev_id, expected_hash],
                row_to_revision_file,
            )
            .optional()
            .context("failed to query prior file state")?;

        if exact.is_some() {
            return Ok(exact);
        }

        self.conn
            .query_row(
                "SELECT id, revision_id, file_path, prev_hash, new_hash, file_mode, is_symlink, symlink_target
                 FROM revision_files
                 WHERE file_path = ?1 AND revision_id < ?2
                 ORDER BY revision_id DESC, id DESC
                 LIMIT 1",
                params![file_path, before_rev_id],
                row_to_revision_file,
            )
            .optional()
            .context("failed to query fallback prior file state")
    }

    pub fn create_revision(
        &mut self,
        timestamp: &str,
        message: &str,
        is_auto_backup: bool,
        track_paths: &[&str],
        files: &[NewRevisionFile],
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;

        for path in track_paths {
            tx.execute(
                "INSERT INTO tracked_files (path, added_at, active)
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(path) DO UPDATE SET active = 1",
                params![path, timestamp],
            )?;
        }

        tx.execute(
            "INSERT INTO revisions (timestamp, message, is_auto_backup) VALUES (?1, ?2, ?3)",
            params![timestamp, message, i64::from(is_auto_backup)],
        )?;
        let rev_id = tx.last_insert_rowid();

        {
            let mut stmt = tx.prepare(
                "INSERT INTO revision_files
                 (revision_id, file_path, prev_hash, new_hash, file_mode, is_symlink, symlink_target)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for file in files {
                stmt.execute(params![
                    rev_id,
                    file.file_path,
                    file.prev_hash,
                    file.new_hash,
                    i64::from(file.file_mode),
                    i64::from(file.is_symlink),
                    file.symlink_target,
                ])?;
            }
        }

        tx.commit().context("failed to commit revision transaction")?;
        Ok(rev_id)
    }

    pub fn get_revision(&self, rev_id: i64) -> Result<Option<Revision>> {
        self.conn
            .query_row(
                "SELECT id, timestamp, message, is_auto_backup FROM revisions WHERE id = ?1",
                params![rev_id],
                row_to_revision,
            )
            .optional()
            .context("failed to query revision")
    }

    pub fn get_revision_files(&self, rev_id: i64) -> Result<Vec<RevisionFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, revision_id, file_path, prev_hash, new_hash, file_mode, is_symlink, symlink_target
             FROM revision_files
             WHERE revision_id = ?1
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![rev_id], row_to_revision_file)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to query revision files")
    }

    pub fn get_revision_file(&self, rev_id: i64, file_path: &str) -> Result<Option<RevisionFile>> {
        self.conn
            .query_row(
                "SELECT id, revision_id, file_path, prev_hash, new_hash, file_mode, is_symlink, symlink_target
                 FROM revision_files
                 WHERE revision_id = ?1 AND file_path = ?2
                 LIMIT 1",
                params![rev_id, file_path],
                row_to_revision_file,
            )
            .optional()
            .context("failed to query file in revision")
    }

    pub fn list_revisions(
        &self,
        path_filter: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<(Revision, Vec<RevisionFile>)>> {
        let limit_val = limit.map(|l| l as i64).unwrap_or(-1);

        let revisions = match path_filter {
            Some(path) => {
                let mut stmt = self.conn.prepare(
                    "SELECT DISTINCT r.id, r.timestamp, r.message, r.is_auto_backup
                     FROM revisions r
                     INNER JOIN revision_files rf ON rf.revision_id = r.id
                     WHERE rf.file_path = ?1
                     ORDER BY r.id DESC
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![path, limit_val], row_to_revision)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, message, is_auto_backup
                     FROM revisions
                     ORDER BY id DESC
                     LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit_val], row_to_revision)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };

        let mut out = Vec::with_capacity(revisions.len());
        for rev in revisions {
            let files = self.get_revision_files(rev.id)?;
            out.push((rev, files));
        }
        Ok(out)
    }
}

fn configure_and_migrate(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable WAL mode")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable foreign_keys")?;
    conn.execute_batch(SCHEMA_SQL)
        .context("failed to initialize SQLite schema")?;
    Ok(())
}

fn row_to_revision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Revision> {
    Ok(Revision {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        message: row.get(2)?,
        is_auto_backup: row.get::<_, i64>(3)? != 0,
    })
}

fn row_to_revision_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<RevisionFile> {
    Ok(RevisionFile {
        id: row.get(0)?,
        revision_id: row.get(1)?,
        file_path: row.get(2)?,
        prev_hash: row.get(3)?,
        new_hash: row.get(4)?,
        file_mode: row.get::<_, i64>(5)? as u32,
        is_symlink: row.get::<_, i64>(6)? != 0,
        symlink_target: row.get(7)?,
    })
}
