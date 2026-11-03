//! ⌘1–⌘9: which sidebar row each number goes to. Favorites count first
//! unless turned off, then the Space's pinned tabs outside collapsed folders,
//! then Today, as the sidebar shows them; ⌘9 goes to the last row or the
//! ninth. Rows come from the active Space and its profile only, and a number
//! past the rows does nothing. Both choices are kept on this device only.

use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crate::quit::SaveFailure;
use crate::tabs::Tab;
use crossbow_core::library::{Library, PinnedNode};
use crossbow_core::store::Store;

const MAC: bool = cfg!(target_os = "macos");

/// Favorites count first in ⌘1–⌘8. On by default, as before.
#[derive(Debug)]
pub struct IncludeFavorites;

impl SwitchSpec for IncludeFavorites {
    const KEY: &'static str = "numbered_tabs_include_favorites";
    const DEFAULT: bool = true;
    const TITLE: &'static str = if MAC { "Include Favorites in ⌘1–⌘8" } else { "Include Favorites in Ctrl+1–Ctrl+8" };
    const DESCRIPTION: &'static str = "Favorites count first, then the Space's pinned tabs and Today. When off, the numbers start at the Space's first tab. This device only.";
    const STILL_OFF: &'static str = "The numbers still start at the Space's first tab";
    const STILL_ON: &'static str = "Favorites still count first";
    const UNREADABLE: &'static str = "The saved choice for Favorites in tab numbers could not be read, so they count first. Changing it saves over it.";
}

/// Where the ninth number goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ninth {
    /// The last row, however many there are. As before, and as in Arc.
    Last,
    /// The ninth row, if there is one.
    Ninth,
}

impl Ninth {
    pub const ALL: [Ninth; 2] = [Ninth::Last, Ninth::Ninth];

    pub fn label(self) -> &'static str {
        match self {
            Ninth::Last => "Last tab",
            Ninth::Ninth => "Ninth tab",
        }
    }

    fn raw(self) -> &'static str {
        match self {
            Ninth::Last => "last",
            Ninth::Ninth => "ninth",
        }
    }
}

const NINTH_KEY: &str = "numbered_tabs_ninth";
pub const NINTH_TITLE: &str = if MAC { "⌘9 goes to" } else { "Ctrl+9 goes to" };
pub const NINTH_CHORD: &str = if MAC { "⌘9" } else { "Ctrl+9" };
/// Go to Last Tab's name while it goes to the ninth tab.
pub const NINTH_ACTION: &str = "Go to Tab 9";

/// This device's choice for the ninth number.
#[derive(Debug)]
pub struct NinthChoice {
    choice: Ninth,
    /// Why the saved choice was not read; cleared by the next save.
    pub notice: Option<String>,
    pub failure: Option<SaveFailure<Ninth>>,
}

impl NinthChoice {
    pub fn load(store: &Store) -> Self {
        let (choice, notice) = match store.kv_get(NINTH_KEY) {
            Ok(None) => (Ninth::Last, None),
            Ok(Some(raw)) => match Ninth::ALL.into_iter().find(|choice| choice.raw() == raw) {
                Some(choice) => (choice, None),
                None => (Ninth::Last, Some(Self::unreadable())),
            },
            Err(_) => (Ninth::Last, Some(Self::unreadable())),
        };
        NinthChoice { choice, notice, failure: None }
    }

    fn unreadable() -> String {
        format!("The saved choice for {NINTH_TITLE} could not be read, so it goes to the last tab. Choosing an option saves over it.")
    }

    pub fn choice(&self) -> Ninth {
        self.choice
    }

    /// Save this device's choice. On failure the previous one stays in use
    /// and `failure` offers it again.
    pub fn choose(&mut self, store: &Store, choice: Ninth) {
        match store.kv_set(NINTH_KEY, choice.raw()) {
            Ok(()) => {
                self.choice = choice;
                self.notice = None;
                self.failure = None;
            }
            Err(error) => self.failure = Some(SaveFailure { wanted: choice, error: error.to_string(), reveal: true }),
        }
    }
}

/// Both choices, read once when first needed.
#[derive(Debug, Default)]
pub struct NumberedTabs {
    favorites: Option<DeviceSwitch<IncludeFavorites>>,
    ninth: Option<NinthChoice>,
}

impl NumberedTabs {
    pub fn favorites_switch(&mut self, store: &Store) -> &mut DeviceSwitch<IncludeFavorites> {
        self.favorites.get_or_insert_with(|| DeviceSwitch::load(store))
    }

    pub fn ninth_choice(&mut self, store: &Store) -> &mut NinthChoice {
        self.ninth.get_or_insert_with(|| NinthChoice::load(store))
    }

    /// The row the number `n` goes to with this device's choices.
    pub fn target(&mut self, store: &Store, rows: &Rows, n: u8) -> Option<Row> {
        let favorites = self.favorites_switch(store).enabled();
        pick(rows, favorites, self.ninth_choice(store).choice(), n)
    }
}

/// A sidebar row a number can go to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A favorite or pinned tab, open or not.
    Item(String),
    /// A Today tab.
    Tab(String),
}

/// The rows numbers count, in the sidebar's order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub favorites: Vec<Row>,
    /// Pinned tabs outside collapsed folders, then Today.
    pub space: Vec<Row>,
}

/// `space`'s rows, with its profile's favorites.
pub fn rows(lib: &Library, tabs: &[Tab], space: &str) -> Rows {
    let Some(owner) = lib.spaces.get(space).filter(|s| lib.profiles.contains_key(&s.profile_id)) else { return Rows::default() };
    let profile = owner.profile_id.as_str();
    let valid_tab = |id: &str, expected: &str| tabs.iter().filter(|tab| tab.item_id.as_deref() == Some(id))
        .all(|tab| tab.space_id == expected && crate::app::tab_profile(lib, tab) == Some(profile));
    let favorites = lib.favorites(profile).into_iter().filter(|(id, _)| valid_tab(id, crate::tabs::FAVORITES_SPACE))
        .map(|(id, _)| Row::Item(id)).collect();
    fn walk(lib: &Library, space: &str, folder: Option<&str>, out: &mut Vec<Row>) {
        for node in lib.pinned_children(space, folder) {
            match node {
                PinnedNode::Item(id, _) => out.push(Row::Item(id)),
                PinnedNode::Folder(id, folder) => if !folder.collapsed { walk(lib, space, Some(&id), out) },
            }
        }
    }
    let mut rows = Vec::new();
    walk(lib, space, None, &mut rows);
    rows.retain(|row| match row {
        Row::Item(id) => lib.items.get(id).is_some_and(|item| item.profile_id == profile) && valid_tab(id, space),
        Row::Tab(_) => false,
    });
    rows.extend(crate::tabs::today(tabs, space).into_iter().filter(|tab| crate::app::tab_profile(lib, tab) == Some(profile))
        .map(|tab| Row::Tab(tab.id.clone())));
    Rows { favorites, space: rows }
}

/// The row the number `n`, 1 to 9, goes to. None past the rows.
pub fn pick(rows: &Rows, favorites: bool, ninth: Ninth, n: u8) -> Option<Row> {
    let counted: Vec<&Row> = if favorites { rows.favorites.iter().chain(&rows.space).collect() } else { rows.space.iter().collect() };
    let row = match n {
        9 if ninth == Ninth::Last => counted.last(),
        1..=9 => counted.get(usize::from(n) - 1),
        _ => None,
    };
    row.map(|row| (*row).clone())
}

#[cfg(test)]
#[path = "numbered_tabs_tests.rs"]
mod tests;
