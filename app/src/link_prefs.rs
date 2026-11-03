//! Settings → Links choices kept on this device only, in the store's
//! key/value table, so they never sync. A failed save keeps the previous
//! choice in use and offers it again.

use crate::quit::SaveFailure;
use crossbow_core::model::archive_label;
use crossbow_core::store::Store;
use std::marker::PhantomData;

/// An on/off choice: where it is kept and how Settings names it.
pub trait SwitchSpec {
    const KEY: &'static str;
    const DEFAULT: bool;
    const TITLE: &'static str;
    const DESCRIPTION: &'static str;
    /// What keeps happening while a save fails, after an off or on choice.
    const STILL_OFF: &'static str;
    const STILL_ON: &'static str;
    /// Why the saved choice was not read, and what happens meanwhile.
    const UNREADABLE: &'static str;
}

/// Links from other apps open in a Little Window instead of a tab.
#[derive(Debug)]
pub struct OtherAppLinks;

impl SwitchSpec for OtherAppLinks {
    const KEY: &'static str = crate::app::little::LINK_KEY;
    const DEFAULT: bool = false;
    const TITLE: &'static str = crate::app::little::LINK_TITLE;
    const DESCRIPTION: &'static str = "Each link opens in a small window of its own, in the Space and profile chosen here. Open in Space… moves it to the main window. This device only.";
    const STILL_OFF: &'static str = "Links from other apps still open as tabs";
    const STILL_ON: &'static str = "Links from other apps still open in Little Windows";
    const UNREADABLE: &'static str = "The saved choice for links from other apps could not be read, so they open as tabs. Changing it saves over it.";
}

/// ⌥⌘-click opens a link in a Little Window. On by default, as in Arc.
#[derive(Debug)]
pub struct OptionClick;

impl SwitchSpec for OptionClick {
    const KEY: &'static str = "option_command_click_opens_little_window";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "⌥⌘-click opens links in a Little Window";
    const DESCRIPTION: &'static str = "The window uses the Space and profile of the tab you clicked in. Off, ⌥⌘-click opens a background tab, as ⌘-click does. This device only.";
    const STILL_OFF: &'static str = "⌥⌘-click still opens a background tab";
    const STILL_ON: &'static str = "⌥⌘-click still opens a Little Window";
    const UNREADABLE: &'static str = "The saved choice for ⌥⌘-click could not be read, so it opens a Little Window. Changing it saves over it.";
}

/// Shift-click opens a link in Peek over its tab.
#[derive(Debug)]
pub struct ShiftPeek;

impl SwitchSpec for ShiftPeek {
    const KEY: &'static str = "shift_click_opens_peek";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Shift-click opens links in Peek";
    const DESCRIPTION: &'static str = "The link opens in a preview over the tab you clicked in. Off, Shift-click opens a new tab. This device only.";
    const STILL_OFF: &'static str = "Shift-click still opens a new tab";
    const STILL_ON: &'static str = "Shift-click still opens Peek";
    const UNREADABLE: &'static str = "The saved choice for Shift-click could not be read, so it opens Peek. Changing it saves over it.";
}

#[derive(Debug)]
pub struct DeviceSwitch<S> {
    /// Where it is kept: `S::KEY`, or one profile's copy of it.
    key: String,
    enabled: bool,
    /// Why the saved choice was not read; cleared by the next save.
    pub notice: Option<String>,
    pub failure: Option<SaveFailure>,
    spec: PhantomData<S>,
}

impl<S: SwitchSpec> DeviceSwitch<S> {
    pub fn load(store: &Store) -> Self {
        Self::load_key(store, S::KEY.to_owned())
    }

    /// `profile`'s own choice, apart from other profiles' on this device.
    pub fn load_for(store: &Store, profile: &str) -> Self {
        Self::load_key(store, format!("{}.{profile}", S::KEY))
    }

