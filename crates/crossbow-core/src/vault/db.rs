//! The vault's own SQLite file. It holds only ciphertext plus ids, times, the
//! vault id and the sites never to offer saving for, and is never opened by
//! the library store, the sync client or a backup.

use super::VaultError;
use super::crypto::Field;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The file format. It is the only marker of a newer Crossbow: a release that
/// changes the file or envelope layout, the envelope version or the key epoch
/// raises it, so a file this client opens holds only envelope version 1 and
/// epoch 1. Format 2 added the sealed profile tag, the last-used time and the
/// never-save list, format 5 the addresses and format 8 the cards; earlier
/// builds refuse each as made by a newer Crossbow.
const FORMAT: i64 = 8;
/// Written by builds before profile tags, before addresses and before
/// cards. Opening any of them upgrades it in place.
const FORMAT_1: i64 = 1;
const FORMAT_2: i64 = 2;
const FORMAT_5: i64 = 5;
/// The highest format a newer Crossbow could write. Formats change only with
/// the file or envelope layout, so no release reaches it; a stored value above
/// it, like zero, can only be damage. Revisit it before a format near it ships.
const NEWEST_PLAUSIBLE_FORMAT: i64 = 15;

/// The formats this build reads: its own and those it upgrades.
const KNOWN_FORMATS: [i64; 4] = [FORMAT_1, FORMAT_2, FORMAT_5, FORMAT];

/// A stored format, judged before any table is read by a build that writes
/// `own` and reads `known`: a format it reads, or a missing format row, is
/// left to the table check; one a newer Crossbow could have written is
/// Newer; anything else is damage. The format-5 build made this check with
/// 5 and formats 1, 2 and 5.
///
/// A format is stored as one payload byte, so one flipped bit turns a format
/// a build reads into a value no release writes: damage, with a recovery,
/// and never an update that doesn't exist. So every new format is at least
/// two bits from every format a build reads: 5 is from 2 (the format-2
/// build checked only its own), 8 from 1, 2 and 5, and the next is 11.
pub(super) fn check_format(stored: Option<i64>, own: i64, known: &[i64]) -> Result<(), VaultError> {
    match stored {
        None => Ok(()),
        Some(format) if known.contains(&format) => Ok(()),
        Some(format) if format > own && format <= NEWEST_PLAUSIBLE_FORMAT && known.iter().all(|known| (format ^ known).count_ones() > 1) => Err(VaultError::Newer),
        Some(_) => Err(VaultError::Damaged),
    }
}
const PRAGMAS: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA secure_delete = ON;
"#;
/// The format marker's table. Its layout is the same in every format, so any
/// build can read the format of a file a newer one wrote.
const META: &str = "CREATE TABLE meta (k TEXT PRIMARY KEY, v BLOB NOT NULL);";
/// `profile` is sealed, so it can't be moved between entries or changed
/// without the key; NULL means an entry from format 1, in no profile.
/// `used_ms` is the last fill, in plaintext like the other times.
const ENTRIES: &str = r#"
CREATE TABLE entries (
  id TEXT PRIMARY KEY,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  origin BLOB NOT NULL,
  username BLOB NOT NULL,
  password BLOB NOT NULL,
  notes BLOB NOT NULL,
  profile BLOB,
  used_ms INTEGER
);
"#;
/// Sites a profile asked never to offer saving for. Plaintext, so it can be
/// read while the vault is locked.
const NEVER_SAVE: &str = r#"
CREATE TABLE never_save (
  profile TEXT NOT NULL,
  origin TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  PRIMARY KEY (profile, origin)
);
"#;
/// Saved addresses. `profile` and `address`, every part in one envelope, are
/// sealed like an entry's cells; `used_ms` is the last fill, in plaintext.
const ADDRESSES: &str = r#"
CREATE TABLE addresses (
  id TEXT PRIMARY KEY,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  profile BLOB NOT NULL,
  address BLOB NOT NULL,
  used_ms INTEGER
);
"#;
/// Saved payment cards. `profile`, `card` (the name, nickname, expiry,
/// network and last four digits in one envelope) and `number` are sealed
/// like an entry's cells; `used_ms` is the last fill, in plaintext. There is
/// no column for a security code, which is never stored.
const CARDS: &str = r#"
CREATE TABLE cards (
  id TEXT PRIMARY KEY,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  profile BLOB NOT NULL,
  card BLOB NOT NULL,
  number BLOB NOT NULL,
  used_ms INTEGER
);
"#;

/// A column as `PRAGMA table_info` reports it: its name, declared type,
/// NOT NULL and primary-key position. No column of this module's has a default.
type Column = (&'static str, &'static str, bool, i64);
const META_COLUMNS: &[Column] = &[("k", "TEXT", false, 1), ("v", "BLOB", true, 0)];
/// The entries columns of format 1. Format 2 added `ADDED_ENTRY_COLUMNS`.
const FORMAT_1_ENTRY_COLUMNS: &[Column] = &[
    ("id", "TEXT", false, 1), ("created_ms", "INTEGER", true, 0), ("updated_ms", "INTEGER", true, 0),
    ("origin", "BLOB", true, 0), ("username", "BLOB", true, 0), ("password", "BLOB", true, 0), ("notes", "BLOB", true, 0),
];
const ADDED_ENTRY_COLUMNS: &[Column] = &[("profile", "BLOB", false, 0), ("used_ms", "INTEGER", false, 0)];
const NEVER_SAVE_COLUMNS: &[Column] = &[("profile", "TEXT", true, 1), ("origin", "TEXT", true, 2), ("created_ms", "INTEGER", true, 0)];
const ADDRESS_COLUMNS: &[Column] = &[
    ("id", "TEXT", false, 1), ("created_ms", "INTEGER", true, 0), ("updated_ms", "INTEGER", true, 0),
    ("profile", "BLOB", true, 0), ("address", "BLOB", true, 0), ("used_ms", "INTEGER", false, 0),
];
const CARD_COLUMNS: &[Column] = &[
    ("id", "TEXT", false, 1), ("created_ms", "INTEGER", true, 0), ("updated_ms", "INTEGER", true, 0),
    ("profile", "BLOB", true, 0), ("card", "BLOB", true, 0), ("number", "BLOB", true, 0), ("used_ms", "INTEGER", false, 0),
];

/// Every row's sealed cells, as `SealedRow::read` reads them.
pub(super) const ROWS: &str = "SELECT rowid, id, typeof(id), CAST(id AS BLOB), created_ms, updated_ms, used_ms, origin, username, profile,
        typeof(password) = 'blob' AND typeof(notes) = 'blob'
 FROM entries ORDER BY created_ms, id";

/// Every address row's sealed cells, as `SealedAddress::read` reads them.
const ADDRESS_ROWS: &str = "SELECT rowid, id, typeof(id), CAST(id AS BLOB), created_ms, updated_ms, used_ms, profile, address FROM addresses ORDER BY created_ms, id";

/// Every card row's sealed cells, as `SealedCard::read` reads them.
const CARD_ROWS: &str = "SELECT rowid, id, typeof(id), CAST(id AS BLOB), created_ms, updated_ms, used_ms, profile, card, number FROM cards ORDER BY created_ms, id";

/// The ids of rows whose id cell can't be read are this, then the rowid. A
/// stored id that starts with it is treated the same way, so none collide.
const DAMAGED_ID: &str = "damaged-row:";

/// A row's id, or, for one whose id cell can't be read, the id it is listed
/// under and where it is found again. Columns 0 to 3 of `ROWS`, `ADDRESS_ROWS` and `CARD_ROWS`.
fn row_id(r: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Option<StoredId>)> {
    let text_id = match r.get_ref(1)? {
        ValueRef::Text(id) => std::str::from_utf8(id).ok().filter(|id| !id.is_empty() && !id.starts_with(DAMAGED_ID)).map(str::to_owned),
        _ => None,
    };
    Ok(match text_id {
        Some(id) => (id, None),
        None => {
            let rowid: i64 = r.get(0)?;
            (format!("{DAMAGED_ID}{rowid}"), Some(StoredId { rowid, kind: r.get(2)?, bytes: r.get(3)? }))
        }
    })
}

