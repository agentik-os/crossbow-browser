//! In-memory library of synced entities, backed by the [`Store`].
//!
//! Every local mutation ticks the hybrid logical clock, writes the entity with
//! `dirty = 1` (the push queue) and updates the in-memory maps. Remote changes
//! are applied only when their clock beats the local one (last-writer-wins).

use crate::backup;
use crate::model::*;
use crate::order::key_between;
use crate::store::{EntityRow, Store, TabRow};
use crate::{new_id, now_ms};
use crossbow_proto::{Hlc, ServerChange, kinds};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};

pub const SETTINGS_ID: &str = "global";
pub(crate) const KV_NODE: &str = "hlc_node";
pub(crate) const KV_LAST_HLC: &str = "hlc_last";
/// Followed by a pin's id: the Space, its version and the profile this
/// device last repaired the pin to. Never synced.
const KV_PIN_REPAIR: &str = "pin_repair:";
const FAVORITES_SPACE: &str = "__favorites__";
pub const SYNC_CURSOR_KEY: &str = "sync_cursor";

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("store: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid pinned move: {0}")]
    InvalidMove(&'static str),
    #[error("invalid deletion: {0}")]
    InvalidDelete(&'static str),
    #[error("invalid profile preferences: {0}")]
    InvalidProfilePreferences(&'static str),
    /// A profile name, move or deletion was refused; the text is a full sentence.
    #[error("{0}")]
    InvalidProfile(&'static str),
}

/// The longest profile name, in characters.
pub const PROFILE_NAME_MAX: usize = 40;

pub type Result<T> = std::result::Result<T, LibraryError>;

/// The smallest clock on `node` above `hlc`, both in `Hlc` order and in the
/// encoded text the server compares. None when there is none: the counter
/// would roll over past the largest 13-digit wall time. `hlc` may come from
/// the server, so this never panics.
pub(crate) fn successor(hlc: &Hlc, node: &str) -> Option<Hlc> {
    const MAX_COUNTER: u32 = 999_999; // six encoded digits
    const MAX_WALL: u64 = 9_999_999_999_999; // thirteen encoded digits
    let next = if hlc.counter < MAX_COUNTER { Hlc::new(hlc.wall_ms, hlc.counter + 1, node) } else { Hlc::new(hlc.wall_ms.checked_add(1)?, 0, node) };
    (next.wall_ms <= MAX_WALL && next > *hlc && next.encode() > hlc.encode()).then_some(next)
}

/// A pulled version this device must not apply: its own version is queued
/// again just above it instead.
pub(crate) struct Supersede {
    pub kind: String,
    pub id: String,
    pub over: Hlc,
    pub missing: Missing,
}

/// What supersedes a pulled row this device has no copy of.
pub(crate) enum Missing {
    /// Leave it: never delete data this device never had.
    Skip,
    Put(String),
}

pub struct Library {
    pub store: Store,
    node: String,
    last_hlc: Option<Hlc>,
    pub profiles: BTreeMap<String, Profile>,
    /// Retain stored identities without interpreting malformed policy as defaults.
    unavailable_profiles: BTreeSet<String>,
    pub spaces: BTreeMap<String, Space>,
    pub folders: BTreeMap<String, Folder>,
    pub items: BTreeMap<String, Item>,
    pub boosts: BTreeMap<String, Boost>,
    pub settings: Settings,
    /// Bumped on every change (local or remote) so the UI can cheaply notice.
    pub revision: u64,
}

impl Library {
    pub fn open(store: Store) -> Result<Library> {
        let node = match store.kv_get(KV_NODE)? {
            Some(n) => n,
            None => {
                let n: String = new_id().chars().filter(|c| c.is_ascii_alphanumeric()).rev().take(10).collect();
                store.kv_set(KV_NODE, &n)?;
                n
            }
        };
        let last_hlc = store.kv_get(KV_LAST_HLC)?.and_then(|s| Hlc::parse(&s).ok());
        let mut lib = Library {
            store,
            node,
            last_hlc,
            profiles: BTreeMap::new(),
            unavailable_profiles: BTreeSet::new(),
            spaces: BTreeMap::new(),
            folders: BTreeMap::new(),
            items: BTreeMap::new(),
            boosts: BTreeMap::new(),
            settings: Settings::default(),
            revision: 0,
        };
        for row in lib.store.all_entities()? {
            lib.load_row(&row);
        }
        Ok(lib)
    }

    /// Apply a batch atomically, including the in-memory state and local clock.
    /// The guard also rolls back if the operation unwinds before committing.
    pub(crate) fn transaction<T>(&mut self, f: impl FnOnce(&mut Library) -> Result<T>) -> Result<T> {
        let snapshot = LibrarySnapshot {
            last_hlc: self.last_hlc.clone(),
            profiles: self.profiles.clone(),
            unavailable_profiles: self.unavailable_profiles.clone(),
            spaces: self.spaces.clone(),
            folders: self.folders.clone(),
            items: self.items.clone(),
            boosts: self.boosts.clone(),
            settings: self.settings.clone(),
            revision: self.revision,
        };
        self.store.begin_transaction()?;
        let mut transaction = LibraryTransaction { library: self, snapshot: Some(snapshot) };
        let value = f(transaction.library)?;
        transaction.library.store.commit_transaction()?;
        transaction.snapshot = None;
        Ok(value)
    }

    /// Create the first profile and space if the library is empty.
    pub fn ensure_defaults(&mut self) -> Result<()> {
        if self.profiles.is_empty() && self.unavailable_profiles.is_empty() {
            self.create_profile("Personal")?;
        }
        if self.spaces.is_empty() {
            let profile = if self.profiles.is_empty() {
                // A stored unreadable profile is not a new library. Keep its
                // identity even when a first selectable Space must be created.
                self.unavailable_profiles.first().cloned().unwrap_or_default()
            } else { self.default_profile_id() };
            self.create_space("Personal", "🏠", Theme::preset(0), &profile)?;
        }
        Ok(())
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    fn tick(&mut self) -> Result<Hlc> {
        let h = Hlc::tick(self.last_hlc.as_ref(), now_ms(), &self.node);
        self.store.kv_set(KV_LAST_HLC, &h.encode())?;
        self.last_hlc = Some(h.clone());
        Ok(h)
    }

    fn put<T: Serialize>(&mut self, kind: &str, id: &str, value: &T) -> Result<()> {
        self.put_payload(kind, id, serde_json::to_string(value)?)
    }

    pub(crate) fn put_payload(&mut self, kind: &str, id: &str, payload: String) -> Result<()> {
        let hlc = self.tick()?;
        let row = EntityRow { kind: kind.into(), id: id.into(), hlc, deleted: false, payload };
        self.store.put_entity(&row, true)?;
        self.load_row(&row);
        Ok(())
    }

    /// Write a local version under `hlc` and keep the clock at or above it.
    fn put_at(&mut self, row: EntityRow) -> Result<()> {
        if self.last_hlc.as_ref().is_none_or(|last| *last < row.hlc) {
            self.store.kv_set(KV_LAST_HLC, &row.hlc.encode())?;
            self.last_hlc = Some(row.hlc.clone());
        }
        self.store.put_entity(&row, true)?;
        self.load_row(&row);
        Ok(())
    }

    /// A fresh clock that is also above `floor`, so a version written with it
    /// wins against a version this device knows of but never applied. None
    /// when no such clock exists.
    pub(crate) fn clock_above(&self, floor: Option<&Hlc>) -> Option<Hlc> {
        // A clock that observed the largest encodable value ticks past six
        // counter digits; such a clock is never written here.
        let fresh = Some(Hlc::tick(self.last_hlc.as_ref(), now_ms(), &self.node)).filter(|hlc| Hlc::parse(&hlc.encode()).is_ok());
        match (fresh, floor) {
            (Some(fresh), Some(floor)) if fresh > *floor => Some(fresh),
            (Some(fresh), None) => Some(fresh),
            (_, Some(floor)) => successor(floor, &self.node),
            (None, None) => None,
        }
    }

    /// Put a local version under a clock from [`Library::clock_above`].
    pub(crate) fn put_payload_at(&mut self, kind: &str, id: &str, payload: String, hlc: Hlc) -> Result<()> {
        self.put_at(EntityRow { kind: kind.into(), id: id.into(), hlc, deleted: false, payload })
    }

    /// How many live rows have a clock with no successor, so they can't be
    /// queued again above their own version.
    pub(crate) fn unadvanceable_live_rows(&self) -> Result<usize> {
        Ok(self.store.all_entities()?.iter().filter(|row| !row.deleted && successor(&row.hlc, &self.node).is_none()).count())
    }

    /// Queue every live row again just above its own version, payload
    /// unchanged. It supersedes the server's copy of that version without
    /// jumping ahead of a newer edit another device has not uploaded yet.
    /// Rows whose clock can't advance are left as they are; returns how many.
    /// Call inside a transaction.
    pub(crate) fn supersede_live_rows(&mut self) -> Result<usize> {
        let rows: Vec<EntityRow> = self.store.all_entities()?.into_iter().filter(|row| !row.deleted).collect();
        let mut stuck = 0;
        for row in &rows {
            match successor(&row.hlc, &self.node) {
                Some(hlc) => self.put_at(EntityRow { hlc, ..row.clone() })?,
                None => stuck += 1,
            }
        }
        Ok(stuck)
    }

    fn tombstone(&mut self, kind: &str, id: &str) -> Result<()> {
        let hlc = self.tick()?;
        let row = EntityRow { kind: kind.into(), id: id.into(), hlc, deleted: true, payload: String::new() };
        self.store.put_entity(&row, true)?;
        self.load_row(&row);
        Ok(())
    }

    fn load_row(&mut self, row: &EntityRow) {
        self.revision += 1;
        fn set<T: DeserializeOwned>(map: &mut BTreeMap<String, T>, row: &EntityRow) {
            if row.deleted {
                map.remove(&row.id);
            } else if let Ok(v) = serde_json::from_str(&row.payload) {
                map.insert(row.id.clone(), v);
            } else {
                log::warn!("ignoring unreadable {} {}", row.kind, row.id);
            }
        }
        match row.kind.as_str() {
            kinds::PROFILE => self.load_profile(&row.id, (!row.deleted).then_some(row.payload.as_str())),
            kinds::SPACE => set(&mut self.spaces, row),
            kinds::FOLDER => set(&mut self.folders, row),
            kinds::ITEM => set(&mut self.items, row),
            kinds::BOOST => set(&mut self.boosts, row),
            kinds::SETTINGS if row.id == SETTINGS_ID => {
                self.settings = if row.deleted {
                    Settings::default()
                } else {
                    serde_json::from_str(&row.payload).unwrap_or_default()
                }
            }
            _ => {} // unknown kinds are kept in the store for newer clients
        }
    }

    fn load_profile(&mut self, id: &str, payload: Option<&str>) {
        self.unavailable_profiles.remove(id);
        self.profiles.remove(id);
        let Some(payload) = payload else { return };
        if let Ok(profile) = serde_json::from_str::<Profile>(payload) {
            self.profiles.insert(id.into(), profile);
            return;
        }
        self.unavailable_profiles.insert(id.into());
        // Display identity can survive an unreadable optional policy, but it is
        // never a usable inherited policy and is never written back as a repair.
        if let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(payload) {
            if let (Some(name), Some(created_ms)) = (fields.get("name").and_then(|v| v.as_str()), fields.get("created_ms").and_then(|v| v.as_u64())) {
                self.profiles.insert(id.into(), Profile { name: name.into(), created_ms, preferences: None });
            }
        }
        log::warn!("profile policy unavailable for {id}");
    }

    /// Commit a pulled page and its resume cursor together. On any SQLite
    /// failure (including COMMIT), the transaction restores entities, clock,
    /// cursor and in-memory maps. Only success authorizes a worker acknowledgement.
    pub fn apply_sync_page(&mut self, changes: &[ServerChange], cursor: u64) -> Result<usize> {
        self.transaction(|lib| {
            let applied = lib.apply_remote(changes)?;
            lib.store.kv_set(SYNC_CURSOR_KEY, &cursor.to_string())?;
            Ok(applied)
        })
    }

    /// Commit a pulled page, the local versions that supersede rows it must
    /// not apply, and the resume cursor, together. The clocks of `skipped`
    /// rows are observed as applied ones are, so later local edits sort
    /// above them; `also` writes more in the same transaction.
    pub(crate) fn apply_sync_page_superseding(&mut self, changes: &[ServerChange], supersede: &[Supersede], skipped: &[Hlc], cursor: u64, also: impl FnOnce(&mut Library) -> Result<()>) -> Result<usize> {
        self.transaction(|lib| {
            let applied = lib.apply_remote(changes)?;
            for hlc in skipped {
                let observed = Hlc::observe(lib.last_hlc.as_ref(), hlc, now_ms(), &lib.node);
                lib.store.kv_set(KV_LAST_HLC, &observed.encode())?;
                lib.last_hlc = Some(observed);
            }
            for s in supersede {
                let local = lib.store.entity(&s.kind, &s.id)?;
                // A newer local version already supersedes it.
                if local.as_ref().is_some_and(|row| row.hlc > s.over) { continue; }
                // A clock at the encodable limit can't be beaten; leave it.
                let Some(hlc) = successor(&s.over, &lib.node) else { continue };
                let row = match (local, &s.missing) {
                    (Some(row), _) => EntityRow { hlc, ..row },
                    (None, Missing::Skip) => continue,
                    (None, Missing::Put(payload)) => EntityRow { kind: s.kind.clone(), id: s.id.clone(), hlc, deleted: false, payload: payload.clone() },
                };
                lib.put_at(row)?;
            }
            also(lib)?;
            lib.store.kv_set(SYNC_CURSOR_KEY, &cursor.to_string())?;
            Ok(applied)
        })
    }

    /// Apply changes pulled from the server. Returns how many were applied.
    pub fn apply_remote(&mut self, changes: &[ServerChange]) -> Result<usize> {
        // Validate only sequential LWW winners before the first write. Obsolete
        // malformed rows cannot block later valid work or the page cursor.
        let mut profile_clocks = BTreeMap::<&str, Option<Hlc>>::new();
        for sc in changes {
            let c = &sc.change;
            if c.kind != kinds::PROFILE { continue; }
            let latest = match profile_clocks.entry(c.id.as_str()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(self.store.entity_hlc(kinds::PROFILE, &c.id)?),
            };
            if latest.as_ref().is_some_and(|local| *local >= c.hlc) { continue; }
            if !c.deleted {
                if c.enc != crossbow_proto::Encoding::Json {
                    return Err(LibraryError::InvalidProfilePreferences("unsupported profile encoding"));
                }
                serde_json::from_str::<Profile>(&c.payload)?;
            }
            *latest = Some(c.hlc.clone());
        }
        let mut applied = 0;
        for sc in changes {
            let c = &sc.change;
            if sc.device_id.is_empty() && c.kind.is_empty() {
                continue;
            }
            let local = self.store.entity_hlc(&c.kind, &c.id)?;
            let observed = Hlc::observe(self.last_hlc.as_ref(), &c.hlc, now_ms(), &self.node);
            self.store.kv_set(KV_LAST_HLC, &observed.encode())?;
            self.last_hlc = Some(observed);
            if local.as_ref().is_some_and(|l| *l >= c.hlc) {
                continue;
            }
            let row = EntityRow {
                kind: c.kind.clone(),
                id: c.id.clone(),
                hlc: c.hlc.clone(),
                deleted: c.deleted,
                payload: c.payload.clone(),
            };
            self.store.put_entity(&row, false)?;
            self.load_row(&row);
            applied += 1;
        }
        Ok(applied)
    }

    // -- queries -------------------------------------------------------------

    pub fn default_profile_id(&self) -> String {
        self.profiles
            .iter()
            .min_by_key(|(_, p)| p.created_ms)
            .map(|(id, _)| id.clone())
            .unwrap_or_default()
    }

    /// Resolve only the requested profile. Missing, deleted or invalid profiles
    /// return None, never another profile's settings; this read writes nothing.
    pub fn effective_profile_preferences(&self, id: &str) -> Option<ResolvedProfilePreferences> {
        if self.unavailable_profiles.contains(id) { return None; }
        let profile = self.profiles.get(id).filter(|_| !id.is_empty())?;
        let preferences = profile.preferences.unwrap_or_default();
        preferences.validate().ok()?;
        Some(ResolvedProfilePreferences {
            search_engine: preferences.search_engine.unwrap_or(self.settings.search_engine),
            auto_archive_hours: preferences.auto_archive_hours.unwrap_or(self.settings.auto_archive_hours),
        })
    }

    pub fn spaces_ordered(&self) -> Vec<(String, Space)> {
        let mut v: Vec<_> = self.spaces.iter().map(|(k, s)| (k.clone(), s.clone())).collect();
        v.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        v
    }

    pub fn favorites(&self, profile_id: &str) -> Vec<(String, Item)> {
        let mut v: Vec<_> = self
            .items
            .iter()
            .filter(|(_, i)| i.kind == ItemKind::Favorite && i.profile_id == profile_id)
            .map(|(k, i)| (k.clone(), i.clone()))
            .collect();
        v.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        v
    }

    /// Pinned items and folders directly inside `folder_id` (None = space root),
    /// merged in order.
    pub fn pinned_children(&self, space_id: &str, folder_id: Option<&str>) -> Vec<PinnedNode> {
        let mut v: Vec<PinnedNode> = self
            .items
            .iter()
            .filter(|(_, i)| {
                i.kind == ItemKind::Pinned
                    && i.space_id.as_deref() == Some(space_id)
                    && i.folder_id.as_deref() == folder_id
            })
            .map(|(k, i)| PinnedNode::Item(k.clone(), i.clone()))
            .chain(
                self.folders
                    .iter()
                    .filter(|(_, f)| f.space_id == space_id && f.parent_id.as_deref() == folder_id)
                    .map(|(k, f)| PinnedNode::Folder(k.clone(), f.clone())),
            )
            .collect();
        v.sort_by(|a, b| a.order().cmp(b.order()).then(a.id().cmp(b.id())));
        v
    }

    fn last_order<'a>(orders: impl Iterator<Item = &'a String>) -> Option<String> {
        orders.max().cloned()
    }

    // -- mutations -----------------------------------------------------------

    pub fn create_profile(&mut self, name: &str) -> Result<String> {
        let id = new_id();
        self.put(kinds::PROFILE, &id, &Profile { name: name.into(), created_ms: now_ms(), preferences: None })?;
        Ok(id)
    }

    /// Read and validate one saved profile without a transaction or cache mutation.
    /// Missing/deleted/invalid records fail; no alternate profile is selected.
    pub fn read_profile(&self, id: &str) -> Result<Profile> {
        if id.is_empty() { return Err(LibraryError::NotFound(id.into())); }
        let payload = self.store.entity_payload(kinds::PROFILE, id)?
            .ok_or_else(|| LibraryError::NotFound(id.into()))?;
        Ok(serde_json::from_str(&payload)?)
    }

    /// Reload this exact profile's cached availability at an explicit recovery
    /// boundary. No write transaction, clock mutation or alternate identity.
    pub fn reload_profile(&mut self, id: &str) -> Result<Profile> {
        if id.is_empty() { return Err(LibraryError::NotFound(id.into())); }
        let payload = self.store.entity_payload(kinds::PROFILE, id)?;
        let clock = self.saved_profile_clock(id);
        self.cache_profile(id, payload.as_deref(), clock.is_ok());
        clock?;
        let payload = payload.ok_or_else(|| LibraryError::NotFound(id.into()))?;
        Ok(serde_json::from_str(&payload)?)
    }

    /// Refresh this exact saved profile and its clock before merging a retry.
    /// This writes nothing. Missing/deleted/invalid records invalidate cached
    /// policy and return an error, never an alternate profile or a fresh identity.
    pub fn refresh_profile(&mut self, id: &str) -> Result<Profile> {
        if id.is_empty() { return Err(LibraryError::NotFound(id.into())); }
        let (payload, clock) = self.transaction(|lib| {
            Ok((lib.store.entity_payload(kinds::PROFILE, id)?, lib.saved_profile_clock(id)))
        })?;
        self.cache_profile(id, payload.as_deref(), clock.is_ok());
        self.last_hlc = clock?;
        let payload = payload.ok_or_else(|| LibraryError::NotFound(id.into()))?;
        Ok(serde_json::from_str(&payload)?)
    }

    fn cache_profile(&mut self, id: &str, payload: Option<&str>, clock_readable: bool) {
        let prior = self.profiles.get(id).cloned();
        let unavailable = self.unavailable_profiles.contains(id);
        self.load_profile(id, payload);
        if !clock_readable && payload.is_some() { self.unavailable_profiles.insert(id.into()); }
        if self.profiles.get(id) != prior.as_ref() || self.unavailable_profiles.contains(id) != unavailable {
            self.revision += 1;
        }
    }

    fn saved_profile_clock(&self, id: &str) -> Result<Option<Hlc>> {
        let saved = self.store.kv_get(KV_LAST_HLC)?.map(|value| Hlc::parse(&value)
            .map_err(|_| LibraryError::InvalidProfilePreferences("saved library clock is unreadable"))).transpose()?;
        let profile = self.store.entity_hlc(kinds::PROFILE, id)?;
        if profile.is_none() && self.store.entity_payload(kinds::PROFILE, id)?.is_some() {
            return Err(LibraryError::InvalidProfilePreferences("saved profile clock is unreadable"));
        }
        Ok(self.last_hlc.iter().chain(saved.iter()).chain(profile.iter()).max().cloned())
    }

    /// Replace only this profile's overrides; None restores legacy inheritance.
    /// Missing/deleted or changed saved profiles are rejected. On write/commit
    /// failure the profile, revision and clock stay unchanged for explicit retry.
    pub fn set_profile_preferences(&mut self, id: &str, preferences: Option<ProfilePreferences>) -> Result<()> {
        if let Some(preferences) = preferences {
            preferences.validate().map_err(LibraryError::InvalidProfilePreferences)?;
        }
        self.transaction(|lib| {
            let mut profile = lib.profiles.get(id).filter(|_| !id.is_empty()).cloned()
                .ok_or_else(|| LibraryError::NotFound(id.into()))?;
            let saved = lib.store.entity_payload(kinds::PROFILE, id)?
                .ok_or_else(|| LibraryError::NotFound(id.into()))?;
            if serde_json::from_str::<Profile>(&saved)? != profile {
                return Err(LibraryError::InvalidProfilePreferences("saved profile changed; reload before editing"));
            }
            profile.preferences = preferences;
            lib.last_hlc = lib.saved_profile_clock(id)?;
            lib.put(kinds::PROFILE, id, &profile)
        })
    }

    /// Trim a profile name and check it is non-empty, at most
    /// [`PROFILE_NAME_MAX`] characters and unused by any other profile, ignoring case.
    pub fn checked_profile_name(&self, name: &str, except: Option<&str>) -> Result<String> {
        let name = name.trim();
        if name.is_empty() { return Err(LibraryError::InvalidProfile("Enter a profile name.")); }
        if name.chars().count() > PROFILE_NAME_MAX { return Err(LibraryError::InvalidProfile("Use 40 characters or fewer.")); }
        if name.chars().any(char::is_control) {
            return Err(LibraryError::InvalidProfile("Remove line breaks and other control characters."));
        }
        let folded = name.to_lowercase();
        if self.profiles.iter().any(|(id, profile)| Some(id.as_str()) != except && profile.name.trim().to_lowercase() == folded) {
            return Err(LibraryError::InvalidProfile("Another profile already uses this name."));
        }
        Ok(name.to_owned())
    }

    /// Create a profile under a checked name.
    pub fn create_named_profile(&mut self, name: &str) -> Result<String> {
        let name = self.checked_profile_name(name, None)?;
        self.create_profile(&name)
    }

    /// Rename a profile, keeping its preferences. Unreadable or externally
    /// changed profiles are refused, never rewritten.
    pub fn rename_profile(&mut self, id: &str, name: &str) -> Result<()> {
        if self.unavailable_profiles.contains(id) {
            return Err(LibraryError::InvalidProfile("This profile's saved settings can't be read, so it can't be changed."));
        }
        let mut profile = self.profiles.get(id).filter(|_| !id.is_empty()).cloned()
            .ok_or_else(|| LibraryError::NotFound(id.into()))?;
        let name = self.checked_profile_name(name, Some(id))?;
        if profile.name == name { return Ok(()); }
        self.transaction(|lib| {
            let saved = lib.store.entity_payload(kinds::PROFILE, id)?
                .ok_or_else(|| LibraryError::NotFound(id.into()))?;
            if serde_json::from_str::<Profile>(&saved)? != profile {
                return Err(LibraryError::InvalidProfile("This profile changed on another device. Try again."));
            }
            profile.name = name;
            lib.put(kinds::PROFILE, id, &profile)
        })
    }

    /// Spaces that use `profile`, in switcher order.
    pub fn spaces_using_profile(&self, profile: &str) -> Vec<String> {
        self.spaces_ordered().into_iter().filter(|(_, space)| space.profile_id == profile).map(|(id, _)| id).collect()
    }

    /// Move a space and its pinned items to another profile in one commit.
    /// Folders, order and URLs are unchanged; favorites stay with their profile.
    pub fn set_space_profile(&mut self, space: &str, profile: &str) -> Result<()> {
        if profile.is_empty() || !self.profiles.contains_key(profile) { return Err(LibraryError::NotFound(profile.into())); }
        if self.unavailable_profiles.contains(profile) {
            return Err(LibraryError::InvalidProfile("This profile's saved settings can't be read, so spaces can't move to it."));
        }
        let current = self.spaces.get(space).ok_or_else(|| LibraryError::NotFound(space.into()))?;
        if current.profile_id == profile { return Ok(()); }
        // The space's new version is the newest, so repair moves its pins.
        self.transaction(|lib| {
            lib.update_space(space, |space| space.profile_id = profile.into())?;
            lib.reconcile_profiles().map(drop)
        })
    }

    /// Bring items back in line with profiles where edits from several
    /// devices crossed: a pinned item takes its Space's profile, and a
    /// favorite of a deleted profile is deleted too. A profile this device
    /// hasn't received yet is not a deletion. Each fix is an ordinary local
    /// edit, so it syncs like any other (sealed when encrypted). A pin keeps
    /// the version of its Space it took its profile from, and a device
    /// holding an older version leaves it alone, so devices holding
    /// different versions (the newest may be sealed under a key neither has)
    /// settle on the newer one instead of writing back and forth, and a
    /// consistent library writes nothing. An older Crossbow drops that
    /// version when it rewrites a pin, so a device-local note keeps the
    /// basis of each repair, and a pin rewritten without the version is
    /// repaired at most once per version and profile of its Space held here.
    /// Returns how many items changed. Call inside a transaction.
    pub(crate) fn reconcile_profiles(&mut self) -> Result<usize> {
        let mut pins = Vec::new();
        for (id, item) in &self.items {
            if item.kind != ItemKind::Pinned { continue; }
            let Some((space_id, space)) = item.space_id.as_deref().and_then(|id| Some((id, self.spaces.get(id)?))) else { continue };
            if space.profile_id.is_empty() || space.profile_id == item.profile_id { continue; }
            let Some(version) = self.store.entity_hlc(kinds::SPACE, space_id)?.map(|hlc| hlc.encode()) else { continue };
            // Encoded clocks sort as the clocks do.
            if item.space_version.as_ref().is_some_and(|newer| *newer > version) { continue; }
            let basis = format!("{space_id} {version} {}", space.profile_id);
            if item.space_version.is_none() && self.store.kv_get(&format!("{KV_PIN_REPAIR}{id}"))?.as_ref() == Some(&basis) { continue; }
            pins.push((id.clone(), space.profile_id.clone(), version, basis));
        }
        let mut orphans = Vec::new();
        for (id, item) in &self.items {
            if item.kind != ItemKind::Favorite || self.profiles.contains_key(&item.profile_id) || self.unavailable_profiles.contains(&item.profile_id) { continue; }
            if self.store.raw_entity(kinds::PROFILE, &item.profile_id)?.is_some_and(|row| row.deleted) { orphans.push(id.clone()); }
        }
        for (pin, profile, version, basis) in pins.iter().cloned() {
            self.update_item(&pin, |item| {
                item.profile_id = profile;
                item.space_version = Some(version);
            })?;
            self.store.kv_set(&format!("{KV_PIN_REPAIR}{pin}"), &basis)?;
        }
        for favorite in &orphans { self.tombstone(kinds::ITEM, favorite)?; }
        Ok(pins.len() + orphans.len())
    }

    /// Why `id` can't be deleted now, checked before any of its data is touched.
    pub fn check_profile_deletion(&self, id: &str) -> Result<()> {
        if id.is_empty() || !self.profiles.contains_key(id) { return Err(LibraryError::NotFound(id.into())); }
        if !self.spaces_using_profile(id).is_empty() {
            return Err(LibraryError::InvalidProfile("Spaces still use this profile. Move them to another profile first."));
        }
        if self.profiles.keys().chain(&self.unavailable_profiles).collect::<BTreeSet<_>>().len() <= 1 {
            return Err(LibraryError::InvalidProfile("Keep at least one profile."));
        }
        Ok(())
    }

    /// Clear `profile`'s history here, with no copy left in the file or its
    /// log, then in every local backup and a restore waiting for the next
    /// launch. Says which backups couldn't be cleaned.
    pub fn clear_history(&self, profile: &str) -> Result<backup::Removed> {
        self.store.clear_history(profile)?;
        self.fold_log();
        Ok(self.store.path().map_or_else(Default::default, |library| backup::forget(&library, backup::Forget::History(profile))))
    }

    /// Delete a profile no space uses, with its favorites, their saved tabs,
    /// its local history and archive, its site permissions, its sites with
    /// blocking off and the sites set to open in it, in one commit, then its
    /// history, archive, site permissions and sites with blocking off in every
    /// local backup. One profile always remains. Says
    /// which backups couldn't be cleaned.
    pub fn delete_profile(&mut self, id: &str) -> Result<backup::Removed> {
        self.check_profile_deletion(id)?;
        let favorites: BTreeSet<String> = self.items.iter()
            .filter(|(_, item)| item.kind == ItemKind::Favorite && item.profile_id == id)
            .map(|(item, _)| item.clone()).collect();
        self.transaction(|lib| {
            for tab in lib.store.tabs()? {
                if tab.space_id == FAVORITES_SPACE && tab.item_id.as_ref().is_some_and(|item| favorites.contains(item)) {
                    lib.store.delete_tab(&tab.id)?;
                }
            }
            lib.store.clear_history(id)?;
            lib.store.clear_archive(id)?;
            crate::site_permissions::reset_all(&lib.store, id)?;
            crate::site_profiles::forget_profile(&lib.store, id)?;
            crate::content_blocking::exceptions::forget_profile(&lib.store, id)?;
            for item in &favorites { lib.tombstone(kinds::ITEM, item)?; }
            lib.tombstone(kinds::PROFILE, id)?;
            lib.reconcile_profiles().map(drop)
        })?;
        self.fold_log();
        Ok(self.store.path().map_or_else(Default::default, |library| backup::forget(&library, backup::Forget::Profile(id))))
    }

    /// Empty the log after rows were deleted, so the copies it holds go;
    /// secure_delete already zeroed the space they left in the file. The
    /// rows are gone either way, so a failure is only logged.
    fn fold_log(&self) {
        match self.store.fold_log() {
            Ok(true) => {}
            Ok(false) => log::warn!("The library's log was in use, so it keeps what was deleted until SQLite next folds it in."),
            Err(error) => log::warn!("Couldn't fold the library's log in: {error}"),
        }
    }

    pub fn create_space(&mut self, name: &str, icon: &str, theme: Theme, profile_id: &str) -> Result<String> {
        let id = new_id();
        let last = Self::last_order(self.spaces.values().map(|s| &s.order));
        let space = Space {
            name: name.into(),
            icon: icon.into(),
            theme,
            profile_id: profile_id.into(),
            order: key_between(last.as_deref(), None),
        };
        self.put(kinds::SPACE, &id, &space)?;
        Ok(id)
    }

    pub fn update_space(&mut self, id: &str, f: impl FnOnce(&mut Space)) -> Result<()> {
        let mut s = self.spaces.get(id).cloned().ok_or_else(|| LibraryError::NotFound(id.into()))?;
        f(&mut s);
        self.put(kinds::SPACE, id, &s)
    }

    /// Delete a space with its folders and pinned items, without closing tabs.
    pub fn delete_space(&mut self, id: &str) -> Result<()> {
        self.transaction(|lib| {
            let (items, folders) = lib.space_deletion_children(id)?;
            lib.tombstone_space(id, &items, &folders)
        })
    }

    /// Close all saved/live tabs in a space and delete its entities atomically.
    /// Live rows supply current URL/title; callers must keep every live view
    /// until success. Eligible Today rows retain the original profile owner.
    pub fn delete_space_and_tabs(&mut self, id: &str, live_tabs: &[TabRow], now_ms: u64) -> Result<()> {
        self.transaction(|lib| {
            let (items, folders) = lib.space_deletion_children(id)?;
            let profile = lib.spaces[id].profile_id.clone();
            let item_ids: BTreeSet<_> = items.iter().map(String::as_str).collect();
            let tabs = lib.deletion_tabs(live_tabs, |tab| {
                tab.space_id == id || tab.item_id.as_deref().is_some_and(|item| item_ids.contains(item))
            })?;
            for tab in &tabs {
                if tab.space_id != id || lib.deletion_tab_profile(tab)? != profile {
                    return Err(LibraryError::InvalidDelete("tab does not belong to the deleted space"));
                }
            }
            for tab in &tabs {
                let archive = (tab.item_id.is_none() && !tab.url.is_empty() && tab.url != "about:blank")
                    .then_some(profile.as_str());
                lib.store.close_tab(tab, archive, now_ms)?;
            }
            lib.tombstone_space(id, &items, &folders)
        })
    }

    fn tombstone_space(&mut self, id: &str, items: &[String], folders: &[String]) -> Result<()> {
        for item in items { self.tombstone(kinds::ITEM, item)?; }
        for folder in folders { self.tombstone(kinds::FOLDER, folder)?; }
        self.tombstone(kinds::SPACE, id)
    }

    fn space_deletion_children(&self, id: &str) -> Result<(Vec<String>, Vec<String>)> {
        self.deletion_space_profile(id)?;
        let folders: Vec<_> = self.folders.iter().filter(|(_, f)| f.space_id == id).map(|(id, _)| id.clone()).collect();
        let folder_ids: BTreeSet<_> = folders.iter().map(String::as_str).collect();
        let mut items = Vec::new();
        for (item_id, item) in &self.items {
            if item.space_id.as_deref() == Some(id) || item.folder_id.as_deref().is_some_and(|folder| folder_ids.contains(folder)) {
                if item.kind != ItemKind::Pinned || item.space_id.as_deref() != Some(id) {
                    return Err(LibraryError::InvalidDelete("space contains an item owned by another parent"));
                }
                self.deletion_item_profile(item_id)?;
                items.push(item_id.clone());
            }
        }
        for (folder_id, folder) in &self.folders {
            if folder.space_id == id || folder.parent_id.as_deref().is_some_and(|parent| folder_ids.contains(parent)) {
                self.deletion_folder_path(folder_id, id)?;
            }
        }
        Ok((items, folders))
    }

    /// Move a space between two neighbours in the switcher order.
    pub fn move_space(&mut self, id: &str, after: Option<&str>, before: Option<&str>) -> Result<()> {
        let a = after.and_then(|a| self.spaces.get(a)).map(|s| s.order.clone());
        let b = before.and_then(|b| self.spaces.get(b)).map(|s| s.order.clone());
        let order = key_between(a.as_deref(), b.as_deref());
        self.update_space(id, |s| s.order = order)
    }

    pub fn add_favorite(&mut self, profile_id: &str, url: &str, title: &str) -> Result<String> {
        let id = new_id();
        let last = Self::last_order(
            self.items
                .values()
                .filter(|i| i.kind == ItemKind::Favorite && i.profile_id == profile_id)
                .map(|i| &i.order),
        );
        let item = Item {
            kind: ItemKind::Favorite,
            profile_id: profile_id.into(),
            space_id: None,
            folder_id: None,
            url: url.into(),
            title: title.into(),
            custom_title: None,
            order: key_between(last.as_deref(), None),
            space_version: None,
        };
        self.put(kinds::ITEM, &id, &item)?;
        Ok(id)
    }

    pub fn add_pinned(&mut self, space_id: &str, folder_id: Option<&str>, url: &str, title: &str) -> Result<String> {
        let profile_id = self
            .spaces
            .get(space_id)
            .map(|s| s.profile_id.clone())
            .ok_or_else(|| LibraryError::NotFound(space_id.into()))?;
        let space_version = self.store.entity_hlc(kinds::SPACE, space_id)?.map(|hlc| hlc.encode());
        let id = new_id();
        let last = self.pinned_children(space_id, folder_id).last().map(|n| n.order().to_string());
        let item = Item {
            kind: ItemKind::Pinned,
            profile_id,
            space_id: Some(space_id.into()),
            folder_id: folder_id.map(Into::into),
            url: url.into(),
            title: title.into(),
            custom_title: None,
            order: key_between(last.as_deref(), None),
            space_version,
        };
        self.put(kinds::ITEM, &id, &item)?;
        Ok(id)
    }

    pub fn update_item(&mut self, id: &str, f: impl FnOnce(&mut Item)) -> Result<()> {
        let mut i = self.items.get(id).cloned().ok_or_else(|| LibraryError::NotFound(id.into()))?;
        f(&mut i);
        self.stamp_space_version(&mut i)?;
        self.put(kinds::ITEM, id, &i)
    }

    /// A pin written here without the version of its Space it took its
    /// profile from (a row from before pins kept it) records the version it
    /// matches here. Otherwise an edit on a device holding an older version
    /// would carry that profile with no version, and the repair note would
    /// stop the device holding the newer one from repairing it again.
    fn stamp_space_version(&self, item: &mut Item) -> Result<()> {
        if item.kind != ItemKind::Pinned || item.space_version.is_some() { return Ok(()); }
        let Some(space_id) = item.space_id.as_deref() else { return Ok(()) };
        if self.spaces.get(space_id).is_none_or(|space| space.profile_id.is_empty() || space.profile_id != item.profile_id) { return Ok(()); }
        item.space_version = self.store.entity_hlc(kinds::SPACE, space_id)?.map(|hlc| hlc.encode());
        Ok(())
    }

    /// Write an item as a Crossbow from before pins kept their Space's
    /// version would: exactly as `f` leaves it.
    #[cfg(test)]
    pub(crate) fn put_item_as_older_crossbow(&mut self, id: &str, f: impl FnOnce(&mut Item)) -> Result<()> {
        let mut item = self.items.get(id).cloned().ok_or_else(|| LibraryError::NotFound(id.into()))?;
        f(&mut item);
        self.put(kinds::ITEM, id, &item)
    }

    /// Remove saved metadata only; unpin/move callers retain their live tabs.
    pub fn delete_item(&mut self, id: &str) -> Result<()> {
        self.tombstone(kinds::ITEM, id)
    }

    /// Close an item's saved/live tabs and tombstone it in a single commit.
    /// Pins and favorites close without archive. Live views must stay intact
    /// until this succeeds, including when a later entity or COMMIT write fails.
    pub fn delete_item_and_tabs(&mut self, id: &str, live_tabs: &[TabRow], now_ms: u64) -> Result<()> {
        self.transaction(|lib| {
            let profile = lib.deletion_item_profile(id)?.to_owned();
            let tabs = lib.deletion_tabs(live_tabs, |tab| tab.item_id.as_deref() == Some(id))?;
            for tab in &tabs {
                if lib.deletion_tab_profile(tab)? != profile {
                    return Err(LibraryError::InvalidDelete("tab does not belong to the deleted item"));
                }
            }
            for tab in &tabs { lib.store.close_tab(tab, None, now_ms)?; }
            lib.tombstone(kinds::ITEM, id)
        })
    }

    fn deletion_entity<T: DeserializeOwned + PartialEq>(&self, kind: &str, id: &str, expected: &T) -> Result<()> {
        if id.is_empty() { return Err(LibraryError::InvalidDelete("entity has no id")); }
        let saved = self.store.entity_payload(kind, id)?
            .and_then(|payload| serde_json::from_str::<T>(&payload).ok());
        if saved.as_ref() != Some(expected) {
            return Err(LibraryError::InvalidDelete("saved entity is missing, malformed or changed"));
        }
        Ok(())
    }

    fn deletion_profile(&self, id: &str) -> Result<()> {
        let profile = self.profiles.get(id).filter(|_| !id.is_empty())
            .ok_or(LibraryError::InvalidDelete("profile ownership is missing"))?;
        self.deletion_entity(kinds::PROFILE, id, profile)
    }

    fn deletion_space_profile(&self, id: &str) -> Result<&str> {
        if id == FAVORITES_SPACE {
            return Err(LibraryError::InvalidDelete("favorites is not a deletable space"));
        }
        let space = self.spaces.get(id).ok_or_else(|| LibraryError::NotFound(id.into()))?;
        self.deletion_entity(kinds::SPACE, id, space)?;
        self.deletion_profile(&space.profile_id)?;
        Ok(&space.profile_id)
    }

    fn deletion_folder_path(&self, id: &str, space: &str) -> Result<()> {
        let mut ancestor = Some(id);
        let mut visited = BTreeSet::new();
        while let Some(id) = ancestor {
            if !visited.insert(id) {
                return Err(LibraryError::InvalidDelete("folder hierarchy contains a cycle"));
            }
            let folder = self.folders.get(id).ok_or_else(|| LibraryError::NotFound(id.into()))?;
            self.deletion_entity(kinds::FOLDER, id, folder)?;
            if folder.space_id != space {
                return Err(LibraryError::InvalidDelete("folder belongs to another space"));
            }
            ancestor = folder.parent_id.as_deref();
        }
        Ok(())
    }

    fn deletion_item_profile(&self, id: &str) -> Result<&str> {
        let item = self.items.get(id).ok_or_else(|| LibraryError::NotFound(id.into()))?;
        self.deletion_entity(kinds::ITEM, id, item)?;
        self.deletion_profile(&item.profile_id)?;
        match item.kind {
            ItemKind::Favorite if item.space_id.is_none() && item.folder_id.is_none() => {},
            ItemKind::Pinned => {
                let space = item.space_id.as_deref().ok_or(LibraryError::InvalidDelete("pin has no source space"))?;
                if self.deletion_space_profile(space)? != item.profile_id {
                    return Err(LibraryError::InvalidDelete("pin and space belong to different profiles"));
                }
                if let Some(folder) = &item.folder_id { self.deletion_folder_path(folder, space)?; }
            }
            _ => return Err(LibraryError::InvalidDelete("favorite has ambiguous space or folder ownership")),
        }
        Ok(&item.profile_id)
    }

    fn deletion_tab_profile(&self, tab: &TabRow) -> Result<&str> {
        if tab.id.is_empty() { return Err(LibraryError::InvalidDelete("tab has no id")); }
        if let Some(item_id) = &tab.item_id {
            let profile = self.deletion_item_profile(item_id)?;
            let item = &self.items[item_id];
            let expected_space = match item.kind {
                ItemKind::Favorite => FAVORITES_SPACE,
                ItemKind::Pinned => item.space_id.as_deref().unwrap(),
            };
            if tab.space_id != expected_space {
                return Err(LibraryError::InvalidDelete("tab and saved item belong to different parents"));
            }
            Ok(profile)
        } else {
            self.deletion_space_profile(&tab.space_id)
        }
    }

    fn deletion_tabs(&self, live_tabs: &[TabRow], belongs: impl Fn(&TabRow) -> bool) -> Result<Vec<TabRow>> {
        let saved: BTreeMap<_, _> = self.store.tabs()?.into_iter().map(|tab| (tab.id.clone(), tab)).collect();
        let mut selected: BTreeMap<_, _> = saved.iter().filter(|(_, tab)| belongs(tab))
            .map(|(id, tab)| (id.clone(), tab.clone())).collect();
        let mut seen = BTreeMap::new();
        for tab in live_tabs {
            if !belongs(tab) {
                return Err(LibraryError::InvalidDelete("live tab is outside the deleted parent"));
            }
            if let Some(previous) = seen.insert(&tab.id, tab) && previous != tab {
                return Err(LibraryError::InvalidDelete("conflicting live rows share a tab id"));
            }
            if let Some(saved) = saved.get(&tab.id)
                && (saved.space_id != tab.space_id || saved.item_id != tab.item_id)
            {
                return Err(LibraryError::InvalidDelete("saved and live tab ownership disagree"));
            }
            selected.insert(tab.id.clone(), tab.clone());
        }
        Ok(selected.into_values().collect())
    }

    pub fn create_folder(&mut self, space_id: &str, parent_id: Option<&str>, name: &str) -> Result<String> {
        let id = new_id();
        let last = self.pinned_children(space_id, parent_id).last().map(|n| n.order().to_string());
        let folder = Folder {
            space_id: space_id.into(),
            parent_id: parent_id.map(Into::into),
            name: name.into(),
            order: key_between(last.as_deref(), None),
            collapsed: false,
        };
        self.put(kinds::FOLDER, &id, &folder)?;
        Ok(id)
    }

    pub fn update_folder(&mut self, id: &str, f: impl FnOnce(&mut Folder)) -> Result<()> {
        let mut x = self.folders.get(id).cloned().ok_or_else(|| LibraryError::NotFound(id.into()))?;
        f(&mut x);
        self.put(kinds::FOLDER, id, &x)
    }

    /// Delete a folder; its children move up to the folder's parent atomically.
    pub fn delete_folder(&mut self, id: &str) -> Result<()> {
        self.transaction(|lib| {
            let folder = lib.folders.get(id).cloned().ok_or_else(|| LibraryError::NotFound(id.into()))?;
            lib.deletion_space_profile(&folder.space_id)?;
            lib.deletion_folder_path(id, &folder.space_id)?;
            let child_items: Vec<String> = lib.items.iter()
                .filter(|(_, item)| item.folder_id.as_deref() == Some(id)).map(|(id, _)| id.clone()).collect();
            let child_folders: Vec<String> = lib.folders.iter()
                .filter(|(_, child)| child.parent_id.as_deref() == Some(id)).map(|(id, _)| id.clone()).collect();
            for item_id in &child_items {
                lib.deletion_item_profile(item_id)?;
                if lib.items[item_id].space_id.as_deref() != Some(folder.space_id.as_str()) {
                    return Err(LibraryError::InvalidDelete("folder contains an item owned by another space"));
                }
            }
            for child in &child_folders { lib.deletion_folder_path(child, &folder.space_id)?; }
            for item in child_items {
                lib.update_item(&item, |item| item.folder_id = folder.parent_id.clone())?;
            }
            for child in child_folders {
                lib.update_folder(&child, |child| child.parent_id = folder.parent_id.clone())?;
            }
            lib.tombstone(kinds::FOLDER, id)
        })
    }

    /// Create a folder at the end of `parent_id`, expanding the parent so the
    /// new folder is visible. A missing parent writes nothing.
    pub fn create_folder_in(&mut self, parent_id: &str, name: &str) -> Result<String> {
        let parent = self.folders.get(parent_id).cloned().ok_or_else(|| LibraryError::NotFound(parent_id.into()))?;
        if !self.spaces.contains_key(&parent.space_id) {
            return Err(LibraryError::InvalidMove("folder has no valid space"));
        }
        self.transaction(|lib| {
            if parent.collapsed {
                lib.update_folder(parent_id, |f| f.collapsed = false)?;
            }
            lib.create_folder(&parent.space_id, Some(parent_id), name)
        })
    }

    /// Put a pinned item into a new folder that takes the item's place.
    pub fn create_folder_with_item(&mut self, item_id: &str, name: &str) -> Result<String> {
        let item = self.items.get(item_id).cloned().ok_or_else(|| LibraryError::NotFound(item_id.into()))?;
        let space = match (item.kind, item.space_id.as_deref()) {
            (ItemKind::Pinned, Some(space)) if self.spaces.contains_key(space) => space.to_owned(),
            _ => return Err(LibraryError::InvalidMove("only pinned items can start a folder")),
        };
        if item.folder_id.as_deref().is_some_and(|parent| self.folders.get(parent).is_none_or(|f| f.space_id != space)) {
            return Err(LibraryError::InvalidMove("pinned item is in a folder of another space"));
        }
        self.transaction(|lib| {
            let id = new_id();
            let folder = Folder { space_id: space, parent_id: item.folder_id.clone(), name: name.into(), order: item.order.clone(), collapsed: false };
            lib.put(kinds::FOLDER, &id, &folder)?;
            lib.update_item(item_id, |i| {
                i.folder_id = Some(id.clone());
                i.order = key_between(None, None);
            })?;
            Ok(id)
        })
    }

    /// Tidy Today: new folders at the end of the Space's pinned area, each
    /// holding new pins for its (url, title) pairs, in one commit. Existing
    /// pins, folders and favorites are untouched; a failure writes nothing.
    /// Returns each new folder's id with its new pins' ids, in order.
    pub fn tidy_into_folders(&mut self, space_id: &str, folders: &[(String, Vec<(String, String)>)]) -> Result<Vec<(String, Vec<String>)>> {
        let space = self.spaces.get(space_id).ok_or_else(|| LibraryError::NotFound(space_id.into()))?;
        if !self.profiles.contains_key(&space.profile_id) {
            return Err(LibraryError::InvalidMove("the space has no available profile"));
        }
        self.transaction(|lib| {
            let mut made = Vec::new();
            for (name, tabs) in folders {
                let folder = lib.create_folder(space_id, None, name)?;
                let mut items = Vec::new();
                for (url, title) in tabs { items.push(lib.add_pinned(space_id, Some(&folder), url, title)?); }
                made.push((folder, items));
            }
            Ok(made)
        })
    }

    /// Undo [`Library::tidy_into_folders`] in one commit, only while what it
    /// made is unchanged: each folder still at the top level of the same Space
    /// holding exactly its own pins and no subfolder. Otherwise nothing is
    /// removed, so later edits are never lost.
    pub fn untidy_folders(&mut self, space_id: &str, made: &[(String, Vec<String>)]) -> Result<()> {
        for (folder_id, items) in made {
            let folder = self.folders.get(folder_id).ok_or_else(|| LibraryError::NotFound(folder_id.clone()))?;
            if folder.space_id != space_id || folder.parent_id.is_some() || self.folders.values().any(|f| f.parent_id.as_deref() == Some(folder_id)) {
                return Err(LibraryError::InvalidDelete("a tidied folder changed"));
            }
            let mut children: Vec<&String> = self.items.iter().filter(|(_, item)| item.folder_id.as_deref() == Some(folder_id)).map(|(id, _)| id).collect();
            let mut expected: Vec<&String> = items.iter().collect();
            children.sort();
            expected.sort();
            if children != expected || items.iter().any(|id| self.items[id].kind != ItemKind::Pinned || self.items[id].space_id.as_deref() != Some(space_id)) {
                return Err(LibraryError::InvalidDelete("a tidied folder changed"));
            }
        }
        self.transaction(|lib| {
            for (folder, items) in made {
                for item in items {
                    lib.deletion_item_profile(item)?;
                    lib.tombstone(kinds::ITEM, item)?;
                }
                lib.deletion_folder_path(folder, space_id)?;
                lib.tombstone(kinds::FOLDER, folder)?;
            }
            Ok(())
        })
    }

    /// Copy a saved item into the slot right after the original.
    pub fn duplicate_item(&mut self, id: &str) -> Result<String> {
        let item = self.items.get(id).cloned().ok_or_else(|| LibraryError::NotFound(id.into()))?;
        let siblings: Vec<String> = match item.kind {
            ItemKind::Pinned => {
                let space = item.space_id.as_deref().ok_or(LibraryError::InvalidMove("pinned item has no space"))?;
                self.pinned_children(space, item.folder_id.as_deref()).iter().map(|n| n.order().to_owned()).collect()
            }
            ItemKind::Favorite => self.favorites(&item.profile_id).into_iter().map(|(_, i)| i.order).collect(),
        };
        let next = siblings.into_iter().filter(|order| *order > item.order).min();
        let mut copy = Item { order: key_between(Some(&item.order), next.as_deref()), ..item };
        self.stamp_space_version(&mut copy)?;
        let copy_id = new_id();
        self.put(kinds::ITEM, &copy_id, &copy)?;
        Ok(copy_id)
    }

    /// Turn a pinned item into the last favorite of its profile. The id stays,
    /// so a tab opened from it stays attached.
    pub fn favorite_pinned(&mut self, id: &str) -> Result<()> {
        let item = self.items.get(id).ok_or_else(|| LibraryError::NotFound(id.into()))?;
        if item.kind != ItemKind::Pinned || !self.profiles.contains_key(&item.profile_id) {
            return Err(LibraryError::InvalidMove("only pinned items of a known profile can become favorites"));
        }
        let last = Self::last_order(
            self.items.values().filter(|i| i.kind == ItemKind::Favorite && i.profile_id == item.profile_id).map(|i| &i.order),
        );
        let order = key_between(last.as_deref(), None);
        self.update_item(id, |i| {
            i.kind = ItemKind::Favorite;
            i.space_id = None;
            i.space_version = None;
            i.folder_id = None;
            i.order = order;
        })
    }

    /// Reorder a pinned item or folder, optionally into another folder.
    /// Items may move between spaces of the same profile; folders stay in their
    /// space because this operation does not migrate their subtree. Invalid
    /// destinations and write failures leave the library and clock unchanged.
    pub fn move_pinned(
        &mut self,
        node_id: &str,
        space_id: &str,
        folder_id: Option<&str>,
        index: usize,
    ) -> Result<()> {
        let destination = self.spaces.get(space_id).ok_or_else(|| LibraryError::NotFound(space_id.into()))?;
        let is_item = if let Some(item) = self.items.get(node_id) {
            if item.kind != ItemKind::Pinned {
                return Err(LibraryError::InvalidMove("only pinned items can be moved with this operation"));
            }
            let source = item
                .space_id
                .as_deref()
                .and_then(|id| self.spaces.get(id))
                .ok_or(LibraryError::InvalidMove("pinned item has no valid source space"))?;
            if item.profile_id != source.profile_id || item.profile_id != destination.profile_id {
                return Err(LibraryError::InvalidMove("pinned items cannot move across profiles"));
            }
            true
        } else {
            let folder = self.folders.get(node_id).ok_or_else(|| LibraryError::NotFound(node_id.into()))?;
            if folder.space_id != space_id {
                return Err(LibraryError::InvalidMove("folders cannot move across spaces"));
            }
            false
        };

        // Check the entire destination path before ticking the clock or writing.
        // A visited set also bounds traversal of malformed legacy/remote trees.
        let mut ancestor = folder_id;
        let mut visited = BTreeSet::new();
        while let Some(id) = ancestor {
            if !is_item && id == node_id {
                return Err(LibraryError::InvalidMove("a folder cannot move into itself or a descendant"));
            }
            if !visited.insert(id) {
                return Err(LibraryError::InvalidMove("destination folder hierarchy contains a cycle"));
            }
            let folder = self.folders.get(id).ok_or_else(|| LibraryError::NotFound(id.into()))?;
            if folder.space_id != space_id {
                return Err(LibraryError::InvalidMove("destination folder belongs to another space"));
            }
            ancestor = folder.parent_id.as_deref();
        }

        let siblings: Vec<PinnedNode> =
            self.pinned_children(space_id, folder_id).into_iter().filter(|n| n.id() != node_id).collect();
        let index = index.min(siblings.len());
        let a = index.checked_sub(1).and_then(|i| siblings.get(i)).map(|n| n.order().to_string());
        let b = siblings.get(index).map(|n| n.order().to_string());
        let order = key_between(a.as_deref(), b.as_deref());
        let folder = folder_id.map(String::from);
        self.transaction(|lib| {
            if is_item {
                let version = lib.store.entity_hlc(kinds::SPACE, space_id)?.map(|hlc| hlc.encode());
                lib.update_item(node_id, |i| {
                    if i.space_id.as_deref() != Some(space_id) { i.space_version = version; }
                    i.order = order;
                    i.folder_id = folder;
                    i.space_id = Some(space_id.into());
                })
            } else {
                lib.update_folder(node_id, |f| {
                    f.order = order;
                    f.parent_id = folder;
                })
            }
        })
    }

    pub fn reorder_favorite(&mut self, id: &str, index: usize) -> Result<()> {
        let profile = self.items.get(id).map(|i| i.profile_id.clone()).unwrap_or_default();
        let siblings: Vec<(String, Item)> = self.favorites(&profile).into_iter().filter(|(k, _)| k != id).collect();
        let index = index.min(siblings.len());
        let a = index.checked_sub(1).and_then(|i| siblings.get(i)).map(|(_, i)| i.order.clone());
        let b = siblings.get(index).map(|(_, i)| i.order.clone());
        let order = key_between(a.as_deref(), b.as_deref());
        self.update_item(id, |i| i.order = order)
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        self.put(kinds::SETTINGS, SETTINGS_ID, &settings)
    }

    pub fn put_boost(&mut self, id: Option<&str>, boost: &Boost) -> Result<String> {
        let id = id.map(String::from).unwrap_or_else(new_id);
        self.put(kinds::BOOST, &id, boost)?;
        Ok(id)
    }

    pub fn delete_boost(&mut self, id: &str) -> Result<()> {
        self.tombstone(kinds::BOOST, id)
    }
}

struct LibrarySnapshot {
    last_hlc: Option<Hlc>,
    profiles: BTreeMap<String, Profile>,
    unavailable_profiles: BTreeSet<String>,
    spaces: BTreeMap<String, Space>,
    folders: BTreeMap<String, Folder>,
    items: BTreeMap<String, Item>,
    boosts: BTreeMap<String, Boost>,
    settings: Settings,
    revision: u64,
}

struct LibraryTransaction<'a> {
    library: &'a mut Library,
    snapshot: Option<LibrarySnapshot>,
}

impl Drop for LibraryTransaction<'_> {
    fn drop(&mut self) {
        let Some(snapshot) = self.snapshot.take() else { return };
        if let Err(error) = self.library.store.rollback_transaction() {
            log::error!("couldn't roll back library transaction: {error}");
        }
        self.library.last_hlc = snapshot.last_hlc;
        self.library.profiles = snapshot.profiles;
        self.library.unavailable_profiles = snapshot.unavailable_profiles;
        self.library.spaces = snapshot.spaces;
        self.library.folders = snapshot.folders;
        self.library.items = snapshot.items;
        self.library.boosts = snapshot.boosts;
        self.library.settings = snapshot.settings;
        self.library.revision = snapshot.revision;
    }
}

#[derive(Clone, Debug)]
pub enum PinnedNode {
    Item(String, Item),
    Folder(String, Folder),
}

impl PinnedNode {
    pub fn id(&self) -> &str {
        match self {
            PinnedNode::Item(id, _) | PinnedNode::Folder(id, _) => id,
        }
    }

    pub fn order(&self) -> &str {
        match self {
            PinnedNode::Item(_, i) => &i.order,
            PinnedNode::Folder(_, f) => &f.order,
        }
    }
}

#[cfg(test)]
#[path = "parent_removal_tests.rs"]
mod parent_removal_tests;

#[cfg(test)]
#[path = "profile_preferences_tests.rs"]
mod profile_preferences_tests;

#[cfg(test)]
#[path = "profile_lifecycle_tests.rs"]
mod profile_lifecycle_tests;

#[cfg(test)]
#[path = "tidy_tests.rs"]
mod tidy_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_proto::{Change, Encoding};

    fn lib() -> Library {
        let mut l = Library::open(Store::open_in_memory().unwrap()).unwrap();
        l.ensure_defaults().unwrap();
        l
    }

    #[test]
    fn transaction_unwind_restores_memory_clock_and_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive-rollback.sqlite");
        let mut l = Library::open(Store::open(&path).unwrap()).unwrap();
        l.ensure_defaults().unwrap();
        let snapshot = (
            l.profiles.clone(), l.spaces.clone(), l.folders.clone(), l.items.clone(),
            l.boosts.clone(), l.settings.clone(), l.revision, l.last_hlc.clone(),
        );
        let entities = l.store.all_entities().unwrap();
        let clock = l.store.kv_get(KV_LAST_HLC).unwrap();
        let dirty = l.store.dirty_count().unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = l.transaction::<()>(|l| {
                let profile = l.create_profile("Temporary")?;
                let space = l.create_space("Temporary", "T", Theme::preset(1), &profile)?;
                let folder = l.create_folder(&space, None, "Temporary")?;
                l.add_pinned(&space, Some(&folder), "https://temporary.example/", "Temporary")?;
                l.put_boost(None, &Boost { name: "Temporary".into(), host: "temporary.example".into(), css: String::new(), js: String::new(), enabled: true })?;
                let mut settings = l.settings.clone();
                settings.ai_features = true;
                l.set_settings(settings)?;
                l.store.archive_tab(&profile, "https://temporary.example/", "Temporary", &space, 1)?;
                l.store.kv_set("temporary", "marker")?;
                panic!("fixture unwind");
            });
        }));
        assert!(panic.is_err());
        assert_eq!((l.profiles.clone(), l.spaces.clone(), l.folders.clone(), l.items.clone(),
            l.boosts.clone(), l.settings.clone(), l.revision, l.last_hlc.clone()), snapshot);
        assert_eq!(l.store.all_entities().unwrap(), entities);
        assert_eq!(l.store.kv_get(KV_LAST_HLC).unwrap(), clock);
        assert_eq!(l.store.dirty_count().unwrap(), dirty);
        assert_eq!(l.store.kv_get("temporary").unwrap(), None);
        assert!(l.store.archive(&l.default_profile_id(), 100).unwrap().is_empty());
        let conn = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM archive", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
        // The failed batch leaves no open transaction or abandoned in-memory clock.
        l.transaction(|l| l.create_profile("After retry")).unwrap();
        assert_eq!(l.profiles.len(), 2);
    }

    #[test]
    fn defaults_and_dirty_queue() {
        let l = lib();
        assert_eq!(l.profiles.len(), 1);
        assert_eq!(l.spaces.len(), 1);
        assert_eq!(l.store.dirty_count().unwrap(), 2);
    }

    #[test]
    fn pinned_ordering_and_folders() {
        let mut l = lib();
        let space = l.spaces_ordered()[0].0.clone();
        let a = l.add_pinned(&space, None, "https://a.example", "A").unwrap();
        let b = l.add_pinned(&space, None, "https://b.example", "B").unwrap();
        let f = l.create_folder(&space, None, "Work").unwrap();
        l.move_pinned(&b, &space, None, 0).unwrap();
        let ids: Vec<_> = l.pinned_children(&space, None).iter().map(|n| n.id().to_string()).collect();
        assert_eq!(ids, vec![b.clone(), a.clone(), f.clone()]);
        l.move_pinned(&a, &space, Some(&f), 0).unwrap();
        assert_eq!(l.pinned_children(&space, Some(&f)).len(), 1);
        l.delete_folder(&f).unwrap();
        assert_eq!(l.items[&a].folder_id, None);
        assert_eq!(l.pinned_children(&space, None).len(), 2);
    }

    #[test]
    fn remote_lww_and_tombstones() {
        let mut l = lib();
        let space = l.spaces_ordered()[0].0.clone();
        let item = l.add_pinned(&space, None, "https://a.example", "A").unwrap();
        let local_hlc = l.store.entity_hlc(kinds::ITEM, &item).unwrap().unwrap();

        // An older remote edit loses.
        let mut older = l.items[&item].clone();
        older.title = "old".into();
        let stale = ServerChange {
            seq: 1,
            device_id: "other".into(),
            change: Change {
                kind: kinds::ITEM.into(),
                id: item.clone(),
                hlc: Hlc::new(local_hlc.wall_ms - 1, 0, "zz"),
                deleted: false,
                enc: Encoding::Json,
                payload: serde_json::to_string(&older).unwrap(),
            },
        };
        assert_eq!(l.apply_remote(&[stale]).unwrap(), 0);
        assert_eq!(l.items[&item].title, "A");

        // A newer remote tombstone wins and is not queued for push.
        let tomb = ServerChange {
            seq: 2,
            device_id: "other".into(),
            change: Change {
                kind: kinds::ITEM.into(),
                id: item.clone(),
                hlc: Hlc::new(local_hlc.wall_ms + 10_000, 0, "zz"),
                deleted: true,
                enc: Encoding::Json,
                payload: String::new(),
            },
        };
        let before = l.store.dirty_count().unwrap();
        assert_eq!(l.apply_remote(&[tomb]).unwrap(), 1);
        assert!(!l.items.contains_key(&item));
        assert!(l.store.dirty_count().unwrap() < before);

        // The next local clock is ahead of the remote one.
        let id2 = l.add_favorite(&l.default_profile_id(), "https://b.example", "B").unwrap();
        let h2 = l.store.entity_hlc(kinds::ITEM, &id2).unwrap().unwrap();
        assert!(h2.wall_ms >= local_hlc.wall_ms + 10_000);
    }

    #[test]
    fn mark_clean_respects_concurrent_edit() {
        let mut l = lib();
        let pushed = l.store.dirty_changes(100).unwrap();
        let space = l.spaces_ordered()[0].0.clone();
        l.update_space(&space, |s| s.name = "Renamed".into()).unwrap();
        for c in &pushed {
            l.store.mark_clean(&c.kind, &c.id, &c.hlc).unwrap();
        }
        // The space was edited after the push snapshot, so it stays dirty.
        let still: Vec<_> = l.store.dirty_changes(100).unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(still, vec![space]);
    }
}
