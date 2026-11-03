//! Content blocking's hook in the engine. The app gives each profile its
//! rules with [`set_content_rules`]; a page's resource handler asks them on
//! CEF's IO thread, before each request starts, and cancels the request when
//! they block it. Reading the rules takes a lock only long enough to clone
//! one `Arc`, so a decision never waits on the UI thread or on I/O.
//!
//! A top-level page load is never cancelled, and neither is a request with no
//! browser, such as a service worker's own fetch or a preview's
//! `CefURLRequest`: those never reach the rules.
//!
//! Each view counts the addresses cancelled on its current page, each once.

use cef::*;
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, PoisonError, RwLock};

/// What a page asked for, as the rules see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Subdocument,
    Script,
    Image,
    Stylesheet,
    Font,
    Object,
    Media,
    Xhr,
    Ping,
    Other,
}

/// One request: its address, its kind, the address of the document that
/// made it (empty when unknown) and the address of the tab's top page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceRequest<'a> {
    pub url: &'a str,
    pub kind: ResourceKind,
    pub context: &'a str,
    pub top: &'a str,
}

/// What a document hides: its host's rules, then the rules for every page,
/// which a renderer keeps by `generation`; 0 when they were built for this
/// document alone. Each part is one `selector{display:none!important}` rule a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidingSheet {
    pub specific: String,
    pub generic: Arc<str>,
    pub generation: u64,
}

/// A profile's rules.
pub trait ContentRules: Send + Sync {
    /// Called on CEF's IO thread: it must not block.
    fn blocks(&self, request: &ResourceRequest<'_>) -> bool;

    /// The element hiding for a document at `document` in a tab on `top`, or
    /// None. Called on the UI thread.
    fn hiding(&self, document: &str, top: &str) -> Option<HidingSheet> {
        let _ = (document, top);
        None
    }
}

/// Gives `profile_id`'s pages these rules from their next request, or none.
pub fn set_content_rules(profile_id: &str, rules: Option<Arc<dyn ContentRules>>) {
    let mut profiles = PROFILES.write().unwrap_or_else(PoisonError::into_inner);
    match rules {
        Some(rules) => profiles.insert(profile_id.into(), rules),
        None => profiles.remove(profile_id),
    };
}

/// Whether `profile_id`'s pages have rules now.
pub fn has_content_rules(profile_id: &str) -> bool {
    rules_for(profile_id).is_some()
}

pub(crate) fn rules_for(profile_id: &str) -> Option<Arc<dyn ContentRules>> {
    PROFILES.read().unwrap_or_else(PoisonError::into_inner).get(profile_id).cloned()
}

/// The kind for CEF's resource type; None for a top-level page.
pub(crate) fn kind(resource_type: ResourceType) -> Option<ResourceKind> {
    use ResourceKind::*;
    Some(match resource_type {
        ResourceType::MAIN_FRAME | ResourceType::NAVIGATION_PRELOAD_MAIN_FRAME => return None,
        ResourceType::SUB_FRAME | ResourceType::NAVIGATION_PRELOAD_SUB_FRAME => Subdocument,
        ResourceType::STYLESHEET => Stylesheet,
        ResourceType::SCRIPT | ResourceType::WORKER | ResourceType::SHARED_WORKER | ResourceType::SERVICE_WORKER => Script,
        ResourceType::IMAGE | ResourceType::FAVICON => Image,
        ResourceType::FONT_RESOURCE => Font,
        ResourceType::OBJECT | ResourceType::PLUGIN_RESOURCE => Object,
        ResourceType::MEDIA => Media,
        ResourceType::XHR => Xhr,
        ResourceType::PING | ResourceType::CSP_REPORT => Ping,
        _ => Other,
    })
}

/// Whether `rules` cancel `request`, made by `frame` in `browser`.
pub(crate) fn cancels(rules: &dyn ContentRules, browser: Option<&Browser>, frame: Option<&Frame>, request: &Request) -> bool {
    let Some(browser) = browser else { return false };
    let Some(kind) = kind(request.resource_type()) else { return false };
    let url = text(&request.url());
    let top = browser.main_frame().map(|top| text(&top.url())).unwrap_or_default();
    // A frame's own navigation was made by the document around it; CEF may
    // pass a frame it has only just created, whose address is still empty.
    let requester = match frame {
        Some(frame) if kind == ResourceKind::Subdocument => frame.parent().or_else(|| browser.main_frame()),
        Some(frame) => Some(frame.clone()),
        None => browser.main_frame(),
    };
    let context = document(requester);
    rules.blocks(&ResourceRequest { url: &url, kind, context: &context, top: &top })
}

/// How many frames up [`document`] looks.
const DEPTH: usize = 16;

/// The address of the nearest web document from `frame` up: an
/// `about:blank` or `about:srcdoc` frame's requests are its creator's.
fn document(mut frame: Option<Frame>) -> String {
    for _ in 0..DEPTH {
        let Some(current) = frame else { break };
        let url = text(&current.url());
        if web(&url) { return url; }
        frame = current.parent();
    }
    String::new()
}

fn web(url: &str) -> bool {
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
}

fn text(value: &CefStringUserfree) -> String {
    CefString::from(value).to_string()
}

/// How many addresses the rules cancelled on a view's current page, each
/// once: Chromium's preloader asks for a script or image, then its parser
/// asks again after the cancel. `count` holds the page's load number in the
/// high half and its count in the low half, so the IO thread adds and the
/// UI thread starts a page without a lock.
///
/// `seen` is a lock-free set of the page's addresses: each slot holds an
/// address's hash tagged with its page, and a slot an older page tagged is
/// free, so a new page clears nothing. Past [`SEEN`] addresses on one page,
/// every further cancel counts, repeats included.
///
/// A request decided while the next page starts can be missed, or, when
/// the page before made it, counted on the new page: CEF reports a page's
/// start on the UI thread, a little after the IO thread may have seen its
/// first requests. Either way an address counts at most once on a page.
pub(crate) struct BlockedCount {
    count: AtomicU64,
    seen: Box<[AtomicU64]>,
    keys: RandomState,
}

/// The addresses one page can have counted once each: 8 KiB a view.
const SEEN: usize = 1024;
/// A slot in use: this bit, its page's tag, then the address's hash.
const USED: u64 = 1 << 63;
const HASH_BITS: u32 = 40;
const HASH: u64 = (1 << HASH_BITS) - 1;
/// Tags repeat after 2^23 pages in one view.
const TAG_BITS: u32 = 23;
const TAG: u64 = (1 << TAG_BITS) - 1;

impl Default for BlockedCount {
    fn default() -> BlockedCount {
        BlockedCount { count: AtomicU64::new(0), seen: (0..SEEN).map(|_| AtomicU64::new(0)).collect(), keys: RandomState::new() }
    }
}

impl BlockedCount {
    /// Page `load` starts, at 0.
    pub(crate) fn start(&self, load: u64) {
        self.count.store(u64::from(load as u32) << 32, Ordering::Relaxed);
    }

