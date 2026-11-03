//! Each profile's lists on this Mac: `Content Blocking/Profile-<hex id>`
//! beside the library holds a copy of each list, named for its SHA-256, and
//! `lists.json`, which names them and the bundled lists the profile turned
//! off. Neither is in the library database, so library backups, restores
//! and sync never hold them.

use super::matcher::{self, CompiledList};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const FOLDER: &str = "Content Blocking";
/// The largest list file accepted.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Which switch applies a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListKind {
    Ads,
    CookieBanners,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    /// The SHA-256 of its text, in hex; also its copy's name.
    pub id: String,
    /// The file name it was added from.
    pub name: String,
    pub kind: ListKind,
    pub added_ms: u64,
    pub rules: usize,
    pub unsupported: usize,
}

/// A profile's lists, why some or all couldn't be read, and the ids of
/// the bundled lists it turned off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved {
    pub lists: Vec<ListEntry>,
    pub notice: Option<String>,
    pub bundled_off: BTreeSet<String>,
}

/// Why a file wasn't added. The lists stay as they were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddError {
    Unreadable,
    TooLarge,
    NotText,
    NoRules,
    Duplicate,
    Save(String),
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddError::Unreadable => f.write_str("Couldn't read the file. Check that it still exists and that Crossbow may open it."),
            AddError::TooLarge => f.write_str("This file is larger than 16 MB, so it wasn't added."),
            AddError::NotText => f.write_str("This file isn't a UTF-8 text filter list, so it wasn't added."),
            AddError::NoRules => f.write_str("This file has no rules Crossbow can use, so it wasn't added."),
            AddError::Duplicate => f.write_str("This list is already added."),
            AddError::Save(error) => write!(f, "Couldn't save the list ({error}). Check disk space and data-folder permissions, then try again."),
        }
    }
}

const MANIFEST: &str = "lists.json";
const VERSION: u64 = 1;
const PARTIAL: &str = ".partial";
/// A list's name is kept to this many characters.
const MAX_NAME: usize = 120;
const DAMAGED: &str = "Saved content blocking lists couldn't be read, so none are in use. Add them again.";
const NEWER: &str = "Content blocking lists were saved by a newer version of Crossbow, so none are in use. Adding a list here replaces them.";
pub const CHANGED: &str = "Couldn't read this list. Remove it, then add it again.";
const REMOVE_FAILED: &str = "Couldn't remove the list. Check disk space and data-folder permissions, then try again.";
const SAVE_FAILED: &str = "Couldn't save the change. Check disk space and data-folder permissions, then try again.";
const NEWER_KEPT: &str = "Content blocking lists were saved by a newer version of Crossbow, so this change can't be saved here.";

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u64,
    lists: Vec<Stored>,
    /// Absent before bundled lists: all on.
    #[serde(default)]
    bundled_off: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    name: String,
    kind: String,
    added_ms: u64,
    rules: usize,
    unsupported: usize,
}

fn kind_key(kind: ListKind) -> &'static str {
    match kind {
        ListKind::Ads => "ads",
        ListKind::CookieBanners => "cookie_banners",
    }
}

/// 64 lowercase hex digits: never a path.
fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A bundled list's id: up to 64 lowercase letters, digits and `-`.
fn valid_bundled_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn folder(data_dir: &Path, profile: &str) -> Option<PathBuf> {
    Some(data_dir.join(FOLDER).join(crate::captures::folder_name(profile)?))
}

enum Unread {
    Damaged,
    Newer,
}

