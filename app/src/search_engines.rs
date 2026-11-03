//! Device-local, per-profile search settings: whether the command bar asks the
//! search engine for suggestions, the engines added to the profile, and which
//! added engine replaces the profile's built-in choice. None of this enters the
//! synced profile: older builds mark a profile whose preferences hold unknown
//! fields unavailable, and suggestion consent stays with the Mac it was given on.
//!
//! Each profile starts with the default sites in `crossbow_core::search::SITES`
//! as ordinary added engines. They are shown until the profile's settings are
//! first saved; from then on the saved list is all there is, so a deleted site
//! stays deleted.

use crate::suggestions::Request;
use crossbow_core::library::Library;
use crossbow_core::search::{CustomEngine, Engine, default_sites, keyword_search};
use crossbow_core::suggest::may_send;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPreferences {
    /// Off unless the user confirmed the privacy notice.
    #[serde(default)]
    pub suggestions: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<CustomEngine>,
    /// An added engine used instead of the profile's built-in engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

fn key(profile: &str) -> String { format!("search.v1.{profile}") }

/// Present once the profile's settings, with its sites, have been saved. Kept
/// apart from `key` so builds without sites still read the settings.
fn sites_key(profile: &str) -> String { format!("search.sites.v1.{profile}") }

pub const UNREADABLE: &str = "The saved search settings for this profile can't be read";

impl SearchPreferences {
    pub fn load(lib: &Library, profile: &str) -> Result<Self, String> {
        if !lib.profiles.contains_key(profile) { return Err("This profile no longer exists.".into()); }
        let read = |key: String| lib.store.kv_get(&key).map_err(|error| format!("Could not read search settings: {error}"));
        let mut preferences = match read(key(profile))? {
            Some(raw) => serde_json::from_str::<Self>(&raw).ok().filter(Self::is_valid).ok_or_else(|| UNREADABLE.to_string())?,
            None => Self::default(),
        };
        if read(sites_key(profile))?.is_none() { preferences.add_sites(); }
        Ok(preferences)
    }

    pub fn save(&self, lib: &Library, profile: &str) -> Result<(), String> {
        if !lib.profiles.contains_key(profile) { return Err("this profile no longer exists".into()); }
        let raw = serde_json::to_string(self).map_err(|_| "could not encode the settings".to_string())?;
        lib.store.kv_set_all(&[(&key(profile), &raw), (&sites_key(profile), "1")]).map_err(|error| error.to_string())
    }

    /// The default sites after the saved engines, leaving out any whose id or
    /// name an engine already has.
    fn add_sites(&mut self) {
        for site in default_sites() {
            let lower = site.name.to_lowercase();
            if self.engines.iter().all(|engine| engine.id != site.id && engine.name.to_lowercase() != lower) { self.engines.push(site); }
        }
    }

    /// Engines that would pass the editor, with distinct ids and a default
    /// that names one of them.
    fn is_valid(&self) -> bool {
        let mut ids = std::collections::HashSet::new();
        self.engines.iter().all(|engine| ids.insert(engine.id.as_str()) && engine.clone().normalized() == *engine && engine.validate(&self.engines, &[]).is_ok())
            && self.default.as_deref().is_none_or(|id| ids.contains(id))
    }

    /// What a profile starts with, and what Reset restores.
    pub fn initial() -> Self {
        let mut preferences = Self::default();
        preferences.add_sites();
        preferences
    }

    pub fn default_engine(&self) -> Option<&CustomEngine> {
        self.default.as_deref().and_then(|id| self.engines.iter().find(|engine| engine.id == id))
    }
}

/// How the command bar searches what is typed for one profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarSearch {
    pub engine: Engine,
    /// The text sent to the engine: without the keyword, if one was typed.
    pub text: String,
    pub keyword: bool,
    pub suggestions: bool,
}

/// None when the profile's policy or its search settings can't be read:
/// search then stays off rather than going to an engine the user didn't pick.
pub fn bar_search(lib: &Library, profile: &str, query: &str) -> Option<BarSearch> {
    let resolved = lib.effective_profile_preferences(profile)?;
    let search = SearchPreferences::load(lib, profile).ok()?;
    let (engine, text, keyword) = match keyword_search(query, &search.engines) {
        Some((engine, text)) => (Engine::Custom(engine.clone()), text.to_string(), true),
        None => {
            let engine = search.default_engine().cloned().map_or(Engine::BuiltIn(resolved.search_engine), Engine::Custom);
            (engine, query.trim().to_string(), false)
        }
    };
    Some(BarSearch { engine, text, keyword, suggestions: search.suggestions })
}

impl BarSearch {
    /// The suggestion request for this search, if one may be sent.
    pub fn request(&self, profile: &str) -> Option<Request> {
        if !self.suggestions || !may_send(&self.text) { return None; }
        Some(Request { profile: profile.into(), url: self.engine.suggest_url(&self.text)? })
    }
}

/// The engine Tab scopes the search to for what is typed in `profile`, by
/// the rules of [`crossbow_core::search::tab_engine`].
pub fn tab_engine(lib: &Library, profile: &str, query: &str) -> Option<Engine> {
    lib.effective_profile_preferences(profile)?;
    let search = SearchPreferences::load(lib, profile).ok()?;
    crossbow_core::search::tab_engine(query, &search.engines)
}