/// The sealed columns of one row, as stored. A cell of the wrong type or
/// encoding marks only its own row damaged, with its values left empty.
pub(super) struct SealedRow {
    pub id: String,
    /// Where a row whose id cell can't be read is found again, to delete it.
    pub stored_id: Option<StoredId>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub used_ms: Option<u64>,
    pub origin: Vec<u8>,
    pub username: Vec<u8>,
    pub profile: Option<Vec<u8>>,
    pub damaged: bool,
}

/// A row by its rowid and its id cell exactly as stored, so a rowid that a
/// VACUUM gave another row never matches it.
#[derive(Clone, Debug)]
pub(super) struct StoredId {
    rowid: i64,
    kind: String,
    bytes: Option<Vec<u8>>,
}

impl SealedRow {
    /// Read cell by cell, so one damaged cell never fails the whole unlock.
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<SealedRow> {
        let (id, stored_id) = row_id(r)?;
        let (created_ms, updated_ms) = (time(r.get_ref(4)?), time(r.get_ref(5)?));
        let used_ms = match r.get_ref(6)? { ValueRef::Null => Some(None), value => time(value).map(Some) };
        let (origin, username) = (sealed(r.get_ref(7)?), sealed(r.get_ref(8)?));
        let profile = match r.get_ref(9)? { ValueRef::Null => Some(None), value => sealed(value).map(Some) };
        let secrets_sealed: bool = r.get(10)?;
        match (stored_id.is_none() && secrets_sealed, created_ms, updated_ms, used_ms, origin, username, profile) {
            (true, Some(created_ms), Some(updated_ms), Some(used_ms), Some(origin), Some(username), Some(profile)) => {
                Ok(SealedRow { id, stored_id, created_ms, updated_ms, used_ms, origin, username, profile, damaged: false })
            }
            _ => Ok(SealedRow { id, stored_id, created_ms: 0, updated_ms: 0, used_ms: None, origin: Vec::new(), username: Vec::new(), profile: None, damaged: true }),
        }
    }
}

/// One address row's sealed cells, as stored, read as `SealedRow` reads an entry's.
pub(super) struct SealedAddress {
    pub id: String,
    pub stored_id: Option<StoredId>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub used_ms: Option<u64>,
    pub profile: Vec<u8>,
    pub address: Vec<u8>,
    pub damaged: bool,
}

impl SealedAddress {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<SealedAddress> {
        let (id, stored_id) = row_id(r)?;
        let (created_ms, updated_ms) = (time(r.get_ref(4)?), time(r.get_ref(5)?));
        let used_ms = match r.get_ref(6)? { ValueRef::Null => Some(None), value => time(value).map(Some) };
        match (stored_id.is_none(), created_ms, updated_ms, used_ms, sealed(r.get_ref(7)?), sealed(r.get_ref(8)?)) {
            (true, Some(created_ms), Some(updated_ms), Some(used_ms), Some(profile), Some(address)) => {
                Ok(SealedAddress { id, stored_id, created_ms, updated_ms, used_ms, profile, address, damaged: false })
            }
            _ => Ok(SealedAddress { id, stored_id, created_ms: 0, updated_ms: 0, used_ms: None, profile: Vec::new(), address: Vec::new(), damaged: true }),
        }
    }
}

/// One card row's sealed cells, as stored, read as `SealedRow` reads an entry's.
pub(super) struct SealedCard {
    pub id: String,
    pub stored_id: Option<StoredId>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub used_ms: Option<u64>,
    pub profile: Vec<u8>,
    pub card: Vec<u8>,
    pub number: Vec<u8>,
    pub damaged: bool,
}

impl SealedCard {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<SealedCard> {
        let (id, stored_id) = row_id(r)?;
        let (created_ms, updated_ms) = (time(r.get_ref(4)?), time(r.get_ref(5)?));
        let used_ms = match r.get_ref(6)? { ValueRef::Null => Some(None), value => time(value).map(Some) };
        match (stored_id.is_none(), created_ms, updated_ms, used_ms, sealed(r.get_ref(7)?), sealed(r.get_ref(8)?), sealed(r.get_ref(9)?)) {
            (true, Some(created_ms), Some(updated_ms), Some(used_ms), Some(profile), Some(card), Some(number)) => {
                Ok(SealedCard { id, stored_id, created_ms, updated_ms, used_ms, profile, card, number, damaged: false })
            }
            _ => Ok(SealedCard { id, stored_id, created_ms: 0, updated_ms: 0, used_ms: None, profile: Vec::new(), card: Vec::new(), number: Vec::new(), damaged: true }),
        }
    }
}

pub(super) struct SealedEntry<'a> {
    pub origin: &'a [u8],
    pub username: &'a [u8],
    pub password: &'a [u8],
    pub notes: &'a [u8],
    pub profile: Option<&'a [u8]>,
}

pub(super) struct Db {
    conn: Connection,
    pub vault_id: String,
    path: PathBuf,
}

/// Classify an error by its source, not by where it surfaced. Damage is found
/// first by checking the file at open (see `layout` and `check_structure`);
/// this is the backstop for what changes later. SQLite's codes for access,
/// locking, I/O, space and memory are storage errors: the file may be fine,
/// Try Again can succeed, and Reset must not touch it. The file's content is
/// damage, which Reset resolves: the corruption codes, a value SQLite
/// rejects, a schema without a table or column this build reads (SQLite's
/// generic error, or rusqlite's SqlInputError for a statement that names it),
/// and every value rusqlite can't convert: the wrong storage class, invalid
/// UTF-8, an integer out of range, a missing row. This module's own misuse of
/// SQLite stays a storage error, so a bug never offers to delete a vault; its
/// statements read fixed columns, so a column index or name out of range is
/// such a bug.
pub(super) fn classify(error: rusqlite::Error) -> VaultError {
    use rusqlite::{Error, ErrorCode};
    match error {
        Error::SqliteFailure(failure, _) => match failure.code {
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase | ErrorCode::TypeMismatch | ErrorCode::TooBig | ErrorCode::ConstraintViolation => VaultError::Damaged,
            ErrorCode::Unknown if failure.extended_code & 0xff == rusqlite::ffi::SQLITE_ERROR => VaultError::Damaged,
            _ => VaultError::Storage,
        },
        Error::SqlInputError { error, .. } if error.extended_code & 0xff == rusqlite::ffi::SQLITE_ERROR => VaultError::Damaged,
        Error::FromSqlConversionFailure(..) | Error::IntegralValueOutOfRange(..) | Error::Utf8Error(..) | Error::InvalidColumnType(..) | Error::QueryReturnedNoRows
        | Error::QueryReturnedMoreThanOneRow => VaultError::Damaged,
        _ => VaultError::Storage,
    }
}

/// A sealed cell's bytes. A cell of another type holds no envelope.
fn sealed(value: ValueRef<'_>) -> Option<Vec<u8>> {
    match value {
        ValueRef::Blob(bytes) => Some(bytes.to_vec()),
        _ => None,
    }
}

/// A stored time: a non-negative integer, or damage.
fn time(value: ValueRef<'_>) -> Option<u64> {
    match value {
        ValueRef::Integer(ms) => u64::try_from(ms).ok(),
        _ => None,
    }
}

/// The check value. One of another type is present but can't open, like
/// damaged bytes, so unlock tries the stored key on the entries and rewrites it.
fn read_check(conn: &Connection) -> Result<Option<Vec<u8>>, VaultError> {
    conn.query_row("SELECT v FROM meta WHERE k = 'check'", [], |r| Ok(sealed(r.get_ref(0)?).unwrap_or_default())).optional().map_err(classify)
}