fn read_manifest(text: &str) -> Result<(Vec<ListEntry>, BTreeSet<String>), Unread> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| Unread::Damaged)?;
    if value.get("version").and_then(serde_json::Value::as_u64).is_some_and(|version| version > VERSION) { return Err(Unread::Newer); }
    let manifest: Manifest = serde_json::from_value(value).map_err(|_| Unread::Damaged)?;
    if manifest.version != VERSION || !manifest.bundled_off.iter().all(|id| valid_bundled_id(id)) { return Err(Unread::Damaged); }
    let lists = manifest.lists.into_iter().map(|stored| {
        let kind = match stored.kind.as_str() {
            "ads" => ListKind::Ads,
            "cookie_banners" => ListKind::CookieBanners,
            _ => return Err(Unread::Damaged),
        };
        if !valid_id(&stored.id) { return Err(Unread::Damaged); }
        Ok(ListEntry { id: stored.id, name: stored.name, kind, added_ms: stored.added_ms, rules: stored.rules, unsupported: stored.unsupported })
    }).collect::<Result<_, _>>()?;
    Ok((lists, manifest.bundled_off))
}

pub fn load(data_dir: &Path, profile: &str) -> Saved {
    let Some(folder) = folder(data_dir, profile) else { return Saved::default() };
    let path = folder.join(MANIFEST);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Saved::default(),
        Err(_) => return Saved { lists: Vec::new(), notice: Some(DAMAGED.into()), ..Default::default() },
    };
    match read_manifest(&text) {
        Ok((lists, bundled_off)) => Saved { lists, notice: None, bundled_off },
        Err(Unread::Newer) => Saved { lists: Vec::new(), notice: Some(NEWER.into()), ..Default::default() },
        Err(Unread::Damaged) => {
            // Kept under another name; a later add starts a new one.
            let _ = fs::rename(&path, folder.join(format!("{MANIFEST}.damaged-{}", crate::now_ms())));
            Saved { lists: Vec::new(), notice: Some(DAMAGED.into()), ..Default::default() }
        }
    }
}

/// A chosen file's bytes, refused when larger than [`MAX_BYTES`].
pub fn read_file(path: &Path) -> Result<Vec<u8>, AddError> {
    let file = fs::File::open(path).map_err(|_| AddError::Unreadable)?;
    let meta = file.metadata().map_err(|_| AddError::Unreadable)?;
    if !meta.is_file() { return Err(AddError::Unreadable); }
    if meta.len() > MAX_BYTES { return Err(AddError::TooLarge); }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| AddError::Unreadable)?;
    if bytes.len() as u64 > MAX_BYTES { return Err(AddError::TooLarge); }
    Ok(bytes)
}

/// Adds `bytes`, named `name`, to `profile`'s lists of `kind`: its copy is
/// written and synced before the manifest names it, so a failure at any
/// step leaves the previous lists.
pub fn add(data_dir: &Path, profile: &str, kind: ListKind, name: &str, bytes: &[u8], now_ms: u64) -> Result<(ListEntry, Arc<CompiledList>), AddError> {
    if bytes.len() as u64 > MAX_BYTES { return Err(AddError::TooLarge); }
    if bytes.contains(&0) { return Err(AddError::NotText); }
    let text = std::str::from_utf8(bytes).map_err(|_| AddError::NotText)?;
    let folder = folder(data_dir, profile).ok_or_else(|| AddError::Save("the profile can't have lists".into()))?;
    let id = matcher::sha256_hex(bytes);
    let Saved { mut lists, bundled_off, .. } = load(data_dir, profile);
    if lists.iter().any(|entry| entry.id == id) { return Err(AddError::Duplicate); }
    let compiled = matcher::compile(text);
    if compiled.rules == 0 { return Err(AddError::NoRules); }
    let save = |error: io::Error| AddError::Save(error.kind().to_string());
    crate::captures::prepare_folder(&data_dir.join(FOLDER)).map_err(save)?;
    crate::captures::prepare_folder(&folder).map_err(save)?;
    let copy = format!("{id}.txt");
    let existed = folder.join(&copy).exists();
    write_private(&folder, &copy, bytes).map_err(save)?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let entry = ListEntry { id, name, kind, added_ms: now_ms, rules: compiled.rules, unsupported: compiled.unsupported };
    lists.push(entry.clone());
    if let Err(error) = write_manifest(&folder, &lists, &bundled_off) {
        if !existed { let _ = fs::remove_file(folder.join(&copy)); }
        return Err(save(error));
    }
    Ok((entry, compiled))
}

