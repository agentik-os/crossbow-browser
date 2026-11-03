//! The lists that come with Crossbow: EasyList and EasyPrivacy, copied
//! unmodified into `lists/` with their versions and hashes in
//! `lists/SOURCES`, under CC BY-SA 3.0 as `lists/LICENSE.md` says. They are
//! built into the app; nothing downloads them, and `cargo xtask
//! update-lists` replaces them by hand.
//!
//! Each list is compiled at most once at a time in this process and shared
//! by every profile that uses it: whoever asks while it compiles waits for
//! that copy. When no profile uses it any more, its memory goes, and the
//! next use compiles it again.

use super::lists::ListKind;
use super::matcher::{self, CompiledList};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// A list built into the app.
pub struct Bundled {
    /// Short and lowercase; what a profile's manifest names to turn it off.
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ListKind,
    pub text: &'static str,
    /// The attribution Settings shows beside it.
    pub credit: &'static str,
    compiled: Mutex<Weak<CompiledList>>,
    compiles: AtomicUsize,
}

impl Bundled {
    pub const fn new(id: &'static str, name: &'static str, kind: ListKind, text: &'static str, credit: &'static str) -> Bundled {
        Bundled { id, name, kind, text, credit, compiled: Mutex::new(Weak::new()), compiles: AtomicUsize::new(0) }
    }

    /// The list compiled: the copy in use, or a new one. Takes seconds, so
    /// never on the UI thread.
    pub fn compiled(&self) -> Arc<CompiledList> {
        // Held while compiling, so a second caller waits for this copy.
        let mut compiled = self.compiled.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(list) = compiled.upgrade() { return list; }
        let list = matcher::compile(self.text);
        self.compiles.fetch_add(1, Ordering::Relaxed);
        *compiled = Arc::downgrade(&list);
        list
    }

    /// How many times this process has compiled it.
    pub fn compiles(&self) -> usize {
        self.compiles.load(Ordering::Relaxed)
    }
}

/// Each file's version, fetch date, size, SHA-256 and address.
pub const SOURCES: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lists/SOURCES"));
/// The licence chosen and the attribution.
pub const LICENSE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lists/LICENSE.md"));
/// The attribution, as Settings shows it.
pub const CREDIT: &str = "EasyList and EasyPrivacy by The EasyList authors (https://easylist.to/), used unmodified under CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/).";

pub static EASYLIST: Bundled = Bundled::new("easylist", "EasyList", ListKind::Ads, include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lists/easylist.txt")), CREDIT);
pub static EASYPRIVACY: Bundled = Bundled::new("easyprivacy", "EasyPrivacy", ListKind::Ads, include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lists/easyprivacy.txt")), CREDIT);
/// The lists the app comes with, in the order Settings shows them.
pub static LISTS: [&Bundled; 2] = [&EASYLIST, &EASYPRIVACY];

#[cfg(test)]
#[path = "bundled_tests.rs"]
mod tests;