fn read_format(conn: &Connection) -> Result<Option<i64>, VaultError> {
    conn.query_row("SELECT v FROM meta WHERE k = 'format'", [], |r| r.get(0)).optional().map_err(classify)
}

/// What a file is, by its schema.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// No schema and at most one page: a file this open just made.
    New,
    Current,
    /// Format 1, whole, with or without its format row, or format 2 or 5,
    /// whole. The upgrade brings it to the current format.
    Upgradable,
}

/// A table and the index of its primary key, as `sqlite_master` lists them.
fn table_objects(name: &str) -> [(String, String, String); 2] {
    [("table".into(), name.into(), name.into()), ("index".into(), format!("sqlite_autoindex_{name}_1"), name.into())]
}

/// A table's columns as `PRAGMA table_info` reports them. A default reads
/// as part of the type, since no column of this module's has one.
fn columns(conn: &Connection, table: &str) -> Result<BTreeSet<(String, String, bool, i64)>, VaultError> {
    let mut stmt = conn.prepare("SELECT name, type || CASE WHEN dflt_value IS NULL THEN '' ELSE ' DEFAULT' END, \"notnull\", pk FROM pragma_table_info(?1)").map_err(classify)?;
    let rows = stmt.query_map([table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(classify)?;
    rows.collect::<Result<_, _>>().map_err(classify)
}

fn expected<'a>(columns: impl IntoIterator<Item = &'a Column>) -> BTreeSet<(String, String, bool, i64)> {
    columns.into_iter().map(|(name, kind, not_null, pk)| (name.to_string(), kind.to_string(), *not_null, *pk)).collect()
}

/// Tables SQLite itself makes and keeps: ANALYZE and PRAGMA optimize write
/// statistics, and AUTOINCREMENT a counter. They hold no vault data.
const SQLITE_TABLES: [&str; 3] = ["sqlite_stat1", "sqlite_stat4", "sqlite_sequence"];

/// Compare the file's schema with the format it says it is, before any query
/// reads its tables: exactly the tables, indexes and columns this module
/// writes, besides SQLite's own tables, nothing missing and nothing more.
/// Anything else is damage, never a table to create: a file that lost a
/// table may still hold its pages. A file with any column or table format 2
/// added is format 2 or later, so one that lost its format row or its
/// never_save table is damage too, never format 1 to upgrade; one with the
/// addresses table is format 5 or later, never format 2; and one with the
/// cards table is format 8, never format 5.
fn layout(conn: &Connection) -> Result<Found, VaultError> {
    let objects: BTreeSet<(String, String, String)> = {
        let mut stmt = conn.prepare("SELECT type, name, tbl_name FROM sqlite_master").map_err(classify)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(classify)?;
        let objects: BTreeSet<(String, String, String)> = rows.collect::<Result<_, _>>().map_err(classify)?;
        objects.into_iter().filter(|(kind, name, table)| !(kind == "table" && name == table && SQLITE_TABLES.contains(&name.as_str()))).collect()
    };
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).map_err(classify)?;
    if objects.is_empty() && pages <= 1 { return Ok(Found::New); }
    let has = |name: &str| objects.contains(&table_objects(name)[0]);
    if !has("meta") || columns(conn, "meta")? != expected(META_COLUMNS) { return Err(VaultError::Damaged); }
    let format = read_format(conn)?;
    check_format(format, FORMAT, &KNOWN_FORMATS)?;
    let (never_save, addresses, cards) = (has("never_save"), has("addresses"), has("cards"));
    let wanted: BTreeSet<_> = ["meta", "entries"].into_iter().chain(never_save.then_some("never_save")).chain(addresses.then_some("addresses"))
        .chain(cards.then_some("cards")).flat_map(table_objects).collect();
    if objects != wanted || (never_save && columns(conn, "never_save")? != expected(NEVER_SAVE_COLUMNS)) || (addresses && columns(conn, "addresses")? != expected(ADDRESS_COLUMNS))
        || (cards && columns(conn, "cards")? != expected(CARD_COLUMNS)) {
        return Err(VaultError::Damaged);
    }
    let entries = columns(conn, "entries")?;
    let current = expected(FORMAT_1_ENTRY_COLUMNS.iter().chain(ADDED_ENTRY_COLUMNS));
    let format_2 = never_save || addresses || cards || ADDED_ENTRY_COLUMNS.iter().any(|(added, ..)| entries.iter().any(|(name, ..)| name == added));
    match format {
        Some(FORMAT) if never_save && addresses && cards && entries == current => Ok(Found::Current),
        Some(FORMAT_5) if never_save && addresses && !cards && entries == current => Ok(Found::Upgradable),
        Some(FORMAT_2) if never_save && !addresses && !cards && entries == current => Ok(Found::Upgradable),
        // Format 1, or format 1 that lost its format row: format 1's columns exactly.
        Some(FORMAT_1) | None if !format_2 && entries == expected(FORMAT_1_ENTRY_COLUMNS) => Ok(Found::Upgradable),
        _ => Err(VaultError::Damaged),
    }
}

/// SQLite's check of every page's structure. Entries can vanish from a
/// damaged b-tree without any read failing, so a file must pass it to open.
fn check_structure(conn: &Connection) -> Result<(), VaultError> {
    #[cfg(test)]
    if SKIP_STRUCTURE.get() { return Ok(()); }
    let result: String = conn.query_row("PRAGMA quick_check(1)", [], |r| r.get(0)).map_err(classify)?;
    if result == "ok" { Ok(()) } else { Err(VaultError::Damaged) }
}

/// Make a new file's tables and its format and vault id rows in one
/// immediate transaction, so a crash leaves it new or whole. The schema is
/// read again under the write lock, since another handle may have made it
/// first. This is the only place a vault id is created, other than Reset.
fn create(conn: &Connection) -> Result<(), VaultError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(classify)?;
    if layout(&tx)? == Found::New {
        tx.execute_batch(&format!("{META}{ENTRIES}{NEVER_SAVE}{ADDRESSES}{CARDS}")).map_err(classify)?;
        tx.execute("INSERT INTO meta (k, v) VALUES ('format', ?1), ('vault_id', ?2)", params![FORMAT, crate::new_id()]).map_err(classify)?;
    }
    tx.commit().map_err(classify)
}