/// `entry`'s copy, compiled; refused when it changed since it was added.
pub fn open(data_dir: &Path, profile: &str, entry: &ListEntry) -> Result<Arc<CompiledList>, String> {
    let folder = folder(data_dir, profile).filter(|_| valid_id(&entry.id)).ok_or(CHANGED)?;
    let bytes = fs::read(folder.join(format!("{}.txt", entry.id))).map_err(|_| CHANGED)?;
    if matcher::sha256_hex(&bytes) != entry.id { return Err(CHANGED.into()); }
    let text = std::str::from_utf8(&bytes).map_err(|_| CHANGED)?;
    Ok(matcher::compile(text))
}

/// Removes a list: the manifest first, then its copy.
pub fn remove(data_dir: &Path, profile: &str, id: &str) -> Result<(), String> {
    let folder = folder(data_dir, profile).filter(|_| valid_id(id)).ok_or(REMOVE_FAILED)?;
    let saved = load(data_dir, profile);
    let lists: Vec<ListEntry> = saved.lists.into_iter().filter(|entry| entry.id != id).collect();
    write_manifest(&folder, &lists, &saved.bundled_off).map_err(|_| REMOVE_FAILED)?;
    let _ = fs::remove_file(folder.join(format!("{id}.txt")));
    Ok(())
}

/// Saves which bundled lists `profile` turned off, by id, keeping its
/// lists. Refused while its manifest is from a newer version, which stays.
pub fn set_bundled_off(data_dir: &Path, profile: &str, off: &BTreeSet<String>) -> Result<(), String> {
    let folder = folder(data_dir, profile).filter(|_| off.iter().all(|id| valid_bundled_id(id))).ok_or(SAVE_FAILED)?;
    let saved = load(data_dir, profile);
    if saved.notice.as_deref() == Some(NEWER) { return Err(NEWER_KEPT.into()); }
    crate::captures::prepare_folder(&data_dir.join(FOLDER)).map_err(|_| SAVE_FAILED)?;
    crate::captures::prepare_folder(&folder).map_err(|_| SAVE_FAILED)?;
    write_manifest(&folder, &saved.lists, off).map_err(|_| SAVE_FAILED.to_owned())
}

/// Deletes `profile`'s folder, as when the profile is deleted.
pub fn remove_profile(data_dir: &Path, profile: &str) -> io::Result<()> {
    let folder = folder(data_dir, profile).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a profile id"))?;
    match fs::remove_dir_all(&folder) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn write_manifest(folder: &Path, lists: &[ListEntry], bundled_off: &BTreeSet<String>) -> io::Result<()> {
    let manifest = Manifest {
        version: VERSION,
        lists: lists.iter().map(|entry| Stored { id: entry.id.clone(), name: entry.name.clone(), kind: kind_key(entry.kind).into(), added_ms: entry.added_ms, rules: entry.rules, unsupported: entry.unsupported }).collect(),
        bundled_off: bundled_off.clone(),
    };
    let text = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    write_private(folder, MANIFEST, &text)
}

/// Writes `name` in `folder` through a private partial file, synced and
/// renamed over it.
fn write_private(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let partial = folder.join(format!(".{name}{PARTIAL}"));
    let _ = fs::remove_file(&partial);
    let written = (|| {
        let mut out = crate::captures::private_file(&partial)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        fs::rename(&partial, folder.join(name))
    })();
    if written.is_err() { let _ = fs::remove_file(&partial); }
    crate::captures::sync_folder(folder);
    written
}

#[cfg(test)]
#[path = "lists_tests.rs"]
mod tests;
