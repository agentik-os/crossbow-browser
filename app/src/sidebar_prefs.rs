//! Settings → General → Sidebar: unread badges and hover previews, chosen for
//! each profile and kept on this device only, in the store's key/value
//! table, so they never sync. Each profile's choices are read once, not
//! every frame.

use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crossbow_core::store::Store;
use std::collections::HashMap;

/// Arc's Show Unread Badge. On by default, as in Arc.
#[derive(Debug)]
pub struct UnreadBadges;

impl SwitchSpec for UnreadBadges {
    const KEY: &'static str = "sidebar_unread_badges";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Show unread badges";
    const DESCRIPTION: &'static str = "When a pinned tab or favorite changes its title in the background, it shows a dot, or the count its title starts with. Viewing the tab clears it.";
    const STILL_OFF: &'static str = "Unread badges stay hidden";
    const STILL_ON: &'static str = "Unread badges still show";
    const UNREADABLE: &'static str = "The saved choice for unread badges could not be read, so they show. Changing it saves over it.";
}

/// Arc's Show Hover Preview. On by default, as in Arc.
#[derive(Debug)]
pub struct HoverPreviews;

impl SwitchSpec for HoverPreviews {
    const KEY: &'static str = "sidebar_hover_previews";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Show hover previews";
    const DESCRIPTION: &'static str = "Rest the pointer on a collapsed folder to see what's in it, or on a pinned tab or favorite that isn't loaded to see its site and when you last opened it. Nothing is loaded to make a preview.";
    const STILL_OFF: &'static str = "Hover previews stay off";
    const STILL_ON: &'static str = "Hover previews still show";
    const UNREADABLE: &'static str = "The saved choice for hover previews could not be read, so they show. Changing it saves over it.";
}

#[derive(Debug)]
pub struct Switches {
    pub unread: DeviceSwitch<UnreadBadges>,
    pub previews: DeviceSwitch<HoverPreviews>,
}

#[derive(Debug, Default)]
pub struct SidebarPrefs {
    profiles: HashMap<String, Switches>,
}

impl SidebarPrefs {
    /// `profile`'s switches, read from the store the first time they are needed.
    pub fn get(&mut self, store: &Store, profile: &str) -> &mut Switches {
        if !self.profiles.contains_key(profile) {
            self.profiles.insert(profile.to_owned(), Switches { unread: DeviceSwitch::load_for(store, profile), previews: DeviceSwitch::load_for(store, profile) });
        }
        self.profiles.get_mut(profile).expect("inserted above")
    }

    pub fn unread(&mut self, store: &Store, profile: &str) -> bool {
        self.get(store, profile).unread.enabled()
    }

    pub fn previews(&mut self, store: &Store, profile: &str) -> bool {
        self.get(store, profile).previews.enabled()
    }
}