#[cfg(test)]
thread_local! {
    /// Runs halfway through an upgrade, inside its transaction; returning
    /// true fails it there, as a crash would.
    pub(super) static UPGRADE_CUT: std::cell::RefCell<Option<Box<dyn FnMut(&Connection) -> bool>>> = const { std::cell::RefCell::new(None) };
    /// Runs after a reset's commit, to damage the file where the reset didn't reach.
    pub(super) static AFTER_RESET: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
    /// Skips SQLite's structure check at open, to measure what a damaged
    /// file's entries still hold without it.
    pub(super) static SKIP_STRUCTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Bring an upgradable file to the current format in one immediate
/// transaction: SQLite's schema changes are transactional, so a crash leaves
/// the file wholly as it was or wholly current, never a mix. The schema is
/// read again under the write lock, since another handle may have upgraded
/// it first. It adds only the columns and tables formats 2, 5 and 8 added,
/// and never a vault id, then checks the result as an open checks a file, schema
/// and structure, before it commits; a result that fails is rolled back.
/// Nothing here needs the key: format-1 entries stay untagged.
fn upgrade(conn: &Connection) -> Result<(), VaultError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(classify)?;
    match layout(&tx)? {
        Found::Current => return tx.commit().map_err(classify),
        Found::Upgradable => {}
        Found::New => return Err(VaultError::Damaged),
    }
    let present = columns(&tx, "entries")?;
    for (column, kind, ..) in ADDED_ENTRY_COLUMNS {
        if !present.iter().any(|(name, ..)| name == column) { tx.execute(&format!("ALTER TABLE entries ADD COLUMN {column} {kind}"), []).map_err(classify)?; }
    }
    for (table, schema) in [("never_save", NEVER_SAVE), ("addresses", ADDRESSES), ("cards", CARDS)] {
        let present: bool = tx.query_row("SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |r| r.get(0)).map_err(classify)?;
        if !present { tx.execute_batch(schema).map_err(classify)?; }
    }
    #[cfg(test)]
    if UPGRADE_CUT.with_borrow_mut(|cut| cut.as_mut().is_some_and(|cut| cut(&tx))) { return Err(VaultError::Storage); }
    tx.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('format', ?1)", params![FORMAT]).map_err(classify)?;
    if layout(&tx)? != Found::Current { return Err(VaultError::Damaged); }
    check_structure(&tx)?;
    tx.commit().map_err(classify)
}

fn file_vault_id(conn: &Connection) -> Result<String, VaultError> {
    let vault_id: Option<String> = conn.query_row("SELECT v FROM meta WHERE k = 'vault_id'", [], |r| r.get(0)).optional().map_err(classify)?;
    vault_id.filter(|id| !id.is_empty()).ok_or(VaultError::Damaged)
}

/// Every write except `reset` is conditional on the file still holding the id
/// this handle read, inside the same statement, so a handle that missed a
/// Reset by another one never writes under the old id.
impl Db {
    /// Open the file, or make a new one. An existing file must have its
    /// format's exact schema and pass SQLite's structure check, and keeps its
    /// vault id: one that lost it is damage, never given a new one.
    pub fn open(path: &Path) -> Result<Db, VaultError> {
        create_private(path);
        let conn = Connection::open(path).map_err(classify)?;
        conn.execute_batch(PRAGMAS).map_err(classify)?;
        let mut found = layout(&conn)?;
        if found == Found::New {
            create(&conn)?;
            found = layout(&conn)?;
        }
        check_structure(&conn)?;
        let vault_id = file_vault_id(&conn)?;
        if found != Found::Current {
            upgrade(&conn)?;
            if layout(&conn)? != Found::Current { return Err(VaultError::Damaged); }
        }
        Ok(Db { conn, vault_id, path: path.to_owned() })
    }

    /// The vault id the file holds now. Another handle may have reset it.
    pub fn file_vault_id(&self) -> Result<String, VaultError> {
        file_vault_id(&self.conn)
    }

    pub fn check(&self) -> Result<Option<Vec<u8>>, VaultError> {
        read_check(&self.conn)
    }

    /// The check value and the count of entries, addresses and cards
    /// together, from one snapshot of the file.
    pub fn check_and_count(&self) -> Result<(Option<Vec<u8>>, usize), VaultError> {
        let tx = self.conn.unchecked_transaction().map_err(classify)?;
        let check = read_check(&tx)?;
        let count = tx.query_row("SELECT (SELECT COUNT(*) FROM entries) + (SELECT COUNT(*) FROM addresses) + (SELECT COUNT(*) FROM cards)", [], |r| r.get::<_, i64>(0)).map_err(classify)?;
        tx.commit().map_err(classify)?;
        Ok((check, count as usize))
    }

    /// Write the first check value. False when a check already exists, as
    /// another handle's first unlock wrote it, or the file was reset.
    pub fn insert_check(&self, sealed: &[u8]) -> Result<bool, VaultError> {
        self.conn.execute(
            "INSERT INTO meta (k, v) SELECT 'check', ?1
             WHERE (SELECT v FROM meta WHERE k = 'vault_id') = ?2 AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'check')",
            params![sealed, self.vault_id],
        ).map(|n| n > 0).map_err(classify)
    }

    /// Take back a first check value whose key was never stored, if it is
    /// still exactly the one this handle wrote.
    pub fn remove_check(&self, sealed: &[u8]) -> Result<bool, VaultError> {
        self.conn.execute("DELETE FROM meta WHERE k = 'check' AND v = ?1", params![sealed]).map(|n| n > 0).map_err(classify)
    }

    /// False when another handle reset the file since this one read its id.
    pub fn set_check(&self, sealed: &[u8]) -> Result<bool, VaultError> {
        self.conn.execute("INSERT OR REPLACE INTO meta (k, v) SELECT 'check', ?1 WHERE (SELECT v FROM meta WHERE k = 'vault_id') = ?2", params![sealed, self.vault_id])
            .map(|n| n > 0).map_err(classify)
    }

    pub fn count(&self) -> Result<usize, VaultError> {
        self.conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get::<_, i64>(0)).map(|n| n as usize).map_err(classify)
    }

    pub fn rows(&self) -> Result<Vec<SealedRow>, VaultError> {
        let mut stmt = self.conn.prepare(ROWS).map_err(classify)?;
        let rows = stmt.query_map([], SealedRow::read).map_err(classify)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(classify)
    }

    /// One sealed secret of a row whose other cells were read.
    pub fn field(&self, id: &str, field: Field) -> Result<Option<Vec<u8>>, VaultError> {
        let column = match field {
            Field::Password => "password",
            Field::Notes => "notes",
            _ => unreachable!("only secrets are read one at a time"),
        };
        // A cell of another type holds no envelope, so this entry alone is unreadable.
        self.conn.query_row(&format!("SELECT {column} FROM entries WHERE id = ?1"), params![id], |r| Ok(sealed(r.get_ref(0)?).unwrap_or_default())).optional().map_err(classify)
    }

    /// False when another handle reset the file since this one read its id.
    pub fn insert(&self, id: &str, now_ms: u64, entry: &SealedEntry<'_>) -> Result<bool, VaultError> {
        self.conn.execute(
            "INSERT INTO entries (id, created_ms, updated_ms, origin, username, password, notes, profile)
             SELECT ?1, ?2, ?2, ?3, ?4, ?5, ?6, ?8 WHERE (SELECT v FROM meta WHERE k = 'vault_id') = ?7",
            params![id, now_ms as i64, entry.origin, entry.username, entry.password, entry.notes, self.vault_id, entry.profile],
        ).map(|n| n > 0).map_err(classify)
    }

    /// False when the row no longer exists or another handle reset the file.
    pub fn update(&self, id: &str, now_ms: u64, entry: &SealedEntry<'_>) -> Result<bool, VaultError> {
        let updated = self.conn.execute(
            "UPDATE entries SET updated_ms = ?2, origin = ?3, username = ?4, password = ?5, notes = ?6, profile = ?8
             WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?7",
            params![id, now_ms as i64, entry.origin, entry.username, entry.password, entry.notes, self.vault_id, entry.profile],
        ).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(updated)
    }

    /// False when the row no longer exists or another handle reset the file.
    pub fn delete(&self, id: &str) -> Result<bool, VaultError> {
        let deleted = self.conn.execute("DELETE FROM entries WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?2", params![id, self.vault_id]).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(deleted)
    }

    /// Delete a row whose id cell can't be read. False when it no longer
    /// exists as read or another handle reset the file.
    pub fn delete_row(&self, row: &StoredId) -> Result<bool, VaultError> {
        let deleted = self.conn.execute(
            "DELETE FROM entries WHERE rowid = ?1 AND typeof(id) = ?2 AND CAST(id AS BLOB) IS ?3 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?4",
            params![row.rowid, row.kind, row.bytes, self.vault_id],
        ).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(deleted)
    }

    /// Reseal one entry's profile tag. False when the row no longer exists or
    /// another handle reset the file.
    pub fn set_profile(&self, id: &str, sealed: &[u8]) -> Result<bool, VaultError> {
        self.conn.execute("UPDATE entries SET profile = ?2 WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?3", params![id, sealed, self.vault_id])
            .map(|n| n > 0).map_err(classify)
    }

    /// Record a fill. False when the row no longer exists or another handle
    /// reset the file.
    pub fn mark_used(&self, id: &str, now_ms: u64) -> Result<bool, VaultError> {
        self.conn.execute("UPDATE entries SET used_ms = ?2 WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?3", params![id, now_ms as i64, self.vault_id])
            .map(|n| n > 0).map_err(classify)
    }

    /// Only rows of readable text: a retyped or non-UTF-8 row matches no site,
    /// so it is left out instead of failing the list. The list is read from
    /// the table itself, never its index, so a damaged index hides no row.
    pub fn never_saved(&self, profile: &str) -> Result<Vec<String>, VaultError> {
        let mut stmt = self.conn.prepare("SELECT origin FROM never_save NOT INDEXED WHERE profile = ?1 ORDER BY origin").map_err(classify)?;
        let rows = stmt.query_map([profile], |r| Ok(match r.get_ref(0)? {
            ValueRef::Text(origin) => std::str::from_utf8(origin).ok().map(str::to_owned),
            _ => None,
        })).map_err(classify)?;
        rows.collect::<Result<Vec<_>, _>>().map(|rows| rows.into_iter().flatten().collect()).map_err(classify)
    }

    pub fn is_never_saved(&self, profile: &str, origin: &str) -> Result<bool, VaultError> {
        self.conn.query_row("SELECT COUNT(*) > 0 FROM never_save NOT INDEXED WHERE profile = ?1 AND origin = ?2", [profile, origin], |r| r.get(0)).map_err(classify)
    }

    pub fn never_save(&self, profile: &str, origin: &str, now_ms: u64) -> Result<(), VaultError> {
        self.conn.execute("INSERT OR IGNORE INTO never_save (profile, origin, created_ms) VALUES (?1, ?2, ?3)", params![profile, origin, now_ms as i64]).map(|_| ()).map_err(classify)
    }

    pub fn remove_never_save(&self, profile: &str, origin: &str) -> Result<(), VaultError> {
        self.conn.execute("DELETE FROM never_save WHERE profile = ?1 AND origin = ?2", [profile, origin]).map(|_| ()).map_err(classify)
    }

    pub fn address_count(&self) -> Result<usize, VaultError> {
        self.conn.query_row("SELECT COUNT(*) FROM addresses", [], |r| r.get::<_, i64>(0)).map(|n| n as usize).map_err(classify)
    }

    pub fn address_rows(&self) -> Result<Vec<SealedAddress>, VaultError> {
        let mut stmt = self.conn.prepare(ADDRESS_ROWS).map_err(classify)?;
        let rows = stmt.query_map([], SealedAddress::read).map_err(classify)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(classify)
    }

    /// False when another handle reset the file since this one read its id.
    pub fn insert_address(&self, id: &str, now_ms: u64, profile: &[u8], address: &[u8]) -> Result<bool, VaultError> {
        self.conn.execute(
            "INSERT INTO addresses (id, created_ms, updated_ms, profile, address)
             SELECT ?1, ?2, ?2, ?3, ?4 WHERE (SELECT v FROM meta WHERE k = 'vault_id') = ?5",
            params![id, now_ms as i64, profile, address, self.vault_id],
        ).map(|n| n > 0).map_err(classify)
    }

    /// Replace an address's parts; its profile stays. False when the row no
    /// longer exists or another handle reset the file.
    pub fn update_address(&self, id: &str, now_ms: u64, address: &[u8]) -> Result<bool, VaultError> {
        let updated = self.conn.execute(
            "UPDATE addresses SET updated_ms = ?2, address = ?3 WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?4",
            params![id, now_ms as i64, address, self.vault_id],
        ).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(updated)
    }

    /// False when the row no longer exists or another handle reset the file.
    pub fn delete_address(&self, id: &str) -> Result<bool, VaultError> {
        let deleted = self.conn.execute("DELETE FROM addresses WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?2", params![id, self.vault_id]).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(deleted)
    }

    /// Delete an address row whose id cell can't be read, as `delete_row` does an entry's.
    pub fn delete_address_row(&self, row: &StoredId) -> Result<bool, VaultError> {
        let deleted = self.conn.execute(
            "DELETE FROM addresses WHERE rowid = ?1 AND typeof(id) = ?2 AND CAST(id AS BLOB) IS ?3 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?4",
            params![row.rowid, row.kind, row.bytes, self.vault_id],
        ).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(deleted)
    }

    /// Record a fill. False when the row no longer exists or another handle
    /// reset the file.
    pub fn mark_address_used(&self, id: &str, now_ms: u64) -> Result<bool, VaultError> {
        self.conn.execute("UPDATE addresses SET used_ms = ?2 WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?3", params![id, now_ms as i64, self.vault_id])
            .map(|n| n > 0).map_err(classify)
    }

    pub fn card_count(&self) -> Result<usize, VaultError> {
        self.conn.query_row("SELECT COUNT(*) FROM cards", [], |r| r.get::<_, i64>(0)).map(|n| n as usize).map_err(classify)
    }

    pub fn card_rows(&self) -> Result<Vec<SealedCard>, VaultError> {
        let mut stmt = self.conn.prepare(CARD_ROWS).map_err(classify)?;
        let rows = stmt.query_map([], SealedCard::read).map_err(classify)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(classify)
    }

    /// One card's sealed number. A cell of another type holds no envelope,
    /// so this card alone is unreadable.
    pub fn card_number(&self, id: &str) -> Result<Option<Vec<u8>>, VaultError> {
        self.conn.query_row("SELECT number FROM cards WHERE id = ?1", params![id], |r| Ok(sealed(r.get_ref(0)?).unwrap_or_default())).optional().map_err(classify)
    }

    /// False when another handle reset the file since this one read its id.
    pub fn insert_card(&self, id: &str, now_ms: u64, profile: &[u8], card: &[u8], number: &[u8]) -> Result<bool, VaultError> {
        self.conn.execute(
            "INSERT INTO cards (id, created_ms, updated_ms, profile, card, number)
             SELECT ?1, ?2, ?2, ?3, ?4, ?5 WHERE (SELECT v FROM meta WHERE k = 'vault_id') = ?6",
            params![id, now_ms as i64, profile, card, number, self.vault_id],
        ).map(|n| n > 0).map_err(classify)
    }

    /// Replace a card's details, and its number when `number` is given; its
    /// profile stays. False when the row no longer exists or another handle
    /// reset the file.
    pub fn update_card(&self, id: &str, now_ms: u64, card: &[u8], number: Option<&[u8]>) -> Result<bool, VaultError> {
        let updated = self.conn.execute(
            "UPDATE cards SET updated_ms = ?2, card = ?3, number = COALESCE(?4, number) WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?5",
            params![id, now_ms as i64, card, number, self.vault_id],
        ).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(updated)
    }

    /// False when the row no longer exists or another handle reset the file.
    pub fn delete_card(&self, id: &str) -> Result<bool, VaultError> {
        let deleted = self.conn.execute("DELETE FROM cards WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?2", params![id, self.vault_id]).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(deleted)
    }

    /// Delete a card row whose id cell can't be read, as `delete_row` does an entry's.
    pub fn delete_card_row(&self, row: &StoredId) -> Result<bool, VaultError> {
        let deleted = self.conn.execute(
            "DELETE FROM cards WHERE rowid = ?1 AND typeof(id) = ?2 AND CAST(id AS BLOB) IS ?3 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?4",
            params![row.rowid, row.kind, row.bytes, self.vault_id],
        ).map(|n| n > 0).map_err(classify)?;
        self.truncate_wal();
        Ok(deleted)
    }

    /// Record a fill. False when the row no longer exists or another handle
    /// reset the file.
    pub fn mark_card_used(&self, id: &str, now_ms: u64) -> Result<bool, VaultError> {
        self.conn.execute("UPDATE cards SET used_ms = ?2 WHERE id = ?1 AND (SELECT v FROM meta WHERE k = 'vault_id') = ?3", params![id, now_ms as i64, self.vault_id])
            .map(|n| n > 0).map_err(classify)
    }

    /// Delete every entry, address and card, the never-save list and the
    /// check value, and give the file a new vault id and the current format, in one transaction.
    /// Writing both rows whole also repairs a lost or retyped one, so the reset
    /// vault opens without a relaunch. It succeeds only if the file then
    /// passes `verify`: a reset can leave damage it never reached.
    pub fn reset(&mut self) -> Result<(), VaultError> {
        let vault_id = crate::new_id();
        let tx = self.conn.unchecked_transaction().map_err(classify)?;
        tx.execute("DELETE FROM entries", []).map_err(classify)?;
        tx.execute("DELETE FROM never_save", []).map_err(classify)?;
        tx.execute("DELETE FROM addresses", []).map_err(classify)?;
        tx.execute("DELETE FROM cards", []).map_err(classify)?;
        tx.execute("DELETE FROM meta WHERE k = 'check'", []).map_err(classify)?;
        tx.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('vault_id', ?1), ('format', ?2)", params![vault_id, FORMAT]).map_err(classify)?;
        tx.commit().map_err(classify)?;
        self.vault_id = vault_id;
        self.truncate_wal();
        #[cfg(test)]
        AFTER_RESET.with_borrow_mut(|damage| if let Some(damage) = damage.as_mut() { damage() });
        self.verify()
    }

    /// Check the file as a fresh open finds it: its schema, SQLite's structure
    /// check and its vault id, then the check value and every entry, address
    /// and card row read as an unlock reads them, and the never-save list counted.
    fn verify(&self) -> Result<(), VaultError> {
        let fresh = Db::open(&self.path)?;
        fresh.check_and_count()?;
        fresh.rows()?;
        fresh.address_rows()?;
        fresh.card_rows()?;
        fresh.conn.query_row("SELECT COUNT(*) FROM never_save", [], |r| r.get::<_, i64>(0)).map_err(classify)?;
        Ok(())
    }

    /// Copy the WAL into the database file and empty it, so the frames holding
    /// replaced or deleted values don't linger; secure_delete has already
    /// zeroed them in the pages that replace them. Best effort: if it fails, the
    /// next checkpoint does the same.
    fn truncate_wal(&self) {
        let _ = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }

    #[cfg(test)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

