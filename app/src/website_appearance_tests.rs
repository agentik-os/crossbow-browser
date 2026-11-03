use super::*;
use crate::shortcuts::tests::Fixture;
use std::cell::RefCell;
use std::rc::Rc;

/// The engine's part, recorded: each profile and the choice it was given.
#[derive(Clone, Default)]
pub(crate) struct Applied(pub(crate) Rc<RefCell<Vec<(String, Appearance)>>>);

impl Apply for Applied {
    fn apply(&mut self, profile: &str, appearance: Appearance) {
        self.0.borrow_mut().push((profile.to_owned(), appearance));
    }
}

impl Applied {
    pub(crate) fn take(&self) -> Vec<(String, Appearance)> {
        let mut applied = std::mem::take(&mut *self.0.borrow_mut());
        applied.sort_by(|a, b| a.0.cmp(&b.0));
        applied
    }
}

#[test]
fn launch_gives_the_engine_every_profiles_saved_choice_before_a_page_opens() {
    let fixture = Fixture::new();
    let mut lib = Library::open(fixture.store()).unwrap();
    lib.ensure_defaults().unwrap();
    let personal = lib.default_profile_id();
    let work = lib.create_profile("Work").unwrap();
    let applied = Applied::default();
    let mut appearance = WebsiteAppearance::new(Box::new(applied.clone()));
    appearance.choose(&lib.store, &work, Appearance::Dark);
    assert_eq!(applied.take(), [(work.clone(), Appearance::Dark)], "applied once saved, to that profile only");

    // Quit and reopen.
    let lib = Library::open(fixture.store()).unwrap();
    let mut appearance = WebsiteAppearance::new(Box::new(applied.clone()));
    appearance.launch(&lib);
    let mut expected = vec![(personal.clone(), Appearance::Automatic), (work.clone(), Appearance::Dark)];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(applied.take(), expected);
    assert_eq!(appearance.get(&lib.store, &personal).choice(), Appearance::Automatic, "follows the system until chosen");
}

#[test]
fn a_failed_save_keeps_the_choice_in_use_and_never_reaches_the_engine() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let applied = Applied::default();
    let mut appearance = WebsiteAppearance::new(Box::new(applied.clone()));
    appearance.choose(&store, "personal", Appearance::Light);
    applied.take();

    let read_only = fixture.read_only();
    appearance.choose(&read_only, "personal", Appearance::Dark);
    let personal = appearance.get(&read_only, "personal");
    assert_eq!(personal.choice(), Appearance::Light);
    assert_eq!(personal.failure.as_ref().map(|failure| failure.wanted), Some(Appearance::Dark));
    assert!(applied.take().is_empty(), "pages keep the saved choice");
    assert_eq!(ProfileAppearance::load(&store, "personal").choice(), Appearance::Light, "nothing was written");

    // Saved at last: the failure clears and the engine hears of it.
    appearance.choose(&store, "personal", Appearance::Dark);
    assert!(appearance.get(&store, "personal").failure.is_none());
    assert_eq!(applied.take(), [("personal".to_string(), Appearance::Dark)]);
}

#[test]
fn a_value_this_build_doesnt_know_reads_as_automatic_and_says_so() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.kv_set("website_appearance.personal", "sepia").unwrap();
    let unknown = ProfileAppearance::load(&store, "personal");
    assert_eq!(unknown.choice(), Appearance::Automatic);
    assert_eq!(unknown.notice.as_deref(), Some(UNREADABLE));
    assert_eq!(ProfileAppearance::load(&store, "work").notice, None, "another profile is unaffected");
}
