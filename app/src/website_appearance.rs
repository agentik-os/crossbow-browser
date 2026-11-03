//! Website appearance: Automatic, Light or Dark for each profile's pages,
//! kept on this device only. Every profile's choice goes to the engine at
//! launch, before a page opens, and a new choice as soon as it's saved; a
//! failed save keeps the previous one in use. Pages are asked through
//! `prefers-color-scheme`, never restyled.

use crate::quit::SaveFailure;
use crossbow_core::library::Library;
use crossbow_core::store::Store;
pub use crossbow_engine::Appearance;
use std::collections::HashMap;

const KEY: &str = "website_appearance";
pub const TITLE: &str = "Website appearance";
pub const DESCRIPTION: &str = "Websites that have light and dark versions show the one chosen here. Automatic follows the system. Pages aren't restyled.";
pub const CHOICES: [Appearance; 3] = [Appearance::Automatic, Appearance::Light, Appearance::Dark];

pub fn label(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Automatic => "Automatic",
        Appearance::Light => "Light",
        Appearance::Dark => "Dark",
    }
}

fn raw(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Automatic => "automatic",
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    }
}

/// Where a saved choice goes: the engine, or a test's record of it.
pub trait Apply {
    fn apply(&mut self, profile: &str, appearance: Appearance);
}

pub struct Engine;

impl Apply for Engine {
    fn apply(&mut self, profile: &str, appearance: Appearance) {
        crossbow_engine::set_website_appearance(profile, appearance);
    }
}

/// One profile's choice on this device.
#[derive(Debug)]
pub struct ProfileAppearance {
    key: String,
    choice: Appearance,
    /// Why the saved choice was not read; cleared by the next save.
    pub notice: Option<String>,
    pub failure: Option<SaveFailure<Appearance>>,
}

impl ProfileAppearance {
    pub fn load(store: &Store, profile: &str) -> Self {
        let key = format!("{KEY}.{profile}");
        let (choice, notice) = match store.kv_get(&key) {
            Ok(None) => (Appearance::Automatic, None),
            Ok(Some(saved)) => match CHOICES.into_iter().find(|choice| raw(*choice) == saved) {
                Some(choice) => (choice, None),
                None => (Appearance::Automatic, Some(UNREADABLE.to_string())),
            },
            Err(_) => (Appearance::Automatic, Some(UNREADABLE.to_string())),
        };
        ProfileAppearance { key, choice, notice, failure: None }
    }

    pub fn choice(&self) -> Appearance {
        self.choice
    }

    /// Save the choice, then apply it. On failure the previous one stays in
    /// use and `failure` offers it again.
    fn choose(&mut self, store: &Store, choice: Appearance) -> bool {
        match store.kv_set(&self.key, raw(choice)) {
            Ok(()) => {
                self.choice = choice;
                self.notice = None;
                self.failure = None;
                true
            }
            Err(error) => {
                self.failure = Some(SaveFailure { wanted: choice, error: error.to_string(), reveal: true });
                false
            }
        }
    }
}

const UNREADABLE: &str = "The saved website appearance could not be read, so websites follow the system. Choosing an option saves over it.";

/// Every profile's choice, read once when first needed.
pub struct WebsiteAppearance {
    profiles: HashMap<String, ProfileAppearance>,
    apply: Box<dyn Apply>,
}

impl Default for WebsiteAppearance {
    fn default() -> Self {
        WebsiteAppearance::new(Box::new(Engine))
    }
}

impl WebsiteAppearance {
    pub fn new(apply: Box<dyn Apply>) -> Self {
        WebsiteAppearance { profiles: HashMap::new(), apply }
    }

    /// Give the engine every profile's saved choice, so each profile's first
    /// page already opens with it.
    pub fn launch(&mut self, lib: &Library) {
        for profile in lib.profiles.keys() {
            let choice = self.get(&lib.store, profile).choice();
            self.apply.apply(profile, choice);
        }
    }

    pub fn get(&mut self, store: &Store, profile: &str) -> &mut ProfileAppearance {
        self.profiles.entry(profile.to_owned()).or_insert_with(|| ProfileAppearance::load(store, profile))
    }

    /// Save `profile`'s choice and, once saved, apply it to its pages.
    pub fn choose(&mut self, store: &Store, profile: &str, choice: Appearance) {
        let saved = self.profiles.entry(profile.to_owned()).or_insert_with(|| ProfileAppearance::load(store, profile)).choose(store, choice);
        if saved { self.apply.apply(profile, choice); }
    }
}

#[cfg(test)]
#[path = "website_appearance_tests.rs"]
pub(crate) mod tests;
