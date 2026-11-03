//! Settings → Spaces: the selected Space's profile and profile management.
//! Moves and deletions close live views, so they run as app actions and their
//! outcome comes back here; names are edited in the library directly.

use super::*;
use crate::app::ProfileDeletion;
use crossbow_core::library::PROFILE_NAME_MAX;

enum Stage {
    Confirm,
    Pending,
    Failed(String),
}

struct Move {
    space: String,
    profile: String,
    space_name: String,
    profile_name: String,
    stage: Stage,
}

struct Rename {
    profile: String,
    draft: String,
    error: Option<String>,
}

struct Delete {
    profile: String,
    name: String,
    stage: Stage,
}

#[derive(Default)]
pub(super) struct ProfilesState {
    new_name: String,
    create_error: Option<String>,
    moving: Option<Move>,
    renaming: Option<Rename>,
    deleting: Option<Delete>,
    /// The latest outcome: text, and whether it is an error.
    notice: Option<(String, bool)>,
    reveal: bool,
    sites: super::site_page::SitesState,
}

impl ProfilesState {
    /// Adding a site: Escape cancels that instead of closing Settings.
    pub(super) fn owns_keyboard(&self) -> bool {
        self.sites.owns_keyboard()
    }

    /// Leaving the page discards a site not yet added.
    pub(super) fn leave(&mut self) {
        self.sites.cancel();
    }

    fn note(&mut self, text: String, error: bool) {
        self.notice = Some((text, error));
        self.reveal = true;
    }

    /// Choosing another profile asks first; choosing the current one cancels.
    fn choose(&mut self, lib: &Library, space: &str, profile: &str) {
        let (Some(current), Some(target)) = (lib.spaces.get(space), lib.profiles.get(profile)) else { return };
        if current.profile_id == profile {
            self.moving = None;
            return;
        }
        self.moving = Some(Move { space: space.into(), profile: profile.into(), space_name: current.name.clone(), profile_name: target.name.clone(), stage: Stage::Confirm });
        self.reveal = true;
    }

    fn confirm_move(&mut self, actions: &mut Vec<Action>) {
        let Some(moving) = self.moving.as_mut().filter(|moving| !matches!(moving.stage, Stage::Pending)) else { return };
        moving.stage = Stage::Pending;
        actions.push(Action::MoveSpaceToProfile { space: moving.space.clone(), profile: moving.profile.clone() });
    }

    pub(super) fn moved(&mut self, space: &str, profile: &str, result: Result<(), String>) {
        let Some(moving) = self.moving.as_mut().filter(|moving| moving.space == space && moving.profile == profile) else { return };
        match result {
            Ok(()) => {
                let text = format!("“{}” now uses “{}”. Its tabs reopen in that profile.", moving.space_name, moving.profile_name);
                self.moving = None;
                self.note(text, false);
            }
            Err(error) => {
                moving.stage = Stage::Failed(error);
                self.reveal = true;
            }
        }
    }

    fn create(&mut self, lib: &mut Library, use_for: Option<&str>) {
        match lib.create_named_profile(&self.new_name) {
            Ok(id) => {
                let name = lib.profiles[&id].name.clone();
                self.new_name.clear();
                self.create_error = None;
                self.note(format!("Created “{name}”."), false);
                if let Some(space) = use_for { self.choose(lib, space, &id); }
            }
            Err(error) => {
                self.create_error = Some(format!("Could not create the profile. {error}"));
                self.reveal = true;
            }
        }
    }

    fn rename(&mut self, lib: &mut Library) {
        let Some(rename) = self.renaming.as_mut() else { return };
        match lib.rename_profile(&rename.profile, &rename.draft) {
            Ok(()) => {
                let name = lib.profiles.get(&rename.profile).map(|profile| profile.name.clone()).unwrap_or_default();
                self.renaming = None;
                self.note(format!("Renamed to “{name}”."), false);
            }
            Err(error) => rename.error = Some(format!("Could not rename. {error}")),
        }
    }

    fn request_delete(&mut self, lib: &Library, profile: &str) {
        let Some(name) = lib.profiles.get(profile).map(|profile| profile.name.clone()) else { return };
        let users = space_names(lib, profile);
        self.deleting = None;
        if !users.is_empty() {
            self.note(format!("“{name}” is used by {users}. Move {} to another profile first.", if users.contains(", ") { "those Spaces" } else { "that Space" }), true);
            return;
        }
        self.notice = None;
        self.deleting = Some(Delete { profile: profile.into(), name, stage: Stage::Confirm });
        self.reveal = true;
    }

    fn confirm_delete(&mut self, actions: &mut Vec<Action>) {
        let Some(deleting) = self.deleting.as_mut().filter(|deleting| !matches!(deleting.stage, Stage::Pending)) else { return };
        deleting.stage = Stage::Pending;
        actions.push(Action::DeleteProfile(deleting.profile.clone()));
    }

    pub(super) fn deleted(&mut self, profile: &str, result: Result<ProfileDeletion, String>) {
        let Some(deleting) = self.deleting.as_mut().filter(|deleting| deleting.profile == profile) else { return };
        let name = deleting.name.clone();
        match result {
            Ok(outcome) => {
                self.deleting = None;
                if self.renaming.as_ref().is_some_and(|rename| rename.profile == profile) { self.renaming = None; }
                let ProfileDeletion { data_pending, backups } = outcome;
                let note = match data_pending {
                    None => format!("Deleted “{name}” and its data on this device."),
                    Some(error) => format!("Deleted “{name}”. Some of its data on this device could not be removed yet ({error}); Crossbow removes it the next time it starts."),
                };
                match crate::backups::removed_note(backups) {
                    Some(more) => self.note(format!("{note} {more}"), false),
                    None => self.note(note, false),
                }
            }
            Err(error) => {
                deleting.stage = Stage::Failed(error);
                self.reveal = true;
            }
        }
    }