// ---------------------------------------------------------------------------
// Recover Passwords: what a damaged file still holds
// ---------------------------------------------------------------------------

/// What a damaged file still yields, read through a read-only connection
/// that never writes it or its WAL.
pub(super) struct Salvage {
    pub vault_id: String,
    pub check: Option<Vec<u8>>,
    pub entries: Table<SalvagedRow>,
    pub addresses: Table<SalvagedAddress>,
    pub cards: Table<SalvagedCard>,
    pub never_save: NeverSaveSalvage,
}

/// What the reads of one table with an `id` key still hold.
pub(super) struct Table<T> {
    /// Every row any read of the table or of its index returned, each
    /// distinct version once, in rowid order, none left out but those that
    /// `is_row` says are none. Nothing here is verified.
    pub rows: BTreeSet<SeenRow<T>>,
    /// Each read's rows, by rowid and id, and whether the read went to its
    /// end without an error. A read that did is complete only if it saw
    /// every row another read saw, which the caller decides, as it knows
    /// which versions are one row.
    pub reads: Vec<(Vec<(i64, Option<String>)>, bool)>,
    /// The table never existed, in a file of a format before it.
    pub never_existed: bool,
}

/// One row as a read returned it: where it is, its id if that reads as
/// text, and its cells if every one reads as the table stores them. A read
/// of the keys alone returns no cells, nor does a row whose cells all read
/// as NULL, as a damaged record header reads a real one. It is still a row,
/// if its rowid is above 0: see `is_row`.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SeenRow<T> {
    pub rowid: i64,
    pub id: Option<String>,
    pub cells: Option<T>,
}