    fn load_key(store: &Store, key: String) -> Self {
        let (enabled, notice) = match store.kv_get(&key) {
            Ok(None) => (S::DEFAULT, None),
            Ok(Some(raw)) if raw == "on" => (true, None),
            Ok(Some(raw)) if raw == "off" => (false, None),
            Ok(Some(_)) | Err(_) => (S::DEFAULT, Some(S::UNREADABLE.to_string())),
        };
        DeviceSwitch { key, enabled, notice, failure: None, spec: PhantomData }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Save this device's choice. On failure the previous one stays in use
    /// and `failure` offers it again.
    pub fn choose(&mut self, store: &Store, enabled: bool) {
        match store.kv_set(&self.key, if enabled { "on" } else { "off" }) {
            Ok(()) => {
                self.enabled = enabled;
                self.notice = None;
                self.failure = None;
            }
            Err(error) => self.failure = Some(SaveFailure { wanted: enabled, error: error.to_string(), reveal: true }),
        }
    }
}

const ARCHIVE_KEY: &str = "little_windows_archive_after_hours";
pub const ARCHIVE_TITLE: &str = "Archive Little Windows after";
/// Hours; 0 is Never.
pub const ARCHIVE_CHOICES: [u32; 4] = [0, 1, 12, 24];
/// As unpinned tabs archive by default.
const ARCHIVE_DEFAULT: u32 = 12;

/// Settings → Links → Archive Little Windows after: how long a window may
/// go unused before its page goes to the archive. Separate from the delay
/// for tabs.
#[derive(Debug)]
pub struct ArchiveAfter {
    hours: u32,
    /// Why the saved choice was not read; cleared by the next save.
    pub notice: Option<String>,
    pub failure: Option<SaveFailure<u32>>,
}

impl ArchiveAfter {
    pub fn load(store: &Store) -> Self {
        let (hours, notice) = match store.kv_get(ARCHIVE_KEY) {
            Ok(None) => (ARCHIVE_DEFAULT, None),
            Ok(Some(raw)) => match raw.parse::<u32>().ok().filter(|hours| ARCHIVE_CHOICES.contains(hours)) {
                Some(hours) => (hours, None),
                None => (ARCHIVE_DEFAULT, Some(Self::unreadable())),
            },
            Err(_) => (ARCHIVE_DEFAULT, Some(Self::unreadable())),
        };
        ArchiveAfter { hours, notice, failure: None }
    }

    fn unreadable() -> String {
        format!("The saved delay for archiving Little Windows could not be read, so they archive after {}. Choosing an option saves over it.", archive_label(ARCHIVE_DEFAULT))
    }

    pub fn hours(&self) -> u32 {
        self.hours
    }

    /// How long a window may go unused; None for Never.
    pub fn after_ms(&self) -> Option<u64> {
        (self.hours > 0).then(|| u64::from(self.hours) * 3_600_000)
    }

    /// Save this device's choice. On failure the previous one stays in use
    /// and `failure` offers it again.
    pub fn choose(&mut self, store: &Store, hours: u32) {
        match store.kv_set(ARCHIVE_KEY, &hours.to_string()) {
            Ok(()) => {
                self.hours = hours;
                self.notice = None;
                self.failure = None;
            }
            Err(error) => self.failure = Some(SaveFailure { wanted: hours, error: error.to_string(), reveal: true }),
        }
    }
}

/// The Settings → Links choices beside the link from other apps one.
#[derive(Debug)]
pub struct LinkPreferences {
    pub option_click: DeviceSwitch<OptionClick>,
    pub shift_peek: DeviceSwitch<ShiftPeek>,
    pub archive_after: ArchiveAfter,
}

impl LinkPreferences {
    pub fn load(store: &Store) -> Self {
        LinkPreferences { option_click: DeviceSwitch::load(store), shift_peek: DeviceSwitch::load(store), archive_after: ArchiveAfter::load(store) }
    }
}
