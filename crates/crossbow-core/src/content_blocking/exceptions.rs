//! Each profile's sites with blocking off. A site is the top page's host
//! cut down by the simplified site rule the lists use, so turning blocking
//! off on a page turns it off on its whole site, frames included.
//!
//! They're kept in the library's key-value table: on this device only,
//! never synced, but in the library's local backups like the switches.
//! Deleting the profile deletes them, from those backups too.

use super::matcher;
use crate::store::Store;
use std::collections::BTreeSet;

pub const UNREADABLE: &str = "The sites with blocking off couldn't be read, so blocking is on everywhere. Turning it off on a site saves over them.";
pub const SAVE_FAILED: &str = "Couldn't save the sites with blocking off. Nothing changed. Check disk space and permissions, then try again.";

/// A profile's sites with blocking off, and why none could be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exceptions {
    pub sites: BTreeSet<String>,
    pub notice: Option<String>,
}

/// Where `profile`'s sites are kept: a JSON array of sites.
pub fn key(profile: &str) -> String {
    format!("content_blocking.exceptions.v1.{profile}")
}

/// `profile`'s sites. A record that can't be read counts as none, so
/// blocking stays on everywhere, and says so.
pub fn load(store: &Store, profile: &str) -> Exceptions {
    let unreadable = || Exceptions { sites: BTreeSet::new(), notice: Some(UNREADABLE.into()) };
    match store.kv_get(&key(profile)) {
        Ok(None) => Exceptions::default(),
        Ok(Some(raw)) => serde_json::from_str::<BTreeSet<String>>(&raw).map_or_else(|_| unreadable(), |sites| Exceptions { sites, notice: None }),
        Err(error) => {
            log::warn!("Couldn't read the sites with blocking off: {error}");
            unreadable()
        }
    }
}

/// Saves `sites` as `profile`'s whole set; none deletes the record.
pub fn save(store: &Store, profile: &str, sites: &BTreeSet<String>) -> Result<(), String> {
    let saved = if sites.is_empty() {
        store.kv_delete(&key(profile))
    } else {
        let raw = serde_json::to_string(sites).map_err(|_| SAVE_FAILED.to_owned())?;
        store.kv_set(&key(profile), &raw)
    };
    saved.map_err(|error| {
        log::warn!("Couldn't save the sites with blocking off: {error}");
        SAVE_FAILED.to_owned()
    })
}

/// Deletes a deleted profile's sites.
pub fn forget_profile(store: &Store, profile: &str) -> crate::store::Result<()> {
    store.kv_delete(&key(profile))
}

/// The site of a web page's address; None for any other address.
pub fn site_of(page: &str) -> Option<String> {
    let url = url::Url::parse(page).ok()?;
    if !matches!(url.scheme(), "http" | "https") { return None; }
    let site = matcher::site(url.host_str()?);
    (!site.is_empty()).then(|| site.to_owned())
}

#[cfg(test)]
#[path = "exceptions_tests.rs"]
mod tests;
