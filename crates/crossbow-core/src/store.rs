//! Local SQLite store: synced entities (with a dirty flag for the push queue),
//! live tabs, the archive, history and small key/value state.

use crossbow_proto::{Change, Encoding, Hlc, kinds};
use rusqlite::{Connection, OptionalExtension, params};
use std::{cell::Cell, collections::HashMap, path::{Path, PathBuf}};

pub type Result<T> = std::result::Result<T, rusqlite::Error>;

/// The library format this version writes, kept in `PRAGMA user_version`.
/// Raise it with each schema change, so a backup is made before the upgrade
/// and a backup from a newer version is never restored here.
pub const LIBRARY_FORMAT: i64 = 1;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
-- Deleted rows are overwritten with zeros, so what Clear History or a profile
-- delete removes leaves no copy in the file's free space.
PRAGMA secure_delete = ON;
CREATE TABLE IF NOT EXISTS entities (
  kind TEXT NOT NULL,
  id TEXT NOT NULL,
  hlc TEXT NOT NULL,
  deleted INTEGER NOT NULL DEFAULT 0,
  payload TEXT NOT NULL,
  dirty INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS entities_dirty ON entities(dirty) WHERE dirty = 1;
-- A rejected version stays dirty and keeps its payload. Editing it changes its
-- HLC and makes it eligible again; this is not a server acknowledgement.
CREATE TABLE IF NOT EXISTS sync_rejections (
  kind TEXT NOT NULL,
  id TEXT NOT NULL,
  hlc TEXT NOT NULL,
  reason TEXT NOT NULL,
  PRIMARY KEY (kind, id)
);
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tabs (
  id TEXT PRIMARY KEY,
  space_id TEXT NOT NULL,
  item_id TEXT,
  url TEXT NOT NULL,
  title TEXT NOT NULL,
  ord TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  last_active_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS archive (
  rowid INTEGER PRIMARY KEY AUTOINCREMENT,
  url TEXT NOT NULL,
  title TEXT NOT NULL,
  space_id TEXT NOT NULL,
  archived_ms INTEGER NOT NULL,
  profile_id TEXT,
  source TEXT NOT NULL DEFAULT 'tab'
);
CREATE TABLE IF NOT EXISTS history (
  url TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  visit_count INTEGER NOT NULL,
  last_visit_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS history_recent ON history(last_visit_ms DESC);
-- Legacy history has no provable profile owner. Keep it intact, but never
-- expose it through a profile's history/search or guess an owner during upgrade.
CREATE TABLE IF NOT EXISTS profile_history (
  profile_id TEXT NOT NULL,
  url TEXT NOT NULL,
  title TEXT NOT NULL,
  visit_count INTEGER NOT NULL,
  last_visit_ms INTEGER NOT NULL,
  PRIMARY KEY (profile_id, url)
);
CREATE INDEX IF NOT EXISTS profile_history_recent ON profile_history(profile_id, last_visit_ms DESC);
"#;

/// What each profile decided for each site, on this device only (see
/// site_permissions.rs). Not a sync entity, and never taken from a backup: a
/// restore keeps this Mac's rows, so it creates the table in an older backup.
pub(crate) const SITE_PERMISSIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS site_permissions (
  profile_id TEXT NOT NULL,
  origin TEXT NOT NULL,
  top_origin TEXT NOT NULL,
  permission TEXT NOT NULL,
  decision TEXT NOT NULL,
  updated_ms INTEGER NOT NULL,
  PRIMARY KEY (profile_id, origin, top_origin, permission)
);
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityRow {
    pub kind: String,
    pub id: String,
    pub hlc: Hlc,
    pub deleted: bool,
    pub payload: String,
}

/// An entity row's stored columns, verbatim: the clock as its stored text.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RawEntity {
    pub hlc: String,
    pub deleted: bool,
    pub payload: String,
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRow {
    pub id: String,
    pub space_id: String,
    pub item_id: Option<String>,
    pub url: String,
    pub title: String,
    pub order: String,
    pub created_ms: u64,
    pub last_active_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivedTab {
    pub rowid: i64,
    pub url: String,
    pub title: String,
    pub space_id: String,
    pub archived_ms: u64,
    pub profile_id: String,
    pub source: ArchiveSource,
}

/// Where an archived page was open. Rows from before sources were kept read as `Tab`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArchiveSource {
    #[default]
    Tab,
    LittleWindow,
}

impl ArchiveSource {
    fn encode(self) -> &'static str {
        match self {
            ArchiveSource::Tab => "tab",
            ArchiveSource::LittleWindow => "little_window",
        }
    }

    /// Unknown values, for example from a later release, read as a tab.
    fn decode(raw: &str) -> Self {
        if raw == "little_window" { ArchiveSource::LittleWindow } else { ArchiveSource::Tab }
    }
}

impl ArchivedTab {
    fn from_row(row: &rusqlite::Row<'_>) -> Result<Self> {
        Ok(Self {
            rowid: row.get(0)?, url: row.get(1)?, title: row.get(2)?, space_id: row.get(3)?,
            archived_ms: row.get::<_, i64>(4)? as u64, profile_id: row.get(5)?,
            source: ArchiveSource::decode(&row.get::<_, String>(6)?),
        })
    }
}

/// An outgoing change from `kind, id, hlc, deleted, payload` columns.
fn outgoing_change(r: &rusqlite::Row<'_>) -> Result<Change> {
    let hlc: String = r.get(2)?;
    Ok(Change {
        kind: r.get(0)?, id: r.get(1)?,
        hlc: Hlc::parse(&hlc).unwrap_or_else(|_| Hlc::new(0, 0, "x")),
        deleted: r.get::<_, i64>(3)? != 0, enc: Encoding::Json, payload: r.get(4)?,
    })
}

enum ArchiveSpace {
    Missing,
    Owned(String),
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visit_count: u32,
    pub last_visit_ms: u64,
}

pub struct Store {
    conn: Connection,
    history_revision: Cell<u64>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        Self::initialize(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::initialize(Connection::open_in_memory()?)
    }

    /// The library file, or `None` for a library kept in memory.
    pub fn path(&self) -> Option<PathBuf> {
        self.conn.path().filter(|path| !path.is_empty()).map(PathBuf::from)
    }

    /// For backups, which copy the database through this connection, and
    /// site permissions, which keep their own table in it.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    fn initialize(conn: Connection) -> Result<Store> {
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(SITE_PERMISSIONS_TABLE)?;
        let has_archive_column = |column: &str| -> Result<bool> {
            Ok(conn.prepare("PRAGMA table_info(archive)")?.query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?.iter().any(|name| name == column))
        };
        // Today's space owner cannot prove the owner of an old archive row.
        // Preserve legacy rows with NULL ownership, hidden from every profile.
        // Rows from before sources were kept all came from tabs.
        for (column, definition) in [("profile_id", "profile_id TEXT"), ("source", "source TEXT NOT NULL DEFAULT 'tab'")] {
            if has_archive_column(column)? { continue; }
            let transaction = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
            if !has_archive_column(column)? {
                conn.execute_batch(&format!("ALTER TABLE archive ADD COLUMN {definition}"))?;
            }
            transaction.commit()?;
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS archive_profile_recent ON archive(profile_id, archived_ms DESC, rowid DESC)")?;
        // Never lowered, and a library opened read-only keeps its mark.
        let format: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if format < LIBRARY_FORMAT {
            let _ = conn.execute_batch(&format!("PRAGMA user_version = {LIBRARY_FORMAT}"));
        }
        Ok(Store { conn, history_revision: Cell::new(0) })
    }

    pub(crate) fn begin_transaction(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")
    }

    pub(crate) fn commit_transaction(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")
    }

    pub(crate) fn rollback_transaction(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")
    }

    // -- key/value -----------------------------------------------------------

    pub fn kv_get(&self, k: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT v FROM kv WHERE k = ?1", [k], |r| r.get(0))
            .optional()
    }

    pub fn kv_set(&self, k: &str, v: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![k, v],
        )?;
        Ok(())
    }

    /// Save a device-local draft only if its original value is still current.
    /// The comparison and write are one SQLite statement, including first use.
    pub fn kv_compare_and_set(&self, k: &str, expected: Option<&str>, v: &str) -> Result<bool> {
        let changed = match expected {
            Some(previous) => self.conn.execute("UPDATE kv SET v = ?3 WHERE k = ?1 AND v = ?2", params![k, previous, v])?,
            None => self.conn.execute("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO NOTHING", params![k, v])?,
        };
        Ok(changed == 1)
    }

    /// Set several keys together: all of them, or none if one write fails.
    pub fn kv_set_all(&self, pairs: &[(&str, &str)]) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        for (k, v) in pairs {
            transaction.execute("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", params![k, v])?;
        }
        transaction.commit()
    }

    pub fn kv_delete(&self, k: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kv WHERE k = ?1", [k])?;
        Ok(())
    }

    // -- entities ------------------------------------------------------------

    pub fn all_entities(&self) -> Result<Vec<EntityRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, id, hlc, deleted, payload FROM entities ORDER BY kind, id")?;
        let rows = stmt.query_map([], |r| {
            let hlc: String = r.get(2)?;
            Ok(EntityRow {
                kind: r.get(0)?,
                id: r.get(1)?,
                hlc: Hlc::parse(&hlc).unwrap_or_else(|_| Hlc::new(0, 0, "x")),
                deleted: r.get::<_, i64>(3)? != 0,
                payload: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    pub(crate) fn entity_payload(&self, kind: &str, id: &str) -> Result<Option<String>> {
        self.conn.query_row(
            "SELECT payload FROM entities WHERE kind = ?1 AND id = ?2 AND deleted = 0",
            params![kind, id], |row| row.get(0),
        ).optional()
    }

    pub(crate) fn entity(&self, kind: &str, id: &str) -> Result<Option<EntityRow>> {
        self.conn.query_row(
            "SELECT kind, id, hlc, deleted, payload FROM entities WHERE kind = ?1 AND id = ?2",
            params![kind, id], |r| {
                let hlc: String = r.get(2)?;
                Ok(EntityRow { kind: r.get(0)?, id: r.get(1)?, hlc: Hlc::parse(&hlc).unwrap_or_else(|_| Hlc::new(0, 0, "x")), deleted: r.get::<_, i64>(3)? != 0, payload: r.get(4)? })
            },
        ).optional()
    }

    pub fn entity_hlc(&self, kind: &str, id: &str) -> Result<Option<Hlc>> {
        let s: Option<String> = self
            .conn
            .query_row("SELECT hlc FROM entities WHERE kind = ?1 AND id = ?2", [kind, id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(s.and_then(|s| Hlc::parse(&s).ok()))
    }

    pub fn put_entity(&self, row: &EntityRow, dirty: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entities (kind, id, hlc, deleted, payload, dirty) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kind, id) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted,
               payload = excluded.payload, dirty = excluded.dirty",
            params![row.kind, row.id, row.hlc.encode(), row.deleted as i64, row.payload, dirty as i64],
        )?;
        Ok(())
    }

    /// One row's columns exactly as stored, dirty flag included, so it can
    /// be put back unchanged with [`Store::put_raw_entity`].
    pub(crate) fn raw_entity(&self, kind: &str, id: &str) -> Result<Option<RawEntity>> {
        self.conn.query_row(
            "SELECT hlc, deleted, payload, dirty FROM entities WHERE kind = ?1 AND id = ?2",
            params![kind, id], |r| Ok(RawEntity { hlc: r.get(0)?, deleted: r.get::<_, i64>(1)? != 0, payload: r.get(2)?, dirty: r.get::<_, i64>(3)? != 0 }),
        ).optional()
    }

    /// Put back a row read by [`Store::raw_entity`], or remove it if it was absent.
    pub(crate) fn put_raw_entity(&self, kind: &str, id: &str, row: Option<&RawEntity>) -> Result<()> {
        match row {
            Some(row) => self.conn.execute(
                "INSERT INTO entities (kind, id, hlc, deleted, payload, dirty) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(kind, id) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted,
                   payload = excluded.payload, dirty = excluded.dirty",
                params![kind, id, row.hlc, row.deleted as i64, row.payload, row.dirty as i64],
            )?,
            None => self.conn.execute("DELETE FROM entities WHERE kind = ?1 AND id = ?2", params![kind, id])?,
        };
        Ok(())
    }

    /// Up to `limit` changes waiting to be pushed.
    pub fn dirty_changes(&self, limit: usize) -> Result<Vec<Change>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, id, hlc, deleted, payload FROM entities WHERE dirty = 1 LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            let hlc: String = r.get(2)?;
            Ok(Change {
                kind: r.get(0)?,
                id: r.get(1)?,
                hlc: Hlc::parse(&hlc).unwrap_or_else(|_| Hlc::new(0, 0, "x")),
                deleted: r.get::<_, i64>(3)? != 0,
                enc: Encoding::Json,
                payload: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// A bounded outgoing page, excluding only explicitly rejected versions.
    /// The exclusion is applied before LIMIT, so even a full page of invalid
    /// records cannot starve later valid records. `dirty_changes` still exposes
    /// every unsynced record, including rejected data, for inspection/backup.
    pub fn sync_changes(&self, limit: usize) -> Result<Vec<Change>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.kind, e.id, e.hlc, e.deleted, e.payload FROM entities e
             WHERE e.dirty = 1 AND NOT EXISTS (
               SELECT 1 FROM sync_rejections r WHERE r.kind = e.kind AND r.id = e.id AND r.hlc = e.hlc
             ) ORDER BY e.kind, e.id LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], outgoing_change)?;
        rows.collect()
    }

    /// One row's waiting version, unless that exact version is paused.
    pub fn sync_change(&self, kind: &str, id: &str) -> Result<Option<Change>> {
        self.conn.query_row(
            "SELECT e.kind, e.id, e.hlc, e.deleted, e.payload FROM entities e
             WHERE e.kind = ?1 AND e.id = ?2 AND e.dirty = 1 AND NOT EXISTS (
               SELECT 1 FROM sync_rejections r WHERE r.kind = e.kind AND r.id = e.id AND r.hlc = e.hlc
             )",
            params![kind, id], outgoing_change,
        ).optional()
    }

    /// Pause only the exact version that was rejected. Never clear its dirty
    /// flag or overwrite a newer local edit while processing a delayed event.
    pub fn reject_sync_change(&self, change: &Change, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_rejections (kind, id, hlc, reason)
             SELECT kind, id, hlc, ?4 FROM entities WHERE kind = ?1 AND id = ?2 AND hlc = ?3 AND dirty = 1
             ON CONFLICT(kind, id) DO UPDATE SET hlc = excluded.hlc, reason = excluded.reason",
            params![change.kind, change.id, change.hlc.encode(), reason],
        )?;
        Ok(())
    }

    pub fn sync_rejection_reason(&self) -> Result<Option<String>> {
        self.conn.query_row(
            "SELECT r.reason FROM sync_rejections r JOIN entities e
             ON e.kind = r.kind AND e.id = r.id AND e.hlc = r.hlc
             WHERE e.dirty = 1 ORDER BY r.kind, r.id LIMIT 1", [], |r| r.get(0),
        ).optional()
    }

    /// Explicit retry (or a new account). The underlying dirty data is intact.
    pub fn retry_sync_rejections(&self) -> Result<()> {
        self.conn.execute("DELETE FROM sync_rejections", [])?;
        Ok(())
    }

    pub fn dirty_count(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM entities WHERE dirty = 1", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
    }

    /// Clear the dirty flag, but only if the row was not edited again since the
    /// push started (same HLC).
    pub fn mark_clean(&self, kind: &str, id: &str, hlc: &Hlc) -> Result<()> {
        self.conn.execute(
            "UPDATE entities SET dirty = 0 WHERE kind = ?1 AND id = ?2 AND hlc = ?3",
            params![kind, id, hlc.encode()],
        )?;
        Ok(())
    }

    /// Mark every entity dirty (first sync of an existing local library).
    pub fn mark_all_dirty(&self) -> Result<()> {
        self.conn.execute("UPDATE entities SET dirty = 1", [])?;
        Ok(())
    }

    /// Nothing waits to upload: the account's sync data was deleted.
    pub fn mark_all_clean(&self) -> Result<()> {
        self.conn.execute("UPDATE entities SET dirty = 0", [])?;
        Ok(())
    }

    // -- tabs ----------------------------------------------------------------

    pub fn tabs(&self) -> Result<Vec<TabRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, space_id, item_id, url, title, ord, created_ms, last_active_ms FROM tabs ORDER BY ord",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TabRow {
                id: r.get(0)?,
                space_id: r.get(1)?,
                item_id: r.get(2)?,
                url: r.get(3)?,
                title: r.get(4)?,
                order: r.get(5)?,
                created_ms: r.get::<_, i64>(6)? as u64,
                last_active_ms: r.get::<_, i64>(7)? as u64,
            })
        })?;
        rows.collect()
    }

    pub fn put_tab(&self, t: &TabRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tabs (id, space_id, item_id, url, title, ord, created_ms, last_active_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET space_id = excluded.space_id, item_id = excluded.item_id,
               url = excluded.url, title = excluded.title, ord = excluded.ord,
               last_active_ms = excluded.last_active_ms",
            params![
                t.id,
                t.space_id,
                t.item_id,
                t.url,
                t.title,
                t.order,
                t.created_ms as i64,
                t.last_active_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn delete_tab(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM tabs WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Archive the latest live metadata and remove its saved row as one commit.
    /// A library deletion batch owns the outer transaction; standalone closes
    /// still roll back the archive and tab together on any write/commit failure.
    pub fn close_tab(&self, tab: &TabRow, archive_profile: Option<&str>, now_ms: u64) -> Result<()> {
        let transaction = if self.conn.is_autocommit() {
            Some(rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?)
        } else { None };
        if let Some(profile) = archive_profile {
            self.archive_tab(profile, &tab.url, &tab.title, &tab.space_id, now_ms)?;
        }
        self.delete_tab(&tab.id)?;
        if let Some(transaction) = transaction { transaction.commit()?; }
        Ok(())
    }

    // -- archive -------------------------------------------------------------

    fn archive_profile_exists(&self, profile: &str) -> Result<bool> {
        if profile.is_empty() { return Ok(false); }
        let payload = self.entity_payload(kinds::PROFILE, profile)?;
        Ok(payload.is_some_and(|payload| serde_json::from_str::<crate::model::Profile>(&payload).is_ok()))
    }

    fn archive_space(&self, space: &str) -> Result<ArchiveSpace> {
        let payload = self.entity_payload(kinds::SPACE, space)?;
        Ok(match payload {
            None => ArchiveSpace::Missing,
            Some(payload) => match serde_json::from_str::<crate::model::Space>(&payload) {
                Ok(space) if !space.profile_id.is_empty() => ArchiveSpace::Owned(space.profile_id),
                _ => ArchiveSpace::Invalid,
            },
        })
    }

    pub fn archive_space_matches(&self, profile: &str, space: &str) -> Result<bool> {
        Ok(self.archive_profile_exists(profile)? && matches!(self.archive_space(space)?, ArchiveSpace::Owned(owner) if owner == profile))
    }

    fn archive_source_matches(&self, profile: &str, space: &str) -> Result<bool> {
        Ok(match self.archive_space(space)? {
            ArchiveSpace::Missing => true,
            ArchiveSpace::Owned(owner) => owner == profile,
            ArchiveSpace::Invalid => false,
        })
    }

    pub fn archive_tab(&self, profile: &str, url: &str, title: &str, space_id: &str, now_ms: u64) -> Result<()> {
        self.archive_page(profile, url, title, space_id, now_ms, ArchiveSource::Tab)
    }

    /// Store the known owner at creation, never infer it again from a later space.
    /// Import and close already hold a transaction; standalone writes get one too.
    pub fn archive_page(&self, profile: &str, url: &str, title: &str, space_id: &str, now_ms: u64, source: ArchiveSource) -> Result<()> {
        let transaction = if self.conn.is_autocommit() {
            Some(rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?)
        } else { None };
        if !self.archive_space_matches(profile, space_id)? {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.conn.execute(
            "INSERT INTO archive (url, title, space_id, archived_ms, profile_id, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![url, title, space_id, now_ms as i64, profile, source.encode()],
        )?;
        // One profile cannot evict another profile's rows or unassigned legacy data.
        self.conn.execute(
            "DELETE FROM archive WHERE profile_id = ?1 AND rowid NOT IN (SELECT rowid FROM archive WHERE profile_id = ?1 ORDER BY archived_ms DESC, rowid DESC LIMIT 2000)",
            [profile],
        )?;
        if let Some(transaction) = transaction { transaction.commit()?; }
        Ok(())
    }

    pub fn archive(&self, profile: &str, limit: usize) -> Result<Vec<ArchivedTab>> {
        if limit == 0 || !self.archive_profile_exists(profile)? { return Ok(Vec::new()); }
        let mut stmt = self.conn.prepare(
            "SELECT rowid, url, title, space_id, archived_ms, profile_id, source FROM archive WHERE profile_id = ?1 ORDER BY archived_ms DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([profile], ArchivedTab::from_row)?;
        let mut visible = Vec::new();
        let mut sources = HashMap::new();
        for row in rows {
            let row = row?;
            let allowed = match sources.get(&row.space_id) {
                Some(allowed) => *allowed,
                None => {
                    let allowed = self.archive_source_matches(profile, &row.space_id)?;
                    sources.insert(row.space_id.clone(), allowed);
                    allowed
                }
            };
            if allowed {
                visible.push(row);
                if visible.len() == limit { break; }
            }
        }
        Ok(visible)
    }

    pub fn archived_tab(&self, profile: &str, rowid: i64) -> Result<Option<ArchivedTab>> {
        if !self.archive_profile_exists(profile)? { return Ok(None); }
        let row = self.conn.query_row(
            "SELECT rowid, url, title, space_id, archived_ms, profile_id, source FROM archive WHERE profile_id = ?1 AND rowid = ?2",
            params![profile, rowid], ArchivedTab::from_row,
        ).optional()?;
        match row {
            Some(row) if self.archive_source_matches(profile, &row.space_id)? => Ok(Some(row)),
            _ => Ok(None),
        }
    }

    /// Recheck source, target and owner inside the same commit as restoration.
    /// Unknown owners and changed/malformed spaces stay untouched, not reassigned.
    pub fn restore_archived(&self, profile: &str, rowid: i64, id: &str, active_space: &str, order: &str, now_ms: u64) -> Result<Option<TabRow>> {
        let transaction = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        if !self.archive_space_matches(profile, active_space)? { return Ok(None); }
        let Some(archived) = self.archived_tab(profile, rowid)? else { return Ok(None) };
        let space = match self.archive_space(&archived.space_id)? {
            ArchiveSpace::Missing => active_space.to_owned(),
            ArchiveSpace::Owned(owner) if owner == profile => archived.space_id,
            _ => return Ok(None),
        };
        let tab = TabRow {
            id: id.into(), space_id: space, item_id: None, url: archived.url, title: archived.title,
            order: order.into(), created_ms: now_ms, last_active_ms: now_ms,
        };
        self.conn.execute(
            "INSERT INTO tabs (id, space_id, item_id, url, title, ord, created_ms, last_active_ms) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?6)",
            params![tab.id, tab.space_id, tab.url, tab.title, tab.order, now_ms as i64],
        )?;
        self.conn.execute("DELETE FROM archive WHERE profile_id = ?1 AND rowid = ?2", params![profile, rowid])?;
        transaction.commit()?;
        Ok(Some(tab))
    }

    // -- history -------------------------------------------------------------

    pub fn history_revision(&self) -> u64 {
        self.history_revision.get()
    }

    pub fn record_visit(&self, profile: &str, url: &str, title: &str, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO profile_history (profile_id, url, title, visit_count, last_visit_ms) VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(profile_id, url) DO UPDATE SET
               title = CASE WHEN excluded.title = '' THEN profile_history.title ELSE excluded.title END,
               visit_count = profile_history.visit_count + 1, last_visit_ms = excluded.last_visit_ms",
            params![profile, url, title, now_ms as i64],
        )?;
        self.history_revision.set(self.history_revision.get().wrapping_add(1));
        Ok(())
    }

    pub fn update_history_title(&self, profile: &str, url: &str, title: &str) -> Result<()> {
        let changed = self.conn.execute("UPDATE profile_history SET title = ?3 WHERE profile_id = ?1 AND url = ?2", params![profile, url, title])?;
        if changed > 0 { self.history_revision.set(self.history_revision.get().wrapping_add(1)); }
        Ok(())
    }

    pub fn recent_history(&self, profile: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, title, visit_count, last_visit_ms FROM profile_history WHERE profile_id = ?1 ORDER BY last_visit_ms DESC, url LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![profile, limit as i64], |r| {
            Ok(HistoryEntry {
                url: r.get(0)?,
                title: r.get(1)?,
                visit_count: r.get::<_, i64>(2)? as u32,
                last_visit_ms: r.get::<_, i64>(3)? as u64,
            })
        })?;
        rows.collect()
    }

    /// When `profile` last visited `url`, if its history has it.
    pub fn last_visit(&self, profile: &str, url: &str) -> Result<Option<u64>> {
        self.conn.query_row("SELECT last_visit_ms FROM profile_history WHERE profile_id = ?1 AND url = ?2", params![profile, url], |r| r.get::<_, i64>(0))
            .optional().map(|ms| ms.map(|ms| ms as u64))
    }

    pub fn clear_history(&self, profile: &str) -> Result<()> {
        let changed = self.conn.execute("DELETE FROM profile_history WHERE profile_id = ?1", [profile])?;
        if changed > 0 { self.history_revision.set(self.history_revision.get().wrapping_add(1)); }
        Ok(())
    }

    /// Fold the log into the library file and empty it, so rows just
    /// deleted leave no copy in it. False when a reader kept it from
    /// finishing.
    pub(crate) fn fold_log(&self) -> Result<bool> {
        let busy: i64 = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        Ok(busy == 0)
    }

    /// Remove one profile's archived tabs; unassigned legacy rows stay.
    pub fn clear_archive(&self, profile: &str) -> Result<()> {
        self.conn.execute("DELETE FROM archive WHERE profile_id = ?1", [profile])?;
        Ok(())
    }
}

#[cfg(test)]
mod kv_tests {
    use super::*;

    #[test]
    fn a_library_records_its_format_and_keeps_a_newer_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let version = || -> i64 { Connection::open(&path).unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap() };
        let store = Store::open(&path).unwrap();
        assert_eq!(store.path().map(|known| known.canonicalize().unwrap()), Some(path.canonicalize().unwrap()));
        drop(store);
        assert_eq!(version(), LIBRARY_FORMAT);
        // A library from a newer version keeps its mark; this one never lowers it.
        Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 7").unwrap();
        drop(Store::open(&path).unwrap());
        assert_eq!(version(), 7);
        assert_eq!(Store::open_in_memory().unwrap().path(), None);
    }

    #[test]
    fn last_visits_are_read_per_profile_and_url() {
        let store = Store::open_in_memory().unwrap();
        store.record_visit("a", "https://mail.example/", "Inbox", 10).unwrap();
        store.record_visit("a", "https://mail.example/", "Inbox", 25).unwrap();
        store.record_visit("b", "https://chat.example/", "Chat", 30).unwrap();
        assert_eq!(store.last_visit("a", "https://mail.example/").unwrap(), Some(25));
        assert_eq!(store.last_visit("b", "https://mail.example/").unwrap(), None, "another profile's history is not read");
        assert_eq!(store.last_visit("a", "https://mail.example/other").unwrap(), None);
        store.clear_history("a").unwrap();
        assert_eq!(store.last_visit("a", "https://mail.example/").unwrap(), None);
        assert_eq!(store.last_visit("b", "https://chat.example/").unwrap(), Some(30));
    }

    #[test]
    fn keys_set_together_are_all_kept_or_none_are() {
        let store = Store::open_in_memory().unwrap();
        store.kv_set("first", "old").unwrap();
        store.conn.execute_batch("CREATE TRIGGER reject BEFORE INSERT ON kv WHEN NEW.k = 'second' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        assert!(store.kv_set_all(&[("first", "new"), ("second", "2")]).is_err());
        assert_eq!((store.kv_get("first").unwrap().as_deref(), store.kv_get("second").unwrap()), (Some("old"), None), "a failed write leaves both keys as they were");
        store.conn.execute_batch("DROP TRIGGER reject").unwrap();
        store.kv_set_all(&[("first", "new"), ("second", "2")]).unwrap();
        assert_eq!((store.kv_get("first").unwrap().as_deref(), store.kv_get("second").unwrap().as_deref()), (Some("new"), Some("2")));
    }
}

#[cfg(test)]
mod archive_write_tests {
    use super::*;

    fn tab() -> TabRow {
        TabRow {
            id: "draft".into(), space_id: "space".into(), item_id: None,
            url: "https://fixture.example/draft".into(), title: "Draft".into(), order: "a0".into(),
            created_ms: 1, last_active_ms: 2,
        }
    }

    fn owned_store(store: Store) -> Store {
        for (kind, id, payload) in [
            (kinds::PROFILE, "profile", serde_json::to_string(&crate::model::Profile { name: "Profile".into(), created_ms: 1, preferences: None }).unwrap()),
            (kinds::SPACE, "space", serde_json::to_string(&crate::model::Space { name: "Space".into(), icon: "S".into(), theme: crate::model::Theme::preset(0), profile_id: "profile".into(), order: "a0".into() }).unwrap()),
            (kinds::SPACE, "old-space", serde_json::to_string(&crate::model::Space { name: "Old space".into(), icon: "S".into(), theme: crate::model::Theme::preset(0), profile_id: "profile".into(), order: "a1".into() }).unwrap()),
        ] {
            store.put_entity(&EntityRow { kind: kind.into(), id: id.into(), hlc: Hlc::new(1, 0, "fixture"), deleted: false, payload }, false).unwrap();
        }
        store
    }

    fn close_failure(sql: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite");
        let store = owned_store(Store::open(&path).unwrap());
        let saved = tab();
        store.put_tab(&saved).unwrap();
        store.archive_tab("profile", "https://fixture.example/existing", "Existing archive", "space", 1).unwrap();
        let archive = store.archive("profile", 2000).unwrap();
        store.conn.execute_batch(sql).unwrap();
        let mut latest = saved.clone();
        latest.url.push_str("/latest");
        latest.title = "Latest draft".into();
        assert!(store.close_tab(&latest, Some("profile"), 3).is_err());
        assert_eq!(store.tabs().unwrap(), vec![saved.clone()], "failed close must retain the saved tab");
        assert_eq!(store.archive("profile", 2000).unwrap(), archive, "failed close must roll back the archive write");
        assert!(store.conn.is_autocommit(), "failed transaction must release its lock");
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.tabs().unwrap(), vec![saved]);
        assert_eq!(store.archive("profile", 2000).unwrap(), archive);
        store.conn.execute_batch("DROP TRIGGER reject_archive_write").unwrap();
        store.close_tab(&latest, Some("profile"), 4).unwrap();
        assert!(store.tabs().unwrap().is_empty());
        let after = store.archive("profile", 2000).unwrap();
        assert_eq!(after.len(), 2, "retry must archive exactly once");
        assert_eq!((&after[0].url, &after[0].title), (&latest.url, &latest.title));
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.tabs().unwrap().is_empty());
        assert_eq!(store.archive("profile", 2000).unwrap(), after);
    }

    #[test]
    fn close_archive_insert_failure_is_atomic_and_retryable() {
        close_failure("CREATE TRIGGER reject_archive_write BEFORE INSERT ON archive BEGIN SELECT RAISE(ABORT, 'fixture archive insert failure'); END;");
    }

    #[test]
    fn close_tab_delete_failure_is_atomic_and_retryable() {
        close_failure("CREATE TRIGGER reject_archive_write BEFORE DELETE ON tabs BEGIN SELECT RAISE(ABORT, 'fixture tab delete failure'); END;");
    }

    #[test]
    fn close_commit_failure_is_atomic_and_retryable() {
        close_failure("CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_child (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_archive_write AFTER DELETE ON tabs BEGIN INSERT INTO fixture_child VALUES (1); END;");
    }

    fn restore_failure(sql: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite");
        let store = owned_store(Store::open(&path).unwrap());
        let saved = tab();
        store.put_tab(&saved).unwrap();
        store.archive_tab("profile", "https://fixture.example/restore", "Restore me", "old-space", 1).unwrap();
        store.conn.execute("UPDATE entities SET deleted = 1 WHERE kind = 'space' AND id = 'old-space'", []).unwrap();
        let archive = store.archive("profile", 2000).unwrap();
        let rowid = archive[0].rowid;
        store.conn.execute_batch(sql).unwrap();
        assert!(store.restore_archived("profile", rowid, "restored", "space", "a1", 3).is_err());
        assert_eq!(store.tabs().unwrap(), vec![saved.clone()], "failed restore must not leave an extra tab");
        assert_eq!(store.archive("profile", 2000).unwrap(), archive, "failed restore must keep its archive row");
        assert!(store.conn.is_autocommit(), "failed transaction must release its lock");
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.tabs().unwrap(), vec![saved.clone()]);
        assert_eq!(store.archive("profile", 2000).unwrap(), archive);
        store.conn.execute_batch("DROP TRIGGER reject_archive_write").unwrap();
        let restored = store.restore_archived("profile", rowid, "restored", "space", "a1", 4).unwrap().unwrap();
        assert_eq!((&restored.url, &restored.title), (&archive[0].url, &archive[0].title));
        assert_eq!(restored.space_id, "space");
        assert!(restored.item_id.is_none());
        assert!(store.archive("profile", 2000).unwrap().is_empty());
        assert!(store.restore_archived("profile", rowid, "duplicate", "space", "a2", 5).unwrap().is_none());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.tabs().unwrap(), vec![saved, restored]);
        assert!(store.archive("profile", 2000).unwrap().is_empty());
    }

    #[test]
    fn restore_tab_insert_failure_is_atomic_and_retryable() {
        restore_failure("CREATE TRIGGER reject_archive_write BEFORE INSERT ON tabs BEGIN SELECT RAISE(ABORT, 'fixture tab insert failure'); END;");
    }

    #[test]
    fn restore_archive_delete_failure_is_atomic_and_retryable() {
        restore_failure("CREATE TRIGGER reject_archive_write BEFORE DELETE ON archive BEGIN SELECT RAISE(ABORT, 'fixture archive delete failure'); END;");
    }

    #[test]
    fn restore_commit_failure_is_atomic_and_retryable() {
        restore_failure("CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_child (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_archive_write AFTER INSERT ON tabs BEGIN INSERT INTO fixture_child VALUES (1); END;");
    }

    #[test]
    fn restore_never_overwrites_an_existing_tab_id() {
        let store = owned_store(Store::open_in_memory().unwrap());
        let saved = tab();
        store.put_tab(&saved).unwrap();
        store.archive_tab("profile", "https://fixture.example/restore", "Restore me", "space", 1).unwrap();
        let archive = store.archive("profile", 2000).unwrap();
        assert!(store.restore_archived("profile", archive[0].rowid, &saved.id, "space", "a1", 2).is_err());
        assert_eq!(store.tabs().unwrap(), vec![saved]);
        assert_eq!(store.archive("profile", 2000).unwrap(), archive);
    }

    #[test]
    fn archive_pruning_failure_rolls_back_the_new_entry_and_tab_close() {
        let store = owned_store(Store::open_in_memory().unwrap());
        let saved = tab();
        store.put_tab(&saved).unwrap();
        store.conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO archive (url, title, space_id, archived_ms, profile_id) SELECT 'https://fixture.example/' || x, 'Existing', 'space', x, 'profile' FROM n;").unwrap();
        let archive = store.archive("profile", 2000).unwrap();
        store.conn.execute_batch("CREATE TRIGGER reject_archive_write BEFORE DELETE ON archive BEGIN SELECT RAISE(ABORT, 'fixture pruning failure'); END;").unwrap();
        assert!(store.close_tab(&saved, Some("profile"), 3000).is_err());
        assert_eq!(store.tabs().unwrap(), vec![saved.clone()]);
        assert_eq!(store.archive("profile", 2001).unwrap(), archive);
        store.conn.execute_batch("DROP TRIGGER reject_archive_write").unwrap();
        store.close_tab(&saved, Some("profile"), 3000).unwrap();
        assert!(store.tabs().unwrap().is_empty());
        let archived = store.archive("profile", 2001).unwrap();
        assert_eq!(archived.len(), 2000);
        assert_eq!(archived[0].url, saved.url);
    }

    #[test]
    fn selective_tab_delete_failure_keeps_only_the_failed_tab_open_for_retry() {
        let store = owned_store(Store::open_in_memory().unwrap());
        let mut tabs = Vec::new();
        for id in ["first", "retry", "last"] {
            let mut row = tab();
            row.id = id.into();
            row.url = format!("https://fixture.example/{id}");
            store.put_tab(&row).unwrap();
            tabs.push(row);
        }
        store.conn.execute_batch("CREATE TRIGGER reject_archive_write BEFORE DELETE ON tabs WHEN OLD.id = 'retry' BEGIN SELECT RAISE(ABORT, 'fixture selective close failure'); END;").unwrap();
        let results: Vec<_> = tabs.iter().map(|tab| store.close_tab(tab, Some("profile"), 3).is_ok()).collect();
        assert_eq!(results, [true, false, true]);
        assert_eq!(store.tabs().unwrap(), vec![tabs[1].clone()]);
        assert_eq!(store.archive("profile", 2000).unwrap().len(), 2);
        store.conn.execute_batch("DROP TRIGGER reject_archive_write").unwrap();
        store.close_tab(&tabs[1], Some("profile"), 4).unwrap();
        assert!(store.tabs().unwrap().is_empty());
        assert_eq!(store.archive("profile", 2000).unwrap().len(), 3);
    }

    #[test]
    fn unsaved_and_non_archived_tabs_close_without_inventing_archive_rows() {
        let store = owned_store(Store::open_in_memory().unwrap());
        let mut tab = tab();
        store.close_tab(&tab, Some("profile"), 3).unwrap();
        assert_eq!(store.archive("profile", 2000).unwrap().len(), 1);
        for item in [None, Some("pin".into()), Some("favorite".into())] {
            tab.item_id = item;
            store.put_tab(&tab).unwrap();
            store.close_tab(&tab, None, 4).unwrap();
            assert!(store.tabs().unwrap().is_empty());
            assert_eq!(store.archive("profile", 2000).unwrap().len(), 1);
        }
    }
}