/// The never-save list as a damaged file still holds it.
pub(super) struct NeverSaveSalvage {
    /// Each (profile, origin) any read returned, with the first time it was
    /// added that a read returned for it, else now.
    pub rows: Vec<(String, String, i64)>,
    /// Distinct rows any read saw, readable or not, none left out.
    pub found: usize,
    /// Whether some read went to its end without an error having seen every
    /// row any read saw, or the file is format 1, which has no list.
    pub complete: bool,
}

/// One entry row's cells as stored: still sealed, its times as read.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SalvagedRow {
    pub id: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub used_ms: Option<u64>,
    pub origin: Vec<u8>,
    pub username: Vec<u8>,
    pub password: Vec<u8>,
    pub notes: Vec<u8>,
    pub profile: Option<Vec<u8>>,
}

/// One address row's cells as stored: still sealed, its times as read.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SalvagedAddress {
    pub id: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub used_ms: Option<u64>,
    pub profile: Vec<u8>,
    pub address: Vec<u8>,
}

/// One card row's cells as stored: still sealed, its times as read.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SalvagedCard {
    pub id: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub used_ms: Option<u64>,
    pub profile: Vec<u8>,
    pub card: Vec<u8>,
    pub number: Vec<u8>,
}

fn read_only(path: &Path) -> Result<Connection, VaultError> {
    use rusqlite::OpenFlags;
    if std::fs::symlink_metadata(path).is_err() { return Err(VaultError::Storage); }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(classify)
}