    /// `url` was cancelled on whichever page is current. Stops at `u32::MAX`.
    pub(crate) fn add(&self, url: &str) {
        self.add_on((self.count.load(Ordering::Relaxed) >> 32) as u32, url);
    }

    /// `url` was cancelled while page `load` was current; once the next page
    /// has started, it isn't counted.
    fn add_on(&self, load: u32, url: &str) {
        if !self.first(load, url) { return; }
        let _ = self.count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |packed| ((packed >> 32) as u32 == load && packed as u32 != u32::MAX).then_some(packed + 1));
    }

    /// Whether `url` is new on page `load`, which then holds it: false when
    /// a slot already does, or once a newer page has tagged one.
    fn first(&self, load: u32, url: &str) -> bool {
        let (tag, hash) = (u64::from(load) & TAG, self.keys.hash_one(url));
        let mark = USED | tag << HASH_BITS | hash & HASH;
        let start = (hash >> HASH_BITS) as usize;
        for at in 0..SEEN {
            let slot = &self.seen[(start + at) % SEEN];
            let mut held = slot.load(Ordering::Relaxed);
            loop {
                if held == mark { return false; }
                if held & USED != 0 {
                    // How many pages older than `load` the slot's is.
                    let older = tag.wrapping_sub(held >> HASH_BITS) & TAG;
                    if older == 0 { break; }
                    if older > TAG / 2 { return false; }
                }
                match slot.compare_exchange_weak(held, mark, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => return true,
                    Err(now) => held = now,
                }
            }
        }
        true
    }

    /// Page `load`'s count; 0 once another page started.
    pub(crate) fn get(&self, load: u64) -> u32 {
        let packed = self.count.load(Ordering::Relaxed);
        if (packed >> 32) as u32 == load as u32 { packed as u32 } else { 0 }
    }
}

static PROFILES: LazyLock<RwLock<HashMap<String, Arc<dyn ContentRules>>>> = LazyLock::new(Default::default);

#[cfg(test)]
#[path = "content_rules_tests.rs"]
pub(crate) mod tests;