/// The engine `profile` searches with when no keyword is typed.
pub fn profile_engine(lib: &Library, profile: &str) -> Option<Engine> {
    bar_search(lib, profile, "").map(|search| search.engine)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn engine(id: &str, name: &str, keyword: Option<&str>, search: &str, suggest: Option<&str>) -> CustomEngine {
        CustomEngine { id: id.into(), name: name.into(), keyword: keyword.map(Into::into), search_url: search.into(), suggest_url: suggest.map(Into::into) }
    }

    #[test]
    fn search_settings_survive_reopening_and_unreadable_ones_turn_search_off() {
        let fixture = crate::shortcuts::tests::Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        assert!(!profile.is_empty());
        assert_eq!(SearchPreferences::load(&lib, &profile), Ok(SearchPreferences::initial()), "a new profile has the default sites");
        let saved = SearchPreferences {
            suggestions: true,
            engines: vec![engine("e1", "Intranet", Some("in"), "https://search.example.test/?q=%s", Some("https://search.example.test/s?q=%s"))],
            default: Some("e1".into()),
        };
        saved.save(&lib, &profile).unwrap();
        let reopened = Library::open(fixture.store()).unwrap();
        assert_eq!(SearchPreferences::load(&reopened, &profile), Ok(saved.clone()));
        assert_eq!(profile_engine(&reopened, &profile), Some(Engine::Custom(saved.engines[0].clone())));
        // Nothing reaches the synced profile.
        assert_eq!(reopened.profiles[&profile].preferences, None);

        // A failed save keeps what was saved.
        let read_only = Library::open(fixture.read_only()).unwrap();
        assert!(SearchPreferences::default().save(&read_only, &profile).is_err());
        assert_eq!(SearchPreferences::load(&Library::open(fixture.store()).unwrap(), &profile), Ok(saved));

        for bad in ["not json", r#"{"suggestions":true,"extra":1}"#, r#"{"engines":[{"id":"a","name":"","search_url":"https://x.test/?q=%s"}]}"#, r#"{"default":"missing"}"#,
            r#"{"engines":[{"id":"a","name":"A","search_url":"https://a.test/?q=%s"},{"id":"a","name":"B","search_url":"https://b.test/?q=%s"}]}"#] {
            lib.store.kv_set(&key(&profile), bad).unwrap();
            assert_eq!(SearchPreferences::load(&lib, &profile), Err(UNREADABLE.into()), "{bad}");
            assert_eq!(profile_engine(&lib, &profile), None, "{bad}");
            assert_eq!(bar_search(&lib, &profile, "rust"), None, "{bad}");
        }
        assert!(SearchPreferences::load(&lib, "missing-profile").is_err());
    }

    #[test]
    fn default_sites_join_each_profile_once_and_edits_survive_reopening() {
        use crossbow_core::search::SITES;
        let fixture = crate::shortcuts::tests::Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let a = lib.default_profile_id();
        let b = lib.create_profile("Second").unwrap();
        let names = |lib: &Library, profile: &str| SearchPreferences::load(lib, profile).unwrap().engines.into_iter().map(|engine| engine.name).collect::<Vec<_>>();
        let sites: Vec<String> = SITES.iter().map(|(_, name, _)| (*name).to_string()).collect();
        assert_eq!(names(&lib, &a), sites, "every profile starts with the sites");
        assert_eq!(names(&lib, &b), sites);
        assert_eq!(lib.store.kv_get(&key(&a)).unwrap(), None, "reading writes nothing");

        // Deleting one and editing another in one profile survives reopening;
        // the deleted site does not come back, and the other profile keeps all five.
        let mut edited = SearchPreferences::load(&lib, &a).unwrap();
        edited.engines.retain(|engine| engine.id != "site-youtube");
        edited.engines[0].search_url = "https://de.wikipedia.org/w/index.php?search=%s".into();
        edited.save(&lib, &a).unwrap();
        let reopened = Library::open(fixture.store()).unwrap();
        assert_eq!(SearchPreferences::load(&reopened, &a), Ok(edited));
        assert_eq!(names(&reopened, &b), sites);

        // Engines saved before the sites existed keep their place, and a site
        // whose name is taken is left out.
        lib.store.kv_set(&key(&b), r#"{"engines":[{"id":"mine","name":"github","search_url":"https://git.example.test/?q=%s"}]}"#).unwrap();
        let expected = ["github", "Wikipedia", "YouTube", "MDN", "Google Maps"];
        assert_eq!(names(&lib, &b), expected);
        // A failed save changes neither the engines nor whether the sites were added.
        let read_only = Library::open(fixture.read_only()).unwrap();
        assert!(SearchPreferences::default().save(&read_only, &b).is_err());
        assert_eq!(names(&Library::open(fixture.store()).unwrap(), &b), expected);
        // Reset brings the sites back.
        SearchPreferences::initial().save(&lib, &b).unwrap();
        assert_eq!(names(&Library::open(fixture.store()).unwrap(), &b), sites);
    }
}