    /// Drop confirmations whose Space or profile disappeared, for example through sync.
    fn forget_stale(&mut self, lib: &Library) {
        if self.moving.as_ref().is_some_and(|moving| !matches!(moving.stage, Stage::Pending)
            && (!lib.spaces.contains_key(&moving.space) || !lib.profiles.contains_key(&moving.profile))) {
            self.moving = None;
        }
        if self.renaming.as_ref().is_some_and(|rename| !lib.profiles.contains_key(&rename.profile)) { self.renaming = None; }
        if self.deleting.as_ref().is_some_and(|deleting| !matches!(deleting.stage, Stage::Pending) && !lib.profiles.contains_key(&deleting.profile)) {
            self.deleting = None;
        }
    }
}

/// Profiles in creation order.
fn ordered(lib: &Library) -> Vec<(String, String)> {
    let mut profiles: Vec<_> = lib.profiles.iter().map(|(id, profile)| (id.clone(), profile.name.clone(), profile.created_ms)).collect();
    profiles.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
    profiles.into_iter().map(|(id, name, _)| (id, name)).collect()
}

fn space_names(lib: &Library, profile: &str) -> String {
    lib.spaces_using_profile(profile).iter().map(|id| lib.spaces[id].name.as_str()).collect::<Vec<_>>().join(", ")
}

/// A sentence with its two buttons; returns (primary, cancel) clicks.
pub(super) fn decision(ui: &mut Ui, ink: &Ink, text: &str, error: bool, primary: &str, busy: bool, reveal: &mut bool) -> (bool, bool) {
    let mut clicked = (false, false);
    let response = ui.vertical(|ui| {
        ui.label(style::text(text, 12.5, style::weight::REGULAR).color(if error { ink.danger } else { ink.text }));
        ui.horizontal_wrapped(|ui| {
            clicked.0 = Button::new(primary).kind(Kind::Primary).loading(busy).show(ui, ink).clicked();
            clicked.1 = Button::new("Cancel").kind(Kind::Plain).enabled(!busy).show(ui, ink).clicked();
        });
    }).response;
    if std::mem::take(reveal) { response.scroll_to_me(Some(egui::Align::Center)); }
    clicked
}

