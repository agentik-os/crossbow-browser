//! New Document: a new tab in the active Space, in that Space's profile, at
//! the profile's document app. With none chosen it opens Settings at the
//! choice instead, which is how a person sets one.

use crossbow_core::document_app;
use crossbow_core::library::Library;

/// The command bar's name for New Document until the profile has an app.
pub const CHOOSE: &str = "Choose a Document App…";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewDocument {
    /// Open `url` in a new tab of `space`, which uses `profile`.
    Open { space: String, profile: String, url: String },
    /// No app is chosen for the Space's profile, or it has no usable profile.
    Choose,
}

/// What New Document does in `space` now.
pub fn resolve(lib: &Library, space: &str) -> NewDocument {
    let Some(owner) = lib.spaces.get(space).filter(|owner| lib.profiles.contains_key(&owner.profile_id)) else { return NewDocument::Choose };
    match document_app::load(&lib.store, &owner.profile_id).0.address() {
        Some(url) => NewDocument::Open { space: space.to_owned(), profile: owner.profile_id.clone(), url: url.to_owned() },
        None => NewDocument::Choose,
    }
}

/// Whether the command bar offers New Document rather than Choose a Document App.
pub fn has_app(lib: &Library, space: &str) -> bool {
    matches!(resolve(lib, space), NewDocument::Open { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::tests::Fixture;
    use crossbow_core::document_app::DocumentApp;
    use crossbow_core::model::Theme;

    #[test]
    fn new_document_opens_the_active_spaces_own_profiles_app_and_otherwise_asks_for_one() {
        let fixture = Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let personal_space = lib.spaces_ordered()[0].0.clone();
        let personal = lib.spaces[&personal_space].profile_id.clone();
        let work = lib.create_profile("Work").unwrap();
        let work_space = lib.create_space("Work", "W", Theme::preset(2), &work).unwrap();
        let reading_space = lib.create_space("Reading", "R", Theme::preset(3), &personal).unwrap();

        assert_eq!(resolve(&lib, &personal_space), NewDocument::Choose, "None by default");
        assert!(!has_app(&lib, &personal_space));
        document_app::save(&lib.store, &work, &DocumentApp::GoogleSheets).unwrap();
        document_app::save(&lib.store, &personal, &DocumentApp::Custom("https://pad.example/new".into())).unwrap();
        assert_eq!(resolve(&lib, &work_space), NewDocument::Open { space: work_space.clone(), profile: work.clone(), url: "https://sheets.new".into() });
        assert_eq!(resolve(&lib, &personal_space), NewDocument::Open { space: personal_space.clone(), profile: personal.clone(), url: "https://pad.example/new".into() });
        assert_eq!(resolve(&lib, &reading_space), NewDocument::Open { space: reading_space.clone(), profile: personal.clone(), url: "https://pad.example/new".into() }, "another Space of the same profile");
        assert!(has_app(&lib, &work_space));

        // Read back after reopening.
        let lib = Library::open(fixture.store()).unwrap();
        assert_eq!(resolve(&lib, &work_space), NewDocument::Open { space: work_space.clone(), profile: work.clone(), url: "https://sheets.new".into() });
        // Never another profile's app: a missing Space or profile asks instead.
        assert_eq!(resolve(&lib, "missing"), NewDocument::Choose);
        let mut lib = lib;
        lib.profiles.remove(&work);
        assert_eq!(resolve(&lib, &work_space), NewDocument::Choose);
        document_app::save(&lib.store, &personal, &DocumentApp::None).unwrap();
        assert_eq!(resolve(&lib, &personal_space), NewDocument::Choose, "None again asks again");
    }

    #[test]
    fn the_command_opens_a_tab_in_the_active_space_or_settings_at_the_choice() {
        // Running it needs the whole app: checked in source.
        let app = include_str!("app.rs");
        assert!(app.contains("Command::NewDocument => push(Action::NewDocument),"));
        assert!(app.contains("Action::NewDocument => match crate::new_document::resolve(&self.lib, &self.active_space) {\n                    crate::new_document::NewDocument::Open { url, .. } => { self.open_new_tab(&url, false); }\n                    crate::new_document::NewDocument::Choose => self.actions.push(Action::ChooseDocumentApp),"));
        assert!(app.contains("let mut settings = ui::settings::SettingsState::new(ui::settings::Page::General, now);\n                    settings.reveal_document_app();"));
        // A new tab goes to the active Space, whose profile it then uses.
        assert!(app.contains("fn open_new_tab(&mut self, url: &str, background: bool) -> String {\n        let after = self.active_tab_id().and_then(|id| self.tab(&id).filter(|t| t.item_id.is_none()).map(|t| t.order.clone()));\n        let space = self.active_space.clone();"));
        let bar = include_str!("ui/command_bar.rs");
        assert!(bar.contains("NEW_DOCUMENT => Some(Action::NewDocument),\n                CHOOSE_DOCUMENT_APP => Some(Action::ChooseDocumentApp),"));
    }
}