#[cfg(test)]
mod archive_profile_tests {
    use super::*;
    use crate::{library::Library, model::Theme};

    fn library(store: Store) -> (Library, String, String, String, String) {
        let mut lib = Library::open(store).unwrap();
        let a = lib.create_profile("A").unwrap();
        let b = lib.create_profile("B").unwrap();
        let sa = lib.create_space("A", "A", Theme::preset(0), &a).unwrap();
        let sb = lib.create_space("B", "B", Theme::preset(0), &b).unwrap();
        (lib, a, b, sa, sb)
    }

    #[test]
    fn legacy_rows_are_preserved_but_not_inferred_from_current_space_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE archive (rowid INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL, title TEXT NOT NULL, space_id TEXT NOT NULL, archived_ms INTEGER NOT NULL); INSERT INTO archive (url, title, space_id, archived_ms) VALUES ('https://legacy.example/', 'Legacy private', 'old-space', 1);").unwrap();
        let (lib, a, _, sa, _) = library(Store::open(&path).unwrap());
        // Even a resolvable present-day space cannot prove the old row's owner.
        connection.execute("UPDATE archive SET space_id = ?1", [&sa]).unwrap();
        assert!(lib.store.archive(&a, 300).unwrap().is_empty());
        assert!(lib.store.restore_archived(&a, 1, "blocked", &sa, "a0", 2).unwrap().is_none());
        assert!(lib.store.tabs().unwrap().is_empty());
        drop(lib);
        let store = Store::open(&path).unwrap();
        assert!(store.archive(&a, 300).unwrap().is_empty());
        assert_eq!(connection.query_row("SELECT title FROM archive WHERE rowid = 1", [], |row| row.get::<_, String>(0)).unwrap(), "Legacy private");
    }

    #[test]
    fn archive_rows_record_their_source_and_older_rows_read_as_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("before-source.sqlite");
        // The archive as the previous release left it: owners, but no source.
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE archive (rowid INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL, title TEXT NOT NULL, space_id TEXT NOT NULL, archived_ms INTEGER NOT NULL, profile_id TEXT);").unwrap();
        connection.execute("INSERT INTO archive (url, title, space_id, archived_ms, profile_id) VALUES ('https://old.example/', 'Old', 'unknown-space', 1, 'unknown-profile')", []).unwrap();
        let (lib, a, _, sa, _) = library(Store::open(&path).unwrap());
        connection.execute("UPDATE archive SET space_id = ?1, profile_id = ?2", [&sa, &a]).unwrap();
        lib.store.archive_page(&a, "https://little.example/", "Little", &sa, 2, ArchiveSource::LittleWindow).unwrap();
        lib.store.archive_tab(&a, "https://tab.example/", "Tab", &sa, 3).unwrap();
        // A release from before sources writes its own column list.
        connection.execute("INSERT INTO archive (url, title, space_id, archived_ms, profile_id) VALUES ('https://older-release.example/', 'Older release', ?1, 4, ?2)", [&sa, &a]).unwrap();
        let sources = |store: &Store| store.archive(&a, 10).unwrap().into_iter().map(|row| (row.url, row.source)).collect::<Vec<_>>();
        let expected = vec![
            ("https://older-release.example/".to_string(), ArchiveSource::Tab),
            ("https://tab.example/".to_string(), ArchiveSource::Tab),
            ("https://little.example/".to_string(), ArchiveSource::LittleWindow),
            ("https://old.example/".to_string(), ArchiveSource::Tab),
        ];
        assert_eq!(sources(&lib.store), expected);
        let little = lib.store.archive(&a, 10).unwrap()[2].rowid;
        assert_eq!(lib.store.archived_tab(&a, little).unwrap().unwrap().source, ArchiveSource::LittleWindow);
        drop(lib);
        // Opening again keeps every source, and a value this release doesn't know reads as a tab.
        let store = Store::open(&path).unwrap();
        assert_eq!(sources(&store), expected);
        connection.execute("UPDATE archive SET source = 'from_a_later_release' WHERE url = 'https://tab.example/'", []).unwrap();
        assert_eq!(store.archive(&a, 10).unwrap()[1].source, ArchiveSource::Tab);
        // Reopen Closed still restores a Little Window's page as a tab.
        let restored = store.restore_archived(&a, little, "restored", &sa, "a0", 5).unwrap().expect("restored");
        assert_eq!(restored.url, "https://little.example/");
        assert_eq!(store.archive(&a, 10).unwrap().len(), 3);
    }

    #[test]
    fn direct_restore_rejects_a_destination_in_another_profile() {
        let (lib, a, b, sa, sb) = library(Store::open_in_memory().unwrap());
        lib.store.archive_tab(&a, "https://a.example/", "A private", &sa, 1).unwrap();
        let rowid = lib.store.archive(&a, 1).unwrap()[0].rowid;
        assert!(lib.store.restore_archived(&a, rowid, "blocked", &sb, "a0", 2).unwrap().is_none());
        assert!(lib.store.archived_tab(&b, rowid).unwrap().is_none());
        assert!(lib.store.restore_archived(&b, rowid, "blocked", &sb, "a0", 2).unwrap().is_none());
        assert!(lib.store.tabs().unwrap().is_empty());
        assert_eq!(lib.store.archive(&a, 1).unwrap()[0].rowid, rowid);
    }

    #[test]
    fn changed_original_space_is_not_a_missing_space_fallback() {
        let (mut lib, a, b, sa, _) = library(Store::open_in_memory().unwrap());
        let fallback = lib.create_space("A fallback", "A", Theme::preset(0), &a).unwrap();
        lib.store.archive_tab(&a, "https://a.example/", "A private", &sa, 1).unwrap();
        let rowid = lib.store.archive(&a, 1).unwrap()[0].rowid;
        lib.update_space(&sa, |space| space.profile_id = b).unwrap();
        assert!(lib.store.restore_archived(&a, rowid, "blocked", &fallback, "a0", 2).unwrap().is_none());
        assert!(lib.store.tabs().unwrap().is_empty());
        assert_eq!(lib.store.conn.query_row("SELECT COUNT(*) FROM archive", [], |row| row.get::<_, u32>(0)).unwrap(), 1);
    }

    #[test]
    fn deleted_profile_is_not_restorable_through_its_remaining_space() {
        let (lib, a, _, sa, _) = library(Store::open_in_memory().unwrap());
        lib.store.archive_tab(&a, "https://a.example/", "A private", &sa, 1).unwrap();
        let rowid = lib.store.archive(&a, 1).unwrap()[0].rowid;
        lib.store.conn.execute("UPDATE entities SET deleted = 1 WHERE kind = 'profile' AND id = ?1", [&a]).unwrap();
        assert!(lib.store.restore_archived(&a, rowid, "blocked", &sa, "a0", 2).unwrap().is_none());
        assert!(lib.store.tabs().unwrap().is_empty());
        assert_eq!(lib.store.conn.query_row("SELECT COUNT(*) FROM archive", [], |row| row.get::<_, u32>(0)).unwrap(), 1);
    }

    #[test]
    fn retention_does_not_prune_another_profile_or_unassigned_legacy_rows() {
        let (lib, a, b, sa, sb) = library(Store::open_in_memory().unwrap());
        lib.store.conn.execute("INSERT INTO archive (url, title, space_id, archived_ms) VALUES ('https://legacy.example/', 'Legacy private', ?1, 0)", [&sa]).unwrap();
        lib.store.archive_tab(&a, "https://a.example/", "A private", &sa, 1).unwrap();
        for i in 0..2001 {
            lib.store.archive_tab(&b, &format!("https://b.example/{i}"), "B private", &sb, i + 2).unwrap();
        }
        let count = |title: &str| lib.store.conn.query_row("SELECT COUNT(*) FROM archive WHERE title = ?1", [title], |row| row.get::<_, u32>(0)).unwrap();
        assert_eq!(count("Legacy private"), 1);
        assert_eq!(count("A private"), 1);
        assert_eq!(count("B private"), 2000);
    }

    #[test]
    fn durable_owners_and_missing_space_fallback_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.sqlite");
        let (mut lib, a, b, sa, sb) = library(Store::open(&path).unwrap());
        let fallback = lib.create_space("A fallback", "A", Theme::preset(0), &a).unwrap();
        lib.store.archive_tab(&a, "https://shared.example/", "A private", &sa, 10).unwrap();
        lib.store.archive_tab(&b, "https://shared.example/", "B private", &sb, 10).unwrap();
        let a_rows = lib.store.archive(&a, 300).unwrap();
        let b_rows = lib.store.archive(&b, 300).unwrap();
        assert_eq!(a_rows[0].profile_id, a);
        assert_eq!(b_rows[0].profile_id, b);
        lib.delete_space(&sa).unwrap();
        drop(lib);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.archive(&a, 300).unwrap(), a_rows);
        assert_eq!(store.archive(&b, 300).unwrap(), b_rows);
        for (profile, active) in [(&b, &sb), (&a, &sb), (&a, &sa), (&a, &String::from("missing"))] {
            assert!(store.restore_archived(profile, a_rows[0].rowid, "blocked", active, "a0", 11).unwrap().is_none());
        }
        assert!(store.conn.is_autocommit());
        let restored = store.restore_archived(&a, a_rows[0].rowid, "restored", &fallback, "a0", 12).unwrap().unwrap();
        assert_eq!(restored.space_id, fallback);
        assert_eq!(restored.title, "A private");
        assert!(store.restore_archived(&a, a_rows[0].rowid, "duplicate", &fallback, "a1", 13).unwrap().is_none());
        drop(store);

        let store = Store::open(&path).unwrap();
        assert!(store.archive(&a, 300).unwrap().is_empty());
        assert_eq!(store.archive(&b, 300).unwrap(), b_rows);
        assert_eq!(store.tabs().unwrap(), vec![restored]);
        assert_eq!(store.conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
    }

    #[test]
    fn unreadable_original_space_is_hidden_preserved_and_not_a_fallback() {
        let (mut lib, a, _, sa, _) = library(Store::open_in_memory().unwrap());
        let fallback = lib.create_space("A fallback", "A", Theme::preset(0), &a).unwrap();
        lib.store.archive_tab(&a, "https://a.example/", "A private", &sa, 1).unwrap();
        let before = lib.store.archive(&a, 1).unwrap();
        let source = lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == kinds::SPACE && row.id == sa).unwrap();
        for payload in ["not JSON", "{}", r#"{"profile_id":"wrong"}"#] {
            lib.store.conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'space' AND id = ?2", params![payload, sa]).unwrap();
            assert!(lib.store.archive(&a, 1).unwrap().is_empty());
            assert!(lib.store.archived_tab(&a, before[0].rowid).unwrap().is_none());
            assert!(lib.store.restore_archived(&a, before[0].rowid, "blocked", &fallback, "a0", 2).unwrap().is_none());
            assert!(lib.store.tabs().unwrap().is_empty());
            assert_eq!(lib.store.conn.query_row("SELECT profile_id FROM archive", [], |row| row.get::<_, String>(0)).unwrap(), a);
        }
        lib.store.put_entity(&source, false).unwrap();
        assert_eq!(lib.store.archive(&a, 1).unwrap(), before);
    }

    #[test]
    fn invalid_ownership_rejects_new_archives_and_keeps_the_saved_tab() {
        let (lib, a, b, sa, sb) = library(Store::open_in_memory().unwrap());
        let mut tab = TabRow { id: "draft".into(), space_id: sa.clone(), item_id: None, url: "https://a.example/".into(), title: "Draft".into(), order: "a0".into(), created_ms: 1, last_active_ms: 1 };
        lib.store.put_tab(&tab).unwrap();
        let original = tab.clone();
        for (profile, space) in [("", sa.as_str()), ("missing", sa.as_str()), (a.as_str(), "missing"), (a.as_str(), sb.as_str()), (b.as_str(), sa.as_str())] {
            assert!(lib.store.archive_tab(profile, &tab.url, &tab.title, space, 2).is_err());
            tab.space_id = space.into();
            assert!(lib.store.close_tab(&tab, Some(profile), 2).is_err());
            assert_eq!(lib.store.tabs().unwrap(), vec![original.clone()]);
            assert_eq!(lib.store.conn.query_row("SELECT COUNT(*) FROM archive", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
            assert!(lib.store.conn.is_autocommit());
        }
        lib.store.close_tab(&original, Some(&a), 3).unwrap();
        assert!(lib.store.tabs().unwrap().is_empty());
        assert_eq!(lib.store.archive(&a, 1).unwrap()[0].profile_id, a);
    }

    #[test]
    fn unavailable_sources_are_filtered_before_reopen_limit_and_ties_are_stable() {
        let (mut lib, a, b, sa, _) = library(Store::open_in_memory().unwrap());
        let changed = lib.create_space("Changed", "A", Theme::preset(0), &a).unwrap();
        lib.store.archive_tab(&a, "https://a.example/first", "First", &sa, 1).unwrap();
        lib.store.archive_tab(&a, "https://a.example/latest", "Latest", &sa, 1).unwrap();
        lib.store.archive_tab(&a, "https://a.example/hidden", "Hidden", &changed, 2).unwrap();
        lib.update_space(&changed, |space| space.profile_id = b.clone()).unwrap();
        assert_eq!(lib.store.archive(&a, 1).unwrap()[0].title, "Latest");
        assert_eq!(lib.store.archive(&a, 300).unwrap().iter().map(|row| row.title.as_str()).collect::<Vec<_>>(), ["Latest", "First"]);
        assert!(lib.store.archive(&b, 300).unwrap().is_empty());
        assert!(lib.store.archive(&a, 0).unwrap().is_empty());
        assert!(lib.store.archive("missing", 300).unwrap().is_empty());
        assert!(lib.store.archive("", 300).unwrap().is_empty());
    }

    #[test]
    fn standalone_pruning_failure_rolls_back_and_remains_retryable() {
        let (lib, a, _, sa, _) = library(Store::open_in_memory().unwrap());
        lib.store.conn.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO archive (url, title, space_id, archived_ms, profile_id) SELECT 'https://fixture.example/' || x, 'Existing', ?1, x, ?2 FROM n", params![sa, a]).unwrap();
        let before = lib.store.archive(&a, 2000).unwrap();
        lib.store.conn.execute_batch("CREATE TRIGGER reject_prune BEFORE DELETE ON archive BEGIN SELECT RAISE(ABORT, 'fixture pruning failure'); END;").unwrap();
        assert!(lib.store.archive_tab(&a, "https://a.example/new", "New", &sa, 3000).is_err());
        assert_eq!(lib.store.archive(&a, 2001).unwrap(), before);
        assert!(lib.store.conn.is_autocommit());
        lib.store.conn.execute_batch("DROP TRIGGER reject_prune").unwrap();
        lib.store.archive_tab(&a, "https://a.example/new", "New", &sa, 3000).unwrap();
        assert_eq!(lib.store.archive(&a, 2001).unwrap().len(), 2000);
        assert_eq!(lib.store.archive(&a, 1).unwrap()[0].title, "New");
    }
}