/// This device's site profiles, Air Traffic Control's rules, and a site a
/// tab's menu asked to add.
pub(super) struct Sites<'a> {
    pub profiles: &'a mut crossbow_core::site_profiles::SiteProfiles,
    pub routing: &'a crossbow_core::link_routing::Routing,
    pub prefill: &'a mut Option<(String, String)>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show(st: &mut ProfilesState, lib: &mut Library, space: &str, new_space: &mut Option<String>, sites: Sites<'_>, actions: &mut Vec<Action>, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    st.forget_stale(lib);
    if let Some((profile, host)) = sites.prefill.take() { st.sites.prefill(&profile, &host); }
    if st.sites.owns_keyboard() && !egui::Popup::is_any_open(ui.ctx()) && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        st.sites.cancel();
    }
    let profiles = ordered(lib);
    let Some(current) = lib.spaces.get(space).map(|space| space.profile_id.clone()) else { return };

    section(ui, pal, "Profile");
    let current_name = lib.profiles.get(&current).map(|profile| profile.name.clone()).unwrap_or_else(|| "Unavailable profile".into());
    let mut chosen = None;
    row(ui, pal, "Profile", "Sites in this Space use this profile's cookies, sign-ins and favorites.", |ui| {
        egui::ComboBox::from_id_salt(("space-profile", space)).selected_text(&current_name).width(170.0).truncate().show_ui(ui, |ui| {
            keep_popup_above_sheet(ui);
            for (id, name) in &profiles {
                let usable = lib.effective_profile_preferences(id).is_some();
                if ui.add_enabled(usable, egui::Button::selectable(*id == current, name)).clicked() { chosen = Some(id.clone()); }
            }
        });
    });
    if let Some(profile) = chosen { st.choose(lib, space, &profile); }
    if let Some(moving) = st.moving.as_ref().filter(|moving| moving.space == space) {
        let (text, error, primary) = match &moving.stage {
            Stage::Failed(error) => (format!("Could not move “{}” to “{}”. {error} Nothing was changed.", moving.space_name, moving.profile_name), true, "Retry"),
            _ => (format!("Move “{}” to “{}”? Its tabs reload in that profile, so sites use that profile's sign-ins.", moving.space_name, moving.profile_name), false, "Confirm"),
        };
        let busy = matches!(moving.stage, Stage::Pending);
        let (confirm, cancel) = decision(ui, &ink, &text, error, primary, busy, &mut st.reveal);
        if cancel { st.moving = None; }
        else if confirm {
            st.confirm_move(actions);
            ui.ctx().request_repaint();
        }
    }

    section(ui, pal, "Profiles");
    ui.label(style::text("Each profile keeps its own cookies, sign-ins, history and favorites on this device. Moving a Space never copies them.", 12.0, style::weight::REGULAR).color(ink.secondary));
    if new_space.as_ref().is_some_and(|id| lib.effective_profile_preferences(id).is_none()) { *new_space = None; }
    row(ui, pal, "New Spaces use", "Applies to New Space in the sidebar, command bar and this page.", |ui| {
        let label = new_space.as_ref().and_then(|id| lib.profiles.get(id)).map(|profile| profile.name.clone()).unwrap_or_else(|| "Active Space's profile".into());
        egui::ComboBox::from_id_salt("new-space-profile").selected_text(label).width(170.0).truncate().show_ui(ui, |ui| {
            keep_popup_above_sheet(ui);
            ui.selectable_value(new_space, None, "Active Space's profile");
            for (id, name) in &profiles {
                if lib.effective_profile_preferences(id).is_some() { ui.selectable_value(new_space, Some(id.clone()), name); }
            }
        });
    });

    let only = profiles.len() <= 1;
    for (id, name) in &profiles {
        let mut rename = false;
        let mut delete = false;
        ui.push_id(("profile-row", id), |ui| {
            // Buttons first, so a long name truncates instead of pushing them out.
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = Button::new("Delete…").kind(Kind::Plain).size(style::Size::Small).enabled(!only).show(ui, &ink);
                delete = response.clicked();
                if only { response.on_disabled_hover_text("Keep at least one profile."); }
                rename = Button::new("Rename").kind(Kind::Plain).size(style::Size::Small).show(ui, &ink).clicked();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(style::text(name, 13.5, style::weight::MEDIUM).color(ink.text)).truncate());
                });
            });
            let users = space_names(lib, id);
            let usage = if users.is_empty() { "Not used by any Space".to_string() } else { format!("Used by {users}") };
            ui.label(style::text(usage, 12.0, style::weight::REGULAR).color(ink.secondary));

            if let Some(edit) = st.renaming.as_mut().filter(|edit| edit.profile == *id) {
                let mut save = false;
                let mut cancel = false;
                ui.horizontal_wrapped(|ui| {
                    let field = ui.add(egui::TextEdit::singleline(&mut edit.draft).id(Id::unique(("profile-name", id))).char_limit(PROFILE_NAME_MAX).desired_width(200.0));
                    if field.changed() { edit.error = None; }
                    save = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    save |= Button::new("Save").kind(Kind::Primary).size(style::Size::Small).show(ui, &ink).clicked();
                    cancel = Button::new("Cancel").kind(Kind::Plain).size(style::Size::Small).show(ui, &ink).clicked();
                });
                if let Some(error) = &edit.error {
                    ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger));
                }
                if cancel { st.renaming = None; }
                else if save {
                    st.rename(lib);
                    // Keep typing after a refused name.
                    if st.renaming.is_some() { ui.memory_mut(|memory| memory.request_focus(Id::unique(("profile-name", id)))); }
                }
            }

            if let Some(deleting) = st.deleting.as_ref().filter(|deleting| deleting.profile == *id) {
                let (text, error, primary) = match &deleting.stage {
                    Stage::Failed(error) => (format!("Could not delete “{}”. {error}", deleting.name), true, "Retry"),
                    _ => (format!("Delete “{}”? This deletes its favorites, history, cookies, site data and captures on this device. This can't be undone.", deleting.name), false, "Delete Profile"),
                };
                let busy = matches!(deleting.stage, Stage::Pending);
                let (confirm, cancel) = decision(ui, &ink, &text, error, primary, busy, &mut st.reveal);
                if cancel { st.deleting = None; }
                else if confirm {
                    st.confirm_delete(actions);
                    ui.ctx().request_repaint();
                }
            }
            super::site_page::rows(&mut st.sites, sites.profiles, lib, sites.routing, id, ui, &ink);
        });
        separator(ui, pal);
        if rename {
            st.renaming = Some(Rename { profile: id.clone(), draft: name.clone(), error: None });
            ui.memory_mut(|memory| memory.request_focus(Id::unique(("profile-name", id))));
        }
        if delete { st.request_delete(lib, id); }
    }

    super::site_page::others(&mut st.sites, sites.profiles, lib, ui, &ink);
    ui.add_space(4.0);
    let mut create = None;
    ui.horizontal_wrapped(|ui| {
        let field = ui.add(egui::TextEdit::singleline(&mut st.new_name).id(Id::unique("new-profile-name")).hint_text("New profile name").char_limit(PROFILE_NAME_MAX).desired_width(200.0));
        if field.changed() { st.create_error = None; }
        let ready = !st.new_name.trim().is_empty();
        if ready && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) { create = Some(None); }
        if Button::new("Create Profile").enabled(ready).size(style::Size::Small).show(ui, &ink).clicked() { create = Some(None); }
        if Button::new("Create and Use for This Space").enabled(ready).size(style::Size::Small).show(ui, &ink).clicked() { create = Some(Some(space)); }
    });
    if let Some(error) = &st.create_error {
        ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    if let Some(use_for) = create {
        st.create(lib, use_for);
        if st.create_error.is_some() { ui.memory_mut(|memory| memory.request_focus(Id::unique("new-profile-name"))); }
    }
    if let Some((text, error)) = &st.notice {
        let response = ui.label(style::text(text, 12.0, style::weight::REGULAR).color(if *error { ink.danger } else { ink.secondary }));
        if st.reveal && st.moving.is_none() && st.deleting.is_none() && !ui.is_sizing_pass() {
            response.scroll_to_me(Some(egui::Align::Center));
            st.reveal = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::*;
    use crate::app::profile_switch::{commit_space_profile, delete_profile_and_data, new_space_profile};
    use crossbow_core::link_routing::Routing;
    use crossbow_core::site_profiles::SiteProfiles;
    use crossbow_core::store::Store;
    use egui::{Event, Key, Modifiers};
    use std::path::PathBuf;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

    /// Settings → Spaces with a disposable library and synthetic Chromium
    /// folders under TMPDIR. Queued actions run through the same functions the
    /// app uses, minus the live views a test has none of.
    struct Harness {
        dir: PathBuf,
        lib: Library,
        space: String,
        personal: String,
        state: ProfilesState,
        new_space: Option<String>,
        actions: Vec<Action>,
        sites: SiteProfiles,
        routing: Routing,
        prefill: Option<(String, String)>,
        /// Escape the page left alone, which closes Settings.
        dismissed: bool,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            let dir = std::env::temp_dir().join(format!("crossbow-profiles-ui-{}", crossbow_core::new_id()));
            std::fs::create_dir(&dir).unwrap();
            let mut lib = Library::open(Store::open(&dir.join("fixture.sqlite")).unwrap()).unwrap();
            lib.ensure_defaults().unwrap();
            let space = lib.spaces_ordered()[0].0.clone();
            let personal = lib.spaces[&space].profile_id.clone();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self {
                dir, lib, space, personal, state: ProfilesState::default(), new_space: None, actions: Vec::new(),
                sites: SiteProfiles::default(), routing: Routing::default(), prefill: None, dismissed: false, ctx, size, frame: 0,
            }
        }

        fn reopen(&self) -> Library {
            Library::open(Store::open(&self.dir.join("fixture.sqlite")).unwrap()).unwrap()
        }

        /// Swap in a read-only connection so real library writes fail.
        fn read_only(&mut self) {
            let path = self.dir.join("fixture.sqlite").to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
            self.lib = Library::open(Store::open(std::path::Path::new(&format!("file:{path}?mode=ro"))).unwrap()).unwrap();
        }

        fn site_data(&self, profile: &str) -> PathBuf {
            let hex: String = profile.bytes().map(|byte| format!("{byte:02x}")).collect();
            let folder = self.dir.join("Chromium").join(format!("Profile-{hex}"));
            std::fs::create_dir_all(folder.join("Local Storage")).unwrap();
            std::fs::write(folder.join("Cookies"), profile).unwrap();
            folder
        }

        /// One capture saved for `profile` in a Captures file beside the library.
        fn capture(&self, profile: &str) -> crossbow_core::captures::Capture {
            let captures = crossbow_core::captures::Captures::open(&self.dir).unwrap();
            let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x03\0\0\0\x02";
            let new = crossbow_core::captures::NewCapture { profile_id: profile, url: "", title: "", created_ms: 1_793_286_245_000, offset_secs: 0, width: 3, height: 2, png };
            captures.save(&new).unwrap()
        }

        /// `profile`'s captures as listed, and the files in its folder.
        fn captures(&self, profile: &str) -> (usize, usize) {
            let captures = crossbow_core::captures::Captures::open(&self.dir).unwrap();
            let files = captures.folder(profile).and_then(|folder| std::fs::read_dir(folder).ok()).map_or(0, Iterator::count);
            (captures.list(profile).unwrap().len(), files)
        }

        fn frame(&mut self, events: Vec<Event>) -> PreferenceFrame {
            let shown = settings_test_frame(&self.ctx, Page::Spaces, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                // As handle_shortcuts does with SettingsState::owns_keyboard.
                if !self.state.owns_keyboard() && !egui::Popup::is_any_open(ui.ctx()) {
                    self.dismissed |= crate::app::take_shortcut(ui.ctx(), Modifiers::NONE, Key::Escape);
                }
                let sites = Sites { profiles: &mut self.sites, routing: &self.routing, prefill: &mut self.prefill };
                show(&mut self.state, &mut self.lib, &self.space, &mut self.new_space, sites, &mut self.actions, ui, pal);
            });
            for action in std::mem::take(&mut self.actions) {
                match action {
                    Action::MoveSpaceToProfile { space, profile } => {
                        let result = commit_space_profile(&mut self.lib, &[], &space, &profile).map(drop).map_err(|error| error.to_string());
                        self.state.moved(&space, &profile, result);
                    }
                    Action::DeleteProfile(profile) => {
                        let result = delete_profile_and_data(&mut self.lib, &self.dir, &profile);
                        self.state.deleted(&profile, result);
                    }
                    _ => panic!("Settings → Spaces queued an unrelated action"),
                }
            }
            shown
        }

        /// Settle animations and check every visible text fits the sheet.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..8 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            // The page clip is the one most text shares; text fields clip and
            // scroll their own text on purpose.
            let page = shown.text.iter().map(|(_, _, clip)| *clip)
                .max_by_key(|clip| shown.text.iter().filter(|(_, _, other)| other == clip).count());
            for (text, bounds, clip) in &shown.text {
                if Some(*clip) == page && bounds.intersects(*clip) {
                    assert!(bounds.min.x >= clip.min.x - 0.5 && bounds.max.x <= clip.max.x + 0.5, "{text:?} overflows the sheet at {:?}: {bounds:?} in {clip:?}", self.size);
                }
            }
            shown
        }

        /// Scroll until `pick` finds a fully visible text; returns its center.
        fn reveal(&mut self, what: &str, pick: impl Fn(&PreferenceFrame) -> Option<(Rect, Rect)>) -> egui::Pos2 {
            let sheet = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0);
            let wheel = sheet.right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..24 {
                let shown = self.settle();
                let delta = match pick(&shown) {
                    Some((bounds, clip)) if clip.expand(0.5).contains_rect(bounds) => return bounds.center(),
                    Some((bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 12 { 100.0 } else { -100.0 },
                };
                self.frame(vec![
                    Event::PointerMoved(wheel),
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE },
                ]);
            }
            let shown: Vec<String> = self.settle().text.into_iter().map(|(text, ..)| text).collect();
            panic!("{what} never became visible at {:?}: {shown:?}", self.size);
        }

        fn visible(&mut self, label: &str) -> egui::Pos2 {
            self.reveal(label, |shown| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)))
        }

        fn shows(&mut self, label: &str) -> bool {
            self.settle().text.iter().any(|(text, ..)| text == label)
        }

        /// A popup menu item: the newest occurrence of its label.
        fn menu_item(&mut self, label: &str) -> egui::Pos2 {
            self.reveal(label, |shown| shown.text.iter().rfind(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)))
        }

        /// A button in the profile row named `name`.
        fn row_button(&mut self, name: &str, label: &str) -> egui::Pos2 {
            self.reveal(&format!("{label} for {name}"), |shown| {
                let rows: Vec<Rect> = shown.text.iter().filter(|(text, ..)| text == name).map(|(_, bounds, _)| *bounds).collect();
                shown.text.iter().find(|(text, bounds, _)| text == label && rows.iter().any(|row| (row.center().y - bounds.center().y).abs() < 8.0))
                    .map(|(_, bounds, clip)| (*bounds, *clip))
            })
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click(&mut self, label: &str) {
            let pos = self.visible(label);
            self.click_at(pos);
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
            self.settle();
        }

        /// Replace the focused field's text, as a person selecting all and typing.
        fn type_text(&mut self, text: &str) {
            self.key(Key::A, Modifiers::COMMAND);
            self.frame(vec![Event::Text(text.into())]);
            self.settle();
        }

        /// Press Tab until the button labelled `label` has keyboard focus.
        fn tab_to(&mut self, label: &str) {
            for _ in 0..60 {
                self.key(Key::Tab, Modifiers::NONE);
                let shown = self.settle();
                let focused = self.ctx.memory(|memory| memory.focused()).and_then(|id| self.ctx.read_response(id));
                let target = shown.text.iter().find(|(text, ..)| text == label);
                if let (Some(focused), Some((_, bounds, clip))) = (focused, target) {
                    if focused.rect.contains(bounds.center()) {
                        assert!(clip.expand(0.5).contains_rect(*bounds), "focused {label} must be scrolled into view");
                        return;
                    }
                }
            }
            panic!("{label} is not reachable with Tab at {:?}", self.size);
        }

        /// Choose `profile` in this Space's Profile menu.
        fn choose_profile(&mut self, current: &str, profile: &str) {
            self.click(current);
            let item = self.menu_item(profile);
            self.click_at(item);
        }

        fn rows(&self) -> Vec<crossbow_core::store::EntityRow> {
            self.lib.store.all_entities().unwrap()
        }

        /// The first `label` under the profile row named `name`, above the next row.
        fn under(&mut self, name: &str, label: &str) -> egui::Pos2 {
            self.reveal(&format!("{label} under {name}"), |shown| {
                let renames: Vec<f32> = shown.text.iter().filter(|(text, ..)| text == "Rename").map(|(_, bounds, _)| bounds.center().y).collect();
                let row = shown.text.iter().map(|(text, bounds, _)| (text, bounds.center().y))
                    .find(|(text, y)| *text == name && renames.iter().any(|rename| (rename - y).abs() < 8.0))?.1;
                let next = renames.iter().copied().filter(|y| *y > row + 8.0).fold(f32::INFINITY, f32::min);
                shown.text.iter().filter(|(text, bounds, _)| text == label && bounds.center().y > row && bounds.center().y < next)
                    .min_by(|a, b| a.1.center().y.total_cmp(&b.1.center().y)).map(|(_, bounds, clip)| (*bounds, *clip))
            })
        }

        /// Open Add Site… under `name` and type `text` into its field.
        fn add_site(&mut self, name: &str, text: &str) {
            let add = self.under(name, "Add Site…");
            self.click_at(add);
            assert!(self.state.owns_keyboard(), "the form is open");
            self.type_text(text);
        }

        /// The site rules a reopened library reads back.
        fn saved(&self) -> Vec<(String, bool, String)> {
            SiteProfiles::load(&self.reopen().store).rules().iter().map(|rule| (rule.site.host.clone(), rule.site.subdomains, rule.profile.clone())).collect()
        }

        fn focused_field(&self) -> Option<Id> {
            self.ctx.memory(|memory| memory.focused())
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const MOVE_TO_WORK: &str = "Move “Personal” to “Work”? Its tabs reload in that profile, so sites use that profile's sign-ins.";

    #[test]
    fn space_profile_menu_asks_first_and_cancel_or_escape_changes_nothing() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.lib.create_profile("Work").unwrap();
            let rows = h.rows();

            // Escape closes the menu without choosing.
            h.click("Personal");
            assert!(egui::Popup::is_any_open(&h.ctx));
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!egui::Popup::is_any_open(&h.ctx));
            assert!(!h.shows(MOVE_TO_WORK));

            h.choose_profile("Personal", "Work");
            h.visible(MOVE_TO_WORK);
            h.visible("Confirm");
            assert_eq!(h.rows(), rows, "choosing only asks");
            h.click("Cancel");
            assert!(!h.shows(MOVE_TO_WORK));
            assert_eq!(h.rows(), rows);
            assert_eq!(h.reopen().spaces[&h.space].profile_id, h.personal);
        }
    }

    #[test]
    fn confirming_a_space_profile_move_by_pointer_or_keyboard_persists_after_reopening() {
        for size in SIZES {
            for keyboard in [false, true] {
                let mut h = Harness::new(size);
                let work = h.lib.create_profile("Work").unwrap();
                let pin = h.lib.add_pinned(&h.space, None, "https://fixture.example/pin", "Pin").unwrap();
                h.choose_profile("Personal", "Work");
                h.visible(MOVE_TO_WORK);
                if keyboard {
                    h.tab_to("Confirm");
                    h.key(Key::Enter, Modifiers::NONE);
                } else {
                    h.click("Confirm");
                }
                h.visible("“Personal” now uses “Work”. Its tabs reopen in that profile.");
                assert!(!h.shows(MOVE_TO_WORK));
                assert_eq!(h.lib.spaces[&h.space].profile_id, work);
                let reopened = h.reopen();
                assert_eq!(reopened.spaces[&h.space].profile_id, work);
                assert_eq!(reopened.items[&pin].profile_id, work, "pinned items move with their Space");
                h.visible("Used by Personal");
                h.visible("Not used by any Space");
            }
        }
    }

    #[test]
    fn failed_space_profile_move_offers_retry_and_changes_nothing() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            h.read_only();
            let rows = h.rows();
            h.choose_profile("Personal", "Work");
            h.click("Confirm");
            let failure = h.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with("Could not move “Personal” to “Work”.")).expect("failure is shown");
            assert!(failure.ends_with("Nothing was changed."), "{failure}");
            h.visible("Retry");
            assert_eq!(h.rows(), rows);
            assert_eq!(h.reopen().spaces[&h.space].profile_id, h.personal);

            h.lib = h.reopen();
            h.click("Retry");
            h.visible("“Personal” now uses “Work”. Its tabs reopen in that profile.");
            assert_eq!(h.reopen().spaces[&h.space].profile_id, work);
        }
    }

    #[test]
    fn profile_rename_is_validated_inline_and_persists() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            let rows = h.rows();

            let rename = h.row_button("Work", "Rename");
            h.click_at(rename);
            assert!(h.ctx.memory(|memory| memory.has_focus(Id::unique(("profile-name", &work)))), "the name field takes focus");
            h.type_text("Personal");
            h.key(Key::Enter, Modifiers::NONE);
            h.visible("Could not rename. Another profile already uses this name.");
            assert_eq!(h.rows(), rows);

            h.click("Cancel");
            assert!(!h.shows("Could not rename. Another profile already uses this name."));
            assert_eq!(h.lib.profiles[&work].name, "Work");

            let rename = h.row_button("Work", "Rename");
            h.click_at(rename);
            h.type_text("");
            h.key(Key::Backspace, Modifiers::NONE);
            h.click("Save");
            h.visible("Could not rename. Enter a profile name.");
            assert_eq!(h.rows(), rows);

            h.type_text(&"Abcdefghij".repeat(6));
            h.click("Save");
            let capped = h.lib.profiles[&work].name.clone();
            assert_eq!(capped.chars().count(), PROFILE_NAME_MAX, "the field stops at the length limit");
            h.visible(&format!("Renamed to “{capped}”."));

            let rename = h.row_button(&capped, "Rename");
            h.click_at(rename);
            h.type_text("  Office  ");
            h.key(Key::Enter, Modifiers::NONE);
            h.visible("Renamed to “Office”.");
            assert_eq!(h.reopen().profiles[&work].name, "Office");
        }
    }

    #[test]
    fn deleting_a_profile_in_use_names_its_spaces_and_the_last_profile_cannot_be_deleted() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let personal = h.site_data(&h.personal);
            let delete = h.row_button("Personal", "Delete…");
            h.click_at(delete);
            assert!(!h.shows("Delete Profile"), "the only profile has no delete action");
            assert!(personal.join("Cookies").exists());

            h.lib.create_profile("Work").unwrap();
            h.lib.create_space("Reading", "R", crossbow_core::model::Theme::preset(1), &h.personal.clone()).unwrap();
            let rows = h.rows();
            let delete = h.row_button("Personal", "Delete…");
            h.click_at(delete);
            h.visible("“Personal” is used by Personal, Reading. Move those Spaces to another profile first.");
            assert!(!h.shows("Delete Profile"));
            assert_eq!(h.rows(), rows);
            assert!(personal.join("Cookies").exists());
        }
    }

    #[test]
    fn deleting_an_unused_profile_confirms_removes_only_its_data_and_survives_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            h.lib.add_favorite(&work, "https://fixture.example/work", "Work favorite").unwrap();
            let kept = h.lib.add_favorite(&h.personal.clone(), "https://fixture.example/personal", "Personal favorite").unwrap();
            let (work_data, personal_data) = (h.site_data(&work), h.site_data(&h.personal));
            let confirm = "Delete “Work”? This deletes its favorites, history, cookies, site data and captures on this device. This can't be undone.";

            let delete = h.row_button("Work", "Delete…");
            h.click_at(delete);
            h.visible(confirm);
            h.click("Cancel");
            assert!(!h.shows(confirm));
            assert!(h.lib.profiles.contains_key(&work) && work_data.join("Cookies").exists());

            let delete = h.row_button("Work", "Delete…");
            h.click_at(delete);
            h.click("Delete Profile");
            h.visible("Deleted “Work” and its data on this device.");
            assert!(!work_data.exists(), "its own Chromium folder is gone");
            assert_eq!(std::fs::read_to_string(personal_data.join("Cookies")).unwrap(), h.personal, "other profiles keep their data");
            let leftovers: Vec<_> = std::fs::read_dir(h.dir.join("Chromium")).unwrap().flatten().map(|entry| entry.file_name()).collect();
            assert_eq!(leftovers.len(), 1, "nothing is left aside: {leftovers:?}");
            let reopened = h.reopen();
            assert!(!reopened.profiles.contains_key(&work));
            assert!(reopened.favorites(&work).is_empty());
            assert_eq!(reopened.favorites(&h.personal).into_iter().map(|(id, _)| id).collect::<Vec<_>>(), [kept]);
            assert!(!h.shows("Work"));
        }
    }

    /// AC4: its history and archive leave this Mac's backups too, and a
    /// backup that can't be cleaned is deleted and counted in the note.
    #[test]
    fn deleting_a_profile_cleans_this_macs_backups_and_counts_one_that_couldnt_be() {
        use crossbow_core::backup::{self, Reason};
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            h.lib.store.record_visit(&work, "https://visited.example/work", "Visited", 1).unwrap();
            let kept = backup::create(&h.lib.store, Reason::Daily).unwrap().unwrap();
            // A backup that can't be read can't be cleaned either.
            let damaged = kept.path.with_file_name("library-20261001T090000Z-daily.sqlite");
            std::fs::write(&damaged, vec![0x5Au8; 8192]).unwrap();
            let holds = |path: &std::path::Path| std::fs::read(path).unwrap().windows(15).any(|bytes| bytes == b"visited.example");
            assert!(holds(&kept.path));

            let delete = h.row_button("Work", "Delete…");
            h.click_at(delete);
            h.click("Delete Profile");
            h.visible("Deleted “Work” and its data on this device. 1 backup couldn't be cleaned, so it was deleted.");
            assert!(!damaged.exists() && !holds(&kept.path), "at {size:?}");
            assert_eq!(backup::list(&h.dir.join("fixture.sqlite")).len(), 1);
        }
    }

    #[test]
    fn failed_profile_deletion_puts_its_data_back_and_retry_deletes() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            let data = h.site_data(&work);
            h.read_only();
            let rows = h.rows();
            let delete = h.row_button("Work", "Delete…");
            h.click_at(delete);
            h.click("Delete Profile");
            let failure = h.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with("Could not delete “Work”.")).expect("failure is shown");
            assert!(failure.ends_with("Nothing was deleted."), "{failure}");
            h.visible("Retry");
            assert_eq!(std::fs::read_to_string(data.join("Cookies")).unwrap(), work, "site data is back in place");
            assert_eq!(h.rows(), rows);

            h.lib = h.reopen();
            h.click("Retry");
            h.visible("Deleted “Work” and its data on this device.");
            assert!(!data.exists());
            assert!(!h.reopen().profiles.contains_key(&work));
        }
    }

    #[test]
    fn deleting_a_profile_deletes_its_captures_and_a_failure_keeps_every_one() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            let personal = h.personal.clone();
            for profile in [&work, &work, &personal] { h.capture(profile); }
            let data = h.site_data(&work);
            let folders = |h: &Harness| -> Vec<String> {
                let mut names: Vec<_> = std::fs::read_dir(h.dir.join("Captures")).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
                names.sort();
                names
            };
            let both = folders(&h);

            // The library commit fails: every capture stays, nothing set aside.
            h.read_only();
            let delete = h.row_button("Work", "Delete…");
            h.click_at(delete);
            h.visible("Delete “Work”? This deletes its favorites, history, cookies, site data and captures on this device. This can't be undone.");
            h.click("Delete Profile");
            let failure = h.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with("Could not delete “Work”.")).expect("failure is shown");
            assert!(failure.ends_with("Nothing was deleted."), "{failure}");
            assert_eq!((h.captures(&work), folders(&h)), ((2, 2), both.clone()));
            h.lib = h.reopen();

            // A damaged Captures file refuses before anything moves.
            let file = h.dir.join("captures.sqlite");
            let intact = std::fs::read(&file).unwrap();
            for suffix in ["-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", file.display())); }
            std::fs::write(&file, b"damaged".repeat(600)).unwrap();
            h.click("Retry");
            h.visible(&format!("Could not delete “Work”. {} Nothing was deleted.", crossbow_core::captures::PROFILE_DAMAGED));
            assert!(h.reopen().profiles.contains_key(&work) && data.join("Cookies").exists() && folders(&h) == both);

            // Repaired, Retry deletes Work's captures with it and keeps Personal's.
            std::fs::write(&file, intact).unwrap();
            h.click("Retry");
            h.visible("Deleted “Work” and its data on this device.");
            assert_eq!((h.captures(&work), h.captures(&personal)), ((0, 0), (1, 1)));
            assert_eq!(folders(&h).len(), 1, "only Personal's folder, nothing left aside: {:?}", folders(&h));
            assert!(!data.exists() && !h.reopen().profiles.contains_key(&work));
        }
    }

    #[test]
    fn creating_a_profile_validates_its_name_and_can_move_this_space_to_it() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let field = h.visible("New profile name");
            h.click_at(field);
            h.type_text("Personal");
            h.click("Create Profile");
            h.visible("Could not create the profile. Another profile already uses this name.");
            assert_eq!(h.lib.profiles.len(), 1);

            // The refused name keeps focus, so typing continues; Enter creates.
            h.type_text("Travel");
            assert!(!h.shows("Could not create the profile. Another profile already uses this name."), "editing clears the error");
            h.key(Key::Enter, Modifiers::NONE);
            h.visible("Created “Travel”.");
            assert_eq!(h.lib.spaces[&h.space].profile_id, h.personal, "Create Profile alone moves nothing");
            assert!(h.lib.profiles.values().any(|profile| profile.name == "Travel"));

            let field = h.visible("New profile name");
            h.click_at(field);
            h.type_text("Studio");
            h.click("Create and Use for This Space");
            h.visible("Move “Personal” to “Studio”? Its tabs reload in that profile, so sites use that profile's sign-ins.");
            h.click("Confirm");
            h.visible("“Personal” now uses “Studio”. Its tabs reopen in that profile.");
            let reopened = h.reopen();
            assert_eq!(reopened.profiles[&reopened.spaces[&h.space].profile_id].name, "Studio");
            assert_eq!(reopened.profiles.len(), 3);
        }
    }

    #[test]
    fn failed_profile_creation_keeps_the_name_for_retry() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.read_only();
            let rows = h.rows();
            let field = h.visible("New profile name");
            h.click_at(field);
            h.type_text("Retry fixture");
            h.click("Create Profile");
            let failure = h.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with("Could not create the profile.")).expect("failure is shown");
            assert!(!failure.contains("Another profile"), "{failure}");
            assert_eq!(h.state.new_name, "Retry fixture");
            assert_eq!(h.rows(), rows);

            h.lib = h.reopen();
            h.click("Create Profile");
            h.visible("Created “Retry fixture”.");
            assert!(h.state.new_name.is_empty());
            assert!(h.reopen().profiles.values().any(|profile| profile.name == "Retry fixture"));
            assert_eq!(h.reopen().spaces[&h.space].profile_id, h.personal);
        }
    }

    #[test]
    fn new_spaces_use_the_profile_chosen_in_settings() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            h.click("Active Space's profile");
            let item = h.menu_item("Work");
            h.click_at(item);
            assert_eq!(h.new_space.as_deref(), Some(work.as_str()));
            assert_eq!(new_space_profile(&h.lib, h.new_space.as_deref(), &h.space), work);
            assert_eq!(h.lib.spaces[&h.space].profile_id, h.personal, "the current Space keeps its profile");

            h.click("Work");
            let item = h.menu_item("Active Space's profile");
            h.click_at(item);
            assert_eq!(h.new_space, None);
            assert_eq!(new_space_profile(&h.lib, h.new_space.as_deref(), &h.space), h.personal);
        }
    }

    #[test]
    fn long_profile_names_truncate_and_keep_their_buttons_in_view() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let long = "W".repeat(PROFILE_NAME_MAX);
            h.lib.create_named_profile(&long).unwrap();
            let rename = h.row_button(&long, "Rename");
            let delete = h.row_button(&long, "Delete…");
            assert!(rename.x < delete.x);
            let shown = h.settle();
            let name = shown.text.iter().find(|(text, ..)| *text == long).unwrap().1;
            let button = shown.text.iter().find(|(text, bounds, _)| text == "Rename" && (bounds.center().y - name.center().y).abs() < 8.0).unwrap().1;
            assert!(name.max.x < button.min.x, "the name truncates before its buttons at {size:?}: {name:?} {button:?}");
            h.visible("Create and Use for This Space");
        }
    }

    const NOT_A_SITE: &str = "Enter a site name, such as example.com, or paste a link to it.";

    #[test]
    fn sites_are_added_moved_and_removed_per_profile_and_survive_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.lib.create_profile("Work").unwrap();
            let personal = h.personal.clone();

            // Add Site… opens a focused field under its profile; text that is
            // not a site is refused and nothing is saved.
            h.add_site("Work", "not a site");
            assert_eq!(h.focused_field(), Some(Id::unique(("site-field", work.as_str()))));
            h.key(Key::Enter, Modifiers::NONE);
            assert!(h.shows(NOT_A_SITE) && h.saved().is_empty(), "at {size:?}");

            // A pasted link becomes its site.
            let field = h.visible("not a site");
            h.click_at(field);
            h.type_text("https://www.GitHub.com/rust-lang");
            assert!(!h.shows(NOT_A_SITE), "typing clears the error");
            h.key(Key::Enter, Modifiers::NONE);
            assert_eq!(h.saved(), [("github.com".to_string(), false, work.clone())]);
            assert!(!h.state.owns_keyboard(), "the form closes");
            // Remove lines up with the profile rows' Delete….
            h.row_button("github.com", "Remove");
            let shown = h.settle();
            let rights = |label: &str| shown.text.iter().filter(|(text, ..)| text == label).map(|(_, bounds, _)| bounds.right()).collect::<Vec<f32>>();
            let (removes, deletes) = (rights("Remove"), rights("Delete…"));
            assert!(!removes.is_empty() && !deletes.is_empty());
            for right in removes { assert!(deletes.iter().all(|delete| (delete - right).abs() < 0.5), "Remove {right} vs Delete… {deletes:?} at {size:?}"); }

            // Another profile asks before taking a site.
            h.add_site("Personal", "github.com");
            let add = h.under("Personal", "Add");
            h.click_at(add);
            assert!(h.shows("Already opens in Work. Move it here?"));
            assert_eq!(h.saved()[0].2, work, "nothing moves until asked");
            h.click("Move Here");
            assert_eq!(h.saved(), [("github.com".to_string(), false, personal.clone())]);

            // An IP address needs Add Anyway; subdomains are a choice.
            h.add_site("Work", "192.168.1.10");
            h.key(Key::Enter, Modifiers::NONE);
            assert!(h.shows("“192.168.1.10” is an IP address, not a site name. Add it anyway?") && h.saved().len() == 1);
            h.click("Add Anyway");
            h.add_site("Work", "example.com");
            h.click("and subdomains");
            let add = h.under("Work", "Add");
            h.click_at(add);
            h.visible("example.com and subdomains");
            let mut saved = h.saved();
            saved.sort();
            assert_eq!(saved, [
                ("192.168.1.10".to_string(), false, work.clone()),
                ("example.com".to_string(), true, work.clone()),
                ("github.com".to_string(), false, personal.clone()),
            ]);

            // Cancel and Escape add nothing; Escape leaves Settings open.
            h.add_site("Work", "cancelled.example");
            let cancel = h.under("Work", "Cancel");
            h.click_at(cancel);
            assert!(!h.state.owns_keyboard());
            h.add_site("Work", "escaped.example");
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.state.owns_keyboard() && !h.dismissed, "Escape cancels the form, not Settings");
            h.key(Key::Escape, Modifiers::NONE);
            assert!(h.dismissed, "then Escape closes Settings");
            assert_eq!(h.saved().len(), 3);

            // Remove, at once and for good.
            let remove = h.row_button("github.com", "Remove");
            h.click_at(remove);
            assert!(!h.shows("github.com") && !h.saved().iter().any(|(host, ..)| host == "github.com"));
        }
    }

    #[test]
    fn site_rows_say_when_a_rule_cannot_apply_and_a_failed_change_keeps_the_site() {
        use crossbow_core::link_routing::{Destination, Match, Rule};
        use crossbow_core::site_profiles::{Site, SiteRule};
        for size in SIZES {
            let mut h = Harness::new(size);
            h.lib.store.kv_set("site_profiles", "{").unwrap();
            h.sites = SiteProfiles::load(&h.lib.store);
            h.visible("Saved site profiles could not be read, so sites open where you are. Changing one here saves over them.");

            let work = h.lib.create_profile("Work").unwrap();
            let spare = h.lib.create_profile("Spare").unwrap();
            let office = h.lib.create_space("Office", "O", crossbow_core::model::Theme::preset(1), &work).unwrap();
            for (host, profile) in [("github.com", work.as_str()), ("example.net", spare.as_str()), ("example.org", "gone")] {
                let site = Site::parse(host, false, false).unwrap();
                h.sites.add(&h.lib.store, SiteRule { site, profile: profile.into() }, false).unwrap();
            }
            assert!(!h.shows("Saved site profiles could not be read, so sites open where you are. Changing one here saves over them."), "a change saves over it");

            // Air Traffic Control sends GitHub links from other apps elsewhere: said beside the site.
            let note = format!("Air Traffic Control sends this site's links from other apps to “{}”.", h.lib.spaces[&h.space].name);
            h.routing = Routing { rules: vec![Rule::new(crossbow_core::new_id(), Match::HostIs, "github.com", h.space.clone()).unwrap()], default: Destination::MostRecent };
            let shown = h.visible(&note);
            let site = h.visible("github.com");
            assert!(shown.y > site.y, "under its site");
            h.routing.rules[0].space = office;
            assert!(!h.shows(&note), "nothing to say when the rule agrees");
            h.visible("No Space uses this profile, so its sites open where you are.");

            // A profile not on this device: listed apart, and removable.
            let remove = h.row_button("example.org · Profile not on this device", "Remove");
            h.click_at(remove);
            assert!(!h.saved().iter().any(|(host, ..)| host == "example.org"));

            // A tab's Always Open… fills the form in, focused.
            h.prefill = Some((work.clone(), "news.example".into()));
            h.visible("news.example");
            assert_eq!(h.focused_field(), Some(Id::unique(("site-field", work.as_str()))));
            h.key(Key::Escape, Modifiers::NONE);

            // A failed save keeps the site and says why.
            h.read_only();
            let remove = h.row_button("github.com", "Remove");
            h.click_at(remove);
            let shown = h.settle();
            assert!(shown.text.iter().any(|(text, ..)| text.starts_with("Could not remove github.com (") && text.ends_with("), so it still opens in Work.")), "at {size:?}");
            assert!(h.sites.owner("github.com").is_some() && h.saved().iter().any(|(host, ..)| host == "github.com"));
        }
    }
}
