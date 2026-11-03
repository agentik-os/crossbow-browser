//! Live tabs. A tab is persisted (url, title, order) in the local store and may
//! or may not have a running web view: background tabs restored after a
//! restart stay unloaded until they are activated, like Arc.

use crossbow_core::store::TabRow;
use crossbow_engine::{NavState, WebView};

/// Space id used for tabs opened from favorites (shared by all spaces).
pub const FAVORITES_SPACE: &str = "__favorites__";

pub struct Tab {
    pub id: String,
    pub space_id: String,
    /// The pinned item or favorite this tab was opened from.
    pub item_id: Option<String>,
    pub url: String,
    pub title: String,
    pub order: String,
    pub created_ms: u64,
    pub last_active_ms: u64,
    pub view: Option<WebView>,
    pub nav: NavState,
    pub muted: bool,
    /// Needs writing to the store.
    pub dirty: bool,
}

impl Tab {
    pub fn from_row(r: TabRow) -> Tab {
        Tab {
            nav: NavState { url: r.url.clone(), title: r.title.clone(), ..Default::default() },
            id: r.id,
            space_id: r.space_id,
            item_id: r.item_id,
            url: r.url,
            title: r.title,
            order: r.order,
            created_ms: r.created_ms,
            last_active_ms: r.last_active_ms,
            view: None,
            muted: false,
            dirty: false,
        }
    }

    pub fn row(&self) -> TabRow {
        TabRow {
            id: self.id.clone(),
            space_id: self.space_id.clone(),
            item_id: self.item_id.clone(),
            url: self.url.clone(),
            title: self.title.clone(),
            order: self.order.clone(),
            created_ms: self.created_ms,
            last_active_ms: self.last_active_ms,
        }
    }

    pub fn display_title(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else if self.url.is_empty() || self.url == "about:blank" {
            "New Tab".into()
        } else {
            crossbow_core::search::display_url(&self.url)
        }
    }

    pub fn is_loading(&self) -> bool {
        self.view.is_some() && self.nav.loading
    }

    pub fn can_auto_archive(&self) -> bool {
        if self.nav.auto_archive_protected || self.nav.loading || self.nav.crashed { return false; }
        // A cold saved tab has no live document. For a live tab, do not trust the
        // snapshot: input may have arrived since sync_from_view earlier this frame.
        self.view.as_ref().is_none_or(WebView::can_auto_archive)
    }

    /// Pull the latest navigation state from the web view. Returns true when
    /// url or title changed (the row must be saved).
    pub fn sync_from_view(&mut self) -> bool {
        let Some(v) = &self.view else { return false };
        self.nav = v.nav();
        let mut changed = false;
        if !self.nav.url.is_empty() && self.nav.url != self.url {
            self.url = self.nav.url.clone();
            changed = true;
        }
        if !self.nav.title.is_empty() && self.nav.title != self.title {
            self.title = self.nav.title.clone();
            changed = true;
        }
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub fn unload(&mut self) {
        if let Some(v) = self.view.take() {
            v.close();
        }
        self.nav.loading = false;
    }
}

/// A Space's Today tabs (not pinned), in sidebar order.
pub fn today<'a>(tabs: &'a [Tab], space: &str) -> Vec<&'a Tab> {
    let mut v: Vec<&Tab> = tabs.iter().filter(|t| t.space_id == space && t.item_id.is_none()).collect();
    v.sort_by(|a, b| a.order.cmp(&b.order).then(a.created_ms.cmp(&b.created_ms)));
    v
}

/// Split view: up to four tabs side by side in one space.
#[derive(Clone, Debug)]
pub struct Split {
    pub tabs: Vec<String>,
    /// Width fractions, same length as `tabs`, summing to 1.
    pub ratios: Vec<f32>,
}

impl Split {
    pub fn new(a: String, b: String) -> Split {
        Split { tabs: vec![a, b], ratios: vec![0.5, 0.5] }
    }

    pub fn add(&mut self, tab: String) {
        if self.tabs.len() < 4 && !self.tabs.contains(&tab) {
            self.tabs.push(tab);
            let n = self.tabs.len() as f32;
            self.ratios = vec![1.0 / n; self.tabs.len()];
        }
    }

    pub fn remove(&mut self, tab: &str) {
        if let Some(i) = self.tabs.iter().position(|t| t == tab) {
            self.tabs.remove(i);
            let r = self.ratios.remove(i);
            let n = self.ratios.len() as f32;
            for x in &mut self.ratios {
                *x += r / n.max(1.0);
            }
        }
    }
}