/// Every row `sql` returns, up to the first that fails to read: damage part
/// way through a b-tree hides only what lies past it in that direction.
/// Whether the read reached its end without an error.
fn rows_until_error<T>(conn: &Connection, sql: &str, mut each: impl FnMut(&rusqlite::Row<'_>) -> Option<T>) -> (Vec<T>, bool) {
    let Ok(mut stmt) = conn.prepare(sql) else { return (Vec::new(), false) };
    let Ok(mut rows) = stmt.query([]) else { return (Vec::new(), false) };
    let mut out = Vec::new();
    loop {
        match rows.next() {
            Ok(Some(row)) => out.extend(each(row)),
            Ok(None) => return (out, true),
            Err(_) => return (out, false),
        }
    }
}

/// The meta rows by key, read by position, so a renamed column still reads:
/// from the table both ways, then through its index. The vault id, the
/// check and the format.
fn salvage_meta(conn: &Connection) -> (Option<String>, Option<Vec<u8>>, Option<i64>) {
    let mut pairs: Vec<(String, Vec<u8>, bool)> = Vec::new();
    let mut format = None;
    for sql in ["SELECT * FROM meta NOT INDEXED", "SELECT * FROM meta NOT INDEXED ORDER BY rowid DESC", "SELECT * FROM meta WHERE k IN ('format', 'vault_id', 'check')"] {
        pairs.extend(rows_until_error(conn, sql, |r| {
            let ValueRef::Text(key) = r.get_ref(0).ok()? else { return None };
            let key = std::str::from_utf8(key).ok()?.to_owned();
            match r.get_ref(1).ok()? {
                ValueRef::Text(value) => Some((key, value.to_vec(), true)),
                ValueRef::Blob(value) => Some((key, value.to_vec(), false)),
                ValueRef::Integer(value) if key == "format" => {
                    format = format.or(Some(value));
                    None
                }
                _ => None,
            }
        }).0);
    }
    let vault_id = pairs.iter().find(|(key, value, text)| key == "vault_id" && *text && !value.is_empty()).and_then(|(_, value, _)| String::from_utf8(value.clone()).ok());
    let check = pairs.iter().find(|(key, _, text)| key == "check" && !*text).map(|(_, value, _)| value.clone());
    (vault_id, check, format)
}

/// The vault id of a damaged file, if it can still be read.
pub(super) fn salvage_vault_id(path: &Path) -> Option<String> {
    salvage_meta(&read_only(path).ok()?).0
}

/// The ways a table keyed by `id` is read: the table, then its index, each
/// forward and backward. Through the index, SQLite reads the id from the
/// index and the other cells from the row it names.
fn id_reads(table: &str) -> [String; 4] {
    [
        "NOT INDEXED".into(),
        "NOT INDEXED ORDER BY rowid DESC".into(),
        format!("INDEXED BY sqlite_autoindex_{table}_1 ORDER BY id"),
        format!("INDEXED BY sqlite_autoindex_{table}_1 ORDER BY id DESC"),
    ]
}
const NEVER_SAVE_READS: [&str; 4] = [
    "NOT INDEXED",
    "NOT INDEXED ORDER BY rowid DESC",
    "INDEXED BY sqlite_autoindex_never_save_1 ORDER BY profile, origin",
    "INDEXED BY sqlite_autoindex_never_save_1 ORDER BY profile DESC, origin DESC",
];

/// Read everything a damaged file still holds, without writing it. Every
/// read takes each row's cells, by position, from the row it returns, and
/// no row is read again by its rowid: damage can hide a row from a seek
/// that a scan still reads. The entries, the addresses and the cards are
/// each read every way, whole and without their last cell, since a last cell that
/// can't be read stops a read of the whole row; then their keys alone,
/// every way, so a row whose cells stop every other read is still seen.
/// Damaged without a readable vault id, which names the key.
pub(super) fn salvage(path: &Path) -> Result<Salvage, VaultError> {
    let conn = read_only(path)?;
    let (vault_id, check, format) = salvage_meta(&conn);
    let vault_id = vault_id.ok_or(VaultError::Damaged)?;
    // One time for every cell that can't be read, so versions of a row compare equal.
    let now = crate::now_ms();
    let entries = salvage_table(&conn, "entries", FORMAT_1_ENTRY_COLUMNS.len(), |r| salvaged_row(r, now));
    let mut addresses = salvage_table(&conn, "addresses", ADDRESS_COLUMNS.len() - 1, |r| salvaged_address(r, now));
    let mut cards = salvage_table(&conn, "cards", CARD_COLUMNS.len() - 1, |r| salvaged_card(r, now));
    // A file of a format before addresses, or cards, has none to read.
    // Without a format row, only format 1's columns say so: format 1 is the
    // one format that may lack the row.
    let format_1_columns = conn.prepare("SELECT * FROM entries").is_ok_and(|stmt| stmt.column_count() == FORMAT_1_ENTRY_COLUMNS.len());
    let before_addresses = matches!(format, Some(FORMAT_1 | FORMAT_2)) || (format.is_none() && format_1_columns);
    addresses.never_existed = before_addresses && conn.prepare("SELECT 1 FROM addresses").is_err();
    let before_cards = before_addresses || format == Some(FORMAT_5);
    cards.never_existed = before_cards && conn.prepare("SELECT 1 FROM cards").is_err();
    let never_save = salvage_never_save(&conn, format, now);
    Ok(Salvage { vault_id, check, entries, addresses, cards, never_save })
}

/// One table keyed by `id`, read every way `salvage` reads it. `required`
/// is how many leading columns every version of the table has; a read
/// without the last column runs only when there are more. `cells` reads a
/// row's cells after its rowid.
fn salvage_table<T: Ord>(conn: &Connection, table: &str, required: usize, cells: impl Fn(&rusqlite::Row<'_>) -> Option<T>) -> Table<T> {
    let (mut rows, mut reads) = (BTreeSet::new(), Vec::new());
    // A row read without its last cell is kept only where no read had it whole.
    let mut read_whole = BTreeSet::new();
    let lists = std::iter::once("*".to_owned()).chain(all_but_last(conn, table, required));
    for (n, list) in lists.enumerate() {
        for how in id_reads(table) {
            let (read, whole) = rows_until_error(conn, &format!("SELECT rowid, {list} FROM {table} {how}"), |r| Some(SeenRow { rowid: r.get(0).ok()?, id: text(r.get_ref(1).ok()?), cells: cells(r) }).filter(|_| is_row(r)));
            reads.push((read.iter().map(|seen| (seen.rowid, seen.id.clone())).collect(), whole));
            for seen in read {
                if n > 0 && read_whole.contains(&(seen.rowid, seen.id.clone())) { continue; }
                if n == 0 && seen.cells.is_some() { read_whole.insert((seen.rowid, seen.id.clone())); }
                rows.insert(seen);
            }
        }
    }
    for how in id_reads(table) {
        // The index holds the id; the table's keys are rowids alone.
        let id = if how.starts_with("INDEXED") { "id" } else { "NULL" };
        let (read, whole) = rows_until_error(conn, &format!("SELECT rowid, {id} FROM {table} {how}"), |r| Some(SeenRow { rowid: r.get(0).ok()?, id: text(r.get_ref(1).ok()?), cells: None }).filter(|_| is_row(r)));
        reads.push((read.iter().map(|seen| (seen.rowid, seen.id.clone())).collect(), whole));
        rows.extend(read);
    }
    Table { rows, reads, never_existed: false }
}

/// The never-save list, read every way as the entries are. A row whose
/// profile and origin read is kept with them, and with the time it was
/// added if that reads too, else `now`. Every row any read returned, that
/// `is_row` says is one, is counted: by its profile and origin, else by
/// those another read gave its rowid, else by its rowid. The list is read
/// in full when one read went to its end having seen every row so counted. A table that can't be read at
/// all in a file with any part of format 2, which added it, isn't.
fn salvage_never_save(conn: &Connection, format: Option<i64>, now: u64) -> NeverSaveSalvage {
    let key = |r: &rusqlite::Row<'_>| match (r.get_ref(1).ok()?, r.get_ref(2).ok()?) {
        (ValueRef::Text(profile), ValueRef::Text(origin)) => Some((std::str::from_utf8(profile).ok()?.to_owned(), std::str::from_utf8(origin).ok()?.to_owned())),
        _ => None,
    };
    let mut reads = Vec::new();
    for how in NEVER_SAVE_READS {
        reads.push(rows_until_error(conn, &format!("SELECT rowid, * FROM never_save {how}"), |r| {
            if !is_row(r) { return None; }
            let created = match r.get_ref(3) {
                Ok(ValueRef::Integer(ms)) => Some(ms),
                _ => None,
            };
            Some((r.get::<_, i64>(0).ok()?, key(r), created))
        }));
    }
    for how in NEVER_SAVE_READS {
        let key_cells = if how.starts_with("INDEXED") { "profile, origin" } else { "NULL, NULL" };
        reads.push(rows_until_error(conn, &format!("SELECT rowid, {key_cells} FROM never_save {how}"), |r| Some((r.get::<_, i64>(0).ok()?, key(r), None)).filter(|_| is_row(r))));
    }
    let every = || reads.iter().flat_map(|(read, _)| read);
    let mut kept: std::collections::BTreeMap<&(String, String), Option<i64>> = std::collections::BTreeMap::new();
    let mut named = std::collections::BTreeMap::new();
    for (rowid, key, created) in every() {
        let Some(key) = key else { continue };
        let added = kept.entry(key).or_default();
        *added = added.or(*created);
        named.entry(*rowid).or_insert(key);
    }
    type Seen = (i64, Option<(String, String)>, Option<i64>);
    fn row<'a>(named: &std::collections::BTreeMap<i64, &'a (String, String)>, (rowid, key, _): &'a Seen) -> Result<&'a (String, String), i64> {
        key.as_ref().or_else(|| named.get(rowid).copied()).ok_or(*rowid)
    }
    let seen: BTreeSet<_> = every().map(|seen| row(&named, seen)).collect();
    let in_full = reads.iter().any(|(read, whole)| *whole && read.iter().map(|seen| row(&named, seen)).collect::<BTreeSet<_>>() == seen);
    let format_2 = matches!(format, Some(FORMAT | FORMAT_5 | FORMAT_2)) || conn.prepare("SELECT * FROM entries").is_ok_and(|stmt| stmt.column_count() > FORMAT_1_ENTRY_COLUMNS.len());
    NeverSaveSalvage {
        found: seen.len(),
        rows: kept.into_iter().map(|((profile, origin), created)| (profile.clone(), origin.clone(), created.unwrap_or(now as i64))).collect(),
        complete: in_full || (!format_2 && conn.prepare("SELECT 1 FROM never_save").is_err()),
    }
}

/// Whether a row a read returned, its rowid first, is one. A damaged cell
/// count or cell pointer reads a "row" where there is none, out of the
/// page's header or its free space: at rowid 0, with every cell NULL. The
/// vault never writes a rowid below 1, so a row with no cell that reads and
/// a rowid of 0 or below isn't one, unless its rowid, its record header and
/// its index entry were all damaged. Every other row is, even with every
/// cell NULL.
fn is_row(r: &rusqlite::Row<'_>) -> bool {
    r.get::<_, i64>(0).is_ok_and(|rowid| rowid > 0) || (1..r.as_ref().column_count()).any(|i| r.get_ref(i).is_ok_and(|cell| cell != ValueRef::Null))
}

/// A select list of every column of `table` but its last, which reads as
/// NULL, by the names its schema gives them, if it has more than `required`
/// and every name is valid text.
fn all_but_last(conn: &Connection, table: &str, required: usize) -> Option<String> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid").ok()?;
    let names: Vec<String> = stmt.query_map([table], |r| r.get(0)).ok()?.collect::<Result<_, _>>().ok()?;
    let quoted = |name: &String| format!("\"{}\", ", name.replace('"', "\"\""));
    (names.len() > required).then(|| names[..names.len() - 1].iter().map(quoted).collect::<String>() + "NULL")
}

/// A cell's text, if it is valid non-empty text.
fn text(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Text(text) => std::str::from_utf8(text).ok().filter(|text| !text.is_empty()).map(str::to_owned),
        _ => None,
    }
}

/// An entry row's cells by their position in format 2's table, which is
/// also format 1's with two columns added, after the rowid. A time that
/// can't be read is `now`, since times aren't sealed; a sealed cell of
/// another type, or a damaged id, which every seal is bound to, gives none.
fn salvaged_row(r: &rusqlite::Row<'_>, now: u64) -> Option<SalvagedRow> {
    let count = r.as_ref().column_count() - 1;
    if count < FORMAT_1_ENTRY_COLUMNS.len() { return None; }
    let cell = |i: usize| r.get_ref(i + 1).ok();
    let id = text(cell(0)?).filter(|id| !id.starts_with(DAMAGED_ID))?;
    let created_ms = cell(1).and_then(time).unwrap_or(now);
    let updated_ms = cell(2).and_then(time).unwrap_or(created_ms);
    let [origin, username, password, notes] = [3, 4, 5, 6].map(|i| cell(i).and_then(sealed));
    let profile = match (count > 7).then(|| cell(7)).flatten() {
        None | Some(ValueRef::Null) => None,
        Some(value) => Some(sealed(value)?),
    };
    let used_ms = (count > 8).then(|| cell(8).and_then(time)).flatten();
    Some(SalvagedRow { id, created_ms, updated_ms, used_ms, origin: origin?, username: username?, password: password?, notes: notes?, profile })
}

/// An address row's cells by their position in the addresses table, after
/// the rowid, as `salvaged_row` reads an entry's.
fn salvaged_address(r: &rusqlite::Row<'_>, now: u64) -> Option<SalvagedAddress> {
    if r.as_ref().column_count() - 1 < ADDRESS_COLUMNS.len() { return None; }
    let cell = |i: usize| r.get_ref(i + 1).ok();
    let id = text(cell(0)?).filter(|id| !id.starts_with(DAMAGED_ID))?;
    let created_ms = cell(1).and_then(time).unwrap_or(now);
    let updated_ms = cell(2).and_then(time).unwrap_or(created_ms);
    let (profile, address) = (cell(3).and_then(sealed)?, cell(4).and_then(sealed)?);
    let used_ms = cell(5).and_then(time);
    Some(SalvagedAddress { id, created_ms, updated_ms, used_ms, profile, address })
}

/// A card row's cells by their position in the cards table, after the
/// rowid, as `salvaged_row` reads an entry's.
fn salvaged_card(r: &rusqlite::Row<'_>, now: u64) -> Option<SalvagedCard> {
    if r.as_ref().column_count() - 1 < CARD_COLUMNS.len() { return None; }
    let cell = |i: usize| r.get_ref(i + 1).ok();
    let id = text(cell(0)?).filter(|id| !id.starts_with(DAMAGED_ID))?;
    let created_ms = cell(1).and_then(time).unwrap_or(now);
    let updated_ms = cell(2).and_then(time).unwrap_or(created_ms);
    let (profile, card, number) = (cell(3).and_then(sealed)?, cell(4).and_then(sealed)?, cell(5).and_then(sealed)?);
    let used_ms = cell(6).and_then(time);
    Some(SalvagedCard { id, created_ms, updated_ms, used_ms, profile, card, number })
}

/// Write a new vault file at `path` holding `rows`, `addresses`, `cards` and
/// the never-save list under `vault_id` and `check`, in one transaction with a
/// rollback journal, so the finished file is one file with no WAL. It is
/// then checked as an open checks a file and flushed to the disk. Anything
/// already at `path` is a working file an earlier attempt left, and is replaced.
pub(super) fn write_recovered(path: &Path, vault_id: &str, check: &[u8], rows: &[&SalvagedRow], addresses: &[&SalvagedAddress], cards: &[&SalvagedCard], never_save: &[(String, String, i64)]) -> Result<(), VaultError> {
    for suffix in ["-wal", "-shm", "-journal", ""] {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(VaultError::Storage),
            _ => {}
        }
    }
    create_private(path);
    let mut conn = Connection::open(path).map_err(classify)?;
    conn.execute_batch("PRAGMA secure_delete = ON;").map_err(classify)?;
    let tx = conn.transaction().map_err(classify)?;
    tx.execute_batch(&format!("{META}{ENTRIES}{NEVER_SAVE}{ADDRESSES}{CARDS}")).map_err(classify)?;
    tx.execute("INSERT INTO meta (k, v) VALUES ('format', ?1), ('vault_id', ?2), ('check', ?3)", params![FORMAT, vault_id, check]).map_err(classify)?;
    for row in rows {
        tx.execute(
            "INSERT INTO entries (id, created_ms, updated_ms, origin, username, password, notes, profile, used_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![row.id, row.created_ms as i64, row.updated_ms as i64, row.origin, row.username, row.password, row.notes, row.profile, row.used_ms.map(|ms| ms as i64)],
        ).map_err(classify)?;
    }
    for row in addresses {
        tx.execute(
            "INSERT INTO addresses (id, created_ms, updated_ms, profile, address, used_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![row.id, row.created_ms as i64, row.updated_ms as i64, row.profile, row.address, row.used_ms.map(|ms| ms as i64)],
        ).map_err(classify)?;
    }
    for row in cards {
        tx.execute(
            "INSERT INTO cards (id, created_ms, updated_ms, profile, card, number, used_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![row.id, row.created_ms as i64, row.updated_ms as i64, row.profile, row.card, row.number, row.used_ms.map(|ms| ms as i64)],
        ).map_err(classify)?;
    }
    for (profile, origin, created_ms) in never_save {
        tx.execute("INSERT OR IGNORE INTO never_save (profile, origin, created_ms) VALUES (?1, ?2, ?3)", params![profile, origin, created_ms]).map_err(classify)?;
    }
    tx.commit().map_err(classify)?;
    if layout(&conn)? != Found::Current { return Err(VaultError::Damaged); }
    check_structure(&conn)?;
    let counts: (i64, i64, i64) = conn.query_row("SELECT (SELECT COUNT(*) FROM entries), (SELECT COUNT(*) FROM addresses), (SELECT COUNT(*) FROM cards)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(classify)?;
    if file_vault_id(&conn)? != vault_id || counts != (rows.len() as i64, addresses.len() as i64, cards.len() as i64) { return Err(VaultError::Damaged); }
    drop(conn);
    // F_FULLFSYNC on macOS: the new file is on the disk before it replaces anything.
    std::fs::File::open(path).and_then(|file| file.sync_all()).map_err(|_| VaultError::Storage)
}

/// Owner-only on Unix, created before SQLite opens it so its WAL and shared
/// memory files inherit the mode. Contents are ciphertext either way; this
/// hides the metadata from other local users.
fn create_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path);
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}
