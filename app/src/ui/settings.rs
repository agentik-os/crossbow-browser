//! The Settings window: general preferences, spaces and themes, profiles,
//! Boosts, account sync, shortcuts and advanced options.

use crate::app::{Action, CrossbowApp};
use crate::sync_glue::{AuthStep, BrowserOffer, BrowserStep, DeleteStep, SyncGlue, valid_code, valid_email};
use crate::ui::style::{self, Button, Ink, Kind};
use crate::ui::icons::{self, Icon};
use crate::ui::theme::Palette;
use crate::ui::{anim, space_icon, theme_editor};
use crossbow_core::library::Library;
use crossbow_core::model::{ARCHIVE_CHOICES, Boost, ProfilePreferences, SearchEngine, Settings, archive_label};
use crossbow_core::sync::SyncStatus;
use crossbow_core::sync::e2ee::{E2eeStatus, OTHER_ACCOUNT, RecoveryCode, Zeroize, Zeroizing};
use egui::{Align2, Color32, FontId, Id, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

#[path = "profile_settings.rs"]
mod profiles;
#[path = "site_settings.rs"]
mod site_page;
#[path = "shortcut_settings.rs"]
mod shortcut_page;
#[path = "link_settings.rs"]
mod link_page;
#[path = "quit_settings.rs"]
mod quit_page;
#[path = "update_settings.rs"]
mod update_page;
#[path = "feel_settings.rs"]
mod feel_page;
#[path = "search_settings.rs"]
mod search_page;
#[path = "document_app_settings.rs"]
mod document_page;
#[path = "ai_feature_settings.rs"]
mod ai_feature_page;
#[path = "sidebar_settings.rs"]
mod sidebar_page;
#[path = "pip_settings.rs"]
mod pip_page;
#[path = "capture_settings.rs"]
mod capture_page;
#[path = "quote_settings.rs"]
mod quote_page;
#[path = "website_appearance_settings.rs"]
mod website_appearance_page;
#[path = "backup_settings.rs"]
mod backup_page;
#[path = "content_blocking_settings.rs"]
mod content_blocking_page;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    General,
    Links,
    Downloads,
    AiProviders,
    AiChat,
    Appearance,
    Spaces,
    Boosts,
    Passwords,
    Privacy,
    Account,
    Shortcuts,
    Advanced,
    About,
}

impl Page {
    const ALL: [Page; 14] = [Page::General, Page::Links, Page::Downloads, Page::AiProviders, Page::AiChat, Page::Appearance, Page::Spaces, Page::Boosts, Page::Passwords, Page::Privacy, Page::Account, Page::Shortcuts, Page::Advanced, Page::About];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Links => "Links",
            Page::Downloads => "Downloads",
            Page::AiProviders => "AI Providers",
            Page::AiChat => "Ask AI",
            Page::Appearance => "Appearance",
            Page::Spaces => "Spaces & Themes",
            Page::Boosts => "Boosts",
            Page::Passwords => "Passwords",
            Page::Privacy => "Privacy",
            Page::Account => "Account & Sync",
            Page::Shortcuts => "Shortcuts",
            Page::Advanced => "Advanced",
            Page::About => "About",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::General => Icon::Gear,
            Page::Links => Icon::Forward,
            Page::Downloads => Icon::Download,
            Page::AiProviders => Icon::Sparkle,
            Page::AiChat => Icon::Sparkle,
            Page::Appearance => Icon::Copy,
            Page::Spaces => Icon::Star,
            Page::Boosts => Icon::Sparkle,
            Page::Passwords => Icon::Lock,
            Page::Privacy => Icon::Shield,
            Page::Account => Icon::Sync,
            Page::Shortcuts => Icon::Search,
            Page::Advanced => Icon::More,
            Page::About => Icon::Globe,
        }
    }
}

struct PendingPreferences {
    saved: Settings,
    draft: Settings,
    error: String,
    reveal_error: bool,
}

struct PendingProfilePreferences {
    profile: String,
    saved: ProfilePreferences,
    draft: ProfilePreferences,
    error: String,
    reveal_error: bool,
}

struct PendingBoostToggle {
    id: String,
    enabled: bool,
    error: String,
}

struct ClearDataOperation {
    task: crossbow_engine::ClearDataTask,
    started_at: std::time::Instant,
}

#[derive(Default)]
pub struct BrowsingDataState {
    operations: std::collections::HashMap<String, ClearDataOperation>,
}

impl BrowsingDataState {
    fn status(&self, profile: &str) -> Option<(crossbow_engine::ClearDataStatus, std::time::Duration)> {
        self.operations.get(profile).map(|operation| (operation.task.status(), operation.started_at.elapsed()))
    }

    fn start(&mut self, data_dir: &std::path::Path, profile: &str) {
        if self.operations.get(profile).is_some_and(|operation| operation.task.status().pending()) { return; }
        let task = crossbow_engine::clear_browsing_data(data_dir, profile);
        self.operations.insert(profile.into(), ClearDataOperation { task, started_at: std::time::Instant::now() });
    }
}

pub struct SettingsState {
    pub page: Page,
    pub opened_at: f64,
    closing_at: Option<f64>,
    email: String,
    code: String,
    resend_at: f64,
    server_url: String,
    selected_space: Option<String>,
    space_name: Option<(String, String)>,
    profiles: profiles::ProfilesState,
    boost: Option<(Option<String>, Boost)>,
    boost_error: Option<String>,
    boost_delete_failed: bool,
    boost_error_reveal: bool,
    pending_boost_toggle: Option<PendingBoostToggle>,
    import_status: Option<String>,
    pending_preferences: Option<PendingPreferences>,
    pending_profile_preferences: Option<PendingProfilePreferences>,
    downloads: Option<crate::downloads::SettingsDraft>,
    ai: Option<crate::ai::SettingsDraft>,
    browser_features: ai_feature_page::Draft,
    chat: Option<crate::chat::Session>,
    clear_confirmation: Option<String>,
    clear_reveal: bool,
    /// Recovery code typed to unlock. Wiped after every attempt, on sign-out
    /// and when leaving the page; its capacity is reserved so typing never
    /// leaves a reallocated copy behind.
    e2ee_code: Zeroizing<String>,
    e2ee_saved: bool,
    e2ee_show_code: bool,
    /// Scroll the Checking line into view: a code was just entered.
    e2ee_reveal_checking: bool,
    e2ee_error: Option<String>,
    /// Sign Out was pressed on an encrypted account and waits for confirmation.
    sign_out_confirm: bool,
    /// Delete Sync Data… is open, with the email typed so far.
    delete_confirm: bool,
    delete_email: String,
    /// Delete Account…'s typed email and code.
    delete_account_email: String,
    delete_account_code: String,
    /// Turn Sync On… is open, and why the last try failed.
    sync_on_confirm: bool,
    sync_on_error: Option<String>,
    shortcuts: shortcut_page::Editor,
    links: link_page::Editor,
    search: search_page::Editor,
    /// Settings → General → New Document opens in.
    document: document_page::Editor,
    pub site_settings: crate::ui::site_permission_settings::Editor,
    /// Settings → Advanced → Restore Data.
    restore: backup_page::Restore,
}

impl SettingsState {
    pub fn new(page: Page, now: f64) -> SettingsState {
        SettingsState {
            page,
            opened_at: now,
            closing_at: None,
            email: String::new(),
            code: String::new(),
            resend_at: 0.0,
            server_url: String::new(),
            selected_space: None,
            space_name: None,
            profiles: Default::default(),
            boost: None,
            boost_error: None,
            boost_delete_failed: false,
            boost_error_reveal: false,
            pending_boost_toggle: None,
            import_status: None,
            pending_preferences: None,
            pending_profile_preferences: None,
            downloads: None,
            ai: None,
            browser_features: Default::default(),
            chat: None,
            clear_confirmation: None,
            clear_reveal: false,
            e2ee_code: Zeroizing::new(String::with_capacity(RECOVERY_FIELD_BYTES)),
            e2ee_saved: false,
            e2ee_show_code: false,
            e2ee_reveal_checking: false,
            e2ee_error: None,
            sign_out_confirm: false,
            delete_confirm: false,
            delete_email: String::new(),
            delete_account_email: String::new(),
            delete_account_code: String::new(),
            sync_on_confirm: false,
            sync_on_error: None,
            shortcuts: Default::default(),
            links: Default::default(),
            search: Default::default(),
            document: Default::default(),
            site_settings: Default::default(),
            restore: Default::default(),
        }
    }

    /// Settings → Shortcuts is recording a chord or asking to confirm one,
    /// Settings → Links is editing a rule or asking to confirm a delete,
    /// Settings → General is asking about suggestions, editing an engine or
    /// typing a document app address,
    /// Settings → Spaces is adding a site, or Settings → Advanced is asking
    /// to confirm a restore. Settings → AI Providers is asking to confirm
    /// Reset Translation Choices.
    pub(crate) fn owns_keyboard(&self) -> bool {
        self.page == Page::Shortcuts && self.shortcuts.owns_keyboard()
            || (self.page == Page::Links && self.links.owns_keyboard())
            || (self.page == Page::General && (self.search.owns_keyboard() || self.document.owns_keyboard()))
            || (self.page == Page::Spaces && self.profiles.owns_keyboard())
            || (self.page == Page::Advanced && self.restore.owns_keyboard())
            || (self.page == Page::AiProviders && self.browser_features.owns_keyboard())
    }

    /// Open at New Document opens in, scrolled into view with its menu focused.
    pub(crate) fn reveal_document_app(&mut self) {
        self.document.reveal();
    }

    /// Hide the recovery code, wipe a typed one and drop a pending
    /// confirmation: on leaving the Account page and on sign-out.
    fn leave_account(&mut self) {
        if !self.e2ee_code.is_empty() { self.e2ee_code.zeroize(); }
        self.e2ee_show_code = false;
        self.e2ee_saved = false;
        self.e2ee_reveal_checking = false;
        self.sign_out_confirm = false;
        self.delete_confirm = false;
        self.delete_email.clear();
        self.delete_account_email.clear();
        self.delete_account_code.clear();
        self.sync_on_confirm = false;
        self.sync_on_error = None;
    }

    pub(crate) fn from_chat_entry(entry: &crate::chat::Entry, lib: &mut Library, space: &str, account: &crate::chat::AccountScope, now: f64) -> Result<Self, &'static str> {
        let chat = entry.open(lib, space, account)?;
        let mut state = Self::new(Page::AiChat, now);
        state.chat = Some(chat);
        Ok(state)
    }

    /// Ask AI with `question` as the first message. It is sent once if Ask AI
    /// in the Command Bar still allows it; otherwise it waits in the composer
    /// with the reason.
    pub(crate) fn from_chat_question(entry: &crate::chat::Entry, question: String, lib: &mut Library, space: &str, account: &crate::chat::AccountScope, ctx: &egui::Context, now: f64) -> Result<Self, &'static str> {
        let mut state = Self::from_chat_entry(entry, lib, space, account, now)?;
        let chat = state.chat.as_mut().expect("from_chat_entry opens a chat");
        chat.draft = question;
        match crate::ask_bar::check(lib, &chat.profile, &chat.draft) {
            Ok(_) => chat.send(lib, space, account, ctx),
            Err(reason) => {
                chat.error = Some(reason);
                chat.reveal_status = true;
            }
        }
        Ok(state)
    }

    fn preferences(&self, lib: &Library) -> Settings {
        let mut settings = lib.settings.clone();
        if let Some(pending) = &self.pending_preferences {
            // Retry only the user's edits, not stale values in other fields
            // that may have changed through sync while the save was blocked.
            macro_rules! retain_edits {
                ($($field:ident),+ $(,)?) => { $(
                    if pending.draft.$field != pending.saved.$field {
                        settings.$field = pending.draft.$field;
                    }
                )+ };
            }
            retain_edits!(auto_archive_hours, search_engine, sidebar_on_right, show_url_in_sidebar, smooth_animations, peek_external_links, ai_features);
        }
        settings
    }

    fn save_preferences(&mut self, lib: &mut Library, settings: Settings) {
        if settings == lib.settings {
            self.cancel_preferences();
            return;
        }
        match lib.set_settings(settings.clone()) {
            Ok(()) => self.cancel_preferences(),
            Err(error) => {
                self.pending_preferences = Some(PendingPreferences {
                    saved: lib.settings.clone(), draft: settings,
                    error: format!("Could not save preferences: {error}. Changes are not applied. Check disk space and data-folder permissions, then Retry Save or Cancel. Closing Settings also discards unsaved changes."),
                    reveal_error: true,
                });
            }
        }
    }

    fn retry_preferences(&mut self, lib: &mut Library) {
        let settings = self.preferences(lib);
        self.save_preferences(lib, settings);
    }

    fn cancel_preferences(&mut self) {
        self.pending_preferences = None;
    }

    fn profile_preferences(&self, lib: &Library, profile: &str) -> Option<ProfilePreferences> {
        lib.effective_profile_preferences(profile)?;
        let mut preferences = lib.profiles.get(profile)?.preferences.unwrap_or_default();
        if let Some(pending) = self.pending_profile_preferences.as_ref().filter(|pending| pending.profile == profile) {
            if pending.draft.search_engine != pending.saved.search_engine {
                preferences.search_engine = pending.draft.search_engine;
            }
            if pending.draft.auto_archive_hours != pending.saved.auto_archive_hours {
                preferences.auto_archive_hours = pending.draft.auto_archive_hours;
            }
        }
        Some(preferences)
    }

    fn save_profile_preferences(&mut self, lib: &mut Library, profile: &str, draft: ProfilePreferences) {
        // A failed draft belongs to its original profile, even after Space changes.
        if self.pending_profile_preferences.as_ref().is_some_and(|pending| pending.profile != profile) { return; }
        let saved = lib.profiles.get(profile).map(|profile| profile.preferences.unwrap_or_default());
        if saved == Some(draft) {
            self.pending_profile_preferences = None;
            return;
        }
        let value = (draft != ProfilePreferences::default()).then_some(draft);
        match lib.set_profile_preferences(profile, value) {
            Ok(()) => self.pending_profile_preferences = None,
            Err(error) => {
                let name = lib.profiles.get(profile).map(|profile| profile.name.as_str()).unwrap_or("removed profile");
                self.pending_profile_preferences = Some(PendingProfilePreferences {
                    profile: profile.into(), saved: saved.unwrap_or_default(), draft,
                    error: format!("Could not save browsing preferences for “{name}”: {error}. Changes are not applied. Check storage, then Retry Save or Cancel. Closing Settings discards unsaved changes."),
                    reveal_error: true,
                });
            }
        }
    }

    fn retry_profile_preferences(&mut self, lib: &mut Library) {
        let Some(pending) = self.pending_profile_preferences.as_mut() else { return };
        let profile = pending.profile.clone();
        if let Err(error) = lib.refresh_profile(&profile) {
            pending.error = match error {
                crossbow_core::library::LibraryError::NotFound(_) => "Could not save browsing preferences for the removed profile. Changes are not applied. Retry after the original profile is restored, or Cancel to discard this draft; another profile cannot receive it.".into(),
                error => format!("Could not reload browsing preferences for the original profile: {error}. Changes are not applied. Check storage, then Retry Save or Cancel. Closing Settings discards unsaved changes."),
            };
            pending.reveal_error = true;
            return;
        }
        let draft = self.profile_preferences(lib, &profile).unwrap();
        self.save_profile_preferences(lib, &profile, draft);
    }

    /// The app's answer to a confirmed Space move from the Spaces page.
    pub(crate) fn space_profile_moved(&mut self, space: &str, profile: &str, result: Result<(), String>) {
        self.profiles.moved(space, profile, result);
    }

    /// The app's answer to a confirmed profile deletion from the Spaces page.
    pub(crate) fn profile_deleted(&mut self, profile: &str, result: Result<crate::app::ProfileDeletion, String>) {
        self.profiles.deleted(profile, result);
    }

    fn set_boost_enabled(&mut self, lib: &mut Library, id: &str, enabled: bool) {
        let result = match lib.boosts.get(id).cloned() {
            None => Err("This Boost no longer exists. Cancel to return to the list.".into()),
            Some(mut boost) => {
                if boost.enabled == enabled {
                    self.pending_boost_toggle = None;
                    return;
                }
                boost.enabled = enabled;
                lib.put_boost(Some(id), &boost).map(|_| ()).map_err(|error| {
                    format!("Could not {} Boost: {error}. The saved value is unchanged. Check disk space and data-folder permissions, then Retry or Cancel.", if enabled { "enable" } else { "disable" })
                })
            }
        };
        self.boost_error_reveal = result.is_err();
        self.pending_boost_toggle = result.err().map(|error| PendingBoostToggle { id: id.into(), enabled, error });
    }

    fn retry_boost_toggle(&mut self, lib: &mut Library) {
        if let Some(pending) = &self.pending_boost_toggle {
            let (id, enabled) = (pending.id.clone(), pending.enabled);
            self.set_boost_enabled(lib, &id, enabled);
        }
    }

    fn delete_boost(&mut self, lib: &mut Library) {
        let Some((Some(id), _)) = &self.boost else { return };
        if !lib.boosts.contains_key(id) {
            self.cancel_boost();
            return;
        }
        match lib.delete_boost(id) {
            Ok(()) => self.cancel_boost(),
            Err(error) => {
                self.boost_delete_failed = true;
                self.boost_error_reveal = true;
                self.boost_error = Some(format!("Could not delete Boost: {error}. Your draft and saved Boost are unchanged. Check disk space and data-folder permissions, then Retry Delete, or Cancel to discard the draft."));
            }
        }
    }

    fn save_boost(&mut self, lib: &mut Library) {
        let Some((id, draft)) = &self.boost else { return };
        self.boost_delete_failed = false;
        // Normalize only the candidate to persist, never the editor draft:
        // failure must preserve even whitespace and the original host input.
        let mut candidate = draft.clone();
        candidate.host = candidate.host.trim().trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.").trim_end_matches('/').to_string();
        match lib.put_boost(id.as_deref(), &candidate) {
            Ok(_) => self.cancel_boost(),
            Err(error) => {
                self.boost_error_reveal = true;
                self.boost_error = Some(format!("Could not save Boost: {error}. Your draft is unchanged. Check the data folder's permissions and free disk space, then Retry Save, or Cancel to discard the draft."));
            }
        }
    }

    fn cancel_boost(&mut self) {
        self.boost = None;
        self.boost_error = None;
        self.boost_delete_failed = false;
        self.boost_error_reveal = false;
    }
}

pub(super) fn sheet_rect(screen: Rect, visibility: f32) -> Rect {
    let size = vec2((screen.width() - 48.0).min(820.0), (screen.height() - 48.0).min(580.0));
    // Translate and fade the sheet without changing layout width each frame:
    // scaling the rectangle rewraps text and invalidates the galley cache.
    Rect::from_center_size(screen.center() + vec2(0.0, (1.0 - visibility) * 14.0), size)
}

pub(super) fn show_backdrop(ctx: &egui::Context, screen: Rect, visibility: f32) -> egui::Response {
    let area = egui::Area::new(Id::unique("settings-backdrop")).order(egui::Order::Foreground).fixed_pos(screen.min).sense(Sense::hover());
    ctx.move_to_top(area.layer());
    ctx.memory_mut(|mem| mem.set_modal_layer(area.layer()));
    area.show(ctx, |ui| {
        let response = ui.allocate_rect(screen, Sense::CLICK);
        ui.painter().rect_filled(screen, 0, Color32::from_black_alpha((80.0 * visibility) as u8));
        response
    }).inner
}

fn backdrop_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, Id::unique("settings-backdrop"))
}

pub(super) fn show_sheet(ctx: &egui::Context, rect: Rect, content: impl FnOnce(&mut Ui)) -> egui::InnerResponse<()> {
    let area = egui::Area::new(Id::unique("settings")).order(egui::Order::Foreground).fixed_pos(rect.min).sense(Sense::hover());
    ctx.set_sublayer(backdrop_layer(), area.layer());
    area.show(ctx, |ui| {
        // Manually positioned child UIs don't reserve their parent's area.
        // Keep the painted sheet in hit testing so its scroll areas get input.
        ui.set_min_size(rect.size());
        content(ui);
    })
}

pub(super) fn keep_popup_above_sheet(ui: &Ui) {
    // egui supports one sublayer depth: sheet-owned menus are siblings of the
    // sheet, not nested children, and stay above it as the modal is raised.
    ui.ctx().set_sublayer(backdrop_layer(), ui.layer_id());
    ui.ctx().move_to_top(ui.layer_id());
}

pub fn show(app: &mut CrossbowApp, ui: &mut Ui) {
    let Some(st) = &app.ui.settings else { return };
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let open_t = anim::ease_out_cubic(anim::progress(&ctx, st.opened_at, 0.22));
    let close_t = st.closing_at.map(|c| anim::progress(&ctx, c, 0.14)).unwrap_or(0.0);
    if close_t >= 1.0 {
        app.dismiss_settings(&ctx);
        return;
    }
    let vis = open_t * (1.0 - close_t);
    let pal = app.palette();
    let screen = ctx.content_rect();
    let rect = sheet_rect(screen, vis);
    let account_scope = app.chat_account_scope();

    let mut close = show_backdrop(&ctx, screen, vis).clicked();

    show_sheet(&ctx, rect, |ui| {
        ui.multiply_opacity(vis);
        set_visuals(ui, &pal);
        let p = ui.painter().clone();
        p.add(egui::epaint::Shadow { offset: [0, 20], blur: 56, spread: 0, color: Color32::from_black_alpha(90) }.as_shape(rect, egui::CornerRadius::same(14)));
        p.rect_filled(rect, 14, pal.panel);
        p.rect_stroke(rect, 14, Stroke::new(1.0, pal.panel_border), egui::StrokeKind::Inside);

        let st = app.ui.settings.as_mut().unwrap();
        // Keep all pages reachable in narrow or short windows.
        let body = if compact_navigation_needed(rect.size()) {
            p.text(rect.left_top() + vec2(20.0, 30.0), Align2::LEFT_CENTER, "Settings", FontId::proportional(17.0), pal.panel_text);
            compact_navigation(ui, rect, &mut st.page);
            Rect::from_min_max(rect.left_top() + vec2(20.0, 60.0), rect.right_bottom() - vec2(20.0, 16.0))
        } else {
            let nav = Rect::from_min_size(rect.min, vec2(196.0, rect.height()));
            p.rect_filled(nav, egui::CornerRadius { nw: 14, sw: 14, ne: 0, se: 0 }, pal.panel_hover.gamma_multiply(0.8));
            p.text(nav.left_top() + vec2(20.0, 30.0), Align2::LEFT_CENTER, "Settings", FontId::proportional(17.0), pal.panel_text);
            for (i, page) in Page::ALL.iter().enumerate() {
                let r = Rect::from_min_size(pos2(nav.left() + 10.0, nav.top() + 56.0 + i as f32 * 32.0), vec2(nav.width() - 20.0, 28.0));
                let resp = ui.interact(r, Id::unique(("settings-nav", i)), Sense::click());
                let sel = st.page == *page;
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, ui.is_enabled(), sel, page.title()));
                if sel {
                    p.rect_filled(r, 7, pal.accent.gamma_multiply(0.9));
                } else if resp.hovered() {
                    p.rect_filled(r, 7, pal.panel_hover);
                }
                if resp.has_focus() {
                    p.rect_stroke(r.expand(2.0), 9, Stroke::new(2.0, pal.accent), egui::StrokeKind::Outside);
                }
                let col = if sel { Color32::WHITE } else { pal.panel_text };
                icons::draw(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(13.0, 13.0)), page.icon(), col);
                p.text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, page.title(), FontId::proportional(13.0), col);
                if resp.clicked() {
                    st.page = *page;
                }
            }
            Rect::from_min_max(pos2(nav.right() + 28.0, rect.top() + 18.0), pos2(rect.right() - 28.0, rect.bottom() - 16.0))
        };
        let cr = Rect::from_center_size(pos2(rect.right() - 22.0, rect.top() + 22.0), vec2(26.0, 26.0));
        if icons::button(ui, cr, Icon::Close, pal.panel_dim, pal.panel_hover, true).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }

        // Page content.
        let ai_visible = st.page == Page::AiProviders && st.closing_at.is_none() && !close;
        crate::ai::reconcile(&mut st.ai, &app.lib, &app.active_space, ai_visible);
        let chat_visible = st.page == Page::AiChat && st.closing_at.is_none() && !close;
        crate::chat::reconcile(&mut st.chat, &app.lib, &app.active_space, &account_scope, chat_visible);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
        let page = st.page;
        if page != Page::Account { st.leave_account(); }
        if page != Page::Shortcuts { st.shortcuts.leave(); }
        if page != Page::Links { st.links.leave(); }
        if page != Page::General { st.search.leave(); st.document.leave(); }
        if page != Page::Privacy { st.site_settings.leave(); }
        if page != Page::Spaces { st.profiles.leave(); }
        if page != Page::AiProviders { st.browser_features = Default::default(); }
        child.label(RichText::new(page.title()).size(20.0).strong().color(pal.panel_text));
        child.add_space(10.0);
        egui::ScrollArea::vertical().id_salt(("settings-scroll", page as u8)).animated(!matches!(page, Page::AiProviders | Page::AiChat)).auto_shrink([false, false]).show(&mut child, |ui| {
            ui.set_width(body.width() - 8.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            match page {
                Page::General => general(app, ui, &pal),
                Page::Links => {
                    let st = app.ui.settings.as_mut().unwrap();
                    link_page::show(&mut st.links, &mut app.link_routing, &mut app.link_window, &mut app.link_prefs, &mut app.hotkey, &app.shortcuts, &app.lib, &app.lib.store, &app.active_space, &mut link_page::Native, ui, &pal);
                }
                Page::Downloads => download_preferences(app.ui.settings.as_mut().unwrap(), &app.lib, &app.active_space, &app.downloads_dir, ui, &pal),
                Page::AiProviders => {
                    if ai_visible {
                        let st = app.ui.settings.as_mut().unwrap();
                        crate::ui::ai_settings::show(&mut st.ai, &mut app.lib, &app.active_space, ui, &pal);
                        if let Some(profile) = crate::downloads::active_profile(&app.lib, &app.active_space).map(str::to_owned) {
                            // The selection this page holds, so a new Save applies at once.
                            let ready = match st.ai.as_ref().filter(|draft| draft.profile == profile).and_then(|draft| draft.saved.as_ref()) {
                                Some(saved) => crate::ai_features::ready_from(saved),
                                None => crate::ai_features::ready(&app.lib, &profile),
                            };
                            ai_feature_page::show(&mut st.browser_features, &app.lib, &profile, ready, ui, &pal);
                        }
                    }
                }
                Page::AiChat => {
                    let st = app.ui.settings.as_mut().unwrap();
                    if chat_visible && crate::ui::chat::show(&mut st.chat, &mut app.lib, &app.active_space, &account_scope, ui, &pal) {
                        st.chat = None;
                        st.page = Page::AiProviders;
                    }
                }
                Page::Appearance => {
                    window_material(&mut app.window_chrome, &app.lib.store, app.material_state, ui, &pal);
                    separator(ui, &pal);
                    ui.add_space(8.0);
                    appearance(&mut app.app_icon, ui, &pal);
                }
                Page::Spaces => spaces(app, ui, &pal),
                Page::Boosts => boosts(app, ui, &pal),
                Page::Passwords => crate::ui::passwords::show(&mut app.passwords, ui, &pal),
                Page::Privacy => {
                    content_blocking_page::show(&mut app.content_blocking, &app.lib, &app.active_space, ui, &pal);
                    crate::ui::site_permissions::settings_page(app, ui, &pal);
                }
                Page::Account => account(app, ui, &pal, now),
                Page::Shortcuts => {
                    let st = app.ui.settings.as_mut().unwrap();
                    shortcut_page::show(&mut st.shortcuts, &mut app.shortcuts, &mut app.numbered, &app.lib.store, app.hotkey.taken(), ui, &pal);
                }
                Page::Advanced => advanced(app, ui, &pal),
                Page::About => about(ui, &pal),
            }
            ui.add_space(16.0);
        });
    });
    if close {
        if let Some(st) = app.ui.settings.as_mut() {
            st.closing_at.get_or_insert(now);
        }
    }
}

pub(super) fn compact_navigation_needed(size: egui::Vec2) -> bool {
    size.x < 640.0 || size.y < 56.0 + Page::ALL.len() as f32 * 32.0 + 16.0
}

fn compact_navigation(ui: &mut Ui, rect: Rect, page: &mut Page) -> egui::InnerResponse<Option<()>> {
    let header = Rect::from_min_max(rect.left_top() + vec2(108.0, 16.0), pos2(rect.right() - 52.0, rect.top() + 48.0));
    let mut nav = ui.new_child(egui::UiBuilder::new().max_rect(header));
    egui::ComboBox::from_id_salt("settings-page").selected_text(page.title()).width((header.width() - 8.0).max(120.0)).show_ui(&mut nav, |ui| {
        keep_popup_above_sheet(ui);
        for target in Page::ALL {
            ui.selectable_value(page, target, target.title());
        }
    })
}

pub fn set_visuals(ui: &mut Ui, pal: &Palette) {
    let mut v = if pal.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(pal.panel_text);
    v.selection.bg_fill = pal.accent.gamma_multiply(0.8);
    v.hyperlink_color = pal.accent;
    v.widgets.inactive.corner_radius = egui::CornerRadius::same(7);
    v.widgets.hovered.corner_radius = egui::CornerRadius::same(7);
    v.widgets.active.corner_radius = egui::CornerRadius::same(7);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.panel_border);
    v.extreme_bg_color = if pal.dark { Color32::from_rgb(0x1e, 0x1e, 0x24) } else { Color32::from_rgb(0xf4, 0xf4, 0xf7) };
    ui.style_mut().visuals = v;
    ui.style_mut().spacing.button_padding = vec2(12.0, 5.0);
    ui.style_mut().spacing.interact_size.y = 26.0;
}

/// A labelled preference row: title and description on the left, control on the right.
/// A title alone is centred on the control's line.
pub(super) fn row(ui: &mut Ui, pal: &Palette, title: &str, desc: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        let width = (w - 230.0).max(160.0);
        if desc.is_empty() {
            let line = vec2(width, ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(line, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_width(width);
                ui.add(egui::Label::new(RichText::new(title).size(13.5).color(pal.panel_text)).wrap());
            });
        } else {
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.label(RichText::new(title).size(13.5).color(pal.panel_text));
                ui.label(RichText::new(desc).size(12.0).color(pal.panel_dim));
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
    separator(ui, pal);
}

pub(super) fn separator(ui: &mut Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0, pal.panel_border);
}

pub(super) fn section(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text.to_uppercase()).size(11.0).color(pal.panel_dim).strong());
}

/// An animated on/off switch.
pub fn toggle(ui: &mut Ui, on: &mut bool, accent: Color32) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.16);
    let off = if ui.visuals().dark_mode { Color32::from_gray(80) } else { Color32::from_gray(200) };
    let bg = Color32::from(egui::Rgba::from(off) * (1.0 - t) + egui::Rgba::from(accent) * t);
    ui.painter().rect_filled(rect, 11, bg);
    let x = anim::lerp(rect.left() + 11.0, rect.right() - 11.0, t);
    ui.painter().circle_filled(pos2(x, rect.center().y), 9.0, Color32::WHITE);
    resp
}

pub(super) fn segmented<T: PartialEq + Copy>(ui: &mut Ui, pal: &Palette, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    let w = options.iter().map(|(_, l)| l.len() as f32 * 7.0 + 22.0).sum::<f32>();
    let (rect, _) = ui.allocate_exact_size(vec2(w + 4.0, 28.0), Sense::hover());
    ui.painter().rect_filled(rect, 8, pal.panel_hover.gamma_multiply(1.6));
    let mut x = rect.left() + 2.0;
    for (i, (v, label)) in options.iter().enumerate() {
        let bw = label.len() as f32 * 7.0 + 22.0;
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(bw, 24.0));
        let resp = ui.interact(r, ui.scope_id().with(("seg", i, label)), Sense::click());
        if *value == *v {
            ui.painter().rect_filled(r, 6, pal.panel);
            ui.painter().rect_stroke(r, 6, Stroke::new(1.0, pal.panel_border), egui::StrokeKind::Inside);
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, FontId::proportional(12.5), if *value == *v { pal.panel_text } else { pal.panel_dim });
        if resp.clicked() && *value != *v {
            *value = *v;
            changed = true;
        }
        x += bw;
    }
    changed
}

// -- pages ---------------------------------------------------------------------------

fn profile_preference_recovery(st: &mut SettingsState, lib: &mut Library, ui: &mut Ui, pal: &Palette) {
    let Some(pending) = st.pending_profile_preferences.as_mut() else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    let mut retry = false;
    let mut cancel = false;
    let response = ui.push_id("profile-preference-recovery", |ui| {
        ui.label(style::text(&pending.error, 12.0, style::weight::REGULAR).color(ink.danger));
        ui.horizontal_wrapped(|ui| {
            retry = Button::new("Retry Save").kind(Kind::Primary).show(ui, &ink).clicked();
            cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked();
        });
    }).response;
    if pending.reveal_error && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        pending.reveal_error = false;
    }
    if cancel { st.pending_profile_preferences = None; }
    else if retry {
        st.retry_profile_preferences(lib);
        ui.ctx().request_repaint();
    }
}

fn preference_recovery(st: &mut SettingsState, lib: &mut Library, ui: &mut Ui, pal: &Palette) {
    profile_preference_recovery(st, lib, ui, pal);
    let Some(pending) = st.pending_preferences.as_mut() else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    let mut retry = false;
    let mut cancel = false;
    let response = ui.vertical(|ui| {
        ui.label(style::text(&pending.error, 12.0, style::weight::REGULAR).color(ink.danger));
        ui.horizontal_wrapped(|ui| {
            retry = Button::new("Retry Save").kind(Kind::Primary).show(ui, &ink).clicked();
            cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked();
        });
    }).response;
    if pending.reveal_error && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        pending.reveal_error = false;
    }
    if cancel {
        st.cancel_preferences();
    } else if retry {
        st.retry_preferences(lib);
        ui.ctx().request_repaint();
    }
}

fn profile_browsing_preferences(st: &mut SettingsState, lib: &mut Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    let Some(profile) = crate::downloads::active_profile(lib, space).map(str::to_owned) else {
        ui.label(style::text("This space has no available profile. Select another space to change browsing preferences.", 13.0, style::weight::REGULAR).color(ink.danger));
        return;
    };
    ui.label(style::text(format!("For {}", lib.profiles[&profile].name), 12.0, style::weight::REGULAR).color(ink.secondary));
    let blocked = st.pending_profile_preferences.as_ref().is_some_and(|pending| pending.profile != profile);
    if blocked {
        ui.label(style::text("Resolve the unsaved preferences above before editing another profile.", 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    let Some(before) = st.profile_preferences(lib, &profile) else {
        ui.label(style::text("Browsing preferences are unavailable for this profile. Saved data is unchanged; restore a valid profile before editing.", 13.0, style::weight::REGULAR).color(ink.danger));
        return;
    };
    let mut preferences = before;
    // Engines added on this Mac are offered too; choosing one is kept here,
    // and choosing a built-in engine again replaces it.
    let (added, chosen) = search_page::added(&mut st.search, lib, &profile);
    let mut choice = None;
    ui.add_enabled_ui(!blocked, |ui| {
        row(ui, pal, "Search engine", "Used by this profile's command bar and selected-text search.", |ui| {
            let built_in = preferences.search_engine.unwrap_or(lib.settings.search_engine).name();
            let selected = chosen.as_ref().and_then(|id| added.iter().find(|(added, _)| added == id)).map_or(built_in, |(_, name)| name.as_str());
            egui::ComboBox::from_id_salt(("profile-engine", &profile)).selected_text(selected).width(150.0).truncate().show_ui(ui, |ui| {
                keep_popup_above_sheet(ui);
                let shared = format!("Shared default ({})", lib.settings.search_engine.name());
                if ui.selectable_label(chosen.is_none() && preferences.search_engine.is_none(), shared).clicked() {
                    preferences.search_engine = None;
                    choice = Some(None);
                }
                for engine in SearchEngine::ALL {
                    if ui.selectable_label(chosen.is_none() && preferences.search_engine == Some(engine), engine.name()).clicked() {
                        preferences.search_engine = Some(engine);
                        choice = Some(None);
                    }
                }
                for (id, name) in &added {
                    if ui.selectable_label(chosen.as_ref() == Some(id), name).clicked() { choice = Some(Some(id.clone())); }
                }
            });
        });
        row(ui, pal, "Archive unpinned tabs after", "Inactive tabs only. Pages you use stay open until you quit.", |ui| {
            let hours = preferences.auto_archive_hours.unwrap_or(lib.settings.auto_archive_hours);
            egui::ComboBox::from_id_salt(("profile-archive", &profile)).selected_text(archive_label(hours)).width(150.0).show_ui(ui, |ui| {
                keep_popup_above_sheet(ui);
                ui.selectable_value(&mut preferences.auto_archive_hours, None, format!("Shared default ({})", archive_label(lib.settings.auto_archive_hours)));
                for hours in ARCHIVE_CHOICES {
                    ui.selectable_value(&mut preferences.auto_archive_hours, Some(hours), archive_label(hours));
                }
            });
        });
    });
    if let Some(choice) = choice {
        search_page::choose(&mut st.search, lib, &profile, choice);
        ui.ctx().request_repaint();
    }
    search_page::show(&mut st.search, lib, &profile, ui, pal);
    document_page::show(&mut st.document, lib, &profile, ui, pal);
    if preferences != before {
        st.save_profile_preferences(lib, &profile, preferences);
        ui.ctx().request_repaint();
    }
}

fn general_preferences(st: &mut SettingsState, lib: &mut Library, space: &str, ui: &mut Ui, pal: &Palette) {
    preference_recovery(st, lib, ui, pal);
    section(ui, pal, "Browsing");
    profile_browsing_preferences(st, lib, space, ui, pal);
    let before = st.preferences(lib);
    let mut s = before.clone();
    row(ui, pal, "Open links from pinned tabs in Peek", "Links to other sites open in a floating preview instead of a new tab.", |ui| {
        toggle(ui, &mut s.peek_external_links, pal.accent);
    });
    section(ui, pal, "Appearance");
    row(ui, pal, "Sidebar position", "", |ui| {
        let mut right = s.sidebar_on_right;
        if segmented(ui, pal, &mut right, &[(false, "Left"), (true, "Right")]) {
            s.sidebar_on_right = right;
        }
    });
    row(ui, pal, "Show full URL in the sidebar", "Otherwise only the site name is shown.", |ui| {
        toggle(ui, &mut s.show_url_in_sidebar, pal.accent);
    });
    row(ui, pal, "Smooth animations", "Turn off to make every transition instant.", |ui| {
        toggle(ui, &mut s.smooth_animations, pal.accent);
    });
    if s != before {
        st.save_preferences(lib, s);
        ui.ctx().request_repaint();
    }
}

fn download_preferences(st: &mut SettingsState, lib: &Library, space: &str, default: &std::path::Path, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    let Some(profile) = crate::downloads::active_profile(lib, space) else {
        st.downloads = None;
        ui.label(style::text("This space has no available profile. Select another space to change downloads.", 13.0, style::weight::REGULAR).color(ink.danger));
        return;
    };
    if st.downloads.as_ref().is_none_or(|state| state.profile != profile) {
        st.downloads = Some(crate::downloads::SettingsDraft::load(lib, profile));
    }
    let state = st.downloads.as_mut().unwrap();
    state.poll();
    ui.label(style::text(format!("For {} · this device only", lib.profiles[profile].name), 12.0, style::weight::REGULAR).color(ink.secondary));
    section(ui, pal, "Download folder");
    let path = state.draft.directory.as_deref().unwrap_or(default).display().to_string();
    ui.add(egui::Label::new(style::text(&path, 13.0, style::weight::MEDIUM).color(ink.text)).truncate()).on_hover_text(&path);
    ui.add_enabled_ui(!state.picking(), |ui| {
        ui.horizontal_wrapped(|ui| {
            if Button::new("Choose Folder…").enabled(cfg!(target_os = "macos")).show(ui, &ink).clicked() {
                state.choose_folder(ui.ctx());
            }
            if Button::new("Use Default").kind(Kind::Plain).enabled(state.draft.directory.is_some()).show(ui, &ink).clicked() {
                state.draft.directory = None;
            }
        });
    });
    if state.picking() {
        ui.label(style::text("Choose a folder or cancel in the native dialog.", 12.0, style::weight::REGULAR).color(ink.secondary));
    } else if !cfg!(target_os = "macos") {
        ui.label(style::text("Folder selection is currently available on macOS only.", 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    separator(ui, pal);
    ui.add_enabled_ui(!state.picking(), |ui| {
        ui.horizontal(|ui| {
            let label = ui.label(style::text("Ask where to save each file", 13.5, style::weight::MEDIUM).color(ink.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = toggle(ui, &mut state.draft.ask_each_time, ink.accent).labelled_by(label.id);
                response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), state.draft.ask_each_time, "Ask where to save each file"));
                if response.has_focus() {
                    ui.painter().rect_stroke(response.rect.expand(2.0), 13.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
                }
                if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
            });
        });
    });
    ui.label(style::text("Saved changes apply to new downloads, including open tabs and Peek. Closing Settings discards unsaved changes.", 12.0, style::weight::REGULAR).color(ink.secondary));
    let mut save = false;
    let mut cancel = false;
    let recovery = ui.vertical(|ui| {
        if let Some(error) = &state.error { ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger)); }
        ui.horizontal_wrapped(|ui| {
            save = Button::new(if state.error.is_some() { "Retry Save" } else { "Save" }).kind(Kind::Primary)
                .enabled(!state.picking() && state.changed()).show(ui, &ink).clicked();
            cancel = Button::new("Cancel").kind(Kind::Plain)
                .enabled(state.changed() || state.error.is_some() || state.picking()).show(ui, &ink).clicked();
        });
    }).response;
    if state.error.is_some() && state.reveal_error && !ui.is_sizing_pass() && ui.is_visible() {
        recovery.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        state.reveal_error = false;
    }
    if cancel { state.cancel(lib); }
    else if save { state.save(lib); }
}

fn general(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    general_preferences(app.ui.settings.as_mut().unwrap(), &mut app.lib, &app.active_space, ui, pal);
    // The sidebar switches end the Appearance section above.
    sidebar_page::show(&mut app.sidebar, &app.lib, &app.active_space, ui, pal);
    website_appearance_page::show(&mut app.website_appearance, &app.lib, &app.active_space, ui, pal);
    pip_page::show(&mut app.pip.prefs, &app.lib, &app.active_space, ui, pal);
    capture_page::show(&mut app.capture.prefs, &app.lib, &app.active_space, ui, pal);
    quote_page::show(&mut app.quote, &app.lib.store, ui, pal);
    quit_page::show(&mut app.quit.borrow_mut(), &app.lib.store, ui, pal);
    let browser_downloads = app.downloads.iter().filter(|download| download.active()).count();
    if update_page::show(&mut app.updates, &app.lib.store, browser_downloads, ui, pal).is_some() {
        app.quit_for_update(ui.ctx());
    }
    section(ui, pal, "Import");
    let st = app.ui.settings.as_mut().unwrap();
    row(ui, pal, "Import from Arc", "Spaces, pinned tabs, folders and favorites from this Mac's Arc.", |ui| {
        if ui.button("Import…").clicked() {
            st.import_status = Some(match crate::import::import_arc(&mut app.lib) {
                Ok(r) => r.summary(),
                Err(e) => e,
            });
        }
    });
    let st = app.ui.settings.as_mut().unwrap();
    row(ui, pal, "Import Chrome bookmarks", "Bookmarks become pinned tabs in a new “Chrome” space.", |ui| {
        if ui.button("Import…").clicked() {
            st.import_status = Some(match crate::import::import_chrome_bookmarks(&mut app.lib) {
                Ok(r) => r.summary(),
                Err(e) => e,
            });
        }
    });
    if let Some(msg) = &app.ui.settings.as_ref().unwrap().import_status {
        ui.label(RichText::new(msg).size(12.5).color(pal.panel_dim));
    }
}

fn material_description(state: crate::native_window::MaterialState, requested: bool) -> &'static str {
    use crate::native_window::OpaqueReason;
    if !requested { return "Opaque window. Web pages always stay opaque."; }
    match state.opaque_reason {
        None => "Native desktop material. Web pages stay opaque.",
        Some(OpaqueReason::ReduceTransparency) => "Using an opaque window because Reduce Transparency is enabled in macOS. Your choice is saved.",
        Some(OpaqueReason::IncreasedContrast) => "Using an opaque window because Increase Contrast is enabled in macOS. Your choice is saved.",
        Some(OpaqueReason::UnsupportedPlatform) => "Desktop transparency is available on macOS only. This platform uses an opaque window.",
        _ => "Native material is unavailable. Using an opaque window; your choice is saved. Try reopening Crossbow.",
    }
}

fn window_material(chrome: &mut crate::window_appearance::WindowChrome, store: &crossbow_core::store::Store, state: crate::native_window::MaterialState, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    ui.push_id("window-material", |ui| {
        ui.label(style::text("Window material", 15.0, style::weight::SEMIBOLD).color(ink.text));
        let supported = cfg!(target_os = "macos");
        ui.add_enabled_ui(supported, |ui| {
            ui.horizontal(|ui| {
                let label = ui.label(style::text("Transparent window", 13.5, style::weight::MEDIUM).color(ink.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = toggle(ui, &mut chrome.draft.transparent, ink.accent).labelled_by(label.id);
                    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), chrome.draft.transparent, "Transparent window"));
                    if response.has_focus() {
                        ui.painter().rect_stroke(response.rect.expand(2.0), 13.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
                    }
                    if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
                });
            });
            ui.add_enabled_ui(chrome.draft.transparent, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 150.0).clamp(80.0, 200.0);
                let response = ui.add(egui::Slider::new(&mut chrome.draft.tint, 0..=100).text("Space tint").suffix("%"));
                if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
            });
        });
        ui.label(style::text(material_description(state, chrome.saved().transparent), 12.0, style::weight::REGULAR).color(ink.secondary));
        if chrome.changed() {
            ui.label(style::text("Not applied", 12.0, style::weight::MEDIUM).color(ink.secondary));
        }
        ui.horizontal_wrapped(|ui| {
            let has_error = chrome.error.is_some();
            if Button::new(if has_error { "Retry" } else { "Apply" }).kind(Kind::Primary).enabled(chrome.changed() || has_error).show(ui, &ink).clicked() {
                chrome.apply(store);
                ui.ctx().request_repaint();
            }
            if (chrome.changed() || has_error) && Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() {
                chrome.cancel();
            }
        });
        if let Some(message) = &chrome.error {
            ui.label(style::text(message, 12.0, style::weight::REGULAR).color(ink.danger));
        }
        ui.label(style::text("Saved on this device only. Space tint does not change website colors or zoom.", 12.0, style::weight::REGULAR).color(ink.secondary));
    });
}

fn appearance(icon: &mut crate::app_icon::AppIcon, ui: &mut Ui, pal: &Palette) {
    use crate::app_icon::{AppIcon, IconChoice, IMAGE_REQUIREMENTS};
    let ink = Ink::new(pal.panel, pal.accent);
    let ctx = ui.ctx().clone();
    let enabled = AppIcon::supported() && !icon.busy();
    let texture = icon.preview_texture(&ctx);
    ui.horizontal(|ui| {
        ui.add(egui::Image::new(&texture).fit_to_exact_size(vec2(80.0, 80.0)).alt_text(format!("{} application icon preview", icon.choice().name())));
        ui.vertical(|ui| {
            ui.label(style::text("Application icon", 15.0, style::weight::SEMIBOLD).color(ink.text));
            ui.label(style::text(format!("Current: {}", icon.current_name()), 13.0, style::weight::MEDIUM).color(ink.secondary));
            if icon.changed() {
                ui.label(style::text(format!("Preview: {} · not applied", icon.choice().name()), 12.0, style::weight::MEDIUM).color(ink.secondary));
            }
        });
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for choice in IconChoice::BUILT_INS {
            let texture = icon.builtin_texture(&ctx, choice);
            let selected = icon.choice() == choice;
            let (rect, response) = ui.allocate_exact_size(vec2(88.0, 98.0), if enabled { Sense::click() } else { Sense::hover() });
            response.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, choice.name()));
            let radius = style::radius::MD;
            if selected || response.hovered() && enabled {
                ui.painter().rect_filled(rect, radius, if selected { ink.fill_strong } else { ink.fill_hover });
            }
            if selected {
                ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, ink.accent), egui::StrokeKind::Inside);
            }
            if response.has_focus() {
                ui.painter().rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
            }
            if response.gained_focus() {
                response.scroll_to_me(Some(egui::Align::Center));
            }
            let image_rect = Rect::from_center_size(pos2(rect.center().x, rect.top() + 38.0), vec2(64.0, 64.0));
            ui.painter().image(texture.id(), image_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            style::paint(ui.painter(), pos2(rect.center().x, rect.bottom() - 15.0), Align2::CENTER_CENTER, choice.name(), 12.0, style::weight::MEDIUM, ink.text);
            if response.clicked() { icon.choose_builtin(choice); }
        }
    });
    ui.add_space(6.0);
    if Button::new("Choose PNG…").enabled(enabled).show(ui, &ink).clicked() {
        icon.choose_png(&ctx);
    }
    ui.label(style::text(IMAGE_REQUIREMENTS, 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if Button::new("Apply").kind(Kind::Primary).enabled(enabled && icon.can_apply()).loading(icon.saving()).show(ui, &ink).clicked() {
            icon.apply(&ctx);
        }
        if Button::new("Reset").enabled(enabled && icon.can_reset()).show(ui, &ink).on_hover_text("Apply Black and remove the saved custom image.").clicked() {
            icon.reset(&ctx);
        }
        if icon.changed() && Button::new("Cancel").kind(Kind::Plain).enabled(enabled).show(ui, &ink).clicked() {
            icon.cancel_changes();
        }
    });
    if icon.busy() {
        ui.label(style::text(icon.busy_label(), 12.0, style::weight::MEDIUM).color(ink.secondary));
    }
    if let Some(message) = &icon.message {
        let response = ui.label(style::text(message, 12.0, style::weight::REGULAR).color(if icon.has_error { ink.danger } else { ink.secondary }));
        // Wait for a real layout pass, then reveal once without stealing focus
        // or leaving an animated target that competes with the user's wheel.
        if !ui.is_sizing_pass() && ui.is_visible() && icon.take_error_reveal() {
            response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        }
    }
    ui.add_space(4.0);
    let scope = if AppIcon::supported() {
        "Changes the Dock and app switcher on this Mac. The Finder icon stays with the application bundle. Icon choices are not synced."
    } else {
        "Icon customization is available on macOS only. This platform keeps the bundled icon."
    };
    ui.label(style::text(scope, 12.0, style::weight::REGULAR).color(ink.secondary));
}

fn spaces(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    let spaces = app.lib.spaces_ordered();
    let st = app.ui.settings.as_mut().unwrap();
    let selected = st.selected_space.clone().filter(|s| app.lib.spaces.contains_key(s)).unwrap_or_else(|| app.active_space.clone());
    st.selected_space = Some(selected.clone());

    ui.horizontal_wrapped(|ui| {
        for (id, s) in &spaces {
            let sel = *id == selected;
            let text = space_icon::label_job(&s.icon, &s.name, 13.0, pal.panel_text);
            let resp = ui.add(egui::Button::new(text).selected(sel).corner_radius(8));
            if resp.clicked() {
                app.ui.settings.as_mut().unwrap().selected_space = Some(id.clone());
                app.actions.push(Action::SwitchSpace(id.clone()));
            }
        }
        if ui.add(egui::Button::new(RichText::new("+ New Space").size(13.0)).corner_radius(8)).clicked() {
            app.actions.push(Action::NewSpace);
        }
    });
    ui.add_space(6.0);
    let Some(space) = app.lib.spaces.get(&selected).cloned() else { return };

    section(ui, pal, "Space");
    let st = app.ui.settings.as_mut().unwrap();
    if st.space_name.as_ref().is_none_or(|(id, _)| *id != selected) {
        st.space_name = Some((selected.clone(), space.name.clone()));
    }
    let mut commit_name = None;
    row(ui, pal, "Name", "", |ui| {
        let (_, name) = st.space_name.as_mut().unwrap();
        let resp = ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
        if resp.lost_focus() && !name.trim().is_empty() && name.trim() != space.name {
            commit_name = Some(name.trim().to_string());
        }
    });
    if let Some(n) = commit_name {
        app.actions.push(Action::RenameSpace(selected.clone(), n));
    }
    ui.label(RichText::new("Icon").size(13.5));
    let picker_id = Id::unique(("space-icon-picker", &selected));
    let tabs = [(space_icon::Tab::Emoji, "Emoji"), (space_icon::Tab::Icons, "Icons")];
    if let Some(icon) = space_icon::picker(ui, pal, picker_id, &space.icon, |ui, tab| segmented(ui, pal, tab, &tabs)) {
        app.actions.push(Action::SetSpaceIcon(selected.clone(), icon));
    }
    separator(ui, pal);

    section(ui, pal, "Theme");
    let mut theme = space.theme.clone();
    let changed = ui.push_id(("space-theme", &selected), |ui| {
        theme_editor::show_with_popup_parent(ui, &mut theme, app.system_dark, &app.texture_bank, Some(backdrop_layer()))
    }).inner;
    if changed {
        // Apply each edit to the selected space; the shared editor is stateless.
        if let Err(error) = app.lib.update_space(&selected, |s| s.theme = theme) {
            app.actions.push(Action::Toast(format!("Could not save the theme: {error}")));
        }
    }

    let state = &mut app.ui;
    let sites = profiles::Sites { profiles: &mut app.site_profiles, routing: app.link_routing.saved(), prefill: &mut app.site_prefill };
    profiles::show(&mut state.settings.as_mut().unwrap().profiles, &mut app.lib, &selected, &mut state.new_space_profile, sites, &mut app.actions, ui, pal);

    ui.add_space(12.0);
    if app.lib.spaces.len() > 1 && ui.add(egui::Button::new(RichText::new("Delete Space").color(Color32::from_rgb(0xe5, 0x48, 0x4d)))).clicked() {
        app.actions.push(Action::DeleteSpace(selected.clone()));
        app.ui.settings.as_mut().unwrap().selected_space = None;
    }
}

fn boosts(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    let current_host = app.focused_tab_id().and_then(|t| app.tab(&t).and_then(|t| crossbow_core::search::host_of(&t.url)));
    boosts_content(app.ui.settings.as_mut().unwrap(), &mut app.lib, current_host.as_deref(), ui, pal);
}

fn boosts_content(st: &mut SettingsState, lib: &mut Library, current_host: Option<&str>, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    ui.label(RichText::new("Boosts restyle or script a site: CSS and JavaScript run on every page of the host, in this browser and on your synced devices.").size(12.5).color(pal.panel_dim));
    ui.add_space(4.0);
    if let Some(pending) = &st.pending_boost_toggle {
        let mut retry = false;
        let mut cancel = false;
        let response = ui.vertical(|ui| {
            ui.label(style::text(&pending.error, 12.0, style::weight::REGULAR).color(ink.danger));
            ui.horizontal_wrapped(|ui| {
                retry = Button::new("Retry").kind(Kind::Primary).show(ui, &ink).clicked();
                cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked();
            });
        }).response;
        reveal_boost_error(st, ui, &response);
        if cancel { st.pending_boost_toggle = None; }
        else if retry { st.retry_boost_toggle(lib); }
    }
    ui.horizontal(|ui| {
        let label = match current_host {
            Some(h) => format!("+ New Boost for {h}"),
            None => "+ New Boost".into(),
        };
        if ui.add_enabled(st.boost.is_none() && st.pending_boost_toggle.is_none(), egui::Button::new(label)).clicked() {
            let host = current_host.unwrap_or_default().to_owned();
            st.cancel_boost();
            st.boost = Some((None, Boost { name: if host.is_empty() { "New Boost".into() } else { format!("{host} Boost") }, host, css: String::new(), js: String::new(), enabled: true }));
        }
    });
    let list: Vec<(String, Boost)> = lib.boosts.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (id, b) in &list {
        ui.horizontal(|ui| {
            let mut enabled = b.enabled;
            let available = st.boost.is_none() && st.pending_boost_toggle.is_none();
            if ui.add_enabled_ui(available, |ui| toggle(ui, &mut enabled, pal.accent)).inner.changed() {
                st.set_boost_enabled(lib, id, enabled);
            }
            ui.label(RichText::new(&b.name).size(13.5));
            ui.label(RichText::new(&b.host).size(12.0).color(pal.panel_dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(st.boost.is_none() && st.pending_boost_toggle.is_none(), egui::Button::new("Edit").small()).clicked() {
                    st.cancel_boost();
                    st.boost = Some((Some(id.clone()), b.clone()));
                }
            });
        });
        separator(ui, pal);
    }
    let Some((id, draft)) = st.boost.as_mut() else {
        if list.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new("No Boosts yet.").color(pal.panel_dim));
        }
        return;
    };
    section(ui, pal, if id.is_some() { "Edit Boost" } else { "New Boost" });
    egui::Grid::new("boost-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(260.0));
        ui.end_row();
        ui.label("Host");
        ui.add(egui::TextEdit::singleline(&mut draft.host).hint_text("example.com").desired_width(260.0));
        ui.end_row();
    });
    ui.label("CSS");
    ui.add(egui::TextEdit::multiline(&mut draft.css).code_editor().desired_rows(6).desired_width(f32::INFINITY).hint_text("body { font-family: Georgia; }"));
    ui.label("JavaScript");
    ui.add(egui::TextEdit::multiline(&mut draft.js).code_editor().desired_rows(4).desired_width(f32::INFINITY).hint_text("document.title = 'Boosted';"));
    let existing = id.is_some();
    let valid_host = !draft.host.trim().is_empty();
    let response = ui.vertical(|ui| {
        let mut save = false;
        let mut delete = false;
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            let save_label = if st.boost_error.is_some() && !st.boost_delete_failed { "Retry Save" } else { "Save" };
            save = ui.add_enabled(valid_host, egui::Button::new(save_label)).clicked();
            cancel = ui.button("Cancel").clicked();
            if existing {
                delete = ui.button(RichText::new(if st.boost_delete_failed { "Retry Delete" } else { "Delete" }).color(ink.danger)).clicked();
            }
        });
        ui.label(RichText::new("Changes apply the next time a matching page loads.").size(12.0).color(pal.panel_dim));
        if save { st.save_boost(lib); }
        if delete { st.delete_boost(lib); }
        if cancel { st.cancel_boost(); }
        if let Some(error) = &st.boost_error {
            ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
        }
    }).response;
    reveal_boost_error(st, ui, &response);
}

fn reveal_boost_error(st: &mut SettingsState, ui: &mut Ui, response: &egui::Response) {
    if st.boost_error_reveal && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        st.boost_error_reveal = false;
    }
}

fn account(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette, now: f64) {
    account_page(app.ui.settings.as_mut().unwrap(), &mut app.sync, &mut app.lib, ui, pal, now);
}

fn account_page(st: &mut SettingsState, sync: &mut SyncGlue, lib: &mut Library, ui: &mut Ui, pal: &Palette, now: f64) {
    let ink = Ink::new(pal.panel, pal.accent);
    ui.label(RichText::new("Sync spaces, pinned tabs, folders, favorites, Boosts and settings across your Macs and PCs. Open tabs, history, cookies and passwords stay on this device.").size(12.5).color(pal.panel_dim));
    ui.add_space(6.0);
    if let Some(s) = sync.session.clone() {
        let e2ee = sync.encryption_status(lib);
        let paused = match e2ee {
            Some(E2eeStatus::Locked) => Some("Enter the recovery code below to sync this device."),
            Some(E2eeStatus::Unreadable) => Some("This device's encryption state can't be read."),
            Some(E2eeStatus::Unavailable) => Some("This device's encryption state couldn't be read."),
            Some(E2eeStatus::OtherAccount) => Some("This device's library belongs to another account."),
            _ => None,
        };
        // A short status fits the control column; the reason wraps beside it.
        let (label, detail, color) = match (&sync.status, paused) {
            (_, Some(reason)) => ("Paused", reason.to_string(), Color32::from_rgb(0xff, 0x9f, 0x0a)),
            (SyncStatus::Live, _) => ("Live", String::new(), Color32::from_rgb(0x34, 0xc7, 0x59)),
            (SyncStatus::Connecting, _) => ("Connecting…", String::new(), Color32::from_rgb(0xff, 0x9f, 0x0a)),
            (SyncStatus::Offline(e), _) => ("Offline", short(e), Color32::from_rgb(0xff, 0x45, 0x3a)),
            (SyncStatus::SignedOut, _) => ("Signed out", String::new(), pal.panel_dim),
        };
        // Signing out removes the only local copy of an encryption key, so it asks first.
        let has_key = sync.encryption_may_hold_key(lib);
        let mut sign_out = false;
        let signed_in = s.display_name.as_ref().map_or_else(|| "Signed in".to_string(), |name| format!("Signed in as {name}"));
        row(ui, pal, &s.email, &signed_in, |ui| {
            if !st.sign_out_confirm && ui.button("Sign Out").clicked() {
                if has_key { st.sign_out_confirm = true; } else { sign_out = true; }
            }
        });
        if st.sign_out_confirm {
            let warning = if e2ee == Some(E2eeStatus::Resetting) { SIGN_OUT_RESETTING } else { SIGN_OUT_WARNING };
            ui.label(style::text(warning, 12.5, style::weight::REGULAR).color(ink.text));
            ui.horizontal_wrapped(|ui| {
                if Button::new("Sign Out").kind(Kind::Primary).show(ui, &ink).clicked() { sign_out = true; }
                if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() { st.sign_out_confirm = false; }
            });
        }
        if sign_out {
            st.leave_account();
            st.e2ee_error = None;
            sync.sign_out(lib);
            return;
        }
        sync.check_browser_sign_in();
        browser_sign_in(sync, ui, pal, Some(&s.email));
        let status = |ui: &mut Ui, detail: &str, label: &str, color: Color32| row(ui, pal, "Status", detail, |ui| {
            ui.label(RichText::new(label).color(color));
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, color);
        });
        if let Some(off) = sync.sync_off_text() {
            // The account's sync data was deleted: nothing here transfers.
            st.delete_confirm = false;
            st.delete_email.clear();
            status(ui, off, "Off", pal.panel_dim);
            sync_off(st, sync, lib, ui, pal);
        } else {
            st.sync_on_confirm = false;
            status(ui, &detail, label, color);
            let held = sync.encryption_hold(lib);
            let last = sync.last_sync_ms.map(ago).unwrap_or_else(|| "Not yet".into());
            let pending = sync.pending;
            let waiting = match sync.upload_hold() {
                _ if held && pending > 0 => "Waiting for encryption before uploading.".to_string(),
                // The first-pull gate holds uploads; say so instead of a bare count.
                Some(hold) if pending > 0 => hold.to_string(),
                _ if pending > 0 => format!("{pending} local change{} waiting", if pending == 1 { "" } else { "s" }),
                _ => "Everything is up to date.".into(),
            };
            row(ui, pal, "Last synced", &waiting, |ui| {
                if ui.button("Sync Now").clicked() {
                    sync.sync_now();
                }
                ui.label(RichText::new(last).color(pal.panel_dim));
            });
            if held {
                // Locked by the account's current key: only its code helps.
                let locked = e2ee == Some(E2eeStatus::Locked);
                ui.label(style::text(if locked { LOCKED_HOLD } else { ENCRYPTION_HOLD }, 12.5, style::weight::REGULAR).color(ink.text));
                if !locked && Button::new("Upload Without Encryption").show(ui, &ink).clicked() {
                    st.sync_on_error = sync.upload_without_encryption(lib).err();
                }
                if let Some(error) = &st.sync_on_error { ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger)); }
            }
            encryption(st, sync, lib, ui, pal);
            section(ui, pal, "Sync data");
            row(ui, pal, "Server copy", "Delete this account's synced library from the sync server. This Mac keeps its own.", |ui| {
                if !st.delete_confirm && ui.button(RichText::new(DELETE_SYNC_DATA).color(ink.danger)).clicked() {
                    st.delete_confirm = true;
                    st.delete_email.clear();
                    sync.delete_error = None;
                }
            });
            if st.delete_confirm { delete_sync_data(st, sync, lib, ui, &ink, &s.email); }
        }
        section(ui, pal, "Devices");
        let devices = sync.devices.clone();
        if devices.is_empty() {
            ui.label(RichText::new("Loading devices…").color(pal.panel_dim));
        }
        for d in devices {
            let this = d.id == s.device_id;
            row(ui, pal, &format!("{}{}", d.name, if this { " (this device)" } else { "" }), &format!("{} · last seen {}", platform_name(&d.platform), ago(d.last_seen_at)), |_| {});
        }
        section(ui, pal, "Delete account");
        row(ui, pal, "Crossbow account", "Delete this account from the sync server. This Mac keeps its library.", |ui| {
            if sync.delete.step == DeleteStep::Closed && ui.button(RichText::new(DELETE_ACCOUNT).color(ink.danger)).clicked() {
                st.delete_account_email.clear();
                st.delete_account_code.clear();
                sync.open_delete_account();
            }
        });
        if sync.delete.step != DeleteStep::Closed { delete_account(st, sync, ui, &ink, &s.email); }
        return;
    }
    if let Some(done) = sync.delete.done {
        ui.label(style::text(done, 12.5, style::weight::REGULAR).color(ink.text));
        ui.add_space(12.0);
    }

    sync.check_browser_sign_in();
    let in_browser = browser_sign_in(sync, ui, pal, None);
    let has_error = sync.error.is_some();
    match sync.auth.clone() {
        // A build without a sync server offers no sign-in until the user enters one.
        _ if !sync.has_server() => {
            ui.label(style::text(crossbow_core::sync::server::NO_SERVER, 12.5, style::weight::REGULAR).color(ink.text));
        }
        AuthStep::Email => {
            style::field_label(ui, &ink, "Email address");
            let resp = style::field(ui, &ink, Id::unique("settings-email"), &mut st.email, "you@example.com", ui.available_width().min(360.0), has_error);
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = valid_email(&st.email);
            ui.add_space(12.0);
            // Continue in Browser is the page's one primary action when offered.
            let kind = if in_browser { Kind::Secondary } else { Kind::Primary };
            if (Button::new("Get a Sign-in Code").kind(kind).enabled(valid && !sync.busy).loading(sync.busy && !sync.browser.busy()).show(ui, &ink).clicked() || enter && valid) && !sync.busy {
                sync.start_auth(&st.email);
                st.code.clear();
                st.resend_at = now + 30.0;
            }
            ui.label(RichText::new("No password. Development servers show a test code instead of sending email.").size(12.0).color(pal.panel_dim));
        }
        AuthStep::Code { challenge_id, email, dev_code } => {
            ui.label(sync.auth.instruction(has_error));
            ui.add_space(12.0);
            let resp = ui.add(egui::TextEdit::singleline(&mut st.code).hint_text("123456").font(FontId::monospace(18.0)).char_limit(6).desired_width(120.0));
            st.code.retain(|c| c.is_ascii_digit());
            if let Some(code) = dev_code {
                ui.label(RichText::new(format!("Development test code: {code}")).size(12.0).color(pal.panel_dim));
            }
            let ready = valid_code(&st.code) && !challenge_id.is_empty() && !sync.busy;
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal_wrapped(|ui| {
                if Button::new("Verify & Sign In").kind(Kind::Primary).enabled(ready).loading(sync.busy).show(ui, &ink).clicked() || enter && ready {
                    sync.verify(&st.code);
                    st.code.clear();
                }
                if Button::new("Use a Different Email").kind(Kind::Plain).show(ui, &ink).clicked() {
                    sync.cancel_auth();
                    st.code.clear();
                }
                let wait = (st.resend_at - now).ceil().max(0.0) as u32;
                let label = if wait > 0 { format!("Resend in {wait}s") } else { "Resend code".into() };
                if Button::new(&label).kind(Kind::Plain).enabled(!sync.busy && (wait == 0 || challenge_id.is_empty() && has_error)).show(ui, &ink).clicked() {
                    sync.start_auth(&email);
                    st.code.clear();
                    st.resend_at = now + 30.0;
                }
                if wait > 0 { ui.ctx().request_repaint_after(std::time::Duration::from_secs(1)); }
            });
        }
    }
    if let Some(e) = &sync.error {
        ui.label(RichText::new(e).color(Color32::from_rgb(0xe5, 0x48, 0x4d)));
    }
    section(ui, pal, "Server");
    if st.server_url.is_empty() {
        st.server_url = sync.base_url.clone();
    }
    let mut apply = None;
    let hint = if sync.has_server() { "Leave as is unless you run your own Crossbow server." } else { "Enter your own Crossbow server to sign in." };
    row(ui, pal, "Sync server", hint, |ui| {
        let resp = ui.add(egui::TextEdit::singleline(&mut st.server_url).desired_width(260.0));
        if resp.lost_focus() && st.server_url.starts_with("http") {
            apply = Some(st.server_url.clone());
        }
    });
    if let Some(u) = apply
        && let Err(why) = sync.set_base_url(lib, &u)
    {
        sync.error = Some(format!("Sync server: {why}."));
    }
}

/// Opens the Clerk Account Portal in the default browser.
pub(super) const MANAGE_ACCOUNT: &str = "Manage Account in Browser";

/// Browser sign-in on the Account page: Continue in Browser when signed
/// out, Link Browser Sign-In… when signed in to `account` without one, the
/// wait for the browser, the link confirmation and the last attempt's
/// notice. True when Continue in Browser is offered.
fn browser_sign_in(sync: &mut SyncGlue, ui: &mut Ui, pal: &Palette, account: Option<&str>) -> bool {
    let ink = Ink::new(pal.panel, pal.accent);
    let offered = sync.browser.available().is_some();
    let mut continue_offered = false;
    match sync.browser.step.clone() {
        BrowserStep::Waiting { .. } => row(ui, pal, "Finish signing in in your browser", "Your default browser is open on the sign-in page. Crossbow waits up to 5 minutes.", |ui| {
            if Button::new("Cancel").show(ui, &ink).clicked() { sync.cancel_browser_sign_in(); }
        }),
        BrowserStep::Redeeming { link } => row(ui, pal, if link { "Checking the browser sign-in…" } else { "Signing in…" }, "", |ui| {
            if Button::new("Cancel").show(ui, &ink).clicked() { sync.cancel_browser_sign_in(); }
        }),
        BrowserStep::Confirm { email, .. } => {
            let browser_email = email.unwrap_or_else(|| "this browser sign-in".into());
            let account = account.unwrap_or_default();
            egui::Frame::new().fill(ink.fill).stroke(Stroke::new(1.0, ink.separator)).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(style::text(format!("Link {browser_email} to {account}?"), 13.0, style::weight::SEMIBOLD).color(ink.text));
                ui.label(style::text("Continue in Browser will then sign in to this account on any device. This account and its synced data stay as they are.", 12.5, style::weight::REGULAR).color(ink.text));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if Button::new("Link").kind(Kind::Primary).show(ui, &ink).clicked() { sync.confirm_browser_link(); }
                    if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() { sync.cancel_browser_sign_in(); }
                });
            });
        }
        BrowserStep::Confirming => row(ui, pal, "Linking…", "", |_| {}),
        BrowserStep::Idle => match (&sync.browser.offer, account) {
            (BrowserOffer::Available(_), None) => {
                continue_offered = true;
                row(ui, pal, "Sign in with your browser", "Use email or Google in your default browser.", |ui| {
                    if Button::new("Continue in Browser").kind(Kind::Primary).enabled(!sync.busy).show(ui, &ink).clicked() { sync.start_browser_sign_in(); }
                });
            }
            (BrowserOffer::Available(_), Some(_)) => match sync.browser.linked {
                Some(false) => row(ui, pal, "Browser sign-in", "Link one to sign in with email or Google in your browser.", |ui| {
                    if Button::new("Link Browser Sign-In…").show(ui, &ink).clicked() { sync.start_browser_sign_in(); }
                }),
                Some(true) => match sync.account_portal() {
                    Some(_) => row(ui, pal, "Browser sign-in", "Linked. Change its email, password or connected accounts in your browser.", |ui| {
                        if Button::new(MANAGE_ACCOUNT).show(ui, &ink).clicked() { sync.manage_account(); }
                    }),
                    None => row(ui, pal, "Browser sign-in", "Linked. Continue in Browser signs in to this account.", |_| {}),
                },
                None => {}
            },
            (BrowserOffer::Unreachable, _) => row(ui, pal, "Browser sign-in", "Couldn't check whether this sync server offers it.", |ui| {
                if Button::new("Check Again").show(ui, &ink).clicked() { sync.recheck_browser_sign_in(); }
            }),
            _ => {}
        },
    }
    if let Some(notice) = sync.browser.notice.clone() {
        ui.add_space(4.0);
        ui.label(style::text(notice.text(), 12.5, style::weight::REGULAR).color(if notice.is_error() { ink.danger } else { ink.text }));
        if notice.retry() && offered && sync.browser.step == BrowserStep::Idle && Button::new("Try Again").show(ui, &ink).clicked() {
            sync.retry_browser_sign_in();
        }
        ui.add_space(8.0);
    }
    continue_offered
}

/// Escape on the Account page ends a browser sign-in in progress, or its
/// link confirmation, or closes Delete Account…, before it closes Settings.
/// True when it did.
pub(crate) fn cancel_account_step(settings: Option<&SettingsState>, sync: &mut SyncGlue) -> bool {
    settings.is_some_and(|st| st.page == Page::Account) && (sync.cancel_browser_sign_in() || sync.cancel_delete_account())
}

const DELETE_SYNC_DATA: &str = "Delete Sync Data…";
/// What Delete Sync Data does, said before it does it.
const DELETE_FACTS: [&str; 4] = [
    "The sync server's copy of this account's library is deleted, for every device.",
    "This Mac keeps its sidebar, Spaces, tabs and pins. Sync turns off here.",
    "Other signed-in devices stop syncing and keep their own copy.",
    "End-to-end encryption keys and recovery codes for that data stop working.",
];
const TURN_ON_FRESH: &str = "Turning sync on uploads this Mac's library to the sync server as a fresh start. Other devices that turn sync on again merge their libraries with it.";
const TURN_ON_ENCRYPTED: &str = "This library was end-to-end encrypted. Its old key and recovery codes no longer work, so nothing uploads until you turn on encryption again, with a new recovery code, or choose to upload without encryption.";
const ENCRYPTION_HOLD: &str = "Uploads are waiting. This library was end-to-end encrypted with a key that's no longer in use. Turn on encryption below to upload it with a new key and recovery code, or upload it without encryption.";
/// The hold on a device Locked by the account's current key: only that
/// key's recovery code lets the library upload.
const LOCKED_HOLD: &str = "Uploads are waiting. This account's library is now end-to-end encrypted with a key from another device. Enter the recovery code from that device below to upload this library encrypted with its key.";
/// After Restore Data: how turning sync on merges the restored library.
const TURN_ON_RESTORED: &str = "Turning sync on merges this restored library with your synced data. Restored items replace their synced copies, here and on your other devices. Items added since the backup, on any device, are kept, and changes made after the restore win.";

/// Delete the account's sync data, once its email is typed. Nothing changes
/// here until the server confirms; then sync turns off.
fn delete_sync_data(st: &mut SettingsState, sync: &mut SyncGlue, lib: &Library, ui: &mut Ui, ink: &Ink, email: &str) {
    egui::Frame::new().fill(ink.danger.gamma_multiply(0.10)).stroke(Stroke::new(1.0, ink.danger.gamma_multiply(0.45))).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(style::text(format!("Delete sync data for {email}?"), 13.0, style::weight::SEMIBOLD).color(ink.text));
        for fact in DELETE_FACTS {
            ui.label(style::text(fact, 12.5, style::weight::REGULAR).color(ink.text));
        }
        ui.add_space(4.0);
        style::field_label(ui, ink, &format!("Type {email} to confirm."));
        let response = style::field(ui, ink, Id::unique("settings-delete-email"), &mut st.delete_email, "Account email", ui.available_width().min(360.0), sync.delete_error.is_some());
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let ready = st.delete_email.trim().eq_ignore_ascii_case(email.trim()) && !sync.deleting;
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let label = if sync.deleting { "Deleting…" } else { "Delete Sync Data" };
            if ui.add_enabled(ready, egui::Button::new(RichText::new(label).color(ink.danger))).clicked() || enter && ready {
                sync.delete_sync_data(lib);
            }
            if Button::new("Cancel").kind(Kind::Plain).enabled(!sync.deleting).show(ui, ink).clicked() {
                st.delete_confirm = false;
                st.delete_email.clear();
                sync.delete_error = None;
            }
        });
        if let Some(error) = &sync.delete_error {
            ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
        }
    });
}

const DELETE_ACCOUNT: &str = "Delete Account…";
/// What Delete Account does, said before it does it.
const DELETE_ACCOUNT_FACTS: [&str; 4] = [
    "The sync server deletes this account: its synced library, devices, sessions and browser sign-in links. Every device signs out.",
    "This Mac keeps its sidebar, Spaces, tabs and pins, and signs out.",
    "Your Clerk user for browser sign-in isn't deleted. Delete it with Clerk if you want it gone.",
    "This can't be undone. Signing in again with this email starts a new, empty account.",
];

/// Delete the account once its email is typed and a code sent to it is
/// entered. Nothing changes here until the server confirms; then this Mac
/// signs out and keeps its library.
fn delete_account(st: &mut SettingsState, sync: &mut SyncGlue, ui: &mut Ui, ink: &Ink, email: &str) {
    egui::Frame::new().fill(ink.danger.gamma_multiply(0.10)).stroke(Stroke::new(1.0, ink.danger.gamma_multiply(0.45))).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(style::text(format!("Delete the account {email}?"), 13.0, style::weight::SEMIBOLD).color(ink.text));
        for fact in DELETE_ACCOUNT_FACTS {
            ui.label(style::text(fact, 12.5, style::weight::REGULAR).color(ink.text));
        }
        ui.add_space(4.0);
        let deleting = matches!(sync.delete.step, DeleteStep::Deleting { .. });
        style::field_label(ui, ink, &format!("Type {email} to confirm."));
        let response = style::field(ui, ink, Id::unique("settings-delete-account-email"), &mut st.delete_account_email, "Account email", ui.available_width().min(360.0), false);
        let typed = st.delete_account_email.trim().eq_ignore_ascii_case(email.trim());
        match sync.delete.step.clone() {
            DeleteStep::Closed => {}
            DeleteStep::Confirm | DeleteStep::SendingCode => {
                let sending = sync.delete.step == DeleteStep::SendingCode;
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.label(style::text(format!("Crossbow emails a code to {email} to confirm it's you."), 12.5, style::weight::REGULAR).color(ink.text));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if (Button::new("Send a Code").enabled(typed && !sending).loading(sending).show(ui, ink).clicked() || enter && typed) && !sending {
                        sync.send_delete_code();
                    }
                    if Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked() { sync.cancel_delete_account(); }
                });
            }
            DeleteStep::Code { dev_code, .. } | DeleteStep::Deleting { dev_code, .. } => {
                ui.label(style::text(format!("Enter the code sent to {email}. It works for 5 minutes."), 12.5, style::weight::REGULAR).color(ink.text));
                let code = ui.add_enabled(!deleting, egui::TextEdit::singleline(&mut st.delete_account_code).id(Id::unique("settings-delete-account-code")).hint_text("123456").font(FontId::monospace(18.0)).char_limit(6).desired_width(120.0));
                st.delete_account_code.retain(|c| c.is_ascii_digit());
                if let Some(code) = dev_code {
                    ui.label(style::text(format!("Development test code: {code}"), 12.0, style::weight::REGULAR).color(ink.text));
                }
                let ready = typed && valid_code(&st.delete_account_code) && !deleting;
                let enter = code.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let label = if deleting { "Deleting…" } else { "Delete Account" };
                    if ui.add_enabled(ready, egui::Button::new(RichText::new(label).color(ink.danger))).clicked() || enter && ready {
                        sync.delete_account(&st.delete_account_email, &st.delete_account_code);
                        st.delete_account_code.clear();
                    }
                    if Button::new("Send a New Code").kind(Kind::Plain).enabled(!deleting).show(ui, ink).clicked() {
                        st.delete_account_code.clear();
                        sync.send_delete_code();
                    }
                    if Button::new("Cancel").kind(Kind::Plain).enabled(!deleting).show(ui, ink).clicked() { sync.cancel_delete_account(); }
                });
            }
        }
        if let Some(error) = &sync.delete.error {
            ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
        }
    });
}

/// Sync is off after a delete of the account's sync data or a restore from a
/// backup; Turn Sync On uploads this library again once the user confirms.
fn sync_off(st: &mut SettingsState, sync: &mut SyncGlue, lib: &mut Library, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    let text = |ui: &mut Ui, message: &str| { ui.label(style::text(message, 12.5, style::weight::REGULAR).color(ink.text)); };
    let restored = sync.sync_off.as_ref().is_some_and(|off| off.restored);
    let detail = if restored { "Merge this restored library with your synced data." } else { "Upload this Mac's library to the sync server again." };
    row(ui, pal, "Sync is off", detail, |ui| {
        if !st.sync_on_confirm && Button::new("Turn Sync On…").show(ui, &ink).clicked() {
            st.sync_on_confirm = true;
            st.sync_on_error = None;
        }
    });
    if st.sync_on_confirm {
        text(ui, if restored { TURN_ON_RESTORED } else { TURN_ON_FRESH });
        if sync.encrypted_before_delete(lib) { text(ui, TURN_ON_ENCRYPTED); }
        ui.horizontal_wrapped(|ui| {
            if Button::new("Turn Sync On").kind(Kind::Primary).show(ui, &ink).clicked() {
                st.sync_on_error = sync.turn_sync_on(lib).err();
                st.sync_on_confirm = st.sync_on_error.is_some();
            }
            if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() {
                st.sync_on_confirm = false;
                st.sync_on_error = None;
            }
        });
    }
    if let Some(error) = &st.sync_on_error { ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger)); }
}

/// Shown before Sign Out removes what may be the only local copy of the
/// library key: when on, resetting, or locked with a key that may be stored.
const SIGN_OUT_WARNING: &str = "Signing out removes this device's copy of the encryption key, if it has one. To sync this library here again, you'll need its recovery code.";
const SIGN_OUT_RESETTING: &str = "Signing out cancels the reset and removes this device's copy of the encryption key, if it has one. To sync this library here again, you'll need its recovery code.";
/// The recovery-code field never grows past this, so the buffer reserved for
/// it is never reallocated and every copy of a typed code can be wiped.
const RECOVERY_FIELD_BYTES: usize = 256;
const RESET_WARNING: &str = "Resetting creates a new key and recovery code. Your other devices need the new code to sync again. Synced items this device can't read are kept, not deleted, and appear once a device that has them enters the new code.";
const UNREADABLE: &str = "This device's encryption state can't be read, so sync is paused and nothing is applied or uploaded here. It may have been momentary, so try again first. If it stays unreadable, reset encryption to use a new recovery code.";
const UNAVAILABLE: &str = "This device's encryption state couldn't be read just now, so sync is paused and nothing is applied or uploaded here. This is usually momentary: check disk access, then try again.";

/// End-to-end encryption for library sync. There is deliberately no control to
/// turn it off again in this version.
fn encryption(st: &mut SettingsState, sync: &mut SyncGlue, lib: &mut Library, ui: &mut Ui, pal: &Palette) {
    let Some(status) = sync.encryption_status(lib) else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    let text = |ui: &mut Ui, message: &str| { ui.label(style::text(message, 12.5, style::weight::REGULAR).color(ink.text)); };
    section(ui, pal, "End-to-end encryption");
    let mut reset = false;
    match status {
        E2eeStatus::Off => {
            let ready = sync.encryption_ready();
            let desc = if ready {
                "Encrypt synced spaces, pinned tabs, folders, favorites, Boosts and settings on this device before upload. You'll save a recovery code for your other devices."
            } else {
                "Available once this device finishes syncing."
            };
            row(ui, pal, "Encryption is off", desc, |ui| {
                if Button::new("Turn On Encryption…").enabled(ready).show(ui, &ink).clicked() {
                    st.e2ee_saved = false;
                    st.e2ee_error = sync.begin_encryption(lib).err();
                }
            });
        }
        E2eeStatus::Confirming => {
            text(ui, "Save this recovery code in a password manager or on paper. You need it to unlock sync on another device or after signing out. Crossbow can't recover it for you.");
            if let Some(code) = sync.recovery_code() { recovery_code(ui, &ink, &code); }
            ui.checkbox(&mut st.e2ee_saved, "I saved this recovery code");
            let confirmable = sync.encryption_confirmable();
            // The line keeps its space once the check ends, so Turn On never
            // moves under the pointer just as it becomes enabled.
            checking_line(ui, &ink, CHECKING_CONFIRM, !confirmable);
            ui.horizontal_wrapped(|ui| {
                if Button::new("Turn On").kind(Kind::Primary).enabled(st.e2ee_saved && confirmable).show(ui, &ink).clicked() {
                    st.e2ee_error = sync.confirm_encryption(lib).err();
                    if st.e2ee_error.is_none() { st.e2ee_saved = false; }
                }
                if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() {
                    st.e2ee_saved = false;
                    st.e2ee_error = sync.cancel_encryption().err();
                }
            });
        }
        E2eeStatus::Locked => {
            text(ui, "This account's library is end-to-end encrypted. Enter its recovery code to sync on this device. Nothing is applied or uploaded here until then.");
            style::field_label(ui, &ink, "Recovery code");
            let field = Id::unique("settings-recovery-code");
            cap_typed_text(ui, field, &st.e2ee_code, RECOVERY_FIELD_BYTES);
            let response = style::field(ui, &ink, field, &mut st.e2ee_code, "CB1-XXXX-XXXX-…", ui.available_width().min(440.0), st.e2ee_error.is_some() || sync.unlock_error.is_some());
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            // The code is checked against the account's newest marker, so
            // Unlock first reads the server past the held page. A code entered
            // while a scan runs waits for it; one code waits at a time. The
            // line keeps its space while no scan runs, so a scan starting
            // never moves Unlock under the pointer.
            let checking = sync.checking_encryption();
            let line = checking_line(ui, &ink, CHECKING_LOCKED, checking);
            // Say so where the user is looking, also at the compact size.
            if checking && std::mem::take(&mut st.e2ee_reveal_checking) { line.scroll_to_me(None); }
            let ready = !st.e2ee_code.trim().is_empty() && !sync.unlock_waiting();
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if Button::new("Unlock").kind(Kind::Primary).enabled(ready).show(ui, &ink).clicked() || enter && ready {
                    st.e2ee_error = sync.unlock_encryption(lib, &st.e2ee_code).err();
                    // Right or wrong, the typed code is wiped.
                    st.e2ee_code.zeroize();
                    st.e2ee_reveal_checking = true;
                    // Enter leaves the cursor in the field, for a code to retype.
                    if enter { response.request_focus(); }
                }
                if sync.e2ee_error.is_some() && Button::new("Try Keychain Again").show(ui, &ink).clicked() {
                    st.e2ee_error = None;
                    sync.reload_encryption(lib);
                }
                reset = Button::new("Reset Encryption…").kind(Kind::Plain).show(ui, &ink).clicked();
            });
        }
        E2eeStatus::Unreadable => {
            text(ui, UNREADABLE);
            ui.horizontal_wrapped(|ui| {
                if Button::new("Try Again").show(ui, &ink).clicked() {
                    st.e2ee_error = None;
                    sync.reload_encryption(lib);
                }
                reset = Button::new("Reset Encryption…").kind(Kind::Plain).show(ui, &ink).clicked();
            });
        }
        E2eeStatus::Unavailable => {
            text(ui, UNAVAILABLE);
            if Button::new("Try Again").show(ui, &ink).clicked() {
                st.e2ee_error = None;
                sync.reload_encryption(lib);
            }
        }
        E2eeStatus::OtherAccount => text(ui, OTHER_ACCOUNT),
        E2eeStatus::On => {
            row(ui, pal, "Encryption is on", "Synced library data is encrypted on this device before upload. The sync service still sees deletions, item counts, sizes and timing.", |ui| {
                let label = if st.e2ee_show_code { "Hide Recovery Code" } else { "Show Recovery Code" };
                if Button::new(label).show(ui, &ink).clicked() {
                    st.e2ee_show_code = !st.e2ee_show_code;
                }
            });
            if st.e2ee_show_code {
                if let Some(code) = sync.recovery_code() { recovery_code(ui, &ink, &code); }
                text(ui, "The recovery code can't be changed except by resetting encryption.");
            }
            row(ui, pal, "Reset encryption", "Use a new key and recovery code if this one was lost or exposed.", |ui| {
                reset = Button::new("Reset Encryption…").show(ui, &ink).clicked();
            });
        }
        E2eeStatus::Resetting => {
            text(ui, RESET_WARNING);
            if let Some(code) = sync.recovery_code() { recovery_code(ui, &ink, &code); }
            ui.checkbox(&mut st.e2ee_saved, "I saved this recovery code");
            // The new marker must land above the account's newest one, so
            // the reset waits for a scan to the end of the stream.
            let ready = sync.reset_ready();
            // Kept in place once the scan ends, as above.
            checking_line(ui, &ink, CHECKING_RESET, !ready);
            ui.horizontal_wrapped(|ui| {
                if Button::new("Reset Encryption").kind(Kind::Primary).enabled(st.e2ee_saved && ready).show(ui, &ink).clicked() {
                    st.e2ee_error = sync.confirm_reset_encryption(lib).err();
                    if st.e2ee_error.is_none() { st.e2ee_saved = false; }
                }
                if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() {
                    st.e2ee_saved = false;
                    st.e2ee_error = None;
                    sync.cancel_reset_encryption();
                }
            });
        }
    }
    if reset {
        st.e2ee_saved = false;
        st.e2ee_show_code = false;
        st.e2ee_code.zeroize();
        st.e2ee_error = sync.begin_reset_encryption(lib).err();
    }
    // Counts stay, across restarts, until dismissed.
    if let Some(notice) = sync.encryption_notice().map(str::to_string) {
        let mut dismiss = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(style::text(&notice, 12.5, style::weight::REGULAR).color(ink.secondary));
            dismiss = Button::new("Dismiss").kind(Kind::Plain).show(ui, &ink).clicked();
        });
        if dismiss { st.e2ee_error = sync.dismiss_encryption_notice(lib).err(); }
    }
    if let Some(error) = st.e2ee_error.as_deref().or(sync.unlock_error.as_deref()).or(sync.e2ee_error.as_deref()) {
        ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
    }
}

const CHECKING_LOCKED: &str = "Checking the server for this account's newest recovery code…";
/// A checking line, drawn only while `shown`, but always taking its space,
/// so the buttons below it stay put as it comes and goes.
fn checking_line(ui: &mut Ui, ink: &Ink, message: &str, shown: bool) -> egui::Response {
    ui.add_visible(shown, egui::Label::new(style::text(message, 12.5, style::weight::REGULAR).color(ink.text)))
}

const CHECKING_CONFIRM: &str = "Checking your other devices for changes…";
const CHECKING_RESET: &str = "Checking the server for newer encryption changes…";

/// Trim typed, pasted, composed and assistive (AccessKit) text so the field
/// never grows past `cap` bytes, and its reserved buffer is never
/// reallocated. A screen reader's SetValue replaces the whole value, focused
/// or not; everything else arrives only while focused. Events are counted in
/// the order the field applies them: selected text is replaced by what comes
/// in, so it counts as room, but only until something may have moved or
/// collapsed the selection.
fn cap_typed_text(ui: &Ui, id: Id, text: &str, cap: usize) {
    use egui::accesskit::{Action, ActionData, ActionRequest};
    use egui::{Event, ImeEvent, Key, Modifiers};
    let focused = ui.memory(|m| m.has_focus(id));
    let selected = egui::text_edit::TextEditState::load(ui.ctx(), id).and_then(|s| s.cursor.char_range()).filter(|_| focused).map_or(0, |range| {
        let range = range.as_sorted_char_range();
        let byte = |at: usize| text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
        byte(range.end.0) - byte(range.start.0)
    });
    let fitting = |text: &str, room: usize| {
        let mut end = room.min(text.len());
        while !text.is_char_boundary(end) { end -= 1; }
        end
    };
    // A key that only types; its text arrives as an event of its own.
    let typing = |key: &Key, modifiers: &Modifiers| {
        (modifiers.is_none() || *modifiers == Modifiers::SHIFT) && (key.name().len() == 1 || matches!(key, Key::Space | Key::Minus))
    };
    ui.input_mut(|input| {
        // The field applies the last SetValue first and puts the cursor at its end.
        let mut set = None;
        for event in &mut input.events {
            if let Event::AccessKitActionRequest(ActionRequest { action: Action::SetValue, target_node, data: Some(ActionData::Value(value)), .. }) = event {
                if *target_node != id.accesskit_id() { continue; }
                let end = fitting(value, cap);
                if end < value.len() { *value = value[..end].into(); }
                set = Some(end);
            }
        }
        // The field holds at most `len` bytes, at least `selected` of them selected.
        let (mut len, mut selected) = set.map_or((text.len(), selected), |len| (len, 0));
        for event in input.events.iter_mut().filter(|_| focused) {
            match event {
                Event::Text(text) | Event::Paste(text) | Event::Ime(ImeEvent::Commit(text)) | Event::Ime(ImeEvent::Preedit { text, .. }) => {
                    let end = fitting(text, cap.saturating_sub(len - selected));
                    text.truncate(end);
                    (len, selected) = (len - selected + end, 0);
                }
                Event::AccessKitActionRequest(ActionRequest { action: Action::ReplaceSelectedText, target_node, data: Some(ActionData::Value(value)), .. }) if *target_node == id.accesskit_id() => {
                    let end = fitting(value, cap.saturating_sub(len - selected));
                    if end < value.len() { *value = value[..end].into(); }
                    (len, selected) = (len - selected + end, 0);
                }
                // Undo and redo may bring back a full field.
                Event::Key { key: Key::Z | Key::Y, pressed: true, modifiers, .. } if modifiers.command || modifiers.ctrl => (len, selected) = (cap, 0),
                // Moving, selecting, deleting: what comes in next may replace nothing.
                Event::Key { key, pressed: true, modifiers, .. } if !typing(key, modifiers) => selected = 0,
                Event::AccessKitActionRequest(ActionRequest { action: Action::SetTextSelection, target_node, .. }) if *target_node == id.accesskit_id() => selected = 0,
                // The field applies clicks after these events; counted anyway.
                Event::Cut | Event::PointerButton { pressed: true, .. } => selected = 0,
                _ => {}
            }
        }
        // A middle click pastes where it points, after all of that, focused or not.
        for event in &mut input.events {
            if let Event::MiddleClickPaste { text, .. } = event {
                let end = fitting(text, cap.saturating_sub(len));
                text.truncate(end);
                len += end;
            }
        }
    });
}

/// Selectable, in two lines of whole groups so it fits the compact sheet. The
/// formatted copy is wiped like the code; egui still keeps its own copy.
fn recovery_code(ui: &mut Ui, ink: &Ink, code: &RecoveryCode) {
    let mut text = Zeroizing::new(String::with_capacity(code.as_str().len()));
    for (index, group) in code.as_str().split('-').enumerate() {
        if index > 0 { text.push(if index == 8 { '\n' } else { '-' }); }
        text.push_str(group);
    }
    ui.add(egui::Label::new(RichText::new(text.as_str()).monospace().size(14.0).color(ink.text)).selectable(true));
}

fn short(s: &str) -> String {
    let s: String = s.chars().take(60).collect();
    s
}

fn platform_name(p: &str) -> &str {
    match p {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

fn ago(ms: u64) -> String {
    let secs = crossbow_core::now_ms().saturating_sub(ms) / 1000;
    match secs {
        0..=9 => "just now".into(),
        10..=59 => format!("{secs} s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

fn advanced_preferences(st: &mut SettingsState, lib: &mut Library, ui: &mut Ui, pal: &Palette) {
    preference_recovery(st, lib, ui, pal);
    section(ui, pal, "AI");
    row(ui, pal, "AI Providers", "Choose a local server for Ask AI and for opt-in browser features such as Ask on Page.", |ui| {
        if Button::new("Configure Providers").show(ui, &Ink::new(pal.panel, pal.accent)).clicked() {
            st.page = Page::AiProviders;
        }
    });
}

fn clear_browsing_data_controls(st: &mut SettingsState, profile: &str, profile_name: Option<&str>, operation: Option<(crossbow_engine::ClearDataStatus, std::time::Duration)>, ui: &mut Ui, pal: &Palette) -> bool {
    use crossbow_engine::ClearDataStep;
    let ink = Ink::new(pal.panel, pal.accent);
    let mut requested = false;
    if st.clear_confirmation.as_deref().is_some_and(|target| target != profile) {
        st.clear_confirmation = None;
        st.clear_reveal = false;
    }
    let pending = operation.as_ref().is_some_and(|(status, _)| status.pending());
    row(ui, pal, "Clear cookies and cache", "Only this profile's cookies and HTTP cache. History and other site storage are kept.", |ui| {
        if ui.add_enabled(!pending && profile_name.is_some(), egui::Button::new(if pending { "Clearing…" } else { "Clear…" })).clicked() {
            st.clear_confirmation = Some(profile.into());
            st.clear_reveal = true;
        }
    });
    if st.clear_confirmation.as_deref() == Some(profile) {
        if let Some(name) = profile_name {
            let mut clear = false;
            let mut cancel = false;
            let confirmation = ui.vertical(|ui| {
                ui.label(style::text(format!("Clear cookies and HTTP cache for “{name}”? Sites may sign you out. This cannot be undone; open pages may create new data."), 12.5, style::weight::REGULAR).color(ink.text));
                ui.horizontal_wrapped(|ui| {
                    clear = Button::new("Clear data").kind(Kind::Primary).enabled(!pending).show(ui, &ink).clicked();
                    cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked();
                });
            });
            if st.clear_reveal {
                confirmation.response.scroll_to_me(Some(egui::Align::Center));
                st.clear_reveal = false;
            }
            if cancel { st.clear_confirmation = None; }
            else if clear {
                requested = true;
                st.clear_confirmation = None;
                st.clear_reveal = true;
                ui.ctx().request_repaint();
            }
        } else {
            st.clear_confirmation = None;
        }
    }
    if let Some((status, elapsed)) = operation {
        for (label, step) in [("Cookies", &status.cookies), ("HTTP cache", &status.cache)] {
            let (message, color) = match step {
                ClearDataStep::Pending => (format!("{label}: waiting for Chromium."), ink.secondary),
                ClearDataStep::Completed => (format!("{label}: Chromium completed the clearing request."), ink.secondary),
                ClearDataStep::Failed(error) | ClearDataStep::Unconfirmed(error) => (format!("{label}: {error}"), ink.danger),
            };
            let response = ui.label(style::text(message, 12.0, style::weight::REGULAR).color(color));
            if st.clear_reveal {
                response.scroll_to_me(Some(egui::Align::Center));
                st.clear_reveal = false;
            }
        }
        if status.pending() {
            let message = if elapsed.as_secs() >= 30 {
                "Completion is still unconfirmed. A request may still be running; do not start another. Closing Settings does not cancel it."
            } else {
                "Clearing is in progress. Closing Settings does not cancel it."
            };
            ui.label(style::text(message, 12.0, style::weight::REGULAR).color(ink.secondary));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        } else if !status.completed() {
            ui.label(style::text("Clearing was not fully confirmed. Check the errors above before retrying with Clear…. Other profiles were not targeted.", 12.0, style::weight::REGULAR).color(ink.danger));
        }
    }
    requested
}

fn advanced(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    advanced_preferences(app.ui.settings.as_mut().unwrap(), &mut app.lib, ui, pal);
    feel_page::show(&mut app.feel.prefs, &app.lib.store, ui, pal);
    section(ui, pal, "Privacy");
    row(ui, pal, "Clear history", "Removes browsing history for this space's profile, here and in this Mac's backups.", |ui| {
        if ui.button("Clear").clicked() {
            let message = match app.lib.clear_history(&app.current_profile()) {
                Ok(removed) => match crate::backups::removed_note(removed) {
                    Some(note) => format!("History cleared for this profile. {note}"),
                    None => "History cleared for this profile".into(),
                },
                Err(_) => "Could not clear history. Try again.".into(),
            };
            app.actions.push(Action::Toast(message));
        }
    });
    let profile = app.lib.spaces.get(&app.active_space).map(|space| space.profile_id.as_str()).unwrap_or("");
    let profile_name = app.lib.profiles.get(profile).map(|profile| profile.name.as_str());
    let status = app.browsing_data.status(profile);
    if clear_browsing_data_controls(app.ui.settings.as_mut().unwrap(), profile, profile_name, status, ui, pal) {
        app.browsing_data.start(&app.data_dir, profile);
    }
    section(ui, pal, "Data");
    let dir = app.data_dir.display().to_string();
    row(ui, pal, "Data folder", &dir, |ui| {
        if ui.button("Show").clicked() {
            crate::ui::overlays::reveal(&app.data_dir.join("crossbow.sqlite").display().to_string());
        }
    });
    row(ui, pal, "Welcome tour", "Run the first-launch setup again.", |ui| {
        if ui.button("Start").clicked() {
            app.ui.settings = None;
            app.actions.push(Action::StartOnboarding);
        }
    });
    let Some(st) = app.ui.settings.as_mut() else { return };
    match backup_page::show(&mut st.restore, &app.lib, app.backups.restore_error.as_deref(), ui, pal) {
        Some(backup_page::Request::Quit) => {
            // The user chose Restore and Quit: quit without asking again.
            app.quit.borrow_mut().allow_next();
            crate::quit::terminate(ui.ctx());
        }
        Some(backup_page::Request::Reveal(path)) => crate::ui::overlays::reveal(&path.display().to_string()),
        None => {}
    }
}

fn about(ui: &mut Ui, pal: &Palette) {
    ui.label(RichText::new("Crossbow").size(24.0).strong());
    ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(pal.panel_dim));
    ui.add_space(8.0);
    ui.label(format!("Engine: Chromium Embedded Framework {}", crossbow_engine::engine_version()));
    ui.label(format!("Platform: {}", crossbow_core::platform()));
    ui.add_space(8.0);
    ui.label(RichText::new("A Rust browser in the spirit of Arc: spaces, a sidebar of pinned tabs, a command bar, split view, Peek, Boosts and account sync.").color(pal.panel_dim));
}

#[cfg(test)]
mod icon_page_tests {
    use super::*;
    use crate::app_icon::{AppIcon, IconChoice};

    struct ErrorFrame {
        offset: f32,
        viewport: Rect,
        message: Option<(Rect, Rect)>,
        apply: Option<(Rect, Rect)>,
    }

    fn error_frame(ctx: &egui::Context, icon: &mut AppIcon, size: egui::Vec2, frame: &mut u32, wheel: f32) -> ErrorFrame {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
        let rect = sheet_rect(screen, 1.0);
        let mut snapshot = ErrorFrame { offset: 0.0, viewport: Rect::NOTHING, message: None, apply: None };
        let mut events = vec![egui::Event::PointerMoved(rect.center())];
        if wheel != 0.0 {
            events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, wheel),
                phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown,
                modifiers: Default::default(),
            });
        }
        let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(*frame) / 60.0), events, ..Default::default() }, |ui| {
            show_backdrop(ui.ctx(), screen, 1.0);
            show_sheet(ui.ctx(), rect, |ui| {
                set_visuals(ui, &pal);
                let body = if rect.width() < 640.0 {
                    Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0))
                } else {
                    Rect::from_min_max(rect.min + vec2(224.0, 18.0), rect.max - vec2(28.0, 16.0))
                };
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                child.label(RichText::new("Appearance").size(20.0).strong().color(pal.panel_text));
                child.add_space(10.0);
                let scroll = egui::ScrollArea::vertical().id_salt(("settings-scroll", Page::Appearance as u8)).auto_shrink([false, false]).show(&mut child, |ui| {
                    ui.set_width(body.width() - 8.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    appearance(icon, ui, &pal);
                    ui.add_space(16.0);
                });
                snapshot.offset = scroll.state.clamped_offset().y;
                snapshot.viewport = scroll.inner_rect;
            });
        });
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                let bounds = Rect::from_min_size(text.pos, text.galley.size());
                let clip = shape.clip_rect.intersect(snapshot.viewport);
                if icon.message.as_deref() == Some(text.galley.job.text.as_str()) { snapshot.message = Some((bounds, clip)); }
                if text.galley.job.text == "Apply" { snapshot.apply = Some((bounds, clip)); }
            }
        }
        output.textures_delta.clear();
        *frame += 1;
        snapshot
    }

    fn fully_visible(bounds: Option<(Rect, Rect)>) -> bool {
        bounds.is_some_and(|(rect, clip)| clip.expand(0.5).contains_rect(rect))
    }

    fn error_reveal_journey(size: egui::Vec2, cold: bool) {
        let dir = tempfile::tempdir().unwrap();
        let mut icon = crate::app_icon::tests::saved_icon_with_draft(dir.path());
        let before = std::fs::read(dir.path().join("appearance/app-icon.bin")).unwrap();
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let mut frame = 0;
        if !cold {
            for _ in 0..8 {
                let settled = error_frame(&ctx, &mut icon, size, &mut frame, 0.0);
                assert_eq!(settled.offset, 0.0, "ordinary appearance must not scroll");
            }
        }
        for save_failure in [false, true] {
            let mut previous_message = None;
            for attempt in 1..=2 {
                crate::app_icon::tests::receive_fixture_error(&mut icon, &ctx, save_failure);
                if let Some(previous) = &previous_message { assert_eq!(icon.message.as_ref(), Some(previous), "retry must exercise identical error text"); }
                previous_message = icon.message.clone();
                for _ in 0..4 { error_frame(&ctx, &mut icon, size, &mut frame, 0.0); }
                let shown = error_frame(&ctx, &mut icon, size, &mut frame, 0.0);
                println!("error reveal size={size:?} cold={cold} save={save_failure} attempt={attempt} offset={} message={:?}", shown.offset, shown.message);
                assert!(fully_visible(shown.message), "new actionable error is clipped: size={size:?}, save={save_failure}, attempt={attempt}, offset={}", shown.offset);
                assert!(fully_visible(shown.apply), "recovery actions must be visible with the error");
                let shown_offset = shown.offset;
                error_frame(&ctx, &mut icon, size, &mut frame, 720.0);
                for _ in 0..40 { error_frame(&ctx, &mut icon, size, &mut frame, 0.0); }
                let away = error_frame(&ctx, &mut icon, size, &mut frame, 0.0);
                if size.y == 420.0 {
                    assert!(away.offset < shown_offset - 30.0, "wheel must let the user leave the error");
                    assert!(!fully_visible(away.message), "test must actually scroll the error out of view");
                } else {
                    assert_eq!(shown_offset, 0.0, "already visible errors must not move normal Settings");
                }
                for _ in 0..80 {
                    let settled = error_frame(&ctx, &mut icon, size, &mut frame, 0.0);
                    assert!((settled.offset - away.offset).abs() < 0.1, "settled error must not pull scrolling back");
                }
                assert_eq!(icon.current_name(), "Black");
                assert_eq!(icon.choice(), IconChoice::Pearl);
                assert_eq!(std::fs::read(dir.path().join("appearance/app-icon.bin")).unwrap(), before);
            }
        }
    }

    #[test]
    fn compact_sheet_reveals_new_and_repeated_errors_without_scroll_lock() {
        error_reveal_journey(vec2(640.0, 420.0), false);
    }

    #[test]
    fn normal_sheet_reveals_new_and_repeated_errors_without_jumping() {
        error_reveal_journey(vec2(1360.0, 860.0), false);
    }

    #[test]
    fn cold_sheet_reveals_pending_errors_after_the_sizing_pass() {
        error_reveal_journey(vec2(640.0, 420.0), true);
    }

    #[test]
    fn icon_content_fits_normal_and_compact_settings_columns() {
        for (width, dark) in [(560.0, false), (544.0, false), (560.0, true), (544.0, true)] {
            let dir = tempfile::tempdir().unwrap();
            let mut icon = AppIcon::new(dir.path());
            icon.choose_builtin(IconChoice::Pearl);
            icon.message = Some("Could not open this image. Choose a local PNG file up to 5 MiB that you have permission to read.".into());
            icon.has_error = true;
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), dark);
            for _ in 0..2 {
                ctx.run_ui(egui::RawInput::default(), |ui| {
                    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 900.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    child.set_width(width);
                    set_visuals(&mut child, &pal);
                    appearance(&mut icon, &mut child, &pal);
                    assert!(child.min_rect().width() <= width + 0.5, "icon content overflows {width}px column");
                }).textures_delta.clear();
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sheet_icon_click_and_focused_wheel_stay_in_the_sheet() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let dir = tempfile::tempdir().unwrap();
            let mut icon = AppIcon::new(dir.path());
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
            let rect = sheet_rect(screen, 1.0);
            let mut point = None;
            let mut offset_before_wheel = 0.0;
            let mut offset = 0.0;
            let mut focused_tile = false;
            for frame in 0..160 {
                let mut events = Vec::new();
                if (10..50).contains(&frame) && frame % 2 == 0 && !focused_tile {
                    events.push(egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
                    events.push(egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
                }
                if let Some(pos) = point {
                    events.push(egui::Event::PointerMoved(pos));
                    if frame == 4 || frame == 5 {
                        events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: egui::Modifiers::NONE });
                    }
                    if frame == 60 {
                        offset_before_wheel = offset;
                        events.push(egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -180.0),
                            phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown,
                            modifiers: Default::default(),
                        });
                    }
                }
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                    assert!(!ui.interact(screen, Id::unique("empty-content"), Sense::click()).clicked(), "Settings input must not activate the browser underneath");
                    assert!(!show_backdrop(ui.ctx(), screen, 1.0).clicked(), "an icon click must not dismiss Settings");
                    show_sheet(ui.ctx(), rect, |ui| {
                        set_visuals(ui, &pal);
                        let body = if rect.width() < 640.0 {
                            Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0))
                        } else {
                            Rect::from_min_max(rect.min + vec2(224.0, 18.0), rect.max - vec2(28.0, 16.0))
                        };
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                        child.label(RichText::new("Appearance").size(20.0));
                        child.add_space(10.0);
                        let scroll = egui::ScrollArea::vertical().id_salt("icon-sheet-test").auto_shrink([false, false]).show(&mut child, |ui| {
                            ui.set_width(body.width() - 8.0);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            appearance(&mut icon, ui, &pal);
                            ui.add_space(16.0);
                        });
                        offset = scroll.state.clamped_offset().y;
                    });
                });
                if frame == 3 {
                    point = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "Graphite" => Some(text.pos + text.galley.size() * 0.5),
                        _ => None,
                    });
                    assert!(point.is_some(), "Graphite must be visible in the sheet");
                }
                if frame >= 10 && !focused_tile {
                    focused_tile = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)).is_some_and(|r| r.rect.size() == vec2(88.0, 98.0) && point.is_some_and(|p| r.rect.contains(p)));
                }
                output.textures_delta.clear();
            }
            assert!(focused_tile, "the wheel regression must exercise a keyboard-focused icon tile");
            assert_eq!(icon.choice(), IconChoice::Graphite);
            assert_eq!(icon.current_name(), "Black");
            assert!(!dir.path().join("appearance/app-icon.bin").exists());
            if size.y == 420.0 {
                assert!(offset > offset_before_wheel + 35.0, "keyboard focus must not pull wheel scrolling back: before={offset_before_wheel}, after={offset}");
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clicking_built_in_tile_only_changes_preview_until_apply() {
        let dir = tempfile::tempdir().unwrap();
        let mut icon = AppIcon::new(dir.path());
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let mut point = None;
        for frame in 0..4 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 420.0))),
                time: Some(frame as f64 / 60.0), ..Default::default()
            };
            if frame >= 2 {
                let pos = point.expect("Graphite label is visible");
                input.events.push(egui::Event::PointerMoved(pos));
                input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: frame == 2, modifiers: egui::Modifiers::NONE });
            }
            let mut output = ctx.run_ui(input, |ui| {
                ui.set_width(544.0);
                set_visuals(ui, &pal);
                appearance(&mut icon, ui, &pal);
            });
            if frame == 1 {
                point = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == "Graphite" => Some(text.pos + text.galley.size() * 0.5),
                    _ => None,
                });
            }
            output.textures_delta.clear();
        }
        assert_eq!(icon.choice(), IconChoice::Graphite);
        assert_eq!(icon.current_name(), "Black");
        assert!(icon.changed());
        assert!(!dir.path().join("appearance/app-icon.bin").exists());
        icon.cancel_changes();
        assert_eq!(icon.choice(), IconChoice::Black);
    }
}

#[cfg(test)]
mod material_tests {
    use super::*;
    use crate::native_window::{AccessibilityDisplay, MaterialState, OpaqueReason};
    use crate::window_appearance::{ChromeSettings, WindowChrome};
    use crossbow_core::store::Store;
    use egui::Pos2;

    fn opaque(reason: OpaqueReason) -> MaterialState {
        MaterialState { transparent: false, opaque_reason: Some(reason), accessibility: AccessibilityDisplay::default() }
    }

    #[test]
    fn material_controls_fit_both_settings_columns_including_recovery_copy() {
        for (width, dark) in [(560.0, false), (544.0, false), (560.0, true), (544.0, true)] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("test.sqlite")).unwrap();
            let mut chrome = WindowChrome::load(&store);
            chrome.draft = ChromeSettings { transparent: true, tint: 35 };
            chrome.error = Some("Could not save window appearance. The window is unchanged and your draft is kept. Check disk space and data-folder permissions, then Retry or Cancel.".into());
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), dark);
            for _ in 0..3 {
                ctx.run_ui(egui::RawInput::default(), |ui| {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))));
                    child.set_width(width);
                    set_visuals(&mut child, &pal);
                    window_material(&mut chrome, &store, opaque(OpaqueReason::ReduceTransparency), &mut child, &pal);
                    assert!(child.min_rect().width() <= width + 0.5, "material controls overflow {width}px column");
                }).textures_delta.clear();
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keyboard_toggle_only_stages_then_pointer_apply_persists() {
        for width in [544.0, 560.0] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("test.sqlite")).unwrap();
            let mut chrome = WindowChrome::load(&store);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let mut apply_point = None;
            let mut toggled = false;
            for frame in 0..10 {
                let mut events = vec![];
                if frame == 2 || frame == 3 {
                    let key = if frame == 2 { egui::Key::Tab } else { egui::Key::Space };
                    for pressed in [true, false] {
                        events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() });
                    }
                }
                if frame == 6 || frame == 7 {
                    let pos = apply_point.expect("Apply must render after staging the toggle");
                    events.push(egui::Event::PointerMoved(pos));
                    events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: frame == 6, modifiers: Default::default() });
                }
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width + 32.0, 420.0))), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                    ui.set_width(width);
                    set_visuals(ui, &pal);
                    window_material(&mut chrome, &store, opaque(OpaqueReason::Disabled), ui, &pal);
                });
                if frame == 4 {
                    toggled = chrome.draft.transparent;
                    assert!(!chrome.saved().transparent, "keyboard toggle must not apply an unsaved draft");
                    assert!(store.kv_get("window_appearance").unwrap().is_none());
                    apply_point = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "Apply" => Some(text.pos + text.galley.size() * 0.5),
                        _ => None,
                    });
                }
                output.textures_delta.clear();
            }
            assert!(toggled, "the first keyboard-focusable control must toggle with Space");
            assert!(chrome.saved().transparent);
            assert!(WindowChrome::load(&store).saved().transparent);
        }
    }

    #[test]
    fn accessibility_fallback_copy_does_not_claim_transparent_rendering() {
        for reason in [OpaqueReason::ReduceTransparency, OpaqueReason::IncreasedContrast, OpaqueReason::InstallationFailed, OpaqueReason::UnsupportedHierarchy] {
            let description = material_description(opaque(reason), true);
            assert!(description.contains("opaque window"));
            assert!(description.contains("saved"));
        }
    }
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    #[test]
    fn review_command_chat_settings_blocks_floating_chrome_and_keeps_sheet_input() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
                let (mut lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
                let other_profile = lib.create_profile("Other synthetic profile").unwrap();
                let other_space = lib.create_space("Other synthetic space", "O", crossbow_core::model::Theme::preset(0), &other_profile).unwrap();
                let mut active_space = space.clone();
                let mut chat = Some(crate::chat::Entry::capture(&lib, &space, &None).unwrap().open(&mut lib, &space, &None).unwrap());
                chat.as_mut().unwrap().draft = "KEEP_SYNTHETIC_DRAFT".into();
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|style| style.animation_time = 0.0);
                let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
                let sheet = sheet_rect(screen, 1.0);
                let tile = Rect::from_min_size(pos2(4.0, 96.0), vec2(54.0, 44.0));
                let control = Rect::from_center_size(sheet.center(), vec2(80.0, 28.0));
                let mut background_clicks = 0;
                let mut dismissals = 0;
                let mut selections = 0;
                let mut paint_order_ok = true;
                // Retained area ordering must not strand the sheet below its backdrop.
                for _ in 0..3 {
                    ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                        show_sheet(ui.ctx(), sheet, |_| {});
                    }).textures_delta.clear();
                }
                for frame in 0..12 {
                    let position = if frame < 7 { pos2(12.0, 118.0) } else { control.center() };
                    let mut events = vec![egui::Event::PointerMoved(position)];
                    if [4, 5, 8, 9].contains(&frame) {
                        events.push(egui::Event::PointerButton { pos: position, button, pressed: frame % 2 == 0, modifiers: Default::default() });
                    }
                    let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                        crate::ui::content::show_floating_sidebar(ui, screen, |ui| {
                            ui.painter().rect_filled(tile, 0, Color32::RED);
                            let response = ui.interact(tile, Id::unique("floating-space-probe"), Sense::click_and_drag());
                            if response.clicked() || response.secondary_clicked() {
                                background_clicks += 1;
                                active_space.clone_from(&other_space);
                            }
                        });
                        if show_backdrop(ui.ctx(), screen, 1.0).clicked() { dismissals += 1; }
                        show_sheet(ui.ctx(), sheet, |ui| {
                            ui.painter().rect_filled(control, 0, Color32::BLUE);
                            if ui.interact(control, Id::unique("settings-control-probe"), Sense::click()).clicked() { selections += 1; }
                            crate::chat::reconcile(&mut chat, &lib, &active_space, &None, true);
                        });
                    });
                    if frame > 2 {
                        let painted_at = |color| output.shapes.iter().position(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == color)).expect("painted marker");
                        paint_order_ok &= painted_at(Color32::RED) < painted_at(Color32::BLUE);
                    }
                    output.textures_delta.clear();
                }
                assert_eq!(background_clicks, 0, "neither click nor context menu may activate a covered space");
                assert!(paint_order_ok, "floating chrome must paint below Settings");
                assert_eq!(active_space, space);
                assert_eq!(chat.as_ref().unwrap().draft, "KEEP_SYNTHETIC_DRAFT");
                assert_eq!(dismissals, usize::from(button == egui::PointerButton::Primary));
                assert_eq!(selections, usize::from(button == egui::PointerButton::Primary));
            }
        }
    }

    #[test]
    fn residual_settings_colour_popup_survives_idle_and_accepts_edits() {
        for (size, modal) in [vec2(640.0, 420.0), vec2(1360.0, 860.0)].into_iter().flat_map(|size| [(size, true), (size, false)]) {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|style| style.animation_time = 0.0);
            let textures = crate::ui::theme::TextureBank::new(&ctx);
            let mut theme = crossbow_core::model::Theme::preset(0);
            let original = theme.clone();
            let colour = Color32::from_rgb(theme.colors[0].0, theme.colors[0].1, theme.colors[0].2);
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            let sheet = sheet_rect(screen, 1.0);
            let mut frame = 0;
            let mut dismissals = 0;
            let mut draw = |events| {
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(frame) / 60.0), events, ..Default::default() }, |ui| {
                    let editor = |ui: &mut Ui| {
                        let body = sheet.shrink(20.0);
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                        egui::ScrollArea::vertical().id_salt("theme-popup-test").auto_shrink([false, false]).show(&mut child, |ui| {
                            if modal {
                                theme_editor::show_with_popup_parent(ui, &mut theme, false, &textures, Some(backdrop_layer()));
                            } else {
                                theme_editor::show(ui, &mut theme, false, &textures);
                            }
                        });
                    };
                    if modal {
                        if show_backdrop(ui.ctx(), screen, 1.0).clicked() { dismissals += 1; }
                        show_sheet(ui.ctx(), sheet, editor);
                    } else {
                        egui::Area::new(Id::unique("theme-standalone")).order(egui::Order::Foreground).fixed_pos(sheet.min).show(ui.ctx(), editor);
                    }
                });
                let swatch = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.fill == colour && (rect.rect.width() - 28.0).abs() < 2.0 && (rect.rect.height() - 24.0).abs() < 2.0 => Some(rect.rect.center()),
                    _ => None,
                });
                let plane = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if (rect.rect.width() - 275.0).abs() < 1.0 && (rect.rect.height() - 275.0).abs() < 1.0 => {
                        let visible = rect.rect.intersect(shape.clip_rect).intersect(screen);
                        visible.is_positive().then(|| visible.min + visible.size() * 0.7)
                    }
                    _ => None,
                });
                output.textures_delta.clear();
                frame += 1;
                (swatch, plane, theme.clone(), dismissals)
            };
            for _ in 0..6 { draw(vec![]); }
            let swatch = draw(vec![]).0.expect("actual colour-stop button must be rendered");
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(swatch), egui::Event::PointerButton { pos: swatch, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            for _ in 0..8 { draw(vec![]); }
            assert!(egui::Popup::is_any_open(&ctx));
            let plane = draw(vec![]).1.expect("the actual colour picker must render its saturation/value plane");
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(plane), egui::Event::PointerButton { pos: plane, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            let changed = draw(vec![]);
            assert_ne!(changed.2.colors[0], original.colors[0], "the settled Settings colour popup must accept edits at {size:?}");
            assert_eq!(changed.2.colors[1..], original.colors[1..]);
            assert_eq!(changed.3, 0, "picker input must not dismiss the Settings backdrop");
            assert!(egui::Popup::is_any_open(&ctx), "editing must keep the colour picker open");
            for pressed in [true, false] {
                draw(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]);
            }
            assert!(!egui::Popup::is_any_open(&ctx));
            assert_eq!(draw(vec![]).2, changed.2, "dismissal must preserve the edited colour");
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(swatch), egui::Event::PointerButton { pos: swatch, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            for _ in 0..8 { draw(vec![]); }
            assert!(egui::Popup::is_any_open(&ctx), "the editor must reopen after dismissal");
            assert_eq!(draw(vec![]).2, changed.2);
            let outside = pos2(2.0, 2.0);
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(outside), egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            draw(vec![]);
            assert!(!egui::Popup::is_any_open(&ctx), "outside click must still close the colour popup");
        }
    }

    #[test]
    fn settings_over_peek_owns_backdrop_and_preserves_the_covered_surface() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0] {
                for over_page in [false, true] {
                    let ctx = egui::Context::default();
                    let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
                    let page = Rect::from_center_size(screen.center() + vec2(0.0, 12.0), (size * vec2(0.78, 0.84)).round());
                    let sheet = sheet_rect(screen, 1.0);
                    let outside = pos2(12.0, screen.center().y);
                    let exposed_page = pos2(page.left() + 10.0, page.center().y);
                    let pointer = if over_page && !sheet.contains(exposed_page) { exposed_page } else { outside };
                    let mut settings_open = false;
                    let mut settings_dismissals = 0;
                    let mut peek_dismissals = 0;
                    let mut covered_page_events = 0;
                    for frame in 0..18 {
                        if frame == 4 { settings_open = true; }
                        let pointer = if frame >= 13 { outside } else { pointer };
                        let mut events = vec![egui::Event::PointerMoved(pointer)];
                        if [8, 9, 14, 15].contains(&frame) {
                            events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: frame == 8 || frame == 14, modifiers: Default::default() });
                        }
                        let mut input = egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() };
                        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                        ctx.run_ui(input, |ui| {
                            let dismissed = crate::ui::overlays::show_peek_surface(ui.ctx(), 1.0, !settings_open, |ui| {
                                let response = ui.interact(page, Id::unique("peek-input"), Sense::click_and_drag());
                                if settings_open {
                                    assert!(!response.enabled());
                                    covered_page_events += crate::ui::content::WebInput { page: &response, popup: None }.events(ui.ctx(), false).len();
                                }
                            });
                            peek_dismissals += usize::from(dismissed);
                            if frame < 14 { assert_eq!(peek_dismissals, 0, "the covered Peek must survive Settings dismissal"); }
                            if settings_open {
                                let clicked = show_backdrop(ui.ctx(), screen, 1.0).clicked();
                                show_sheet(ui.ctx(), sheet, |_| {});
                                if clicked {
                                    settings_dismissals += 1;
                                    settings_open = false;
                                }
                            }
                        }).textures_delta.clear();
                    }
                    assert_eq!(settings_dismissals, 1, "Settings, not Peek, owns the exposed backdrop; size={size:?}, scale={scale}, over_page={over_page}");
                    assert_eq!(covered_page_events, 0);
                    assert!(!settings_open);
                    assert_eq!(peek_dismissals, 1, "Peek can dismiss normally after Settings closes");
                }
            }
        }
    }

    #[test]
    fn provider_navigation_fits_short_and_narrow_settings_sheets() {
        assert!(Page::ALL.contains(&Page::AiProviders));
        for screen in [vec2(640.0, 420.0), vec2(1360.0, 420.0), vec2(480.0, 860.0)] {
            assert!(compact_navigation_needed(sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), screen), 1.0).size()));
        }
        let normal = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(1360.0, 860.0)), 1.0);
        assert!(!compact_navigation_needed(normal.size()));
        assert!(56.0 + (Page::ALL.len() - 1) as f32 * 32.0 + 28.0 < normal.height() - 16.0);
    }

    #[test]
    fn compact_navigation_pointer_opens_menu() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 420.0));
        let rect = sheet_rect(screen, 1.0);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let mut page = Page::General;
        let mut button = Rect::NOTHING;
        let mut menu_open = false;
        for frame in 0..12 {
            let pointer = if button.is_positive() { button.center() } else { rect.center() };
            let mut events = vec![egui::Event::PointerMoved(pointer)];
            if frame == 5 || frame == 6 {
                events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: frame == 5, modifiers: Default::default() });
            }
            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                assert!(!show_backdrop(ui.ctx(), screen, 1.0).clicked());
                show_sheet(ui.ctx(), rect, |ui| {
                    set_visuals(ui, &pal);
                    let response = compact_navigation(ui, rect, &mut page);
                    button = response.response.rect;
                    menu_open = response.inner.is_some();
                    let body = Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                    child.label("General");
                    egui::ScrollArea::vertical().id_salt("compact-navigation-body").auto_shrink([false, false]).show(&mut child, |ui| {
                        for row in 0..30 { ui.label(format!("Synthetic row {row}")); }
                    });
                });
            }).textures_delta.clear();
        }
        assert!(menu_open, "the actual compact Settings selector must open on a pointer click");
        assert_eq!(page, Page::General);
    }

    #[test]
    fn compact_navigation_selects_ai_pages_and_escape_keeps_the_page() {
        for (size, target) in [
            (vec2(640.0, 420.0), Page::AiProviders), (vec2(1360.0, 420.0), Page::AiProviders),
            (vec2(640.0, 420.0), Page::AiChat), (vec2(1360.0, 420.0), Page::AiChat),
        ] {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
            let rect = sheet_rect(screen, 1.0);
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let mut page = Page::General;
            let mut frame = 0;
            let mut draw = |events| {
                let mut button = Rect::NOTHING;
                let mut menu_open = false;
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(frame) / 60.0), events, ..Default::default() }, |ui| {
                    show_backdrop(ui.ctx(), screen, 1.0);
                    show_sheet(ui.ctx(), rect, |ui| {
                        set_visuals(ui, &pal);
                        let response = compact_navigation(ui, rect, &mut page);
                        button = response.response.rect;
                        menu_open = response.inner.is_some();
                    });
                });
                let option = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == target.title() => {
                        let bounds = Rect::from_min_size(text.pos, text.galley.size());
                        assert!(shape.clip_rect.expand(0.5).contains_rect(bounds));
                        Some(bounds.center())
                    }
                    _ => None,
                });
                output.textures_delta.clear();
                frame += 1;
                (button, menu_open, page, option)
            };
            for _ in 0..8 { draw(vec![]); }
            let button = draw(vec![]).0;
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(button.center()), egui::Event::PointerButton { pos: button.center(), button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            for _ in 0..8 { draw(vec![]); }
            let opened = draw(vec![]);
            assert!(opened.1);
            let pos = opened.3.expect("AI page must be visible in the compact menu");
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert_eq!(draw(vec![]).2, target);
            for pressed in [true, false] {
                draw(vec![egui::Event::PointerMoved(button.center()), egui::Event::PointerButton { pos: button.center(), button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(draw(vec![]).1);
            for pressed in [true, false] {
                draw(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]);
            }
            let closed = draw(vec![]);
            assert!(!closed.1);
            assert_eq!(closed.2, target);
        }
    }

    #[test]
    fn animated_sheet_keeps_layout_dimensions_and_navigation_mode() {
        for size in [vec2(1360.0, 860.0), vec2(640.0, 420.0), vec2(700.0, 600.0)] {
            let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
            let settled = sheet_rect(screen, 1.0);
            for frame in 0..=30 {
                let rect = sheet_rect(screen, frame as f32 / 30.0);
                assert!((rect.width() - settled.width()).abs() < 0.001);
                assert!((rect.height() - settled.height()).abs() < 0.001);
                assert_eq!(rect.width() < 640.0, settled.width() < 640.0);
                assert!(screen.contains_rect(rect));
            }
        }
    }

    #[test]
    fn sheet_receives_wheel_over_manually_positioned_content() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let ctx = egui::Context::default();
            let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
            let rect = sheet_rect(screen, 1.0);
            let pointer = rect.center();
            let mut offset = 0.0;
            let mut area_rect = Rect::NOTHING;
            for frame in 0..30 {
                let mut events = vec![egui::Event::PointerMoved(pointer)];
                if frame == 5 {
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, -180.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: Default::default(),
                    });
                }
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                    egui::Area::new(Id::unique("settings-backdrop")).order(egui::Order::Middle).fixed_pos(screen.min).show(ui.ctx(), |ui| {
                        ui.allocate_rect(screen, Sense::click());
                    });
                    area_rect = show_sheet(ui.ctx(), rect, |ui| {
                        let body = Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                        child.label("General");
                        let scroll = egui::ScrollArea::vertical().id_salt("settings-scroll-test").auto_shrink([false, false]).show(&mut child, |ui| {
                            for n in 0..60 { ui.label(format!("Synthetic row {n}")); }
                        });
                        offset = scroll.state.clamped_offset().y;
                    }).response.rect;
                }).textures_delta.clear();
            }
            assert!(offset > 100.0, "Settings must receive the wheel, not its backdrop: offset={offset}");
            assert_eq!(area_rect, rect, "the hit-test bounds must cover the painted sheet");
        }
    }

    #[test]
    fn animated_sheet_reuses_wrapped_text_layout() {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1360.0, 860.0));
        let mut previous = None;
        for frame in 0..=30 {
            ctx.run_ui(egui::RawInput { time: Some(frame as f64 / 120.0), ..Default::default() }, |ui| {
                let rect = sheet_rect(screen, frame as f32 / 30.0);
                let width = rect.width() - 196.0 - 56.0 - 8.0 - 230.0;
                let galley = ui.ctx().fonts_mut(|fonts| fonts.layout(
                    "Tabs you haven't used in this long move to the archive.".into(),
                    FontId::proportional(12.0), Color32::GRAY, width,
                ));
                if let Some(previous) = &previous {
                    assert!(std::sync::Arc::ptr_eq(previous, &galley), "animation reshaped unchanged text");
                }
                previous = Some(galley);
            }).textures_delta.clear();
        }
    }
}

#[cfg(test)]
mod repair_tests {
    use super::*;
    use crate::test_codes::{Hidden, failure};
    use crossbow_core::store::Store;
    use std::path::PathBuf;

    // Only a uniquely created synthetic database is touched. Set TMPDIR when
    // running this module to keep fixtures in a disposable directory.
    pub(super) struct Fixture {
        pub(super) dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("crossbow-profile-boost-{}", crossbow_core::new_id()));
            std::fs::create_dir(&dir).unwrap();
            let fixture = Self { dir };
            fixture.library().ensure_defaults().unwrap();
            fixture
        }

        fn library(&self) -> Library {
            Library::open(Store::open(&self.dir.join("fixture.sqlite")).unwrap()).unwrap()
        }

        fn read_only(&self) -> Library {
            // URI mode=ro makes real production Store writes fail without
            // chmod, external SQLite tools, mocks or test-only store hooks.
            let path = self.dir.join("fixture.sqlite").to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
            Library::open(Store::open(std::path::Path::new(&format!("file:{path}?mode=ro"))).unwrap()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn failed_preferences_keep_all_edits_and_retry_once_before_reopening() {
        let fixture = Fixture::new();
        let mut lib = fixture.read_only();
        let saved = lib.settings.clone();
        let desired = Settings {
            auto_archive_hours: 0, search_engine: SearchEngine::Kagi, sidebar_on_right: true,
            show_url_in_sidebar: false, smooth_animations: false, peek_external_links: false, ai_features: true,
        };
        let entities = lib.store.all_entities().unwrap();
        let revision = lib.revision;
        let mut state = SettingsState::new(Page::General, 0.0);
        for _ in 0..2 {
            state.save_preferences(&mut lib, desired.clone());
            assert_eq!(state.preferences(&lib), desired);
            assert!(state.pending_preferences.as_ref().unwrap().error.contains("Changes are not applied"));
            assert_eq!(lib.settings, saved);
            assert_eq!(lib.revision, revision);
            assert_eq!(lib.store.all_entities().unwrap(), entities);
            assert_eq!(fixture.library().settings, saved);
        }
        drop(lib);
        let mut writable = fixture.library();
        let revision = writable.revision;
        state.retry_preferences(&mut writable);
        assert!(state.pending_preferences.is_none());
        assert_eq!(writable.settings, desired);
        assert_eq!(writable.revision, revision + 1);
        state.save_preferences(&mut writable, desired.clone());
        assert_eq!(writable.revision, revision + 1, "unchanged preferences must not write again");
        drop(writable);
        assert_eq!(fixture.library().settings, desired);
    }

    #[test]
    fn preference_cancel_and_revert_keep_the_saved_values_without_writes() {
        let fixture = Fixture::new();
        let mut lib = fixture.read_only();
        let saved = lib.settings.clone();
        let entities = lib.store.all_entities().unwrap();
        let mut state = SettingsState::new(Page::General, 0.0);
        for cancel in [true, false] {
            let mut desired = saved.clone();
            desired.search_engine = SearchEngine::DuckDuckGo;
            state.save_preferences(&mut lib, desired);
            assert!(state.pending_preferences.is_some());
            if cancel {
                state.cancel_preferences();
            } else {
                state.save_preferences(&mut lib, saved.clone());
            }
            assert!(state.pending_preferences.is_none());
            assert_eq!(state.preferences(&lib), saved);
            assert_eq!(lib.store.all_entities().unwrap(), entities);
        }
        drop(lib);
        assert_eq!(fixture.library().settings, saved);
    }

    #[test]
    fn preference_retry_merges_only_edited_fields_with_newer_saved_values() {
        let fixture = Fixture::new();
        let mut lib = fixture.read_only();
        let mut state = SettingsState::new(Page::General, 0.0);
        let mut desired = lib.settings.clone();
        desired.search_engine = SearchEngine::Kagi;
        state.save_preferences(&mut lib, desired);
        drop(lib);

        let mut writable = fixture.library();
        let mut newer = writable.settings.clone();
        newer.sidebar_on_right = true;
        newer.auto_archive_hours = 24;
        newer.ai_features = true;
        writable.set_settings(newer.clone()).unwrap();
        state.page = Page::Advanced;
        let mut retry = state.preferences(&writable);
        assert!(retry.ai_features && retry.sidebar_on_right);
        assert_eq!(retry.auto_archive_hours, 24);
        assert_eq!(retry.search_engine, SearchEngine::Kagi);
        retry.ai_features = false;
        state.save_preferences(&mut writable, retry.clone());
        assert!(state.pending_preferences.is_none());
        drop(writable);
        assert_eq!(fixture.library().settings, retry);
    }

    #[test]
    fn failed_preference_entity_write_preserves_live_values_and_allows_retry() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let saved = lib.settings.clone();
        let revision = lib.revision;
        let conn = rusqlite::Connection::open(fixture.dir.join("fixture.sqlite")).unwrap();
        conn.execute_batch("CREATE TRIGGER block_fixture_preferences BEFORE INSERT ON entities WHEN NEW.kind = 'settings' BEGIN SELECT RAISE(ABORT, 'fixture preferences blocked'); END;").unwrap();
        let mut state = SettingsState::new(Page::Advanced, 0.0);
        let mut desired = saved.clone();
        desired.ai_features = true;
        state.save_preferences(&mut lib, desired.clone());
        assert_eq!(lib.settings, saved);
        assert_eq!(lib.revision, revision);
        assert_eq!(fixture.library().settings, saved);
        assert_eq!(state.preferences(&lib), desired);
        assert!(state.pending_preferences.as_ref().unwrap().error.contains("fixture preferences blocked"));
        conn.execute_batch("DROP TRIGGER block_fixture_preferences;").unwrap();
        state.retry_preferences(&mut lib);
        assert!(state.pending_preferences.is_none());
        drop(lib);
        assert_eq!(fixture.library().settings, desired);
    }

    #[test]
    fn profile_preference_retry_keeps_original_owner_and_merges_only_edits() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let a = lib.default_profile_id();
        let b = lib.create_profile("Profile B").unwrap();
        let shared = lib.settings.clone();
        drop(lib);
        let mut lib = fixture.read_only();
        let mut state = SettingsState::new(Page::General, 0.0);
        let draft = ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None };
        state.save_profile_preferences(&mut lib, &a, draft);
        assert_eq!(lib.profiles[&a].preferences, None);
        assert!(state.pending_profile_preferences.is_some());
        drop(lib);
        let mut lib = fixture.library();
        lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(24) })).unwrap();
        let other = ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: Some(0) };
        lib.set_profile_preferences(&b, Some(other)).unwrap();
        assert_eq!(state.profile_preferences(&lib, &b), Some(other));
        state.save_profile_preferences(&mut lib, &b, draft);
        assert_eq!(lib.profiles[&b].preferences, Some(other), "another profile cannot replace a failed draft");
        state.retry_profile_preferences(&mut lib);
        assert!(state.pending_profile_preferences.is_none());
        let expected = ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: Some(24) };
        assert_eq!(lib.profiles[&a].preferences, Some(expected));
        let revision = lib.revision;
        state.save_profile_preferences(&mut lib, &a, expected);
        assert_eq!(lib.revision, revision, "no redundant save");
        drop(lib);
        let reopened = fixture.library();
        assert_eq!(reopened.profiles[&a].preferences, Some(expected));
        assert_eq!(reopened.profiles[&b].preferences, Some(other));
        assert_eq!(reopened.settings, shared);
    }

    #[test]
    fn profile_preferences_unavailable_policy_does_not_render_inherited_controls() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let fixture = Fixture::new();
            let profile = fixture.library().default_profile_id();
            let conn = rusqlite::Connection::open(fixture.dir.join("fixture.sqlite")).unwrap();
            conn.execute("UPDATE entities SET payload = json_set(payload, '$.preferences', json('{\"auto_archive_hours\":2}')) WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
            let mut lib = fixture.library();
            let mut state = SettingsState::new(Page::General, 0.0);
            assert!(lib.profiles.contains_key(&profile), "display identity is preserved");
            assert!(lib.effective_profile_preferences(&profile).is_none());
            assert!(state.profile_preferences(&lib, &profile).is_none(), "invalid policy cannot masquerade as inherited defaults");
            let before = lib.store.all_entities().unwrap();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let mut frame = 0;
            let mut shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]);
            for _ in 0..8 { shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            assert!(shown.text.iter().any(|(text, _, _)| text.contains("Browsing preferences are unavailable")));
            assert!(!shown.text.iter().any(|(text, _, _)| text == "Search engine" || text == "Archive unpinned tabs after"));
            assert_eq!(lib.store.all_entities().unwrap(), before);
        }
    }

    #[test]
    fn profile_preference_retry_refreshes_a_conflict_through_the_visible_control() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let fixture = Fixture::new();
            let mut lib = fixture.library();
            let profile = lib.default_profile_id();
            let shared = lib.settings.clone();
            let mut other = fixture.library();
            other.set_profile_preferences(&profile, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(24) })).unwrap();
            let conn = rusqlite::Connection::open(fixture.dir.join("fixture.sqlite")).unwrap();
            conn.execute("UPDATE entities SET payload = json_set(payload, '$.name', 'Renamed elsewhere') WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
            let mut state = SettingsState::new(Page::General, 0.0);
            state.save_profile_preferences(&mut lib, &profile, ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None });
            assert!(state.pending_profile_preferences.is_some());
            assert!(lib.profiles[&profile].preferences.is_none());
            let before = lib.store.all_entities().unwrap();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let mut frame = 0;
            let mut shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]);
            for _ in 0..8 { shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            assert_eq!(lib.store.all_entities().unwrap(), before, "idle frames do not resolve a conflict by writing");
            let pos = shown.point("Retry Save");
            for pressed in [true, false] {
                preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
                ]);
            }
            assert!(state.pending_profile_preferences.is_none(), "explicit retry must refresh the original saved profile before merging");
            let expected = Some(ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: Some(24) });
            assert_eq!(lib.profiles[&profile].preferences, expected);
            assert_eq!(lib.profiles[&profile].name, "Renamed elsewhere");
            drop(lib);
            let reopened = fixture.library();
            assert_eq!(reopened.profiles[&profile].preferences, expected);
            assert_eq!(reopened.profiles[&profile].name, "Renamed elsewhere");
            assert_eq!(reopened.settings, shared);
        }
    }

    #[test]
    fn rr03_profile_retry_recovers_restored_exact_id_after_corruption_through_visible_control() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let mut fixture = ProfilePreferenceFixture::new(size);
            let profile = fixture.profile();
            let mut other = fixture.database.library();
            let b = other.create_profile("Other profile").unwrap();
            other.set_profile_preferences(&b, Some(ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: Some(0) })).unwrap();
            other.set_profile_preferences(&profile, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(24) })).unwrap();
            fixture.state.save_profile_preferences(&mut fixture.lib, &profile, ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None });
            assert!(fixture.state.pending_profile_preferences.is_some());
            let conn = rusqlite::Connection::open(fixture.database.dir.join("fixture.sqlite")).unwrap();
            let payload: String = conn.query_row("SELECT payload FROM entities WHERE kind = 'profile' AND id = ?1", [&profile], |row| row.get(0)).unwrap();
            conn.execute("UPDATE entities SET payload = 'unreadable fixture profile' WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
            fixture.click("Retry Save");
            assert!(!fixture.lib.profiles.contains_key(&profile));
            assert!(fixture.state.pending_profile_preferences.is_some());
            conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'profile' AND id = ?2", rusqlite::params![payload, profile]).unwrap();
            other.set_profile_preferences(&profile, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(168) })).unwrap();
            let rows = fixture.lib.store.all_entities().unwrap();
            fixture.settle();
            assert_eq!(fixture.lib.store.all_entities().unwrap(), rows, "idle frames must not retry a retained draft");
            fixture.click("Retry Save");
            assert!(fixture.state.pending_profile_preferences.is_none(), "cache absence must not block exact saved-ID refresh");
            let reopened = fixture.database.library();
            assert_eq!(reopened.profiles[&profile].preferences, Some(ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: Some(168) }));
            assert_eq!(reopened.profiles[&b], other.profiles[&b]);
            assert_eq!(reopened.profiles.len(), 2, "retry must not create another profile");
        }
    }

    #[test]
    fn rr03_profile_retry_keeps_deleted_original_and_cancel_never_redirects_to_another_profile() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for tombstone in [false, true] {
                let mut fixture = ProfilePreferenceFixture::new(size);
                let profile = fixture.profile();
                let b = fixture.lib.create_profile("Other profile").unwrap();
                let b_space = fixture.lib.create_space("Other space", "B", crossbow_core::model::Theme::preset(0), &b).unwrap();
                fixture.lib.set_profile_preferences(&b, Some(ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: Some(0) })).unwrap();
                let mut other = fixture.database.library();
                other.set_profile_preferences(&profile, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(24) })).unwrap();
                let draft = ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None };
                fixture.state.save_profile_preferences(&mut fixture.lib, &profile, draft);
                let conn = rusqlite::Connection::open(fixture.database.dir.join("fixture.sqlite")).unwrap();
                conn.execute("UPDATE entities SET payload = 'unreadable fixture profile' WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
                fixture.click("Retry Save");
                assert!(!fixture.lib.profiles.contains_key(&profile));
                let sql = if tombstone { "UPDATE entities SET deleted = 1, payload = '' WHERE kind = 'profile' AND id = ?1" }
                    else { "DELETE FROM entities WHERE kind = 'profile' AND id = ?1" };
                conn.execute(sql, [&profile]).unwrap();
                fixture.space = b_space;
                let rows = fixture.lib.store.all_entities().unwrap();
                for _ in 0..2 {
                    fixture.click("Retry Save");
                    let pending = fixture.state.pending_profile_preferences.as_ref().unwrap();
                    assert_eq!(pending.profile, profile);
                    assert_eq!(pending.draft, draft);
                    assert!(pending.error.contains("removed profile"));
                    assert_eq!(fixture.lib.store.all_entities().unwrap(), rows);
                }
                fixture.click("Cancel");
                assert!(fixture.state.pending_profile_preferences.is_none());
                assert_eq!(fixture.lib.store.all_entities().unwrap(), rows);
                let reopened = fixture.database.library();
                assert!(!reopened.profiles.contains_key(&profile));
                assert_eq!(reopened.profiles[&b], other.profiles[&b]);
                assert_eq!(reopened.profiles.len(), 1);
            }
        }
    }

    #[test]
    fn profile_preference_conflict_retry_retains_only_user_edits_after_another_failure() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let profile = lib.default_profile_id();
        let mut other = fixture.library();
        other.set_profile_preferences(&profile, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(24) })).unwrap();
        let mut state = SettingsState::new(Page::General, 0.0);
        state.save_profile_preferences(&mut lib, &profile, ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None });
        let conn = rusqlite::Connection::open(fixture.dir.join("fixture.sqlite")).unwrap();
        conn.execute_batch("CREATE TRIGGER block_profile_retry BEFORE INSERT ON entities WHEN NEW.kind = 'profile' BEGIN SELECT RAISE(ABORT, 'fixture retry blocked'); END;").unwrap();
        state.retry_profile_preferences(&mut lib);
        let pending = state.pending_profile_preferences.as_ref().unwrap();
        assert!(pending.error.contains("fixture retry blocked"));
        assert_eq!(pending.draft.search_engine, Some(SearchEngine::Kagi));
        assert_eq!(pending.saved.auto_archive_hours, Some(24));
        assert_eq!(pending.draft.auto_archive_hours, Some(24));
        conn.execute_batch("DROP TRIGGER block_profile_retry;").unwrap();
        other.set_profile_preferences(&profile, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(168) })).unwrap();
        state.retry_profile_preferences(&mut lib);
        assert!(state.pending_profile_preferences.is_none());
        let expected = Some(ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: Some(168) });
        assert_eq!(lib.profiles[&profile].preferences, expected);
        drop(lib);
        assert_eq!(fixture.library().profiles[&profile].preferences, expected);
    }

    #[test]
    fn profile_preference_retry_refresh_failure_never_resurrects_a_saved_tombstone() {
        let fixture = Fixture::new();
        let mut lib = fixture.read_only();
        let profile = lib.default_profile_id();
        let draft = ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None };
        let mut state = SettingsState::new(Page::General, 0.0);
        state.save_profile_preferences(&mut lib, &profile, draft);
        drop(lib);
        let mut lib = fixture.library();
        let conn = rusqlite::Connection::open(fixture.dir.join("fixture.sqlite")).unwrap();
        conn.execute("UPDATE entities SET deleted = 1, payload = '' WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
        let before = lib.store.all_entities().unwrap();
        for _ in 0..2 {
            state.retry_profile_preferences(&mut lib);
            let pending = state.pending_profile_preferences.as_ref().unwrap();
            assert_eq!(pending.profile, profile);
            assert_eq!(pending.draft, draft);
            assert!(pending.reveal_error);
            assert_eq!(lib.store.all_entities().unwrap(), before);
        }
        state.pending_profile_preferences = None;
        assert!(fixture.library().profiles.get(&profile).is_none());
    }

    #[test]
    fn profile_preference_removal_cannot_resurrect_or_redirect_a_failed_draft() {
        let fixture = Fixture::new();
        let mut other = fixture.library();
        let profile = other.default_profile_id();
        let b = other.create_profile("Other profile").unwrap();
        let mut lib = fixture.read_only();
        let mut state = SettingsState::new(Page::General, 0.0);
        state.save_profile_preferences(&mut lib, &profile, ProfilePreferences { search_engine: None, auto_archive_hours: Some(0) });
        let conn = rusqlite::Connection::open(fixture.dir.join("fixture.sqlite")).unwrap();
        conn.execute("DELETE FROM entities WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
        lib.profiles.remove(&profile);
        let revision = lib.revision;
        let entities = lib.store.all_entities().unwrap();
        state.retry_profile_preferences(&mut lib);
        assert_eq!(state.pending_profile_preferences.as_ref().unwrap().profile, profile);
        assert!(state.pending_profile_preferences.as_ref().unwrap().error.contains("removed profile"));
        assert_eq!(state.profile_preferences(&lib, &profile), None);
        assert_eq!(lib.revision, revision);
        assert_eq!(lib.store.all_entities().unwrap(), entities);
        assert!(!lib.profiles.contains_key(&profile));
        state.pending_profile_preferences = None;
        assert!(state.pending_profile_preferences.is_none());
        let reopened = fixture.library();
        assert!(!reopened.profiles.contains_key(&profile));
        assert_eq!(reopened.profiles[&b], other.profiles[&b]);
        assert_eq!(reopened.profiles.len(), 1);
    }

    #[test]
    fn profile_preference_reset_inherits_without_overwriting_newer_shared_defaults() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let profile = lib.default_profile_id();
        let mut state = SettingsState::new(Page::General, 0.0);
        state.save_profile_preferences(&mut lib, &profile, ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: Some(0) });
        lib = fixture.read_only();
        state.save_profile_preferences(&mut lib, &profile, ProfilePreferences::default());
        assert!(state.pending_profile_preferences.is_some());
        let mut lib = fixture.library();
        let mut shared = lib.settings.clone();
        shared.search_engine = SearchEngine::DuckDuckGo;
        shared.auto_archive_hours = 168;
        lib.set_settings(shared.clone()).unwrap();
        state.retry_profile_preferences(&mut lib);
        assert!(state.pending_profile_preferences.is_none());
        drop(lib);
        let reopened = fixture.library();
        assert_eq!(reopened.profiles[&profile].preferences, None);
        assert_eq!(reopened.settings, shared);
        let effective = reopened.effective_profile_preferences(&profile).unwrap();
        assert_eq!(effective.search_engine, SearchEngine::DuckDuckGo);
        assert_eq!(effective.auto_archive_hours, 168);
    }

    pub(super) struct PreferenceFrame {
        pub(super) text: Vec<(String, Rect, Rect)>,
        pub(super) switches: Vec<Rect>,
        /// Each text's colour, in the same order as `text`.
        colors: Vec<Color32>,
        pub(super) scroll_offset: egui::Vec2,
    }

    impl PreferenceFrame {
        fn point(&self, label: &str) -> egui::Pos2 {
            let (_, bounds, clip) = self.text.iter().find(|(text, _, _)| text == label).unwrap_or_else(|| panic!("missing control: {}", redact(label)));
            assert!(clip.expand(0.5).contains_rect(*bounds), "control must be visible: {}", redact(label));
            bounds.center()
        }

        /// The colour a label is drawn in.
        pub(super) fn color(&self, label: &str) -> Color32 {
            let index = self.text.iter().position(|(text, _, _)| text == label).unwrap_or_else(|| panic!("missing control: {}", redact(label)));
            self.colors[index]
        }

        /// Every label, for a failure message, with recovery codes redacted.
        pub(super) fn labels(&self) -> Vec<String> {
            self.text.iter().map(|(text, _, _)| redact(text)).collect()
        }
    }

    pub(super) use crate::test_codes::redact;

    fn preference_frame(ctx: &egui::Context, state: &mut SettingsState, lib: &mut Library, size: egui::Vec2, scale: f32, frame: &mut u32, events: Vec<egui::Event>) -> PreferenceFrame {
        settings_test_frame(ctx, state.page, size, scale, frame, events, |ui, pal| match state.page {
            Page::General => {
                let space = lib.spaces_ordered()[0].0.clone();
                general_preferences(state, lib, &space, ui, pal);
            }
            Page::Advanced => advanced_preferences(state, lib, ui, pal),
            Page::Boosts => boosts_content(state, lib, Some("example.com"), ui, pal),
            _ => panic!("fixture only renders preference controls"),
        })
    }

    pub(super) fn settings_test_frame(ctx: &egui::Context, page: Page, size: egui::Vec2, scale: f32, frame: &mut u32, events: Vec<egui::Event>, mut content: impl FnMut(&mut Ui, &Palette)) -> PreferenceFrame {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
        let rect = sheet_rect(screen, 1.0);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let mut input = egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(*frame) / 60.0), events, ..Default::default() };
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
        let mut scroll_offset = egui::Vec2::ZERO;
        let mut output = ctx.run_ui(input, |ui| {
            show_backdrop(ui.ctx(), screen, 1.0);
            show_sheet(ui.ctx(), rect, |ui| {
                set_visuals(ui, &pal);
                let body = if compact_navigation_needed(rect.size()) {
                    Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0))
                } else {
                    Rect::from_min_max(rect.min + vec2(224.0, 18.0), rect.max - vec2(28.0, 16.0))
                };
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                child.label(RichText::new(page.title()).size(20.0).strong());
                child.add_space(10.0);
                scroll_offset = egui::ScrollArea::vertical().id_salt(("settings-scroll", page as u8)).animated(!matches!(page, Page::AiProviders | Page::AiChat)).auto_shrink([false, false]).show(&mut child, |ui| {
                    ui.set_width(body.width() - 8.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    content(ui, &pal);
                    ui.add_space(16.0);
                }).state.clamped_offset();
            });
        });
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        let colors = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => {
                let section = text.galley.job.sections.first().map_or(Color32::PLACEHOLDER, |section| section.format.color);
                Some(text.override_text_color.unwrap_or(if section == Color32::PLACEHOLDER { text.fallback_color } else { section }))
            }
            _ => None,
        }).collect();
        let switches = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.rect.size() == vec2(38.0, 22.0) && shape.clip_rect.contains_rect(rect.rect) => Some(rect.rect),
            _ => None,
        }).collect();
        output.textures_delta.clear();
        *frame += 1;
        PreferenceFrame { text, switches, colors, scroll_offset }
    }

    struct AiFixture {
        database: Fixture,
        lib: Library,
        space: String,
        state: SettingsState,
        scan: Option<crate::ai::tests::BlockedScan>,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl AiFixture {
        fn new(size: egui::Vec2) -> Self {
            let database = Fixture::new();
            let lib = database.library();
            let space = lib.spaces_ordered()[0].0.clone();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { database, lib, space, state: SettingsState::new(Page::Advanced, 0.0), scan: None, ctx, size, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            crate::ai::reconcile(&mut self.state.ai, &self.lib, &self.space, self.state.page == Page::AiProviders);
            crate::chat::reconcile(&mut self.state.chat, &self.lib, &self.space, &None, self.state.page == Page::AiChat);
            if self.state.page == Page::AiProviders && self.state.ai.is_none() {
                if let Some(profile) = crate::downloads::active_profile(&self.lib, &self.space) {
                    let mut draft = crate::ai::SettingsDraft::load(&self.lib, profile);
                    // Keep filesystem discovery synthetic; all other controls use production code.
                    draft.discovery = Some(crossbow_ai::providers::Discovery::default());
                    self.state.ai = Some(draft);
                }
            }
            settings_test_frame(&self.ctx, self.state.page, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                match self.state.page {
                    Page::Advanced => advanced_preferences(&mut self.state, &mut self.lib, ui, pal),
                    Page::AiChat => {
                        if crate::ui::chat::show(&mut self.state.chat, &mut self.lib, &self.space, &None, ui, pal) {
                            self.state.chat = None;
                            self.state.page = Page::AiProviders;
                        }
                    }
                    Page::AiProviders => {
                        if let Some(scan) = &self.scan {
                            let profile = crate::downloads::active_profile(&self.lib, &self.space).unwrap().to_owned();
                            let draft = self.state.ai.as_mut().unwrap();
                            draft.poll();
                            crate::ui::ai_settings::content(draft, &mut self.lib, &profile, ui, pal, |state, ctx| scan.retry(state, ctx));
                        } else {
                            crate::ui::ai_settings::show(&mut self.state.ai, &mut self.lib, &self.space, ui, pal);
                        }
                    }
                    _ => unreachable!(),
                }
            })
        }

        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..8 { self.frame(vec![]); }
            self.frame(vec![])
        }

        fn visible(&mut self, label: &str) -> PreferenceFrame {
            let rect = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0);
            let scroll_point = rect.right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..24 {
                let shown = self.settle();
                if let Some((_, bounds, clip)) = shown.text.iter().find(|(text, _, _)| text == label) {
                    if clip.expand(0.5).contains_rect(*bounds) { return shown; }
                    let delta = if bounds.center().y > clip.center().y { -100.0 } else { 100.0 };
                    self.frame(vec![
                        egui::Event::PointerMoved(scroll_point),
                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                    ]);
                } else {
                    // Standard labels/editors cull offscreen text shapes. Search
                    // upward first, then downward, rather than assuming their side.
                    self.frame(vec![
                        egui::Event::PointerMoved(scroll_point),
                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, if attempt < 12 { 100.0 } else { -100.0 }), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                    ]);
                }
            }
            let shown = self.settle();
            panic!("AI control did not scroll into view: {}; size={:?}; text={:?}; chat={:?}", redact(label), self.size, shown.labels(), self.state.chat.as_ref().map(|chat| (&chat.draft, chat.busy(), chat.reveal_status)));
        }

        fn click(&mut self, label: &str) {
            let pos = self.visible(label).point(label);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
        }

        fn key(&mut self, key: egui::Key) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]);
            }
        }

        fn focus(&mut self, label: &str) {
            for _ in 0..24 {
                self.key(egui::Key::Tab);
                let shown = self.settle();
                let response = self.ctx.memory(|memory| memory.focused()).and_then(|id| self.ctx.read_response(id));
                if let Some(response) = response {
                    if shown.text.iter().any(|(text, bounds, _)| text == label && response.rect.contains(bounds.center())) { return; }
                }
            }
            panic!("AI control not keyboard reachable: {label}");
        }

        fn finish_request(&mut self) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while self.state.ai.as_ref().unwrap().busy() {
                self.frame(vec![]);
                assert!(std::time::Instant::now() < deadline, "UI diagnostic did not settle");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        fn profile(&self) -> &str { &self.lib.spaces[&self.space].profile_id }
    }

    #[test]
    fn review_stream_completion_keeps_readers_scroll_at_both_sizes() {
        use crate::chat::streaming_tests::{StreamServer, COMPLETE};
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        let text = "Synthetic paragraph to read while the request is in flight.\n".repeat(120);
        let partial = format!("data: {}\n\n", serde_json::json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]}));
        for (size, long_prompt, followup, at_end) in [
            (vec2(640.0, 420.0), true, false, true), (vec2(1360.0, 860.0), true, false, true),
            (vec2(688.0, 600.0), true, false, true), (vec2(688.0, 600.0), true, false, false),
            (vec2(640.0, 420.0), true, false, false), (vec2(1360.0, 860.0), true, false, false),
            (vec2(640.0, 420.0), false, false, false), (vec2(1360.0, 860.0), false, false, false),
            (vec2(640.0, 420.0), true, true, false), (vec2(1360.0, 860.0), true, true, false),
        ] {
            let stop = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let server = StreamServer::new(&[&[&partial, if at_end { stop } else { COMPLETE }]]);
            let mut fixture = AiFixture::new(size);
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_endpoint(server.endpoint);
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            fixture.state.page = Page::AiChat;
            fixture.settle();
            fixture.state.chat.as_mut().unwrap().stream = true;
            if followup {
                fixture.state.chat.as_mut().unwrap().messages = vec![
                    crossbow_ai::Message::user("Earlier synthetic question"),
                    crossbow_ai::Message { role: crossbow_ai::Role::Assistant, content: "Earlier completed answer\n".repeat(20) },
                ];
            }
            fixture.click("Ask a question…");
            let prompt = if long_prompt { "Synthetic multiline prompt, not answer text.\n".repeat(70) } else { "Synthetic long-answer request".into() };
            fixture.frame(vec![egui::Event::Text(prompt)]);
            fixture.click("Send");
            server.received();
            server.release.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while fixture.state.chat.as_ref().unwrap().partial.is_empty() {
                fixture.frame(vec![]);
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            fixture.settle();
            let point = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), size), 1.0).right_bottom() - vec2(60.0, 40.0);
            // Measure actual answer text, both mid-answer and at the clamped
            // content end where removing a trailing row could move every line.
            let mut anchor = None;
            let mut previous_offset = None;
            for _ in 0..20 {
                fixture.frame(vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, if at_end { -10000.0 } else { -250.0 }), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                ]);
                for _ in 0..60 { fixture.frame(vec![]); }
                let shown = fixture.settle();
                if let Some((_, bounds, clip)) = shown.text.iter().find(|(value, _, _)| value == &text) {
                    let reached = if at_end {
                        previous_offset == Some(shown.scroll_offset.y) && bounds.intersect(*clip).height() > 0.0
                    } else {
                        bounds.top() < clip.top() - 100.0 && bounds.bottom() > clip.bottom() + 100.0
                    };
                    if reached {
                        anchor = Some((*bounds, *clip, shown.scroll_offset.y));
                        break;
                    }
                }
                previous_offset = Some(shown.scroll_offset.y);
            }
            let (before, clip, offset) = anchor.expect("fixture must scroll into the answer");
            assert!(offset > 0.0);
            assert!(!fixture.state.chat.as_ref().unwrap().reveal_status);
            server.release.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while fixture.state.chat.as_ref().unwrap().busy() {
                fixture.frame(vec![]);
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            server.join();
            assert!(fixture.state.chat.as_ref().unwrap().error.is_none());
            let shown = fixture.settle();
            let (_, after, after_clip) = shown.text.iter().find(|(value, _, _)| value.starts_with(&text))
                .expect("completed answer must remain visible, not be replaced by the prompt");
            assert_eq!(clip, *after_clip);
            assert!((after.top() - before.top()).abs() < 1.0 && (after.width() - before.width()).abs() < 1.0,
                "completion moved answer paragraphs at {size:?}, long_prompt={long_prompt}, followup={followup}, at_end={at_end}: {before:?} -> {after:?}; scroll {offset} -> {}", shown.scroll_offset.y);
        }
    }

    #[test]
    fn streaming_chat_controls_are_explicit_keyboard_reachable_and_reset_at_both_sizes() {
        use crate::chat::streaming_tests::{StreamServer, WorkerWakes, PARTIAL, COMPLETE};
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let server = StreamServer::new(&[&[PARTIAL, COMPLETE]]);
            let mut fixture = AiFixture::new(size);
            let wakes = WorkerWakes::new(&fixture.ctx);
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_endpoint(server.endpoint);
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            fixture.state.page = Page::AiChat;
            let shown = fixture.visible("Stream answer");
            assert_eq!(shown.switches.len(), 1);
            assert!(!fixture.state.chat.as_ref().unwrap().stream);
            let mut focused = false;
            for _ in 0..24 {
                fixture.key(egui::Key::Tab);
                let response = fixture.ctx.memory(|memory| memory.focused()).and_then(|id| fixture.ctx.read_response(id));
                if response.is_some_and(|response| response.rect.size() == vec2(38.0, 22.0)) {
                    focused = true;
                    break;
                }
            }
            assert!(focused, "streaming switch must be in the keyboard focus order");
            fixture.key(egui::Key::Space);
            assert!(fixture.state.chat.as_ref().unwrap().stream);
            for expected in [false, true] {
                let pos = fixture.visible("Stream answer").switches[0].center();
                for pressed in [true, false] {
                    fixture.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
                }
                assert_eq!(fixture.state.chat.as_ref().unwrap().stream, expected);
            }
            assert!(server.no_request(), "toggling streaming must not contact a provider");
            fixture.click("Ask a question…");
            fixture.frame(vec![egui::Event::Text("Synthetic UI stream question".into())]);
            fixture.click("Send");
            assert!(fixture.state.chat.as_ref().unwrap().busy());
            let request = server.received();
            let payload: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(payload["stream"], true);
            assert_eq!(payload["messages"], serde_json::json!([{"role":"user","content":"Synthetic UI stream question"}]));
            let pos = fixture.visible("Stream answer").switches[0].center();
            for pressed in [true, false] {
                fixture.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(fixture.state.chat.as_ref().unwrap().stream, "in-flight mode must be immutable");
            server.release.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while fixture.state.chat.as_ref().unwrap().partial.is_empty() {
                fixture.frame(vec![]);
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            wakes.wait(); // The sole progress wake from the held first fragment.
            fixture.visible("Assistant (incomplete)");
            let shown = fixture.visible("Synthetic partial");
            let (_, bounds, clip) = shown.text.iter().find(|(text, _, _)| text == "Synthetic partial").unwrap();
            assert!(clip.expand(0.5).contains_rect(*bounds));
            assert!(fixture.state.chat.as_ref().unwrap().messages.is_empty());
            fixture.click("Cancel Request");
            let chat = fixture.state.chat.as_ref().unwrap();
            assert!(!chat.busy() && chat.error.is_some() && chat.messages.is_empty());
            assert_eq!(chat.partial, "Synthetic partial");
            assert_eq!(chat.draft, "Synthetic UI stream question");
            assert!(server.no_request(), "cancellation must not retry or fall back");
            server.release.send(()).unwrap();
            server.join();
            wakes.wait(); // Terminal wake is after client teardown/permit release.
            fixture.click("New Chat");
            fixture.settle();
            let chat = fixture.state.chat.as_ref().unwrap();
            assert!(!chat.stream && chat.partial.is_empty() && chat.messages.is_empty() && chat.draft.is_empty());
        }
    }

    #[test]
    fn command_chat_opens_blank_without_contact_and_reopens_without_draft() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut fixture = AiFixture::new(size);
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_endpoint(crossbow_ai::local::LocalEndpoint { port: listener.local_addr().unwrap().port(), ..Default::default() });
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            let bar = crate::app::CommandBar {
                query: "Ask AI".into(), edit_current: false, origin: None,
                chat_entry: crate::chat::Entry::capture(&fixture.lib, &fixture.space, &None), needs_focus: false,
                selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
            };
            let Action::OpenChat(entry) = bar.ask_ai_action() else { panic!("missing command entry") };
            fixture.state = SettingsState::from_chat_entry(&entry, &mut fixture.lib, &fixture.space, &None, 0.0).unwrap();
            assert_eq!(fixture.state.page, Page::AiChat);
            fixture.settle();
            let chat = fixture.state.chat.as_ref().unwrap();
            assert_eq!(chat.profile, profile);
            assert!(chat.draft.is_empty() && chat.messages.is_empty() && !chat.busy());
            fixture.visible("Send");
            fixture.click("Ask a question…");
            fixture.frame(vec![egui::Event::Text("Unsent synthetic draft".into())]);
            let editor = fixture.state.chat.as_mut().unwrap().editor_id(&fixture.ctx);
            assert_eq!(fixture.state.chat.as_ref().unwrap().draft, "Unsent synthetic draft");
            fixture.state = SettingsState::new(Page::General, 0.0);
            assert!(egui::TextEdit::load_state(&fixture.ctx, editor).is_none());
            fixture.state = SettingsState::from_chat_entry(&entry, &mut fixture.lib, &fixture.space, &None, 1.0).unwrap();
            fixture.settle();
            assert!(fixture.state.chat.as_ref().unwrap().draft.is_empty());
            fixture.click("Configure Providers");
            assert_eq!(fixture.state.page, Page::AiProviders);
            assert!(fixture.state.chat.is_none());
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "entry/typing/reopen must not contact the saved provider");
        }
    }

    #[test]
    fn a_command_bar_question_opens_ask_ai_and_is_sent_once() {
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let server = crate::ai::tests::Server::sequence(&[("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":"Rayleigh scattering"},"finish_reason":"stop"}]}"#)]);
            let mut fixture = AiFixture::new(size);
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_endpoint(server.endpoint);
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            let ready = crate::ai_features::Ready { endpoint: server.endpoint, model: "fixture-model".into() };
            crate::ai_features::enable(&fixture.lib, &profile, crate::ask_bar::FEATURE, &ready).unwrap();
            let entry = crate::chat::Entry::capture(&fixture.lib, &fixture.space, &None).unwrap();
            let question = "Why is the sky blue?";
            fixture.state = SettingsState::from_chat_question(&entry, question.into(), &mut fixture.lib, &fixture.space, &None, &fixture.ctx, 0.0).unwrap();
            assert_eq!(fixture.state.page, Page::AiChat);
            assert!(fixture.state.chat.as_ref().unwrap().busy(), "sent at once at {size:?}");
            let request = server.received();
            let payload: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(payload["messages"], serde_json::json!([{"role":"user","content":question}]), "only the typed text is sent");
            server.release.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while fixture.state.chat.as_ref().unwrap().busy() {
                fixture.frame(vec![]);
                assert!(std::time::Instant::now() < deadline, "the answer arrives");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            fixture.settle();
            let chat = fixture.state.chat.as_ref().unwrap();
            assert_eq!(chat.messages.iter().map(|message| message.content.as_str()).collect::<Vec<_>>(), [question, "Rayleigh scattering"]);
            assert!(chat.draft.is_empty() && !chat.busy(), "sent once, not again");
            server.join();

            // Turned off, or text the bar never sends: the question waits in the
            // composer with the reason, and nothing is sent.
            crate::ai_features::disable(&fixture.lib, &profile, crate::ask_bar::FEATURE).unwrap();
            for (text, reason) in [
                (question, "Ask AI in the Command Bar is off. Turn it on in Settings → AI Providers."),
                ("https://example.test/private", "Not sent: it looks like a web address."),
            ] {
                if text != question { crate::ai_features::enable(&fixture.lib, &profile, crate::ask_bar::FEATURE, &ready).unwrap(); }
                let state = SettingsState::from_chat_question(&entry, text.into(), &mut fixture.lib, &fixture.space, &None, &fixture.ctx, 1.0).unwrap();
                let chat = state.chat.as_ref().unwrap();
                assert!(!chat.busy() && chat.messages.is_empty(), "{text:?} is not sent");
                assert_eq!((chat.draft.as_str(), chat.error.as_deref()), (text, Some(reason)));
            }
        }
    }

    #[test]
    fn review_chat_recovery_reloads_an_externally_repaired_profile() {
        for (size, reopen) in [(vec2(640.0, 420.0), false), (vec2(1360.0, 860.0), true)] {
            let mut fixture = AiFixture::new(size);
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            let external = fixture.database.library();
            let row = external.store.all_entities().unwrap().into_iter().find(|row| row.kind == "profile" && row.id == profile).unwrap();
            let mut invalid: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
            invalid["preferences"] = serde_json::json!({"auto_archive_hours":2});
            external.store.put_entity(&crossbow_core::store::EntityRow { payload: invalid.to_string(), ..row.clone() }, false).unwrap();
            fixture.lib = fixture.database.library();
            assert!(fixture.lib.profiles.contains_key(&profile));
            assert!(fixture.lib.effective_profile_preferences(&profile).is_none());
            fixture.state.page = Page::AiChat;
            fixture.settle();
            assert!(fixture.state.chat.as_ref().unwrap().selection.is_none());

            external.store.put_entity(&row, false).unwrap();
            assert!(fixture.lib.effective_profile_preferences(&profile).is_none(), "external repair must leave a stale cache before recovery");
            if reopen {
                fixture.click("Configure Providers");
                fixture.state.page = Page::AiChat;
            } else { fixture.click("New Chat"); }
            fixture.settle();
            let chat = fixture.state.chat.as_ref().unwrap();
            assert!(chat.selection.is_some(), "explicit recovery immediately invalidated the repaired profile");
            assert!(chat.error.is_none() && !chat.busy());
            assert!(fixture.lib.effective_profile_preferences(&profile).is_some());
            fixture.click("Ask a question…");
            fixture.frame(vec![egui::Event::Text("Recovered synthetic draft".into())]);
            assert_eq!(fixture.state.chat.as_ref().unwrap().draft, "Recovered synthetic draft");
        }
    }

    #[test]
    fn review_chat_recovery_remains_reachable_after_cached_identity_eviction() {
        for fault in ["malformed", "deleted", "missing"] {
            for (size, reopen) in [(vec2(640.0, 420.0), false), (vec2(1360.0, 860.0), true)] {
                let mut fixture = AiFixture::new(size);
                let profile = fixture.profile().to_owned();
                let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
                selection.set_model(Some("fixture-model".into()));
                selection.save(&mut fixture.lib, Some(&profile));
                let external = fixture.database.library();
                let row = external.store.all_entities().unwrap().into_iter().find(|row| row.kind == "profile" && row.id == profile).unwrap();
                fixture.state.page = Page::AiChat;
                fixture.click("Ask a question…");
                fixture.frame(vec![egui::Event::Text("Discard this synthetic draft".into())]);
                match fault {
                    "malformed" => external.store.put_entity(&crossbow_core::store::EntityRow { payload: "invalid".into(), ..row.clone() }, false).unwrap(),
                    "deleted" => external.store.put_entity(&crossbow_core::store::EntityRow { deleted: true, ..row.clone() }, false).unwrap(),
                    _ => { rusqlite::Connection::open(fixture.database.dir.join("fixture.sqlite")).unwrap()
                        .execute("DELETE FROM entities WHERE kind='profile' AND id=?1", [&profile]).unwrap(); }
                }
                if reopen {
                    fixture.click("Configure Providers");
                    fixture.state.page = Page::AiChat;
                } else { fixture.click("New Chat"); }
                fixture.settle();
                assert!(!fixture.lib.profiles.contains_key(&profile), "fixture must evict the display identity");
                let chat = fixture.state.chat.as_ref().expect("failed reload must leave explicit recovery reachable");
                assert_eq!(chat.profile, profile);
                assert!(chat.selection.is_none() && !chat.busy() && chat.draft.is_empty());
                fixture.click("New Chat"); // Still invalid: no fallback, recreation or hidden retry.
                fixture.settle();
                assert!(fixture.state.chat.as_ref().unwrap().selection.is_none());
                assert!(fixture.lib.read_profile(&profile).is_err());

                external.store.put_entity(&row, false).unwrap();
                fixture.settle();
                assert!(!fixture.lib.profiles.contains_key(&profile), "idle paint must not retry storage");
                if reopen {
                    fixture.click("Configure Providers");
                    fixture.state.page = Page::AiChat;
                } else { fixture.click("New Chat"); }
                fixture.settle();
                let chat = fixture.state.chat.as_ref().unwrap();
                assert_eq!(chat.profile, profile);
                assert!(chat.selection.is_some() && chat.error.is_none() && !chat.busy());
                assert!(chat.draft.is_empty() && chat.messages.is_empty());
                fixture.click("Ask a question…");
                fixture.frame(vec![egui::Event::Text("Recovered exact-owner draft".into())]);
                assert_eq!(fixture.state.chat.as_ref().unwrap().draft, "Recovered exact-owner draft");
            }
        }
    }

    #[test]
    fn review_chat_undo_cannot_restore_a_cleared_sessions_private_draft() {
        for boundary in 0..3 {
            let mut fixture = AiFixture::new(vec2(640.0, 420.0));
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            fixture.state.page = Page::AiChat;
            fixture.click("Ask a question…");
            fixture.frame(vec![egui::Event::Text("private synthetic draft A".into())]);
            fixture.frame += 120;
            fixture.settle();
            let editor = fixture.ctx.memory(|memory| memory.focused()).unwrap();
            assert!(egui::TextEdit::load_state(&fixture.ctx, editor).is_some());
            match boundary {
                0 => fixture.click("New Chat"),
                1 => {
                    fixture.click("Configure Providers");
                    fixture.state.page = Page::AiChat;
                }
                _ => {
                    let second = fixture.lib.create_profile("Synthetic B").unwrap();
                    let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &second);
                    selection.set_model(Some("different-model".into()));
                    selection.save(&mut fixture.lib, Some(&second));
                    fixture.lib.spaces.get_mut(&fixture.space).unwrap().profile_id = second;
                }
            }
            fixture.settle();
            assert!(fixture.state.chat.as_ref().unwrap().draft.is_empty());
            assert!(egui::TextEdit::load_state(&fixture.ctx, editor).is_none(), "closed chat retained editor snapshots");
            fixture.click("Ask a question…");
            for pressed in [true, false] {
                fixture.frame(vec![egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
            }
            assert!(fixture.state.chat.as_ref().unwrap().draft.is_empty(), "undo crossed chat boundary {boundary}");
        }
    }

    #[test]
    fn review_chat_keyboard_focus_reveals_the_bounded_long_composer() {
        let mut fixture = AiFixture::new(vec2(640.0, 420.0));
        let profile = fixture.profile().to_owned();
        let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
        selection.set_model(Some("fixture-model".into()));
        selection.save(&mut fixture.lib, Some(&profile));
        fixture.state.page = Page::AiChat;
        fixture.click("Ask a question…");
        let text = "Synthetic line\n".repeat(100);
        // Populate a long draft with its caret/inner scroll still at the start.
        // A completed conversation leaves enough outer content to scroll it away.
        fixture.state.chat.as_mut().unwrap().draft = text.clone();
        fixture.state.chat.as_mut().unwrap().messages = vec![crossbow_ai::Message::user("Prior synthetic turn\n".repeat(50))];
        let editor = fixture.ctx.memory(|memory| memory.focused()).unwrap();
        fixture.settle();
        fixture.focus("Send");
        let rect = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), fixture.size), 1.0);
        let scroll_point = rect.right_bottom() - vec2(60.0, 40.0);
        fixture.frame(vec![
            egui::Event::PointerMoved(scroll_point),
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -500.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
        ]);
        let away = fixture.settle();
        assert!(!away.text.iter().any(|(label, bounds, clip)| label == &text && clip.height() > 0.0 && bounds.intersect(*clip).height() >= 18.0), "fixture must move composer out of view before refocusing");
        for pressed in [true, false] {
            fixture.frame(vec![egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::SHIFT }]);
        }
        assert_eq!(fixture.ctx.memory(|memory| memory.focused()), Some(editor));
        let shown = fixture.settle();
        let (_, bounds, clip) = shown.text.iter().find(|(label, _, _)| label == &text).expect("composer text shape");
        assert!(clip.height() > 0.0 && bounds.intersect(*clip).height() >= 18.0, "focused composer viewport is offscreen: {bounds:?} / {clip:?}");
    }

    #[test]
    fn chat_controls_send_retry_cancel_clear_and_leave_at_normal_and_compact_sizes() {
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0), vec2(1360.0, 420.0)] {
            let server = crate::ai::tests::Server::sequence(&[
                ("503 Unavailable", "private error body"),
                ("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":"Synthetic answer <script>plain text</script>"},"finish_reason":"stop"}]}"#),
                ("200 OK", r#"{"choices":[{"message":{"content":"Cancelled late answer"},"finish_reason":"stop"}]}"#),
            ]);
            let mut fixture = AiFixture::new(size);
            let wakes = crate::chat::streaming_tests::WorkerWakes::new(&fixture.ctx);
            let profile = fixture.profile().to_owned();
            let mut selection = crate::ai::SettingsDraft::load(&fixture.lib, &profile);
            selection.set_endpoint(server.endpoint);
            selection.set_model(Some("fixture-model".into()));
            selection.save(&mut fixture.lib, Some(&profile));
            fixture.state.page = Page::AiChat;
            fixture.click("Ask a question…");
            fixture.frame(vec![egui::Event::Text("Synthetic question".into())]);
            assert_eq!(fixture.state.chat.as_ref().unwrap().draft, "Synthetic question");
            fixture.focus("Send");
            fixture.key(egui::Key::Enter);
            assert!(fixture.state.chat.as_ref().unwrap().busy());
            assert!(server.received().contains("Synthetic question"));
            server.release.send(()).unwrap();
            let settle = |fixture: &mut AiFixture| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                while fixture.state.chat.as_ref().unwrap().busy() {
                    fixture.frame(vec![]);
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                fixture.settle();
            };
            settle(&mut fixture);
            let chat = fixture.state.chat.as_ref().unwrap();
            assert_eq!(chat.draft, "Synthetic question");
            assert!(chat.messages.is_empty());
            let error = chat.error.clone().unwrap();
            fixture.visible(&error);
            assert!(!fixture.state.chat.as_ref().unwrap().reveal_status);
            fixture.click("Send");
            assert!(server.received().contains("Synthetic question"));
            server.release.send(()).unwrap();
            settle(&mut fixture);
            assert_eq!(fixture.state.chat.as_ref().unwrap().messages.len(), 2);
            fixture.visible("Synthetic answer <script>plain text</script>");
            fixture.click("Ask a question…");
            fixture.frame(vec![egui::Event::Text("Cancel this follow-up".into())]);
            fixture.click("Send");
            let request = server.received();
            assert!(request.contains("Synthetic answer <script>plain text</script>"));
            fixture.click("Cancel Request");
            assert!(!fixture.state.chat.as_ref().unwrap().busy());
            assert_eq!(fixture.state.chat.as_ref().unwrap().draft, "Cancel this follow-up");
            server.finish();
            // Non-streaming has one terminal wake per request. Hold CHAT_TEST
            // through client teardown, not merely the fixture socket closing.
            for _ in 0..3 { wakes.wait(); }
            fixture.settle();
            assert_eq!(fixture.state.chat.as_ref().unwrap().messages.len(), 2);
            fixture.click("New Chat");
            assert!(fixture.state.chat.as_ref().unwrap().messages.is_empty());
            assert!(fixture.state.chat.as_ref().unwrap().draft.is_empty());
            fixture.click("Configure Providers");
            assert_eq!(fixture.state.page, Page::AiProviders);
            assert!(fixture.state.chat.is_none());
            fixture.state.page = Page::AiChat;
            fixture.settle();
            assert!(fixture.state.chat.as_ref().unwrap().messages.is_empty());
        }
    }

    #[test]
    fn ai_controls_open_select_save_failure_keyboard_retry_cancel_and_reopen() {
        use crossbow_ai::local::LocalProvider;
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0), vec2(1360.0, 420.0)] {
            let mut fixture = AiFixture::new(size);
            fixture.click("Configure Providers");
            assert_eq!(fixture.state.page, Page::AiProviders);
            fixture.click("Ollama");
            fixture.click("LM Studio");
            assert_eq!(fixture.state.ai.as_ref().unwrap().draft.endpoint.provider, LocalProvider::LmStudio);
            assert!(!crate::ai::SettingsDraft::load(&fixture.lib, fixture.profile()).changed());
            fixture.lib = fixture.database.read_only();
            fixture.focus("Save Selection");
            fixture.key(egui::Key::Enter);
            let error = fixture.state.ai.as_ref().unwrap().error.clone().unwrap();
            fixture.settle().point(&error);
            assert!(error.contains("Could not save provider preferences"));
            assert!(fixture.state.ai.as_ref().unwrap().changed());
            fixture.lib = fixture.database.library();
            fixture.settle();
            assert_eq!(crate::ai::SettingsDraft::load(&fixture.lib, fixture.profile()).draft.endpoint.provider, LocalProvider::Ollama);
            fixture.key(egui::Key::Enter);
            assert!(!fixture.state.ai.as_ref().unwrap().changed(), "focused Save retries with Enter: focus={:?}, error={:?}", fixture.ctx.memory(|memory| memory.focused()), fixture.state.ai.as_ref().unwrap().error);
            assert!(fixture.state.ai.as_ref().unwrap().error.is_none());
            fixture.click("LM Studio");
            fixture.click("llama.cpp");
            fixture.click("Cancel / Reload");
            // Reload also restarts discovery in production; avoid reading owner paths here.
            fixture.state.ai.as_mut().unwrap().discovery = Some(crossbow_ai::providers::Discovery::default());
            assert_eq!(fixture.state.ai.as_ref().unwrap().draft.endpoint.provider, LocalProvider::LmStudio);
            fixture.state = SettingsState::new(Page::AiProviders, 1.0);
            fixture.lib = fixture.database.library();
            fixture.settle();
            assert_eq!(fixture.state.ai.as_ref().unwrap().draft.endpoint.provider, LocalProvider::LmStudio);
            assert!(!fixture.state.ai.as_ref().unwrap().can_test());
            let a = fixture.profile().to_string();
            let b = fixture.lib.create_profile("AI fixture B").unwrap();
            fixture.space = fixture.lib.create_space("Other", "", crossbow_core::model::Theme::preset(0), &b).unwrap();
            fixture.settle();
            assert_eq!(fixture.state.ai.as_ref().unwrap().profile, b);
            assert_eq!(fixture.state.ai.as_ref().unwrap().draft, crate::ai::Preferences::default());
            assert_eq!(crate::ai::SettingsDraft::load(&fixture.lib, &a).draft.endpoint.provider, LocalProvider::LmStudio);
            fixture.state.page = Page::Advanced;
            fixture.settle();
            assert!(fixture.state.ai.is_none());
            fixture.state.page = Page::AiProviders;
            fixture.space = "missing".into();
            fixture.settle().point("This space has no available profile. Select another space to configure AI providers.");
            assert!(fixture.state.ai.is_none());
        }
    }

    #[test]
    fn ai_discovery_controls_cancel_timeout_and_keyboard_retry_without_snapback() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0), vec2(1360.0, 420.0)] {
            let mut fixture = AiFixture::new(size);
            fixture.click("Configure Providers");
            fixture.settle();
            fixture.scan = Some(crate::ai::tests::BlockedScan::start(fixture.state.ai.as_mut().unwrap(), &fixture.ctx));
            fixture.click("Cancel Scan");
            assert!(!fixture.state.ai.as_ref().unwrap().discovering());
            let error = fixture.state.ai.as_ref().unwrap().discovery_error.clone().unwrap();
            assert!(error.contains("cancelled"));
            fixture.settle().point(&error);
            fixture.focus("Scan Again");
            fixture.key(egui::Key::Enter);
            let error = fixture.state.ai.as_ref().unwrap().discovery_error.clone().unwrap();
            assert!(error.contains("still finishing"));
            fixture.settle().point(&error);
            assert!(!fixture.state.ai.as_ref().unwrap().discovering());
            fixture.scan.as_mut().unwrap().release();
            fixture.settle();
            assert!(fixture.state.ai.as_ref().unwrap().discovery.is_none());
            // Status text changed above the action; keyboard focus must survive.
            fixture.key(egui::Key::Enter);
            crate::ai::tests::settle_scan(fixture.state.ai.as_mut().unwrap());
            assert_eq!(fixture.state.ai.as_ref().unwrap().discovery, Some(crossbow_ai::providers::Discovery::default()));
            assert!(fixture.state.ai.as_ref().unwrap().discovery_error.is_none());

            fixture.scan = Some(crate::ai::tests::BlockedScan::start(fixture.state.ai.as_mut().unwrap(), &fixture.ctx));
            fixture.scan.as_ref().unwrap().expire(fixture.state.ai.as_mut().unwrap());
            let error = fixture.state.ai.as_ref().unwrap().discovery_error.clone().unwrap();
            assert!(error.contains("timed out"));
            fixture.settle().point(&error);
            assert!(!fixture.state.ai.as_ref().unwrap().reveal_discovery_error);
            fixture.visible("Refresh Models").point("Refresh Models");
            fixture.settle().point("Refresh Models");
            fixture.scan.as_mut().unwrap().release();
            fixture.settle();
            assert!(fixture.state.ai.as_ref().unwrap().discovery.is_none());
            fixture.click("Scan Again");
            crate::ai::tests::settle_scan(fixture.state.ai.as_mut().unwrap());
            assert_eq!(fixture.state.ai.as_ref().unwrap().discovery, Some(crossbow_ai::providers::Discovery::default()));
            assert!(fixture.state.ai.as_ref().unwrap().discovery_error.is_none());
        }
    }

    #[test]
    fn ai_controls_refresh_error_retry_model_test_and_cancel() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let server = crate::ai::tests::Server::sequence(&[
                ("503 Unavailable", "fixture error"),
                ("200 OK", r#"{"data":[{"id":"fixture-model"}]}"#),
                ("200 OK", r#"{"choices":[{"message":{"content":"Crossbow connection ready."},"finish_reason":"stop"}]}"#),
                ("200 OK", r#"{"data":[{"id":"stale-model"}]}"#),
            ]);
            let mut fixture = AiFixture::new(size);
            fixture.click("Configure Providers");
            fixture.settle();
            fixture.state.ai.as_mut().unwrap().set_endpoint(server.endpoint);
            fixture.click("Refresh Models");
            assert!(server.received().starts_with("GET /v1/models "));
            server.release.send(()).unwrap();
            fixture.finish_request();
            let error = fixture.state.ai.as_ref().unwrap().error.clone().unwrap();
            assert!(error.contains("503") && !error.contains("fixture error"));
            let shown = fixture.settle();
            assert!(shown.text.iter().any(|(text, _, _)| text == &error), "diagnostic error absent: state={:?}, text={:?}", fixture.state.ai.as_ref().unwrap().error, shown.labels());
            shown.point(&error);
            assert!(!fixture.state.ai.as_ref().unwrap().reveal_error);
            fixture.visible("Refresh Models").point("Refresh Models");
            fixture.settle().point("Refresh Models"); // A revealed error must not force scrolling back.
            fixture.click("Refresh Models");
            assert!(server.received().starts_with("GET /v1/models "));
            server.release.send(()).unwrap();
            fixture.finish_request();
            assert!(!fixture.state.ai.as_ref().unwrap().can_test());
            fixture.click("Select a model");
            fixture.click("fixture-model");
            assert!(fixture.state.ai.as_ref().unwrap().can_test());
            fixture.click("Send Test Phrase");
            let request = server.received();
            assert!(request.starts_with("POST /v1/chat/completions "));
            let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["messages"], serde_json::json!([{"role":"user","content":crossbow_ai::local::TEST_PROMPT}]));
            server.release.send(()).unwrap();
            fixture.finish_request();
            fixture.visible("Crossbow connection ready.").point("Crossbow connection ready.");
            fixture.click("Refresh Models");
            server.received();
            fixture.click("Cancel Request");
            assert!(!fixture.state.ai.as_ref().unwrap().busy());
            server.finish();
            fixture.settle();
            let state = fixture.state.ai.as_ref().unwrap();
            assert!(state.models.is_empty() && state.answer.is_none());
            assert!(state.error.as_deref().unwrap().contains("cancelled"));
        }
    }

    struct ProfilePreferenceFixture {
        database: Fixture,
        lib: Library,
        space: String,
        state: SettingsState,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl ProfilePreferenceFixture {
        fn new(size: egui::Vec2) -> Self {
            let database = Fixture::new();
            let lib = database.library();
            let space = lib.spaces_ordered()[0].0.clone();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { database, lib, space, state: SettingsState::new(Page::General, 0.0), ctx, size, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, self.state.page, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                match self.state.page {
                    Page::General => general_preferences(&mut self.state, &mut self.lib, &self.space, ui, pal),
                    Page::Advanced => advanced_preferences(&mut self.state, &mut self.lib, ui, pal),
                    _ => unreachable!(),
                }
            })
        }

        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..8 { self.frame(vec![]); }
            self.frame(vec![])
        }

        fn click(&mut self, label: &str) {
            let pos = self.settle().point(label);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
        }

        fn key(&mut self, key: egui::Key) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]);
            }
        }

        fn profile(&self) -> String { self.lib.spaces[&self.space].profile_id.clone() }
    }

    #[test]
    fn profile_archive_menu_saves_every_choice_and_restores_inheritance() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let mut fixture = ProfilePreferenceFixture::new(size);
            let profile = fixture.profile();
            let shared = fixture.lib.settings.clone();
            for hours in ARCHIVE_CHOICES {
                let current = fixture.lib.effective_profile_preferences(&profile).unwrap().auto_archive_hours;
                fixture.click(archive_label(current));
                fixture.click(archive_label(hours));
                assert_eq!(fixture.lib.effective_profile_preferences(&profile).unwrap().auto_archive_hours, hours);
                assert_eq!(fixture.database.library().profiles[&profile].preferences.unwrap().auto_archive_hours, Some(hours));
                assert_eq!(fixture.lib.settings, shared);
            }
            let current = fixture.lib.effective_profile_preferences(&profile).unwrap().auto_archive_hours;
            fixture.click(archive_label(current));
            fixture.click(&format!("Shared default ({})", archive_label(shared.auto_archive_hours)));
            assert!(fixture.database.library().profiles[&profile].preferences.is_none());
            fixture.space = "missing-space".into();
            let shown = fixture.settle();
            shown.point("This space has no available profile. Select another space to change browsing preferences.");
            assert!(!shown.text.iter().any(|(text, _, _)| text == "Search engine"));
        }
    }

    #[test]
    fn profile_preference_recovery_survives_page_changes_and_keyboard_activation() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for cancel in [false, true] {
                let mut fixture = ProfilePreferenceFixture::new(size);
                let profile = fixture.profile();
                fixture.lib = fixture.database.read_only();
                fixture.state.save_profile_preferences(&mut fixture.lib, &profile, ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: Some(0) });
                fixture.state.page = Page::Advanced;
                fixture.lib = fixture.database.library();
                fixture.settle();
                assert!(fixture.lib.profiles[&profile].preferences.is_none());
                fixture.key(egui::Key::Tab);
                if cancel { fixture.key(egui::Key::Tab); }
                fixture.key(egui::Key::Enter);
                assert!(fixture.state.pending_profile_preferences.is_none());
                let reopened = fixture.database.library();
                assert_eq!(reopened.effective_profile_preferences(&profile).unwrap().search_engine, if cancel { SearchEngine::Google } else { SearchEngine::Bing });
                assert_eq!(reopened.effective_profile_preferences(&profile).unwrap().auto_archive_hours, if cancel { 12 } else { 0 });
            }
        }
    }

    #[test]
    fn profile_preference_recovery_after_space_change_saves_only_its_original_profile() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for cancel in [false, true] {
                let mut fixture = ProfilePreferenceFixture::new(size);
                let a = fixture.profile();
                let b = fixture.lib.create_profile("Other profile").unwrap();
                let sb = fixture.lib.create_space("Other", "B", crossbow_core::model::Theme::preset(0), &b).unwrap();
                fixture.lib = fixture.database.read_only();
                fixture.state.save_profile_preferences(&mut fixture.lib, &a, ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: Some(0) });
                fixture.space = sb;
                fixture.lib = fixture.database.library();
                let shown = fixture.settle();
                let pending = fixture.state.pending_profile_preferences.as_ref().unwrap();
                shown.point(&pending.error);
                assert_eq!(pending.profile, a);
                assert!(fixture.lib.profiles[&a].preferences.is_none());
                assert!(fixture.lib.profiles[&b].preferences.is_none());
                fixture.click(if cancel { "Cancel" } else { "Retry Save" });
                let reopened = fixture.database.library();
                assert!(fixture.state.pending_profile_preferences.is_none());
                assert_eq!(reopened.effective_profile_preferences(&a).unwrap().search_engine, if cancel { SearchEngine::Google } else { SearchEngine::Bing });
                assert!(reopened.profiles[&b].preferences.is_none());
            }
        }
    }

    struct DownloadsFixture {
        database: Fixture,
        lib: Library,
        space: String,
        state: SettingsState,
        ctx: egui::Context,
        size: egui::Vec2,
        scale: f32,
        frame: u32,
    }

    impl DownloadsFixture {
        fn new(size: egui::Vec2, scale: f32) -> Self {
            let database = Fixture::new();
            let lib = database.library();
            let space = lib.spaces.keys().next().unwrap().clone();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { database, lib, space, state: SettingsState::new(Page::Downloads, 0.0), ctx, size, scale, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::Downloads, self.size, self.scale, &mut self.frame, events, |ui, pal| {
                download_preferences(&mut self.state, &self.lib, &self.space, &self.database.dir, ui, pal);
            })
        }

        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..8 { self.frame(vec![]); }
            self.frame(vec![])
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
        }

        fn click(&mut self, label: &str) {
            let pos = self.settle().point(label);
            self.click_at(pos);
        }

        fn key(&mut self, key: egui::Key) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]);
            }
        }

        fn profile(&self) -> &str { crate::downloads::active_profile(&self.lib, &self.space).unwrap() }
    }

    #[test]
    fn downloads_controls_save_cancel_retry_and_reopen_at_compact_and_normal_sizes() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0] {
                let mut fixture = DownloadsFixture::new(size, scale);
                fixture.lib = fixture.database.read_only();
                let shown = fixture.settle();
                shown.point("Choose Folder…");
                let switch = shown.switches[0];
                fixture.click_at(switch.center());
                assert!(fixture.state.downloads.as_ref().unwrap().draft.ask_each_time);
                assert!(!crate::downloads::Preferences::load(&fixture.lib, fixture.profile()).unwrap().ask_each_time);
                fixture.click("Save");
                let shown = fixture.settle();
                let state = fixture.state.downloads.as_ref().unwrap();
                assert!(state.error.as_deref().unwrap().contains("Could not save download preferences"));
                shown.point(state.error.as_deref().unwrap());
                shown.point("Retry Save");
                shown.point("Cancel");
                fixture.click("Cancel");
                assert!(!fixture.state.downloads.as_ref().unwrap().draft.ask_each_time);
                let shown = fixture.settle();
                fixture.click_at(shown.switches[0].center());
                fixture.click("Save");
                fixture.settle();
                fixture.lib = fixture.database.library();
                fixture.settle();
                assert!(!crate::downloads::Preferences::load(&fixture.lib, fixture.profile()).unwrap().ask_each_time, "idle frames do not retry a failed save");
                fixture.click("Retry Save");
                assert!(fixture.state.downloads.as_ref().unwrap().error.is_none());
                fixture.state = SettingsState::new(Page::Downloads, 1.0);
                fixture.lib = fixture.database.library();
                let shown = fixture.settle();
                assert!(fixture.state.downloads.as_ref().unwrap().draft.ask_each_time, "Settings and SQLite reopening read the persisted preference");
                shown.point("Save");
                fixture.click_at(shown.switches[0].center());
                assert!(!fixture.state.downloads.as_ref().unwrap().draft.ask_each_time);
                fixture.state = SettingsState::new(Page::Downloads, 2.0);
                fixture.settle();
                assert!(fixture.state.downloads.as_ref().unwrap().draft.ask_each_time, "closing Settings discards an unsaved edit");
            }
        }
    }

    #[test]
    fn downloads_controls_keyboard_and_profile_switch_keep_drafts_local() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let mut fixture = DownloadsFixture::new(size, 1.0);
            let a = fixture.profile().to_string();
            let b = fixture.lib.create_profile("Fixture B").unwrap();
            let space_b = fixture.lib.create_space("B", "", crossbow_core::model::Theme::preset(0), &b).unwrap();
            fixture.settle();
            fixture.key(egui::Key::Tab); // Choose Folder
            fixture.key(egui::Key::Tab); // Ask where to save; disabled Use Default is skipped.
            fixture.key(egui::Key::Space);
            assert!(fixture.state.downloads.as_ref().unwrap().draft.ask_each_time);
            fixture.space = space_b;
            fixture.settle();
            assert_eq!(fixture.state.downloads.as_ref().unwrap().profile, b);
            assert!(!fixture.state.downloads.as_ref().unwrap().draft.ask_each_time);
            assert!(!crate::downloads::Preferences::load(&fixture.lib, &a).unwrap().ask_each_time);
            fixture.space = "missing-space".into();
            let shown = fixture.settle();
            assert!(fixture.state.downloads.is_none());
            shown.point("This space has no available profile. Select another space to change downloads.");
            assert!(!shown.text.iter().any(|(text, _, _)| text == "Choose Folder…"));
        }
    }

    struct ClearingFixture {
        ctx: egui::Context,
        state: SettingsState,
        size: egui::Vec2,
        profile: &'static str,
        name: Option<&'static str>,
        operation: Option<(crossbow_engine::ClearDataStatus, std::time::Duration)>,
        requests: usize,
        frame: u32,
    }

    impl ClearingFixture {
        fn new(size: egui::Vec2) -> Self {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { ctx, state: SettingsState::new(Page::Advanced, 0.0), size, profile: "profile-a", name: Some("Fixture A"), operation: None, requests: 0, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::Advanced, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                self.requests += usize::from(clear_browsing_data_controls(&mut self.state, self.profile, self.name, self.operation.clone(), ui, pal));
            })
        }

        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..8 { self.frame(vec![]); }
            self.frame(vec![])
        }

        fn click(&mut self, label: &str) {
            let pos = self.settle().point(label);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
        }

        fn key(&mut self, key: egui::Key) {
            self.frame([true, false].into_iter().map(|pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() }).collect());
            self.settle();
        }
    }

    #[test]
    fn clearing_requires_explicit_confirmation_and_cancel_never_dispatches() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let mut fixture = ClearingFixture::new(size);
            fixture.click("Clear…");
            assert_eq!(fixture.state.clear_confirmation.as_deref(), Some("profile-a"));
            assert_eq!(fixture.requests, 0);
            fixture.settle().point("Clear cookies and HTTP cache for “Fixture A”? Sites may sign you out. This cannot be undone; open pages may create new data.");
            fixture.click("Cancel");
            assert!(fixture.state.clear_confirmation.is_none());
            assert_eq!(fixture.requests, 0);
            fixture.click("Clear…");
            fixture.click("Clear data");
            assert_eq!(fixture.requests, 1);
            assert!(fixture.state.clear_confirmation.is_none());
            fixture.settle();
            assert_eq!(fixture.requests, 1, "idle frames must not repeat clearing");
        }
    }

    #[test]
    fn clearing_keyboard_confirm_and_cancel_use_the_same_controls() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for cancel in [false, true] {
                let mut fixture = ClearingFixture::new(size);
                fixture.settle();
                fixture.key(egui::Key::Tab);
                fixture.key(egui::Key::Enter);
                assert!(fixture.state.clear_confirmation.is_some());
                fixture.key(egui::Key::Tab);
                if cancel { fixture.key(egui::Key::Tab); }
                fixture.key(egui::Key::Enter);
                assert!(fixture.state.clear_confirmation.is_none());
                assert_eq!(fixture.requests, usize::from(!cancel));
            }
        }
    }

    #[test]
    fn clearing_cancels_stale_profile_confirmation_and_disables_missing_profile() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let mut fixture = ClearingFixture::new(size);
            fixture.click("Clear…");
            fixture.profile = "profile-b";
            fixture.name = Some("Fixture B");
            fixture.settle();
            assert!(fixture.state.clear_confirmation.is_none());
            assert_eq!(fixture.requests, 0);
            fixture.click("Clear…");
            assert_eq!(fixture.state.clear_confirmation.as_deref(), Some("profile-b"));
            fixture.name = None;
            fixture.settle();
            fixture.click("Clear…");
            assert!(fixture.state.clear_confirmation.is_none());
            assert_eq!(fixture.requests, 0);
        }
    }

    #[test]
    fn clearing_unconfirmed_and_partial_status_never_allows_duplicate_dispatch() {
        use crossbow_engine::{ClearDataStatus, ClearDataStep::*};
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for cookies in [Pending, Completed, Failed("Fixture rejected"), Unconfirmed("Fixture unconfirmed")] {
                let mut fixture = ClearingFixture::new(size);
                fixture.operation = Some((ClearDataStatus { cookies, cache: Pending }, std::time::Duration::from_secs(31)));
                let shown = fixture.settle();
                assert!(shown.text.iter().any(|(text, _, _)| text.contains("Completion is still unconfirmed")));
                fixture.click("Clearing…");
                assert!(fixture.state.clear_confirmation.is_none());
                assert_eq!(fixture.requests, 0);
                fixture.state = SettingsState::new(Page::Advanced, 1.0);
                fixture.settle();
                assert_eq!(fixture.requests, 0, "reopening Settings cannot dispatch or cancel the app-owned operation");
                fixture.operation = Some((ClearDataStatus { cookies: Completed, cache: Completed }, std::time::Duration::from_secs(32)));
                fixture.click("Clear…");
                fixture.click("Clear data");
                assert_eq!(fixture.requests, 1, "a confirmed terminal result requires another explicit confirmation");
            }
        }
    }

    #[test]
    fn clearing_error_readback_survives_settings_recreation_and_keeps_profile_identity() {
        use crossbow_engine::ClearDataStep;
        let fixture = Fixture::new();
        let mut data = BrowsingDataState::default();
        data.start(&fixture.dir, "profile-a");
        let before = data.status("profile-a").unwrap().0;
        assert!(matches!(before.cookies, ClearDataStep::Failed(_)), "this source test does not initialize Chromium");
        let mut settings = SettingsState::new(Page::Advanced, 0.0);
        settings.clear_confirmation = Some("profile-b".into());
        drop(settings);
        let reopened = SettingsState::new(Page::Advanced, 1.0);
        assert!(reopened.clear_confirmation.is_none());
        assert_eq!(data.status("profile-a").unwrap().0, before);
        assert!(data.status("profile-b").is_none());
        data.start(&fixture.dir, "profile-b");
        assert_eq!(data.status("profile-a").unwrap().0, before);
        assert_eq!(data.operations.len(), 2);
        assert!(!fixture.dir.join("Chromium").exists());
    }

    #[test]
    fn profile_preference_controls_show_failed_save_then_cancel_retry_and_reopen() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0] {
                let fixture = Fixture::new();
                let mut lib = fixture.read_only();
                let saved = lib.settings.clone();
                let profile = lib.default_profile_id();
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let mut state = SettingsState::new(Page::General, 0.0);
                let mut frame = 0;
                for cancel in [true, false] {
                    let mut shown = preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![]);
                    for _ in 0..5 { shown = preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![]); }
                    for label in ["Google", "DuckDuckGo"] {
                        let pos = shown.point(label);
                        for pressed in [true, false] {
                            shown = preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![
                                egui::Event::PointerMoved(pos),
                                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
                            ]);
                        }
                        for _ in 0..5 { shown = preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![]); }
                    }
                    assert_eq!(lib.settings, saved);
                    assert_eq!(state.profile_preferences(&lib, &profile).unwrap().search_engine, Some(SearchEngine::DuckDuckGo));
                    assert_eq!(lib.effective_profile_preferences(&profile).unwrap().search_engine, SearchEngine::Google);
                    let pending = state.pending_profile_preferences.as_ref().expect("failed UI save must remain recoverable");
                    shown.point(&pending.error);
                    shown.point("Retry Save");
                    shown.point("Cancel");
                    let revision = lib.revision;
                    for _ in 0..10 { preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![]); }
                    assert_eq!(lib.revision, revision, "idle frames must not retry the database write");
                    assert!(!state.pending_profile_preferences.as_ref().unwrap().reveal_error);
                    if !cancel {
                        lib = fixture.library();
                        for _ in 0..10 { shown = preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![]); }
                        assert!(lib.profiles[&profile].preferences.is_none(), "restored storage must wait for the explicit retry");
                    }
                    let pos = shown.point(if cancel { "Cancel" } else { "Retry Save" });
                    for pressed in [true, false] {
                        preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
                        ]);
                    }
                    for _ in 0..5 { preference_frame(&ctx, &mut state, &mut lib, size, scale, &mut frame, vec![]); }
                    assert!(state.pending_profile_preferences.is_none());
                    let expected = if cancel { SearchEngine::Google } else { SearchEngine::DuckDuckGo };
                    assert_eq!(lib.effective_profile_preferences(&profile).unwrap().search_engine, expected);
                    assert_eq!(fixture.library().effective_profile_preferences(&profile).unwrap().search_engine, expected);
                    assert_eq!(lib.settings, saved, "profile controls never modify shared preferences");
                }
                drop(lib);
                let reopened = fixture.library();
                let reopened_state = SettingsState::new(Page::General, 0.0);
                assert_eq!(reopened_state.profile_preferences(&reopened, &profile).unwrap().search_engine, Some(SearchEngine::DuckDuckGo));
            }
        }
    }

    #[test]
    fn preference_recovery_survives_page_changes_and_keyboard_activation() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for cancel in [false, true] {
                let fixture = Fixture::new();
                let mut lib = fixture.read_only();
                let saved = lib.settings.clone();
                let mut desired = saved.clone();
                desired.search_engine = SearchEngine::Bing;
                let mut state = SettingsState::new(Page::General, 0.0);
                state.save_preferences(&mut lib, desired.clone());
                state.page = Page::Advanced;
                lib = fixture.library();
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let mut frame = 0;
                for _ in 0..8 { preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
                assert!(state.pending_preferences.is_some(), "navigation must keep the unsaved draft");
                assert_eq!(lib.settings, saved, "opening another preference page must not retry the write");
                let keys = if cancel { vec![egui::Key::Tab, egui::Key::Tab, egui::Key::Enter] } else { vec![egui::Key::Tab, egui::Key::Enter] };
                for key in keys {
                    let events = [true, false].into_iter().map(|pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() }).collect();
                    preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, events);
                }
                assert!(state.pending_preferences.is_none(), "keyboard must activate the actual recovery control");
                let expected = if cancel { saved } else { desired };
                assert_eq!(lib.settings, expected);
                drop(lib);
                assert_eq!(fixture.library().settings, expected);
            }
        }
    }

    fn draft() -> Boost {
        Boost {
            name: "  Fixture Boost — exact draft  ".into(),
            host: "  https://www.example.com/  ".into(),
            css: "/* preserve whitespace */\r\nbody { color: red; }\n\n".into(),
            js: "// fixture only\n  document.title = 'Draft';\n".into(),
            enabled: false,
        }
    }

    #[test]
    fn boost_toggle_failure_retries_only_enabled_and_does_not_resurrect_deleted_rows() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let saved = draft();
        let id = lib.put_boost(None, &saved).unwrap();
        drop(lib);
        let mut readonly = fixture.read_only();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        state.set_boost_enabled(&mut readonly, &id, true);
        assert!(!readonly.boosts[&id].enabled);
        assert!(state.pending_boost_toggle.as_ref().unwrap().error.contains("Could not enable"));
        state.retry_boost_toggle(&mut readonly);
        assert_eq!(readonly.boosts[&id], saved);
        drop(readonly);
        let mut writable = fixture.library();
        let mut newer = saved.clone();
        newer.css = "body { color: blue; }".into();
        newer.name = "Updated fixture".into();
        writable.put_boost(Some(&id), &newer).unwrap();
        state.retry_boost_toggle(&mut writable);
        assert!(state.pending_boost_toggle.is_none());
        newer.enabled = true;
        assert_eq!(writable.boosts[&id], newer);
        drop(writable);
        assert_eq!(fixture.library().boosts[&id], newer);
        let mut readonly = fixture.read_only();
        state.set_boost_enabled(&mut readonly, &id, false);
        drop(readonly);
        let mut writable = fixture.library();
        writable.delete_boost(&id).unwrap();
        let revision = writable.revision;
        state.retry_boost_toggle(&mut writable);
        assert!(state.pending_boost_toggle.as_ref().unwrap().error.contains("no longer exists"));
        assert_eq!(writable.revision, revision);
        assert!(!writable.boosts.contains_key(&id));
        assert!(!fixture.library().boosts.contains_key(&id));
    }

    #[test]
    fn boost_toggle_cancel_and_already_applied_retry_do_not_write() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let saved = draft();
        let id = lib.put_boost(None, &saved).unwrap();
        drop(lib);
        let mut readonly = fixture.read_only();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        state.set_boost_enabled(&mut readonly, &id, true);
        state.pending_boost_toggle = None;
        assert_eq!(readonly.boosts[&id], saved);
        assert_eq!(fixture.library().boosts[&id], saved);
        state.set_boost_enabled(&mut readonly, &id, true);
        drop(readonly);
        let mut writable = fixture.library();
        let mut newer = saved;
        newer.enabled = true;
        writable.put_boost(Some(&id), &newer).unwrap();
        let revision = writable.revision;
        state.retry_boost_toggle(&mut writable);
        assert!(state.pending_boost_toggle.is_none());
        assert_eq!(writable.revision, revision, "an already-applied requested value is a no-op");
        assert_eq!(fixture.library().boosts[&id], newer);
    }

    #[test]
    fn boost_delete_failure_preserves_editor_and_retries_or_cancels() {
        for cancel in [false, true] {
            let fixture = Fixture::new();
            let mut lib = fixture.library();
            let saved = draft();
            let id = lib.put_boost(None, &saved).unwrap();
            drop(lib);
            let mut readonly = fixture.read_only();
            let mut state = SettingsState::new(Page::Boosts, 0.0);
            let mut edited = saved.clone();
            edited.css.push_str("/* exact unsaved edit */\n");
            state.boost = Some((Some(id.clone()), edited.clone()));
            for _ in 0..2 {
                state.delete_boost(&mut readonly);
                assert!(state.boost_delete_failed);
                assert!(state.boost_error.as_ref().unwrap().contains("Retry Delete"));
                assert_eq!(state.boost, Some((Some(id.clone()), edited.clone())));
                assert_eq!(readonly.boosts[&id], saved);
                assert_eq!(fixture.library().boosts[&id], saved);
            }
            drop(readonly);
            let mut writable = fixture.library();
            if cancel { state.cancel_boost(); } else { state.delete_boost(&mut writable); }
            assert!(state.boost.is_none() && state.boost_error.is_none());
            assert!(!state.boost_delete_failed);
            assert_eq!(writable.boosts.contains_key(&id), cancel);
            drop(writable);
            assert_eq!(fixture.library().boosts.contains_key(&id), cancel);
        }
    }

    #[test]
    fn failed_boost_delete_can_save_the_retained_draft_instead() {
        let fixture = Fixture::new();
        let mut lib = fixture.library();
        let saved = draft();
        let id = lib.put_boost(None, &saved).unwrap();
        drop(lib);
        let mut readonly = fixture.read_only();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        let mut edited = saved;
        edited.host = "example.com".into();
        edited.name = "Keep edited fixture".into();
        state.boost = Some((Some(id.clone()), edited.clone()));
        state.delete_boost(&mut readonly);
        state.save_boost(&mut readonly);
        assert!(!state.boost_delete_failed, "a Save failure must not retain Retry Delete semantics");
        assert!(state.boost_error.as_ref().unwrap().contains("Retry Save"));
        drop(readonly);
        let mut writable = fixture.library();
        state.save_boost(&mut writable);
        assert!(state.boost.is_none() && state.boost_error.is_none());
        assert_eq!(writable.boosts[&id], edited);
        drop(writable);
        assert_eq!(fixture.library().boosts[&id], edited);
    }

    #[test]
    fn boost_controls_show_and_recover_toggle_and_delete_failures() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let fixture = Fixture::new();
            let mut lib = fixture.library();
            let saved = Boost { name: "Fixture Boost".into(), host: "example.com".into(), css: String::new(), js: String::new(), enabled: false };
            let id = lib.put_boost(None, &saved).unwrap();
            drop(lib);
            let mut lib = fixture.read_only();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let mut state = SettingsState::new(Page::Boosts, 0.0);
            let mut frame = 0;
            for _ in 0..8 { preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            for key in [egui::Key::Tab, egui::Key::Tab, egui::Key::Space] {
                let events = [true, false].into_iter().map(|pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Default::default() }).collect();
                preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, events);
            }
            assert!(state.pending_boost_toggle.is_some(), "actual keyboard toggle must expose rejection");
            assert!(!lib.boosts[&id].enabled);
            let mut shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]);
            for _ in 0..6 { shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            shown.point(&state.pending_boost_toggle.as_ref().unwrap().error);
            let pos = shown.point("Retry");
            lib = fixture.library();
            for _ in 0..6 { preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            assert!(!lib.boosts[&id].enabled, "idle frames must not retry toggles");
            for pressed in [true, false] {
                preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(state.pending_boost_toggle.is_none());
            assert!(fixture.library().boosts[&id].enabled);
            for _ in 0..6 { shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            let pos = shown.point("Edit");
            for pressed in [true, false] {
                preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(state.boost.is_some());
            lib = fixture.read_only();
            for _ in 0..12 {
                shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![egui::Event::PointerMoved(size.to_pos2() * 0.5), egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -120.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() }]);
            }
            let pos = shown.point("Delete");
            for pressed in [true, false] {
                preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(state.boost_delete_failed && state.boost.is_some());
            for _ in 0..8 { shown = preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![]); }
            shown.point(state.boost_error.as_ref().unwrap());
            let pos = shown.point("Retry Delete");
            shown.point("Cancel");
            assert!(!state.boost_error_reveal);
            lib = fixture.library();
            for pressed in [true, false] {
                preference_frame(&ctx, &mut state, &mut lib, size, 1.0, &mut frame, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(state.boost.is_none() && state.boost_error.is_none());
            assert!(!fixture.library().boosts.contains_key(&id));
        }
    }

    #[test]
    fn failed_new_boost_save_retains_exact_draft_and_retries_once() {
        let fixture = Fixture::new();
        let original = draft();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        state.boost = Some((None, original.clone()));
        let mut lib = fixture.read_only();
        assert!(lib.put_boost(None, &original).is_err(), "fixture must reject real writes");

        state.save_boost(&mut lib);

        assert_eq!(state.boost, Some((None, original.clone())));
        assert!(state.boost_error.as_ref().unwrap().contains("Could not save"));
        assert!(lib.boosts.is_empty());
        state.save_boost(&mut lib);
        assert_eq!(state.boost, Some((None, original.clone())));
        assert!(state.boost_error.is_some());
        drop(lib);

        let mut writable = fixture.library();
        state.save_boost(&mut writable);
        assert!(state.boost.is_none());
        assert!(state.boost_error.is_none());
        assert_eq!(writable.boosts.len(), 1);
        let mut saved = original;
        saved.host = "example.com".into();
        assert_eq!(writable.boosts.values().next(), Some(&saved));
        drop(writable);
        let reopened = fixture.library();
        assert_eq!(reopened.boosts.len(), 1);
        assert_eq!(reopened.boosts.values().next(), Some(&saved));
    }

    #[test]
    fn failed_existing_boost_save_retries_latest_edits_under_the_same_id() {
        let fixture = Fixture::new();
        let mut writable = fixture.library();
        let saved = Boost { name: "Saved fixture".into(), host: "example.com".into(), css: String::new(), js: String::new(), enabled: true };
        let id = writable.put_boost(None, &saved).unwrap();
        drop(writable);
        let original = draft();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        state.boost = Some((Some(id.clone()), original.clone()));
        let mut lib = fixture.read_only();
        state.save_boost(&mut lib);
        assert_eq!(state.boost, Some((Some(id.clone()), original.clone())));
        assert_eq!(lib.boosts.get(&id), Some(&saved));
        drop(lib);

        // Editing remains possible after the failure, and retry persists the
        // latest buffer rather than an old normalized/snapshotted draft.
        let latest = &mut state.boost.as_mut().unwrap().1;
        latest.name.push_str(" after failure");
        latest.css.push_str("\n/* retry */\n");
        let mut expected = latest.clone();
        expected.host = "example.com".into();
        let mut writable = fixture.library();
        state.save_boost(&mut writable);
        assert!(state.boost.is_none());
        assert!(state.boost_error.is_none());
        assert_eq!(writable.boosts.len(), 1);
        assert_eq!(writable.boosts.get(&id), Some(&expected));
        drop(writable);
        let reopened = fixture.library();
        assert_eq!(reopened.boosts.len(), 1);
        assert_eq!(reopened.boosts.get(&id), Some(&expected));
    }

    #[test]
    fn failed_new_boost_save_can_cancel_without_creating_a_boost() {
        let fixture = Fixture::new();
        let mut lib = fixture.read_only();
        let original = draft();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        state.boost = Some((None, original.clone()));
        state.save_boost(&mut lib);
        assert_eq!(state.boost, Some((None, original)));
        assert!(state.boost_error.is_some());
        state.cancel_boost();
        assert!(state.boost.is_none());
        assert!(state.boost_error.is_none());
        drop(lib);
        assert!(fixture.library().boosts.is_empty());
    }

    #[test]
    fn failed_existing_boost_save_can_cancel_without_changing_saved_boost() {
        let fixture = Fixture::new();
        let mut writable = fixture.library();
        let saved = Boost { name: "Saved fixture".into(), host: "example.com".into(), css: "body {}".into(), js: String::new(), enabled: true };
        let id = writable.put_boost(None, &saved).unwrap();
        drop(writable);
        let original = draft();
        let mut state = SettingsState::new(Page::Boosts, 0.0);
        state.boost = Some((Some(id.clone()), original.clone()));
        let mut lib = fixture.read_only();

        state.save_boost(&mut lib);

        assert_eq!(state.boost, Some((Some(id.clone()), original)));
        assert!(state.boost_error.is_some());
        assert_eq!(lib.boosts.get(&id), Some(&saved));
        state.cancel_boost();
        assert!(state.boost.is_none());
        assert!(state.boost_error.is_none());
        drop(lib);
        let reopened = fixture.library();
        assert_eq!(reopened.boosts.len(), 1);
        assert_eq!(reopened.boosts.get(&id), Some(&saved));
    }

    // -- end-to-end encryption ---------------------------------------------------------

    use crate::sync_glue::sync_test_server::{Reply, Request, Server, SyncService};
    use crossbow_core::sync::key_store::{KeyStore, MemoryKeyStore};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// One signed-in device on a loopback sync service, rendering the real
    /// encryption section and polling the real sync glue every frame.
    pub(super) struct EncryptionFixture {
        pub(super) _database: Fixture,
        pub(super) _server: Server,
        pub(super) lib: Library,
        pub(super) sync: SyncGlue,
        pub(super) state: SettingsState,
        pub(super) ctx: egui::Context,
        pub(super) size: egui::Vec2,
        pub(super) frame: u32,
        /// Turn On Encryption… was drawn disabled in a frame since this was cleared.
        turn_on_disabled: bool,
    }

    impl EncryptionFixture {
        fn new(service: &Arc<SyncService>, device: &str, size: egui::Vec2) -> Self {
            let mut fixture = Self::signed_out(account_server(service, "fixture-account", device), size);
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            fixture
        }

        /// Signed out, so a test can sign in through the real UI and glue.
        pub(super) fn signed_out(server: Server, size: egui::Vec2) -> Self {
            Self::with_keys(server, size, Box::new(MemoryKeyStore::default()))
        }

        /// Signed out, with a key store the test keeps a handle on.
        fn with_keys(server: Server, size: egui::Vec2, keys: Box<dyn KeyStore>) -> Self {
            let database = Fixture::new();
            let lib = database.library();
            // In-memory key and session stores only.
            let mut sync = SyncGlue::with_key_store(&database.dir, &lib, || {}, keys);
            sync.set_base_url(&lib, &server.url).unwrap();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { _database: database, _server: server, lib, sync, state: SettingsState::new(Page::Account, 0.0), ctx, size, frame: 0, turn_on_disabled: false }
        }

        pub(super) fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::Account, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                let ctx = ui.ctx().clone();
                self.sync.poll(&mut self.lib, &ctx);
                // What Turn On Encryption… is drawn with in this frame.
                self.turn_on_disabled |= !self.sync.encryption_ready();
                let now = ui.ctx().input(|i| i.time);
                account_page(&mut self.state, &mut self.sync, &mut self.lib, ui, pal, now);
            })
        }

        pub(super) fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            self.frame(vec![])
        }

        /// Wait until this device is Locked and the scan a Locked device runs
        /// has read to the end of the stream. While it runs, a code entered
        /// waits for it instead of unlocking at once. Right after a restart,
        /// before the first frame starts that scan, the device already reads
        /// Locked and not checking.
        fn locked(&mut self, what: &str) {
            self.until(what, |f| f.status() == Some(E2eeStatus::Locked) && f.sync.encryption_scanned());
        }

        /// Where these labels sit once the page has stopped scrolling.
        fn at_rest(&mut self, labels: &[&str]) -> Vec<egui::Pos2> {
            let shown = self.visible(labels[0]);
            let mut at: Vec<_> = labels.iter().map(|label| shown.point(label)).collect();
            for _ in 0..20 {
                let shown = self.settle();
                let again: Vec<_> = labels.iter().map(|label| shown.point(label)).collect();
                if again == at { break; }
                at = again;
            }
            at
        }

        /// A primary click at `pos`, wherever the controls now are.
        fn press_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
        }

        pub(super) fn until(&mut self, what: &str, done: impl Fn(&mut Self) -> bool) {
            self.until_for(what, std::time::Duration::from_secs(10), done);
        }

        fn until_for(&mut self, what: &str, limit: std::time::Duration, done: impl Fn(&mut Self) -> bool) {
            let deadline = std::time::Instant::now() + limit;
            while !done(self) {
                self.frame(vec![]);
                assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}: status={:?} error={:?}", self.sync.status, self.sync.error);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        fn status(&mut self) -> Option<E2eeStatus> {
            self.sync.encryption_status(&self.lib)
        }

        pub(super) fn visible(&mut self, label: &str) -> PreferenceFrame {
            let scroll_point = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0).right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..24 {
                let shown = self.settle();
                let delta = match shown.text.iter().find(|(text, _, _)| text == label) {
                    Some((_, bounds, clip)) if clip.expand(0.5).contains_rect(*bounds) => return shown,
                    Some((_, bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 12 { 100.0 } else { -100.0 },
                };
                self.frame(vec![
                    egui::Event::PointerMoved(scroll_point),
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                ]);
            }
            let shown = self.settle();
            panic!("encryption control did not scroll into view: {}; size={:?}; text={:?}", redact(label), self.size, shown.labels());
        }

        pub(super) fn click(&mut self, label: &str) {
            let pos = self.visible(label).point(label);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            self.settle();
        }

        /// Press Turn On Encryption… by pointer, or by keyboard focus and Enter,
        /// once it is enabled. The fixture socket stays open, so nothing
        /// disables the button between the readiness check and the press, and
        /// a press that meets it disabled fails the test. Only the frames that
        /// can activate it count: a click needs the button enabled on press
        /// and release, Enter acts on its key-down.
        fn press_turn_on(&mut self, keyboard: bool) {
            const TURN_ON: &str = "Turn On Encryption…";
            self.until("Turn On Encryption… enabled", |f| f.sync.encryption_ready());
            let pos = if keyboard { self.focus(TURN_ON); None } else { Some(self.visible(TURN_ON).point(TURN_ON)) };
            self.turn_on_disabled = false;
            let missed = match pos {
                Some(pos) => {
                    for pressed in [true, false] {
                        self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
                    }
                    self.turn_on_disabled
                }
                None => {
                    let enter = |pressed| vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }];
                    self.frame(enter(true));
                    let missed = self.turn_on_disabled;
                    self.frame(enter(false));
                    missed
                }
            };
            assert!(!missed, "{TURN_ON} was disabled during the press at {:?}: status={:?}", self.size, self.sync.status);
            self.settle();
        }

        pub(super) fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
        }

        pub(super) fn focus(&mut self, label: &str) {
            for _ in 0..24 {
                self.key(egui::Key::Tab, egui::Modifiers::NONE);
                let shown = self.settle();
                let response = self.ctx.memory(|memory| memory.focused()).and_then(|id| self.ctx.read_response(id));
                if let Some(response) = response {
                    if shown.text.iter().any(|(text, bounds, _)| text == label && response.rect.contains(bounds.center())) { return; }
                }
            }
            panic!("encryption control not keyboard reachable: {}; size={:?}", redact(label), self.size);
        }

        /// The recovery code exactly as rendered, scrolled into view and checked
        /// to fit its column.
        fn shown_code(&mut self) -> Option<Hidden<String>> {
            let shown = self.settle();
            let text = shown.text.iter().find(|(text, _, _)| text.starts_with("CB1-"))?.0.clone();
            let shown = self.visible(&text);
            let (_, bounds, clip) = shown.text.iter().find(|(shown, _, _)| *shown == text).unwrap();
            assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5, "recovery code is clipped horizontally at {:?}", self.size);
            Some(Hidden::new(text))
        }

        /// Every label on the page stays inside the sheet's width.
        pub(super) fn fits_width(&mut self) {
            let shown = self.settle();
            for (text, bounds, clip) in &shown.text {
                assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5, "{:?} overflows the sheet at {:?}: {bounds:?} outside {clip:?}", redact(text), self.size);
            }
        }

        fn server_plaintext(service: &SyncService) -> String {
            service.rows().iter().filter(|r| r["enc"] != "xchacha20_v1").map(|r| r["payload"].as_str().unwrap().to_string()).collect()
        }

        fn plaintext_seen(service: &SyncService) -> bool {
            service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("Fixture Secret Space"))
        }

        /// Tab, as a keyboard user would, until this widget has focus.
        fn focus_id(&mut self, id: Id) {
            for _ in 0..24 {
                self.key(egui::Key::Tab, egui::Modifiers::NONE);
                self.settle();
                if self.ctx.memory(|memory| memory.focused()) == Some(id) { return; }
            }
            panic!("text field not keyboard reachable at {:?}", self.size);
        }

        /// The recovery code the glue holds, as tests compare it.
        fn code(&self) -> Option<Hidden<String>> {
            self.sync.recovery_code().as_ref().map(Hidden::from)
        }

        /// What the recovery code field holds, as tests compare it.
        fn typed(&self) -> Hidden<String> {
            Hidden::new(self.state.e2ee_code.as_str().to_owned())
        }

        /// Turn encryption on through the glue and wait for the sealed upload.
        fn turn_on(&mut self, service: &SyncService) -> Hidden<String> {
            self.sync.begin_encryption(&self.lib).unwrap();
            let code = Hidden::from(&self.sync.recovery_code().unwrap());
            self.until("turn-on confirmation", |f| f.sync.confirm_encryption(&mut f.lib).is_ok());
            self.until("ciphertext upload", |f| f.lib.store.dirty_count().unwrap() == 0 && service.rows().iter().any(|r| r["kind"] == "e2ee"));
            code
        }

        /// Quit with a restore waiting and open again: launch puts the restore in
        /// place before the library opens, then a new glue starts as at launch.
        fn relaunch_restored(&mut self, keys: Box<dyn KeyStore>) {
            let stand_in = Library::open(Store::open_in_memory().unwrap()).unwrap();
            drop(std::mem::replace(&mut self.sync, SyncGlue::with_key_store(&self._database.dir, &stand_in, || {}, Box::new(MemoryKeyStore::default()))));
            drop(std::mem::replace(&mut self.lib, stand_in));
            assert_eq!(crossbow_core::backup::finish_restore(&self._database.dir.join("fixture.sqlite")), Ok(true));
            self.lib = self._database.library();
            self.sync = SyncGlue::with_key_store(&self._database.dir, &self.lib, || {}, keys);
            self.state = SettingsState::new(Page::Account, 0.0);
        }

        /// Quit and relaunch: a new glue on the same library and key store.
        fn restart(&mut self, keys: Box<dyn KeyStore>) {
            self.sync = SyncGlue::with_key_store(&self._database.dir, &self.lib, || {}, keys);
            let url = self._server.url.clone();
            self.sync.set_base_url(&self.lib, &url).unwrap();
            self.sync.resume_fixture(&url, "fixture-account");
        }

        /// Sign in through the Account page: email, development code, Verify.
        pub(super) fn sign_in(&mut self) {
            self.focus_id(Id::unique("settings-email"));
            self.key(egui::Key::A, egui::Modifiers::COMMAND);
            self.frame(vec![egui::Event::Text("fixture@example.com".into())]);
            self.click("Get a Sign-in Code");
            self.until("sign-in code", |f| matches!(&f.sync.auth, AuthStep::Code { challenge_id, .. } if !challenge_id.is_empty()));
            self.click("123456");
            self.frame(vec![egui::Event::Text("123456".into())]);
            self.click("Verify & Sign In");
            self.until("sign-in answer", |f| !f.sync.busy);
        }

        /// The service tells this device, on its open sync socket, that
        /// another device's upload changed the account, as it does after
        /// every upload.
        fn changed(&self, service: &SyncService, by_device: &str) {
            let event = crossbow_core::proto::ServerEvent::Changed { cursor: service.cursor(), by_device: by_device.into() };
            assert_eq!(self._server.notify(&serde_json::to_string(&event).unwrap()), 1, "the device had no single sync socket open at {:?}", self.size);
        }
    }

    pub(super) fn sizes() -> [egui::Vec2; 2] {
        [vec2(640.0, 420.0), vec2(1360.0, 860.0)]
    }

    /// One account's loopback sync service with the real sign-in endpoints.
    fn account_server(service: &Arc<SyncService>, account: &str, device: &str) -> Server {
        Server::start(account_reply(service, account, device))
    }

    fn account_reply(service: &Arc<SyncService>, account: &str, device: &str) -> impl Fn(&Request) -> Reply + Send + Sync + 'static {
        let (shared, account, device) = (service.clone(), account.to_string(), device.to_string());
        move |request| shared.reply(request, &device).unwrap_or_else(|| {
            let mut identity = serde_json::json!({
                "account": { "id": account, "email": "fixture@example.com", "created_at": 1 },
                "device": { "id": device, "name": device, "platform": "macos", "created_at": 1, "last_seen_at": 1 },
            });
            match request.path.as_str() {
                // The Account page asks whether the server offers browser sign-in; this one doesn't.
                "/v1/health" => Reply::json(200, r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"fixture"}"#),
                "/v1/me" => {
                    identity["devices"] = serde_json::json!([]);
                    Reply::json(200, identity.to_string())
                }
                "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"fixture-challenge","expires_in":600,"dev_code":"123456"}"#),
                "/v1/auth/verify" => {
                    identity["session_token"] = serde_json::json!(format!("fixture-token-{account}"));
                    Reply::json(200, identity.to_string())
                }
                "/v1/auth/logout" => Reply::json(200, "{}"),
                "/v1/sync/ws" => Reply::socket(),
                path => panic!("unexpected fixture request: {path}"),
            }
        })
    }

    #[test]
    fn a_build_without_a_sync_server_says_so_and_offers_no_sign_in() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::signed_out(account_server(&service, "fixture-account", "device-a"), size);
            // As a source build starts on a new library: no server named anywhere.
            fixture.lib.store.kv_delete(crossbow_core::sync::BASE_URL_KEY).unwrap();
            fixture.sync = SyncGlue::with_key_store(&fixture._database.dir, &fixture.lib, || {}, Box::new(MemoryKeyStore::default()));
            let shown = fixture.visible(crossbow_core::sync::server::NO_SERVER);
            assert!(!shown.text.iter().any(|(text, _, _)| text == "Get a Sign-in Code" || text == "Email address"), "{:?}", shown.labels());
            fixture.visible("Sync server");
            fixture.fits_width();
            assert!(fixture._server.requests.lock().unwrap().is_empty(), "nothing contacted a server");

            // A server of the user's own brings sign-in back.
            let url = fixture._server.url.clone();
            fixture.sync.set_base_url(&fixture.lib, &url).unwrap();
            fixture.visible("Get a Sign-in Code");
        }
    }

    #[test]
    fn signing_out_of_an_encrypted_account_asks_first_and_keeps_the_record() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);
            fixture.click("Sign Out");
            fixture.visible(SIGN_OUT_WARNING);
            assert!(fixture.sync.signed_in(), "nothing happens before the user confirms");
            fixture.click("Cancel");
            assert!(fixture.sync.signed_in() && fixture.sync.has_library_key("fixture-account"));
            fixture.visible("Encryption is on");

            fixture.click("Sign Out");
            fixture.focus("Sign Out");
            fixture.key(egui::Key::Enter, egui::Modifiers::NONE);
            fixture.until("signed out", |f| !f.sync.signed_in());
            assert!(!fixture.sync.has_library_key("fixture-account"), "the user's own Sign Out removes the key");
            assert!(fixture.lib.store.kv_get("e2ee_library").unwrap().is_some(), "the account stays known as encrypted");
            fixture.visible("Get a Sign-in Code");
        }
    }

    #[test]
    fn encryption_turns_on_only_after_the_code_is_saved_and_shows_it_again_on_request() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            fixture.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            assert!(EncryptionFixture::plaintext_seen(&service), "fixture must observe plaintext before encryption");
            assert_eq!(fixture.status(), Some(E2eeStatus::Off));

            fixture.press_turn_on(true);
            assert_eq!(fixture.status(), Some(E2eeStatus::Confirming));
            let code = fixture.shown_code().expect("the new recovery code must be shown");
            assert_eq!(crossbow_core::sync::e2ee::RecoveryCode::parse(code.reveal()).unwrap().key_id(), crossbow_core::sync::e2ee::RecoveryCode::parse(fixture.sync.recovery_code().unwrap().as_str()).unwrap().key_id());
            // Turn On stays disabled until the user confirms they saved the code.
            fixture.click("Turn On");
            assert_eq!(fixture.status(), Some(E2eeStatus::Confirming));
            fixture.focus("I saved this recovery code");
            fixture.key(egui::Key::Space, egui::Modifiers::NONE);
            assert!(fixture.state.e2ee_saved);
            fixture.focus("Turn On");
            fixture.key(egui::Key::Enter, egui::Modifiers::NONE);
            fixture.settle();
            assert_eq!(fixture.status(), Some(E2eeStatus::On));
            fixture.visible("Encryption is on");
            assert!(fixture.shown_code().is_none(), "the code is hidden once encryption is on");

            fixture.until("ciphertext upload", |f| f.lib.store.dirty_count().unwrap() == 0 && service.rows().iter().any(|r| r["kind"] == "e2ee"));
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            assert!(!service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("Fixture Secret Space")));

            fixture.click("Show Recovery Code");
            assert_eq!(fixture.shown_code(), Some(code.clone()), "the code on screen is the one turned on");
            fixture.focus("Hide Recovery Code");
            fixture.key(egui::Key::Enter, egui::Modifiers::NONE);
            fixture.settle();
            assert!(fixture.shown_code().is_none());
            fixture.visible("Show Recovery Code");
            assert!(fixture.visible("Encryption is on").text.iter().all(|(text, _, _)| !text.contains("Turn Off")), "this version has no downgrade control");
        }
    }

    #[test]
    fn encryption_cancel_removes_the_new_key_and_keeps_syncing_json() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.press_turn_on(false);
            assert_eq!(fixture.status(), Some(E2eeStatus::Confirming));
            fixture.click("I saved this recovery code");
            fixture.click("Cancel");
            assert_eq!(fixture.status(), Some(E2eeStatus::Off));
            assert!(fixture.sync.recovery_code().is_none());
            assert!(fixture.shown_code().is_none());
            fixture.visible("Turn On Encryption…");
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            fixture.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            fixture.until("plaintext push", |_| EncryptionFixture::plaintext_seen(&service));
            assert!(service.rows().iter().all(|r| r["enc"] == "json" && r["kind"] != "e2ee"));
            // A new setup creates a different key; the cancelled one is gone.
            fixture.until("ready for a new setup", |f| f.sync.encryption_ready());
            fixture.press_turn_on(false);
            assert!(fixture.shown_code().is_some());
            fixture.click("Cancel");
        }
    }

    #[test]
    fn locked_device_rejects_wrong_codes_inline_and_unlocks_with_the_right_one() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            let space = owner.lib.spaces_ordered()[0].0.clone();
            owner.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = owner.turn_on(&service);

            let mut device = EncryptionFixture::new(&service, "device-b", size);
            device.until("locked state", |f| f.status() == Some(E2eeStatus::Locked));
            assert!(matches!(&device.sync.status, SyncStatus::Offline(e) if e.contains("paused")));
            assert!(!device.lib.spaces.contains_key(&space), "nothing is applied while locked");
            device.visible("Recovery code");
            device.visible("Unlock");
            device.visible("Enter the recovery code below to sync this device.");
            device.fits_width();

            // A well-formed code for another key is rejected and nothing unlocks.
            let other = crossbow_core::sync::e2ee::RecoveryCode::for_key(&crossbow_core::sync::e2ee::LibraryKey::generate().unwrap());
            device.click("CB1-XXXX-XXXX-…");
            device.frame(vec![egui::Event::Text(other.as_str().into())]);
            device.click("Unlock");
            device.visible("That recovery code doesn't unlock this account's library. Check it and try again.");
            assert_eq!(device.status(), Some(E2eeStatus::Locked));

            // A typo is caught before decryption.
            let mut typo = code.reveal().clone().into_bytes();
            let at = typo.len() - 1;
            typo[at] = if typo[at] == b'A' { b'B' } else { b'A' };
            let field = Id::unique("settings-recovery-code");
            device.focus_id(field);
            device.key(egui::Key::A, egui::Modifiers::COMMAND);
            device.frame(vec![egui::Event::Text(String::from_utf8(typo).unwrap())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.visible("That recovery code has a typo. Check each group and try again.");
            assert_eq!(device.status(), Some(E2eeStatus::Locked));

            // The right code, typed loosely, unlocks from the keyboard.
            device.focus_id(field);
            device.key(egui::Key::A, egui::Modifiers::COMMAND);
            device.frame(vec![egui::Event::Text(code.reveal().to_ascii_lowercase().replace('-', " "))]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("unlocked library", |f| f.status() == Some(E2eeStatus::On) && f.lib.spaces.contains_key(&space));
            assert!(device.state.e2ee_code.is_empty() && device.state.e2ee_error.is_none());
            assert_eq!(device.lib.spaces[&space].name, "Fixture Secret Space");
            device.visible("Encryption is on");
            device.until("encrypted upload from the unlocked device", |f| f.lib.store.dirty_count().unwrap() == 0);
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            assert!(!matches!(&device.sync.status, SyncStatus::Offline(e) if e.contains("paused")));
        }
    }

    #[test]
    fn turning_on_names_items_too_large_to_encrypt_and_changes_nothing() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            let boost = Boost { name: "Large".into(), host: "large.example".into(), css: "x".repeat(50_000), js: String::new(), enabled: true };
            fixture.lib.put_boost(None, &boost).unwrap();
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            assert!(service.rows().iter().any(|r| r["kind"] == "boost" && r["enc"] == "json"), "the large Boost synced unencrypted");
            fixture.press_turn_on(false);
            fixture.visible("1 item is too large to encrypt (Boost). Shrink or delete it, then try again.");
            assert_eq!(fixture.status(), Some(E2eeStatus::Off));
            assert!(fixture.sync.recovery_code().is_none() && fixture.shown_code().is_none());
            assert!(!fixture.sync.has_library_key("fixture-account"));
            fixture.visible("Turn On Encryption…");
        }
    }

    const RESET_DONE: &str = "Encryption was reset. Other devices need the new recovery code to sync.";

    #[test]
    fn resetting_encryption_needs_the_new_code_saved_and_can_be_cancelled() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let old = fixture.turn_on(&service);

            // Cancel keeps the current key.
            fixture.click("Reset Encryption…");
            fixture.visible(RESET_WARNING);
            fixture.fits_width();
            assert_eq!(fixture.status(), Some(E2eeStatus::Resetting));
            assert_ne!(fixture.shown_code().map(|shown| shown.map(|text| text.replace('\n', "-"))), Some(old.clone()), "the reset shows a new code");
            fixture.click("Cancel");
            assert_eq!(fixture.status(), Some(E2eeStatus::On));
            assert_eq!(fixture.code(), Some(old.clone()), "Cancel keeps the current code");

            fixture.click("Reset Encryption…");
            let new = fixture.shown_code().unwrap();
            fixture.click("Reset Encryption");
            assert_eq!(fixture.status(), Some(E2eeStatus::Resetting), "the reset waits until the new code is saved");
            fixture.focus("I saved this recovery code");
            fixture.key(egui::Key::Space, egui::Modifiers::NONE);
            fixture.until("the reset checked the server", |f| f.sync.reset_ready());
            fixture.focus("Reset Encryption");
            fixture.key(egui::Key::Enter, egui::Modifiers::NONE);
            fixture.settle();
            assert_eq!(fixture.status(), Some(E2eeStatus::On));
            fixture.until("re-encrypted upload", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.encryption_notice().is_some());
            fixture.visible(RESET_DONE);
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            fixture.click("Show Recovery Code");
            assert_eq!(fixture.shown_code(), Some(new.clone()), "the new code stays on screen");
            assert_eq!(crossbow_core::sync::e2ee::RecoveryCode::parse(new.reveal()).unwrap().key_id(), crossbow_core::sync::e2ee::RecoveryCode::parse(fixture.sync.recovery_code().unwrap().as_str()).unwrap().key_id());
            fixture.visible("The recovery code can't be changed except by resetting encryption.");
        }
    }

    #[test]
    fn an_unreadable_encryption_state_explains_itself_and_offers_a_reset() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            fixture.lib.store.kv_set("e2ee_library", "not json").unwrap();
            fixture.until("unreadable state", |f| f.status() == Some(E2eeStatus::Unreadable) && matches!(&f.sync.status, SyncStatus::Offline(e) if e.contains("paused")));
            fixture.visible(UNREADABLE);
            fixture.visible("Paused");
            fixture.visible("This device's encryption state can't be read.");
            fixture.fits_width();
            assert!(service.rows().is_empty(), "nothing is uploaded while the state is unreadable");
            assert!(fixture.visible(UNREADABLE).text.iter().all(|(text, _, _)| text != "Unlock"), "an unreadable state is not a recovery-code prompt");

            // Reset is never the only way out: Try Again reads the record again.
            fixture.click("Try Again");
            fixture.until("unreadable again", |f| f.status() == Some(E2eeStatus::Unreadable) && matches!(&f.sync.status, SyncStatus::Offline(e) if e.contains("paused")));
            fixture.visible(UNREADABLE);
            fixture.visible("Try Again");
            fixture.fits_width();
            assert!(service.rows().is_empty());

            fixture.click("Reset Encryption…");
            fixture.visible(RESET_WARNING);
            fixture.fits_width();
            fixture.click("I saved this recovery code");
            fixture.until("the reset checked the server", |f| f.sync.reset_ready());
            fixture.click("Reset Encryption");
            assert_eq!(fixture.status(), Some(E2eeStatus::On));
            fixture.until("encrypted upload", |f| f.lib.store.dirty_count().unwrap() == 0 && !service.rows().is_empty());
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            fixture.visible("Encryption is on");
        }
    }

    #[test]
    fn a_declined_keychain_read_can_be_retried_without_restarting() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let keys = MemoryKeyStore::default();
            let mut fixture = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-a"), size, Box::new(keys.clone()));
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);

            // At the next launch the keychain prompt is declined.
            keys.fail_reads(true);
            fixture.restart(Box::new(keys.clone()));
            fixture.until("locked by the keychain", |f| f.status() == Some(E2eeStatus::Locked));
            fixture.visible("Secure key storage is unavailable or access was declined.");
            fixture.fits_width();
            assert!(keys.contains("fixture-account"), "the key is still stored; only the read failed");
            keys.fail_reads(false);
            fixture.click("Try Keychain Again");
            fixture.until("unlocked from the keychain", |f| f.status() == Some(E2eeStatus::On));
            fixture.visible("Encryption is on");
            assert!(fixture.sync.e2ee_error.is_none());
            assert!(fixture.settle().text.iter().all(|(text, _, _)| text != "Try Keychain Again"));
        }
    }

    /// Hide or restore the record's table from a second connection, as a
    /// momentary SQLite failure would.
    fn hide_records(fixture: &EncryptionFixture, hidden: bool) {
        let connection = rusqlite::Connection::open(fixture._database.dir.join("fixture.sqlite")).unwrap();
        let (from, to) = if hidden { ("kv", "kv_hidden") } else { ("kv_hidden", "kv") };
        connection.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}")).unwrap();
    }

    #[test]
    fn a_failed_encryption_read_pauses_sync_and_try_again_resumes_it() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let keys = MemoryKeyStore::default();
            let mut fixture = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-a"), size, Box::new(keys.clone()));
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            fixture.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();

            // At the next launch the record can't be read for a moment.
            let rows = service.rows();
            hide_records(&fixture, true);
            fixture.restart(Box::new(keys.clone()));
            fixture.until("paused by the failed read", |f| f.status() == Some(E2eeStatus::Unavailable) && matches!(&f.sync.status, SyncStatus::Offline(e) if e.contains("paused")));
            fixture.visible(UNAVAILABLE);
            fixture.visible("Paused");
            fixture.visible("This device's encryption state couldn't be read.");
            fixture.visible("Try Again");
            fixture.fits_width();
            assert!(fixture.settle().text.iter().all(|(text, _, _)| text != "Reset Encryption…" && text != "Unlock"), "a failed read offers neither a reset nor a code prompt");
            assert!(!shows(&fixture.settle(), ENCRYPTION_HOLD), "a failed read is not a library waiting for encryption");
            assert_eq!(service.rows(), rows, "nothing is uploaded while the record can't be read");

            hide_records(&fixture, false);
            fixture.click("Try Again");
            fixture.until("encrypted again", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0);
            fixture.visible("Encryption is on");
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            assert!(!EncryptionFixture::plaintext_seen(&service));
        }
    }

    #[test]
    fn the_code_this_device_used_before_is_named_after_another_device_resets() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut a = EncryptionFixture::new(&service, "device-a", size);
            let space = a.lib.spaces_ordered()[0].0.clone();
            a.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            a.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let old = a.turn_on(&service);

            // Device B joins with that code, then resets encryption.
            let mut b = EncryptionFixture::new(&service, "device-b", size);
            b.locked("locked");
            b.sync.unlock_encryption(&b.lib, old.reveal()).unwrap();
            b.until("joined", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            b.sync.begin_reset_encryption(&b.lib).unwrap();
            let new = Hidden::from(&b.sync.recovery_code().unwrap());
            b.until("the reset checked the server", |f| f.sync.reset_ready());
            b.sync.confirm_reset_encryption(&mut b.lib).unwrap();
            b.until("sealed again under the new key", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());

            a.sync.sync_now();
            a.locked("locked by the reset");
            let field = Id::unique("settings-recovery-code");
            a.focus_id(field);
            a.frame(vec![egui::Event::Text(old.reveal().clone())]);
            a.key(egui::Key::Enter, egui::Modifiers::NONE);
            a.visible(crossbow_core::sync::e2ee::USED_BEFORE);
            a.fits_width();
            assert_eq!(a.status(), Some(E2eeStatus::Locked), "the earlier key is never adopted");

            a.focus_id(field);
            a.key(egui::Key::A, egui::Modifiers::COMMAND);
            a.frame(vec![egui::Event::Text(new.reveal().clone())]);
            a.key(egui::Key::Enter, egui::Modifiers::NONE);
            a.until("unlocked with the newest code", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert_eq!(a.lib.spaces[&space].name, "Fixture Secret Space");
            a.visible("Encryption is on");
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    #[test]
    fn a_synced_change_that_wont_decrypt_is_skipped_with_a_notice() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            fixture.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);
            // A server replays the sealed space under a newer clock, so it
            // can't open here; the rest of sync carries on.
            service.rewrite("space", &space, |row| row["hlc"] = serde_json::json!("2100000000000-000000-server"));
            fixture.sync.sync_now();
            fixture.until("page applied", |f| f.lib.store.kv_get("sync_cursor").unwrap() == Some(service.cursor().to_string()));
            fixture.visible("1 synced item couldn't be decrypted on this device and wasn't applied. It may come from a device still using an earlier recovery code, or it may have been altered.");
            fixture.fits_width();
            assert_eq!(fixture.lib.spaces[&space].name, "Fixture Secret Space");
            assert!(!matches!(&fixture.sync.status, SyncStatus::Offline(e) if e.contains("paused") || e.contains("stopped")));
            assert_eq!(fixture.lib.store.dirty_count().unwrap(), 0, "the skipped row is not replaced");
        }
    }

    #[test]
    fn turning_on_names_items_whose_sync_clock_cant_advance_and_changes_nothing() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::new(&service, "device-a", size);
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            service.rewrite("item", "at-the-limit", |row| {
                row["hlc"] = serde_json::json!("9999999999999-999999-legacy");
                row["payload"] = serde_json::json!(r#"{"kind":"pinned","url":"https://limit.example/","title":"Limit"}"#);
            });
            fixture.sync.sync_now();
            fixture.until("row at the clock limit", |f| f.lib.store.entity_hlc("item", "at-the-limit").unwrap().is_some() && f.sync.encryption_ready());
            fixture.press_turn_on(false);
            fixture.visible("1 synced item has a clock that can't be advanced, so it can't be encrypted. It's stored on this device even if it isn't shown.");
            fixture.fits_width();
            assert_eq!(fixture.status(), Some(E2eeStatus::Off));
            assert!(fixture.sync.recovery_code().is_none() && !fixture.sync.has_library_key("fixture-account"));
            fixture.visible("Turn On Encryption…");
        }
    }

    /// The Dismiss control sits on its notice's row, not somewhere else.
    fn dismiss_beside(shown: &PreferenceFrame, notice: &str) {
        let at = |label: &str| shown.text.iter().find(|(text, _, _)| text == label).unwrap_or_else(|| panic!("not shown: {}", redact(label))).1;
        let (notice, dismiss) = (at(notice), at("Dismiss"));
        assert!(dismiss.top() <= notice.bottom() + 8.0 && dismiss.bottom() >= notice.top(), "Dismiss at {dismiss:?} isn't beside its notice at {notice:?}");
    }

    #[test]
    fn an_encryption_notice_stays_after_a_restart_until_it_is_dismissed() {
        const SKIPPED: &str = "1 synced item couldn't be decrypted on this device and wasn't applied. It may come from a device still using an earlier recovery code, or it may have been altered.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let keys = MemoryKeyStore::default();
            let mut fixture = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-a"), size, Box::new(keys.clone()));
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            fixture.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);
            service.rewrite("space", &space, |row| row["hlc"] = serde_json::json!("2100000000000-000000-server"));
            fixture.sync.sync_now();
            fixture.until("page applied", |f| f.lib.store.kv_get("sync_cursor").unwrap() == Some(service.cursor().to_string()));
            fixture.visible(SKIPPED);
            dismiss_beside(&fixture.visible("Dismiss"), SKIPPED);
            fixture.fits_width();

            // Relaunched, the count is still there until the user dismisses it.
            fixture.restart(Box::new(keys.clone()));
            fixture.until("on after the restart", |f| f.status() == Some(E2eeStatus::On) && f.sync.last_sync_ms.is_some());
            fixture.visible(SKIPPED);
            dismiss_beside(&fixture.visible("Dismiss"), SKIPPED);
            fixture.fits_width();
            fixture.click("Dismiss");
            assert!(fixture.sync.encryption_notice().is_none());
            assert!(fixture.settle().text.iter().all(|(text, _, _)| text != SKIPPED && text != "Dismiss"));

            fixture.restart(Box::new(keys.clone()));
            fixture.until("on after another restart", |f| f.status() == Some(E2eeStatus::On) && f.sync.last_sync_ms.is_some());
            assert!(fixture.sync.encryption_notice().is_none(), "a dismissed notice came back");
            assert!(fixture.visible("Encryption is on").text.iter().all(|(text, _, _)| text != SKIPPED && text != "Dismiss"));
        }
    }

    #[test]
    fn a_reset_outrun_by_another_devices_reset_says_so_and_takes_that_code() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut a = EncryptionFixture::new(&service, "device-a", size);
            let space = a.lib.spaces_ordered()[0].0.clone();
            a.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            a.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let old = a.turn_on(&service);
            let mut b = EncryptionFixture::new(&service, "device-b", size);
            b.locked("locked");
            b.sync.unlock_encryption(&b.lib, old.reveal()).unwrap();
            b.until("joined", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            a.sync.sync_now();
            a.until("A caught up with B", |f| f.lib.store.kv_get("sync_cursor").unwrap() == Some(service.cursor().to_string()));

            // A resets; then B resets too, after A's marker reached the server.
            a.sync.begin_reset_encryption(&a.lib).unwrap();
            a.until("A's reset checked the server", |f| f.sync.reset_ready());
            a.sync.confirm_reset_encryption(&mut a.lib).unwrap();
            a.until("A sealed again", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none() && f.sync.encryption_notice().is_some());
            a.visible(RESET_DONE);
            b.sync.begin_reset_encryption(&b.lib).unwrap();
            let newest = Hidden::from(&b.sync.recovery_code().unwrap());
            b.until("B's reset checked the server", |f| f.sync.reset_ready());
            b.sync.confirm_reset_encryption(&mut b.lib).unwrap();
            b.until("B sealed again", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());

            // A never keeps claiming its reset worked.
            a.sync.sync_now();
            a.locked("A locked by B's reset");
            // Any counts follow; the notice leads with what happened to the reset.
            let notice = a.sync.encryption_notice().unwrap().to_string();
            assert!(notice.starts_with(crossbow_core::sync::e2ee::OUTRUN), "{notice}");
            a.visible(&notice);
            dismiss_beside(&a.visible("Dismiss"), &notice);
            a.fits_width();
            assert!(a.settle().text.iter().all(|(text, _, _)| !text.contains(RESET_DONE)), "a stale success notice stayed");

            let field = Id::unique("settings-recovery-code");
            a.focus_id(field);
            a.frame(vec![egui::Event::Text(newest.reveal().clone())]);
            a.key(egui::Key::Enter, egui::Modifiers::NONE);
            a.until("A unlocked with B's code", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(a.visible("Encryption is on").text.iter().all(|(text, _, _)| !text.contains(crossbow_core::sync::e2ee::OUTRUN)));
            assert_eq!(a.lib.spaces[&space].name, "Fixture Secret Space");
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    #[test]
    fn a_sign_in_stops_when_this_devices_encryption_record_cant_be_read() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = EncryptionFixture::signed_out(account_server(&service, "fixture-account", "device-a"), size);
            fixture.sign_in();
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);
            fixture.click("Sign Out");
            fixture.click("Sign Out");
            fixture.until("signed out", |f| !f.sync.signed_in());

            // The record can't be read while the same account signs in again.
            let record = fixture.lib.store.kv_get("e2ee_library").unwrap().unwrap();
            let other = rusqlite::Connection::open(fixture._database.dir.join("fixture.sqlite")).unwrap();
            other.execute("UPDATE kv SET v = X'FF' WHERE k = 'e2ee_library'", []).unwrap();
            assert!(fixture.lib.store.kv_get("e2ee_library").is_err());
            let (rows, mark) = (service.rows(), fixture._server.requests.lock().unwrap().len());
            let logouts = fixture._server.requests.lock().unwrap().iter().filter(|r| r.path == "/v1/auth/logout").count();
            fixture.sign_in();
            fixture.visible(crossbow_core::sync::e2ee::SIGN_IN_STOPPED);
            fixture.fits_width();
            assert!(!fixture.sync.signed_in());
            fixture._server.wait_for("/v1/auth/logout", logouts + 1);
            assert!(!fixture._server.requests.lock().unwrap()[mark..].iter().any(|r| r.path.starts_with("/v1/sync/")), "the stopped sign-in synced");
            assert_eq!(service.rows(), rows);
            assert_eq!(fixture.lib.store.kv_get("sync_session").unwrap(), None, "the stopped sign-in was saved");
            fixture.visible("Get a Sign-in Code");

            // Readable again, the same sign-in goes through, Locked until its code.
            other.execute("UPDATE kv SET v = ?1 WHERE k = 'e2ee_library'", [&record]).unwrap();
            fixture.sign_in();
            fixture.until("locked", |f| f.status() == Some(E2eeStatus::Locked));
            fixture.visible("Recovery code");
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    #[test]
    fn sign_out_asks_first_whenever_a_key_may_be_stored_here() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let keys = MemoryKeyStore::default();
            let mut fixture = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-a"), size, Box::new(keys.clone()));
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);

            // Locked only because the keychain read failed: the key is still stored.
            keys.fail_reads(true);
            fixture.restart(Box::new(keys.clone()));
            fixture.locked("locked by the keychain");
            fixture.click("Sign Out");
            fixture.visible(SIGN_OUT_WARNING);
            fixture.fits_width();
            assert!(fixture.sync.signed_in(), "nothing happens before the user confirms");
            fixture.click("Cancel");
            assert!(fixture.sync.signed_in() && keys.contains("fixture-account"));

            // A reset started from Locked says Sign Out cancels it.
            fixture.click("Reset Encryption…");
            assert_eq!(fixture.status(), Some(E2eeStatus::Resetting));
            fixture.click("Sign Out");
            fixture.visible(SIGN_OUT_RESETTING);
            fixture.fits_width();
            fixture.click("Sign Out");
            fixture.until("signed out", |f| !f.sync.signed_in());
            assert!(!keys.contains("fixture-account"), "the user's own Sign Out removes the key");
            fixture.visible("Get a Sign-in Code");

            // A device that never stored a key has nothing to warn about.
            let mut other = EncryptionFixture::new(&service, "device-b", size);
            other.until("locked", |f| f.status() == Some(E2eeStatus::Locked));
            other.click("Sign Out");
            assert!(!other.sync.signed_in(), "a device without a key asked before signing out");
            assert!(other.settle().text.iter().all(|(text, _, _)| text != SIGN_OUT_WARNING));
        }
    }

    #[test]
    fn a_key_that_cant_be_stored_on_unlock_or_reset_is_explained_and_retried() {
        const UNLOCK_UNSAVED: &str = "The code is correct, but it couldn't be stored. Secure key storage is unavailable or access was declined.";
        const RESET_UNSAVED: &str = "Couldn't store the new key, so the reset wasn't applied. Try again, or Cancel to keep the current code.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = owner.turn_on(&service);

            // Unlock: the code is right, but the key store refuses it.
            let keys = MemoryKeyStore::default();
            let mut device = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-b"), size, Box::new(keys.clone()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            keys.fail_writes(true);
            let field = Id::unique("settings-recovery-code");
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("checked", |f| !f.sync.checking_encryption());
            device.visible(UNLOCK_UNSAVED);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            assert!(!keys.contains("fixture-account"));
            keys.fail_writes(false);
            device.until("checked", |f| !f.sync.checking_encryption());
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(device.settle().text.iter().all(|(text, _, _)| text != UNLOCK_UNSAVED));

            // Reset: the key store refuses the new key, so the reset isn't
            // applied, and Cancel keeps the current code.
            device.click("Reset Encryption…");
            device.shown_code().unwrap();
            device.click("I saved this recovery code");
            device.until("the reset checked the server", |f| f.sync.reset_ready());
            let rows = service.rows();
            keys.fail_writes(true);
            device.click("Reset Encryption");
            device.visible(RESET_UNSAVED);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Resetting), "the new code stays on screen");
            device.settle();
            assert_eq!(service.rows(), rows, "the reset reached the server");
            device.click("Cancel");
            assert_eq!(device.status(), Some(E2eeStatus::On));
            assert_eq!(device.code(), Some(code.clone()), "Cancel keeps the previous code");
            assert!(device.settle().text.iter().all(|(text, _, _)| text != RESET_UNSAVED));

            // Try Again stores the same new key.
            device.click("Reset Encryption…");
            let new = device.shown_code().unwrap();
            device.click("I saved this recovery code");
            device.until("the reset checked the server", |f| f.sync.reset_ready());
            device.click("Reset Encryption");
            device.visible(RESET_UNSAVED);
            assert_eq!(device.shown_code(), Some(new.clone()), "Try Again shows the same new code");
            keys.fail_writes(false);
            device.until("ready to retry", |f| f.sync.reset_ready());
            device.click("Reset Encryption");
            assert_eq!(device.status(), Some(E2eeStatus::On));
            device.until("sealed again", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            device.visible(RESET_DONE);
            assert!(device.settle().text.iter().all(|(text, _, _)| text != RESET_UNSAVED));
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    /// A key store that quits the app (panics) while it stores a key, once
    /// `quit` is set: the app stops between the reset's record and its key.
    struct QuitOnSave {
        keys: MemoryKeyStore,
        quit: Arc<AtomicBool>,
    }

    impl KeyStore for QuitOnSave {
        fn load(&self, account: &str) -> Result<Option<crossbow_core::sync::e2ee::LibraryKey>, String> {
            self.keys.load(account)
        }

        fn save(&self, account: &str, key: &crossbow_core::sync::e2ee::LibraryKey) -> Result<(), String> {
            assert!(!self.quit.load(Ordering::SeqCst), "fixture: the app quit while the new key was being stored");
            self.keys.save(account, key)
        }

        fn clear(&self, account: &str) -> Result<(), String> {
            self.keys.clear(account)
        }
    }

    #[test]
    fn a_reset_cut_short_by_a_quit_is_undone_by_the_previous_code_until_it_reached_the_server() {
        const RESET_SENT: &str = "That's the recovery code this device used before it reset encryption, and the reset already reached the sync server. Enter the new recovery code.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let (keys, quit) = (MemoryKeyStore::default(), Arc::new(AtomicBool::new(false)));
            let store = || Box::new(QuitOnSave { keys: keys.clone(), quit: quit.clone() });
            let mut device = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-a"), size, store());
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = device.turn_on(&service);
            let field = Id::unique("settings-recovery-code");
            let rows = service.rows();
            let quit_during_reset = |device: &mut EncryptionFixture| -> Hidden<String> {
                device.click("Reset Encryption…");
                let new = Hidden::from(&device.sync.recovery_code().unwrap());
                device.click("I saved this recovery code");
                device.until("the reset checked the server", |f| f.sync.reset_ready());
                quit.store(true, Ordering::SeqCst);
                let (sync, lib) = (&mut device.sync, &mut device.lib);
                assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync.confirm_reset_encryption(lib))).is_err());
                quit.store(false, Ordering::SeqCst);
                device.restart(store());
                device.locked("locked after the restart");
                new
            };

            // The code from before the reset undoes it while nothing was uploaded.
            quit_during_reset(&mut device);
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("undone", |f| f.status() == Some(E2eeStatus::On) && !f.sync.checking_encryption());
            assert_eq!(device.code(), Some(code.clone()), "the code from before the reset is kept");
            device.visible("Encryption is on");
            device.settle();
            assert_eq!(service.rows(), rows, "the undone reset reached the server");

            // Once the new marker has reached the server (here: marked as
            // uploaded), that code is refused and the new one finishes it.
            let new = quit_during_reset(&mut device);
            let marker = device.lib.store.entity_hlc("e2ee", "library-v1").unwrap().unwrap();
            device.lib.store.mark_clean("e2ee", "library-v1", &marker).unwrap();
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("checked", |f| !f.sync.checking_encryption());
            device.visible(RESET_SENT);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(new.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("finished", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert_eq!(device.code(), Some(new.clone()), "the new code is kept");
            assert!(device.settle().text.iter().all(|(text, _, _)| text != RESET_SENT));
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    /// Two reset tests below (`a_reset_cut_short_…`) failed under full-suite
    /// load: a code entered after a restart met the scan the restart starts
    /// (Unlock was then disabled while it ran, so Enter did nothing; AA1),
    /// or that scan's Checking line moved Reset Encryption… between measuring
    /// and clicking it. Here the scan is held for 400 ms, and the fixture's
    /// wait for Locked must outlast it, so the code is typed after the scan.
    /// A code entered during the scan is the next test.
    #[test]
    fn the_fixtures_locked_wait_outlasts_the_scan_a_restart_starts() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = owner.turn_on(&service);
            let keys = MemoryKeyStore::default();
            let (server, _, released) = switchable_server(&service, "device-b");
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(keys.clone()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            released.store(false, Ordering::SeqCst);
            device.restart(Box::new(keys.clone()));
            let gate = released.clone();
            let release = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(400));
                gate.store(true, Ordering::SeqCst);
            });
            device.locked("locked after the restart");
            device.frame(vec![]);
            assert!(!device.sync.checking_encryption(), "the scan the restart starts was still to come at {size:?}");
            device.focus_id(Id::unique("settings-recovery-code"));
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On));
            release.join().unwrap();
        }
    }

    /// AA1: Enter in the code field while the scan a restart starts is
    /// still reading the stream. The code waits for that scan, the field
    /// keeps focus and is wiped, the Checking line is in view at both sizes,
    /// and the device unlocks when the scan ends with no second press.
    #[test]
    fn enter_during_the_restart_scan_waits_for_it_and_unlocks() {
        const FIELD: &str = "settings-recovery-code";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = owner.turn_on(&service);
            let keys = MemoryKeyStore::default();
            let (server, _, released) = switchable_server(&service, "device-b");
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(keys.clone()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            released.store(false, Ordering::SeqCst);
            device.restart(Box::new(keys.clone()));
            device.until("scan running", |f| f.status() == Some(E2eeStatus::Locked) && f.sync.checking_encryption());
            device.focus_id(Id::unique(FIELD));
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            assert!(device.sync.unlock_waiting(), "Enter during the scan was dropped at {size:?}");
            // The page scrolls the line into view, animated as the page scrolls.
            let mut shown = device.settle();
            for _ in 0..60 {
                if shown.text.iter().any(|(text, bounds, clip)| text == CHECKING_LOCKED && clip.expand(0.5).contains_rect(*bounds)) { break; }
                shown = device.frame(vec![]);
            }
            assert!(device.sync.checking_encryption(), "the scan ended early at {size:?}");
            assert_eq!(device.ctx.memory(|memory| memory.focused()), Some(Id::unique(FIELD)), "the field lost focus at {size:?}");
            assert!(device.state.e2ee_code.is_empty(), "the entered code stays in the field at {size:?}");
            let (_, bounds, clip) = shown.text.iter().find(|(text, _, _)| text == CHECKING_LOCKED).unwrap_or_else(|| panic!("no Checking line at {size:?}"));
            assert!(clip.expand(0.5).contains_rect(*bounds), "the Checking line is out of view at {size:?}");

            std::thread::sleep(std::time::Duration::from_millis(300));
            released.store(true, Ordering::SeqCst);
            device.until("unlocked without a second press", |f| f.status() == Some(E2eeStatus::On));
            assert!(device.state.e2ee_error.is_none() && device.sync.unlock_error.is_none());
        }
    }

    /// Sync requests fail while `offline` is set, and pull answers wait while
    /// `released` is false.
    fn switchable_server(service: &Arc<SyncService>, device: &str) -> (Server, Arc<AtomicBool>, Arc<AtomicBool>) {
        let (offline, released) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
        let (down, gate, reply) = (offline.clone(), released.clone(), account_reply(service, "fixture-account", device));
        let server = Server::start(move |request| {
            if down.load(Ordering::SeqCst) && request.path.starts_with("/v1/sync/") {
                return Reply::json(503, r#"{"error":{"code":"unavailable","message":"fixture network down"}}"#);
            }
            let mut answer = reply(request);
            if request.path.starts_with("/v1/sync/pull?") { answer.gate = Some(gate.clone()); }
            answer
        });
        (server, offline, released)
    }

    #[test]
    fn unlock_reads_the_server_first_and_never_calls_a_code_it_couldnt_check_wrong() {
        const UNCHECKED: &str = "Couldn't check the server for a newer recovery code. If you just reset encryption on another device, connect and try again.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            owner.turn_on(&service);

            // D locks and finishes its scan; only then does A reset.
            let (server, offline, released) = switchable_server(&service, "device-d");
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(MemoryKeyStore::default()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            device.settle();
            owner.sync.begin_reset_encryption(&owner.lib).unwrap();
            let new = Hidden::from(&owner.sync.recovery_code().unwrap());
            owner.until("the reset checked the server", |f| f.sync.reset_ready());
            owner.sync.confirm_reset_encryption(&mut owner.lib).unwrap();
            owner.until("new marker uploaded", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());

            // Offline, D can't look for a newer marker, so A's code isn't called wrong.
            offline.store(true, Ordering::SeqCst);
            let field = Id::unique("settings-recovery-code");
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(new.reveal().clone())]);
            device.key(egui::Key::Enter, egui::Modifiers::NONE);
            device.until("checked", |f| !f.sync.checking_encryption());
            device.visible(UNCHECKED);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            assert!(device.state.e2ee_code.is_empty());

            // Online, with no Sync Now, Unlock reads the server first, says
            // so, and then takes A's code.
            offline.store(false, Ordering::SeqCst);
            // The scans that failed offline retry with backoff. Let the next
            // one finish first, so none starts between measuring Unlock and
            // pressing it.
            device.until_for("the retry scan", std::time::Duration::from_secs(60), |f| f.sync.encryption_scanned());
            released.store(false, Ordering::SeqCst);
            device.focus_id(field);
            device.frame(vec![egui::Event::Text(new.reveal().clone())]);
            device.click("Unlock");
            device.until("checking", |f| f.sync.checking_encryption());
            device.visible(CHECKING_LOCKED);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            assert!(device.settle().text.iter().all(|(text, _, _)| text != UNCHECKED), "the earlier answer stayed on screen");
            released.store(true, Ordering::SeqCst);
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(device.settle().text.iter().all(|(text, _, _)| text != CHECKING_LOCKED));
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    /// Recheck 9, AB5: a background scan starting between seeing Unlock and
    /// pressing it doesn't move the button, so the press queues the code
    /// instead of landing on nothing with the code left in the field.
    #[test]
    fn unlock_stays_put_when_a_scan_starts_and_a_press_then_queues_the_code() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = owner.turn_on(&service);
            let (server, _, released) = switchable_server(&service, "device-b");
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(MemoryKeyStore::default()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            device.focus_id(Id::unique("settings-recovery-code"));
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            let mut pos = device.visible("Unlock").point("Unlock");
            // Let the scroll come to rest.
            for _ in 0..20 {
                let again = device.settle().point("Unlock");
                if again == pos { break; }
                pos = again;
            }
            assert!(!device.sync.checking_encryption());

            // A scan starts, and stays running, before the press.
            released.store(false, Ordering::SeqCst);
            device.sync.sync_now();
            device.until("scan running", |f| f.sync.checking_encryption());
            let during = device.frame(vec![]);
            assert!(during.text.iter().any(|(text, _, _)| text == CHECKING_LOCKED), "no Checking line at {size:?}");
            assert_eq!(during.point("Unlock"), pos, "Unlock moved when the scan started at {size:?}");
            for pressed in [true, false] {
                device.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            assert!(device.sync.unlock_waiting(), "the press missed Unlock at {size:?}");
            assert!(device.state.e2ee_code.is_empty(), "the code stayed in the field at {size:?}");
            released.store(true, Ordering::SeqCst);
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On));
        }
    }

    /// The fixtures keep the sync socket open and silent, as the service does
    /// between changes. So while a test holds pulls, no refused reconnect
    /// reports Offline and asks for a catch-up pull the hold would keep
    /// waiting. Five seconds of holding cover the reconnects a refused socket
    /// makes after 1 and 3 seconds.
    #[test]
    fn a_fixture_holding_pulls_never_reports_offline_or_waits_to_catch_up() {
        let service = Arc::new(SyncService::default());
        let (server, _, released) = switchable_server(&service, "device-a");
        let mut device = EncryptionFixture::with_keys(server, sizes()[0], Box::new(MemoryKeyStore::default()));
        let url = device._server.url.clone();
        device.sync.resume_fixture(&url, "fixture-account");
        device.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
        released.store(false, Ordering::SeqCst);
        let held_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < held_until {
            device.frame(vec![]);
            assert!(!matches!(device.sync.status, SyncStatus::Offline(_)) && device.sync.encryption_ready(), "the held device went offline or stopped being ready: status={:?}", device.sync.status);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        released.store(true, Ordering::SeqCst);
    }

    /// The Confirming check ends just as Turn On becomes enabled. Its line
    /// keeps its height, so Turn On and Cancel stay under the pointer, and a
    /// press where Turn On was drawn turns encryption on.
    #[test]
    fn turn_on_stays_put_when_the_check_before_it_ends() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let (server, _, released) = switchable_server(&service, "device-a");
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(MemoryKeyStore::default()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            released.store(false, Ordering::SeqCst);
            device.press_turn_on(false);
            device.until("checking", |f| f.status() == Some(E2eeStatus::Confirming) && !f.sync.encryption_confirmable());
            device.visible(CHECKING_CONFIRM);
            device.click("I saved this recovery code");
            let at = device.at_rest(&["Turn On", "Cancel"]);
            released.store(true, Ordering::SeqCst);
            device.until("the check ended", |f| f.sync.encryption_confirmable());
            let after = device.frame(vec![]);
            assert!(!shows(&after, CHECKING_CONFIRM));
            assert_eq!(vec![after.point("Turn On"), after.point("Cancel")], at, "Turn On or Cancel moved when the check ended at {size:?}");
            device.press_at(at[0]);
            device.until("on", |f| f.status() == Some(E2eeStatus::On));
        }
    }

    /// The same for Reset: its check ends just as Reset Encryption becomes
    /// enabled, and Reset Encryption and Cancel stay where they were.
    #[test]
    fn reset_encryption_stays_put_when_the_check_before_it_ends() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let (server, _, released) = switchable_server(&service, "device-a");
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(MemoryKeyStore::default()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let old = device.turn_on(&service);
            device.until("scanned", |f| f.sync.encryption_scanned());
            released.store(false, Ordering::SeqCst);
            device.click("Reset Encryption…");
            device.until("checking", |f| f.status() == Some(E2eeStatus::Resetting) && !f.sync.reset_ready());
            device.visible(CHECKING_RESET);
            device.click("I saved this recovery code");
            let at = device.at_rest(&["Reset Encryption", "Cancel"]);
            released.store(true, Ordering::SeqCst);
            device.until("the check ended", |f| f.sync.reset_ready());
            let after = device.frame(vec![]);
            assert!(!shows(&after, CHECKING_RESET));
            assert_eq!(vec![after.point("Reset Encryption"), after.point("Cancel")], at, "Reset Encryption or Cancel moved when the check ended at {size:?}");
            device.press_at(at[0]);
            device.until("reset", |f| f.status() == Some(E2eeStatus::On));
            assert_ne!(device.code(), Some(old.clone()), "the press missed Reset Encryption at {size:?}");
        }
    }

    /// Recheck 9, AB2, in Settings: a device signed out while another
    /// device deleted the account's sync data signs in again, retires its
    /// old key and stays on with uploads held. The page says why, and offers
    /// both ways on: Turn On Encryption… and Upload Without Encryption.
    #[test]
    fn a_device_signed_out_during_a_delete_says_why_uploads_wait_and_offers_both_ways_on() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = synced(&service, size);
            let code = owner.turn_on(&service);
            let mut away = EncryptionFixture::new(&service, "device-b", size);
            away.locked("locked");
            away.sync.unlock_encryption(&away.lib, code.reveal()).unwrap();
            away.until("unlocked", |f| f.status() == Some(E2eeStatus::On));
            away.sync.sign_out(&away.lib);
            away.until("signed out", |f| !f.sync.signed_in());
            owner.sync.delete_sync_data(&owner.lib);
            owner.until("deleted", |f| f.sync.sync_off.is_some());

            away.sign_in();
            away.until("held", |f| f.sync.signed_in() && f.sync.encryption_ready() && f.sync.encryption_hold(&f.lib));
            assert_eq!(away.status(), Some(E2eeStatus::Off), "the retired key's record stayed");
            assert!(!away.sync.has_library_key("fixture-account"));
            away.visible("Waiting for encryption before uploading.");
            away.visible(ENCRYPTION_HOLD);
            away.visible("Upload Without Encryption");
            away.visible("Turn On Encryption…");
            away.fits_width();
            for _ in 0..90 { away.frame(vec![]); std::thread::sleep(std::time::Duration::from_millis(10)); }
            assert!(service.rows().is_empty(), "the library uploaded before the user chose");

            // Turn On Encryption… is one way on: a new code, sealed uploads.
            away.press_turn_on(false);
            let new = away.shown_code().expect("the new recovery code is on screen");
            assert_ne!(new, code, "turning on again makes a new code");
            away.click("I saved this recovery code");
            away.until("confirmable", |f| f.sync.encryption_confirmable());
            away.click("Turn On");
            away.until("sealed upload", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && !service.rows().is_empty());
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            assert!(!shows(&away.settle(), ENCRYPTION_HOLD));
        }
    }

    /// Recheck 10, AC3: a held device that another device's key then locks
    /// asks for that device's recovery code. Upload Without Encryption isn't
    /// offered there; the code unlocks it and the library uploads sealed.
    #[test]
    fn a_held_device_locked_by_the_accounts_current_key_asks_for_that_code_and_offers_no_plaintext_upload() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = synced(&service, size);
            let code = owner.turn_on(&service);
            let mut away = EncryptionFixture::new(&service, "device-b", size);
            away.locked("locked");
            away.sync.unlock_encryption(&away.lib, code.reveal()).unwrap();
            away.until("unlocked", |f| f.status() == Some(E2eeStatus::On));
            away.sync.sign_out(&away.lib);
            away.until("signed out", |f| !f.sync.signed_in());
            owner.sync.delete_sync_data(&owner.lib);
            owner.until("deleted", |f| f.sync.sync_off.is_some());
            away.sign_in();
            away.until("held", |f| f.sync.signed_in() && f.sync.encryption_ready() && f.sync.encryption_hold(&f.lib));
            // Off and held: both ways on.
            assert_eq!(away.status(), Some(E2eeStatus::Off));
            away.visible(ENCRYPTION_HOLD);
            away.visible("Upload Without Encryption");
            away.fits_width();

            // A Mac that signs in after the delete turns encryption on.
            let mut other = EncryptionFixture::signed_out(account_server(&service, "fixture-account", "device-c"), size);
            other.sign_in();
            other.until("first upload", |f| f.lib.store.dirty_count().unwrap() == 0 && !service.rows().is_empty() && f.sync.upload_hold().is_none());
            let current = other.turn_on(&service);
            away.changed(&service, "device-c");
            away.until("locked by the current key", |f| f.status() == Some(E2eeStatus::Locked) && f.sync.encryption_scanned() && f.sync.encryption_hold(&f.lib));
            away.visible(LOCKED_HOLD);
            away.visible("Unlock");
            away.fits_width();
            let shown = away.settle();
            assert!(!shows(&shown, ENCRYPTION_HOLD) && !shows(&shown, "Upload Without Encryption"), "a Locked device offers no plaintext upload at {size:?}");
            for _ in 0..30 { away.frame(vec![]); std::thread::sleep(std::time::Duration::from_millis(10)); }
            assert!(EncryptionFixture::server_plaintext(&service).is_empty(), "nothing uploaded unencrypted while Locked");

            away.focus_id(Id::unique("settings-recovery-code"));
            away.frame(vec![egui::Event::Text(current.reveal().clone())]);
            away.click("Unlock");
            away.until("unlocked with the current code", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(!shows(&away.settle(), LOCKED_HOLD));
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    /// Recheck 9, AB5: a failing Settings test never writes a recovery code
    /// to the test output.
    #[test]
    fn test_failures_never_print_a_recovery_code() {
        let service = Arc::new(SyncService::default());
        let mut owner = EncryptionFixture::new(&service, "device-a", sizes()[0]);
        owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
        owner.press_turn_on(false);
        let code = owner.shown_code().expect("the recovery code is on screen");
        // A label naming the code that isn't on screen, as shown_code's own lookup could.
        let missing = [code.reveal().as_str(), " (not on screen)"].concat();
        let message = failure(|| { owner.visible(&missing); });
        assert!(message.contains("did not scroll into view") && message.contains("(not on screen)"), "unexpected failure message");
        assert!(!message.contains(&code.reveal()[4..]), "a failing test printed the recovery code");
        owner.sync.cancel_encryption().unwrap();
    }

    /// AD4: the codes a test reads, off the screen or the glue or from
    /// turning encryption on, print redacted when an assert over them fails.
    #[test]
    fn a_failed_assert_on_codes_the_fixture_reads_prints_them_redacted() {
        let printed = |message: &str, groups: &[String]| groups.iter().any(|group| message.contains(group.as_str())) || !message.contains("Hidden(..)");
        let service = Arc::new(SyncService::default());
        let mut owner = EncryptionFixture::new(&service, "device-a", sizes()[0]);
        owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
        owner.press_turn_on(false);
        let groups: Vec<String> = owner.sync.recovery_code().unwrap().as_str().split('-').skip(1).map(str::to_owned).collect();
        let shown = owner.shown_code();
        let messages = [
            failure(|| assert_ne!(owner.shown_code(), shown.clone(), "the code on screen")),
            failure(|| assert_ne!(owner.code(), owner.code())),
            failure(|| panic!("{:?}", shown.clone())),
        ];
        for (n, message) in messages.iter().enumerate() {
            assert!(!printed(message, &groups), "failure {n} printed the code on screen");
        }
        owner.sync.cancel_encryption().unwrap();

        let service = Arc::new(SyncService::default());
        let mut device = EncryptionFixture::new(&service, "device-b", sizes()[0]);
        device.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
        let code = device.turn_on(&service);
        let groups: Vec<String> = device.sync.recovery_code().unwrap().as_str().split('-').skip(1).map(str::to_owned).collect();
        let message = failure(|| assert_ne!(Some(code.clone()), device.code(), "the code turned on"));
        assert!(!printed(&message, &groups), "the code turned on was printed");
    }

    #[test]
    fn assistive_text_input_never_grows_the_recovery_code_field() {
        use egui::accesskit::{Action, ActionData, ActionRequest, TreeId};
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            owner.turn_on(&service);
            let mut device = EncryptionFixture::new(&service, "device-b", size);
            device.locked("locked");
            device.visible("Recovery code");
            let field = Id::unique("settings-recovery-code");
            let request = |action, value: &str| egui::Event::AccessKitActionRequest(ActionRequest {
                action, target_tree: TreeId::ROOT, target_node: field.accesskit_id(), data: Some(ActionData::Value(value.into())),
            });
            let reserved = device.state.e2ee_code.capacity();

            // A screen reader sets the whole value without focusing the field,
            // cut at a character boundary.
            assert_ne!(device.ctx.memory(|memory| memory.focused()), Some(field));
            device.frame(vec![request(Action::SetValue, &"é".repeat(200))]);
            assert_eq!(device.typed(), Hidden::new("é".repeat(RECOVERY_FIELD_BYTES / 2)), "the field holds the text cut at a character boundary");
            assert_eq!(device.state.e2ee_code.capacity(), reserved, "the code's buffer was reallocated");
            device.frame(vec![request(Action::SetValue, &"X".repeat(400))]);
            assert_eq!(device.typed(), Hidden::new("X".repeat(RECOVERY_FIELD_BYTES)), "the field holds the text up to its size");
            assert_eq!(device.state.e2ee_code.capacity(), reserved, "the code's buffer was reallocated");

            // Dictation replaces the selection in the focused field.
            device.focus_id(field);
            device.key(egui::Key::A, egui::Modifiers::COMMAND);
            device.frame(vec![request(Action::ReplaceSelectedText, &"Y".repeat(400))]);
            assert_eq!(device.typed(), Hidden::new("Y".repeat(RECOVERY_FIELD_BYTES)), "the field holds the text up to its size");
            device.frame(vec![request(Action::ReplaceSelectedText, "Z")]);
            assert_eq!(device.typed(), Hidden::new("Y".repeat(RECOVERY_FIELD_BYTES)), "a full field took more text");
            assert_eq!(device.state.e2ee_code.capacity(), reserved, "the code's buffer was reallocated");

            // A value that fits is taken whole, focused or not.
            device.frame(vec![request(Action::SetValue, "CB1-ABCD")]);
            assert_eq!(device.typed(), Hidden::new("CB1-ABCD"), "the field holds what was typed");
            assert_eq!(device.state.e2ee_code.capacity(), reserved);
            device.fits_width();
        }
    }

    #[test]
    fn a_selection_moved_earlier_in_the_same_frame_is_no_room_for_a_paste() {
        use egui::accesskit::{Action, ActionData, ActionRequest, TextPosition, TextSelection, TreeId};
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            owner.turn_on(&service);
            let mut device = EncryptionFixture::new(&service, "device-b", size);
            device.locked("locked");
            let field = Id::unique("settings-recovery-code");
            let reserved = device.state.e2ee_code.capacity();
            let key = |key, pressed, modifiers| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            // An assistive client puts the cursor at the start of the field's text.
            let start = TextPosition { node: field.with(0usize).with(0usize).accesskit_id(), character_index: 0 };
            let collapse = egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetTextSelection, target_tree: TreeId::ROOT, target_node: field.accesskit_id(),
                data: Some(ActionData::SetTextSelection(TextSelection { anchor: start, focus: start })),
            });
            let capped = |device: &EncryptionFixture, what: &str| {
                assert_eq!(device.state.e2ee_code.len(), RECOVERY_FIELD_BYTES, "{what}");
                assert_eq!(device.state.e2ee_code.capacity(), reserved, "{what}: the code's buffer was reallocated");
            };
            device.focus_id(field);

            // Select all, then in one frame a key collapses the selection
            // before a paste: the paste only fills what is left.
            for (what, moves) in [
                ("ArrowRight", vec![key(egui::Key::ArrowRight, true, egui::Modifiers::NONE), key(egui::Key::ArrowRight, false, egui::Modifiers::NONE)]),
                ("End", vec![key(egui::Key::End, true, egui::Modifiers::NONE)]),
                ("SetTextSelection", vec![collapse.clone()]),
            ] {
                device.key(egui::Key::A, egui::Modifiers::COMMAND);
                device.frame(vec![egui::Event::Text("Z".into())]);
                device.frame(vec![egui::Event::Paste("X".repeat(199))]);
                assert_eq!(device.state.e2ee_code.len(), 200);
                device.key(egui::Key::A, egui::Modifiers::COMMAND);
                device.frame(moves.into_iter().chain([egui::Event::Paste("Y".repeat(300))]).collect());
                capped(&device, what);
            }

            // A middle click pastes where it points, also into the field
            // while another control has focus. The compact sheet starts with
            // the field below its fold, and focus can scroll it.
            let sheet = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), size), 1.0);
            let into_view = |device: &mut EncryptionFixture| {
                for _ in 0..24 {
                    device.settle();
                    let shown = device.ctx.read_response(field).expect("the field is shown");
                    if shown.interact_rect.contains(shown.rect.center()) { return shown.rect.center(); }
                    let delta = if shown.rect.center().y > sheet.center().y { -100.0 } else { 100.0 };
                    device.frame(vec![
                        egui::Event::PointerMoved(sheet.right_bottom() - vec2(60.0, 40.0)),
                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                    ]);
                }
                panic!("the recovery code field did not scroll into view at {size:?}");
            };
            for focused in [true, false] {
                device.key(egui::Key::A, egui::Modifiers::COMMAND);
                device.frame(vec![egui::Event::Text("Z".into())]);
                device.frame(vec![egui::Event::Paste("X".repeat(199))]);
                if !focused { device.ctx.memory_mut(|m| m.surrender_focus(field)); }
                let at = into_view(&mut device);
                assert_eq!(device.ctx.memory(|m| m.has_focus(field)), focused);
                device.frame(vec![egui::Event::PointerMoved(at), egui::Event::MiddleClickPaste { pos: at, text: "Y".repeat(300) }]);
                capped(&device, if focused { "MiddleClickPaste" } else { "MiddleClickPaste without focus" });
                device.focus_id(field);
            }

            // Typing over a selection still replaces it, even in a full field.
            device.key(egui::Key::A, egui::Modifiers::COMMAND);
            device.frame(vec![key(egui::Key::Q, true, egui::Modifiers::NONE), egui::Event::Text("q".into()), key(egui::Key::Q, false, egui::Modifiers::NONE)]);
            assert_eq!(device.typed(), Hidden::new("q"), "the field holds what was typed");
            assert_eq!(device.state.e2ee_code.capacity(), reserved);
            device.fits_width();
        }
    }

    #[test]
    fn a_reset_cut_short_here_and_outrun_elsewhere_names_the_newest_code() {
        const SUPERSEDED: &str = "Encryption was reset on another device after this one started a reset. Enter the newest recovery code.";
        const USED_BEFORE: &str = "That's the recovery code this device used before. Encryption was reset on another device since then. Enter the newest recovery code.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let k1 = owner.turn_on(&service);

            let (keys, quit) = (MemoryKeyStore::default(), Arc::new(AtomicBool::new(false)));
            let store = || Box::new(QuitOnSave { keys: keys.clone(), quit: quit.clone() });
            let mut device = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-b"), size, store());
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            let field = Id::unique("settings-recovery-code");
            let enter = |device: &mut EncryptionFixture, code: &str| {
                device.focus_id(field);
                device.frame(vec![egui::Event::Text(code.into())]);
                device.key(egui::Key::Enter, egui::Modifiers::NONE);
                device.until("checked", |f| !f.sync.checking_encryption());
            };
            enter(&mut device, k1.reveal());
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());

            // B's reset is cut short by a quit before its key is stored.
            device.click("Reset Encryption…");
            let k2 = Hidden::from(&device.sync.recovery_code().unwrap());
            device.click("I saved this recovery code");
            device.until("the reset checked the server", |f| f.sync.reset_ready());
            quit.store(true, Ordering::SeqCst);
            let (sync, lib) = (&mut device.sync, &mut device.lib);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync.confirm_reset_encryption(lib))).is_err());
            quit.store(false, Ordering::SeqCst);
            device.restart(store());
            device.locked("locked after the restart");

            // A, unaware, resets and uploads its marker.
            owner.click("Reset Encryption…");
            let k3 = Hidden::from(&owner.sync.recovery_code().unwrap());
            owner.click("I saved this recovery code");
            owner.until("the reset checked the server", |f| f.sync.reset_ready());
            owner.click("Reset Encryption");
            owner.until("A's reset uploaded", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());

            // B's own abandoned code and the code before it are refused with
            // their reasons; the newest code unlocks.
            enter(&mut device, k2.reveal());
            device.visible(SUPERSEDED);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            enter(&mut device, k1.reveal());
            device.visible(USED_BEFORE);
            device.fits_width();
            enter(&mut device, k3.reveal());
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(device.settle().text.iter().all(|(text, _, _)| text != SUPERSEDED && text != USED_BEFORE));
            owner.until("A still on its code", |f| f.status() == Some(E2eeStatus::On) && f.sync.upload_hold().is_none());
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    #[test]
    fn a_reset_cut_short_says_why_a_replaced_or_unchecked_code_is_refused() {
        const REPLACED: &str = "That's the code of a reset this device started and then replaced with a newer one. Enter the newest recovery code.";
        const UNCHECKED_RESET: &str = "Couldn't check the server for a newer reset from another device, so this device's reset wasn't finished. Connect, then try again.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let k1 = owner.turn_on(&service);

            let (keys, quit) = (MemoryKeyStore::default(), Arc::new(AtomicBool::new(false)));
            let store = || Box::new(QuitOnSave { keys: keys.clone(), quit: quit.clone() });
            let (server, offline, _) = switchable_server(&service, "device-b");
            let mut device = EncryptionFixture::with_keys(server, size, store());
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            let field = Id::unique("settings-recovery-code");
            let enter = |device: &mut EncryptionFixture, code: &str| {
                device.focus_id(field);
                device.frame(vec![egui::Event::Text(code.into())]);
                device.key(egui::Key::Enter, egui::Modifiers::NONE);
                device.until("checked", |f| !f.sync.checking_encryption());
            };
            enter(&mut device, k1.reveal());
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());

            // Two resets in a row, each cut short by a quit before its key is
            // stored; the second starts from the Locked page.
            let cut_short = |device: &mut EncryptionFixture| -> Hidden<String> {
                device.click("Reset Encryption…");
                let code = Hidden::from(&device.sync.recovery_code().unwrap());
                device.click("I saved this recovery code");
                device.until("the reset checked the server", |f| f.sync.reset_ready());
                quit.store(true, Ordering::SeqCst);
                let (sync, lib) = (&mut device.sync, &mut device.lib);
                assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync.confirm_reset_encryption(lib))).is_err());
                quit.store(false, Ordering::SeqCst);
                device.restart(store());
                device.locked("locked after the restart");
                code
            };
            let first = cut_short(&mut device);
            let second = cut_short(&mut device);

            // The first code was replaced on this device, not elsewhere.
            enter(&mut device, first.reveal());
            device.visible(REPLACED);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));

            // Offline, the newest code doesn't finish the reset; online, it does.
            offline.store(true, Ordering::SeqCst);
            enter(&mut device, second.reveal());
            device.visible(UNCHECKED_RESET);
            device.fits_width();
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            offline.store(false, Ordering::SeqCst);
            enter(&mut device, second.reveal());
            device.until("finished", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert_eq!(device.code(), Some(second.clone()), "the second code is kept");
            assert!(device.settle().text.iter().all(|(text, _, _)| text != REPLACED && text != UNCHECKED_RESET));
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    #[test]
    fn a_reset_dropped_for_the_accounts_code_says_so_when_that_code_wasnt_stored() {
        const DROPPED: &str = "That's the code of a reset this device dropped when the account's current recovery code was entered. Enter that code again.";
        const SUPERSEDED: &str = "Encryption was reset on another device after this one started a reset. Enter the newest recovery code.";
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let k1 = owner.turn_on(&service);

            let (keys, quit) = (MemoryKeyStore::default(), Arc::new(AtomicBool::new(false)));
            let store = || Box::new(QuitOnSave { keys: keys.clone(), quit: quit.clone() });
            let mut device = EncryptionFixture::with_keys(account_server(&service, "fixture-account", "device-b"), size, store());
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.locked("locked");
            let field = Id::unique("settings-recovery-code");
            let enter = |device: &mut EncryptionFixture, code: &str| {
                device.focus_id(field);
                device.frame(vec![egui::Event::Text(code.into())]);
                device.key(egui::Key::Enter, egui::Modifiers::NONE);
                device.until("checked", |f| !f.sync.checking_encryption());
            };
            enter(&mut device, k1.reveal());
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            let cut_short = |device: &mut EncryptionFixture| -> Hidden<String> {
                device.click("Reset Encryption…");
                let code = Hidden::from(&device.sync.recovery_code().unwrap());
                device.click("I saved this recovery code");
                device.until("the reset checked the server", |f| f.sync.reset_ready());
                quit.store(true, Ordering::SeqCst);
                let (sync, lib) = (&mut device.sync, &mut device.lib);
                assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync.confirm_reset_encryption(lib))).is_err());
                quit.store(false, Ordering::SeqCst);
                device.restart(store());
                device.locked("locked after the restart");
                code
            };

            // B's reset is cut short, A resets, and B, outrun, resets again
            // from the Locked page; that reset is cut short too.
            let first = cut_short(&mut device);
            owner.click("Reset Encryption…");
            let k3 = Hidden::from(&owner.sync.recovery_code().unwrap());
            owner.click("I saved this recovery code");
            owner.until("the reset checked the server", |f| f.sync.reset_ready());
            owner.click("Reset Encryption");
            owner.until("A's reset uploaded", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            let second = cut_short(&mut device);

            // A's code, the account's, is correct but its key can't be stored.
            keys.fail_writes(true);
            enter(&mut device, k3.reveal());
            device.visible("The code is correct, but it couldn't be stored. Secure key storage is unavailable or access was declined.");
            assert_eq!(device.status(), Some(E2eeStatus::Locked));
            keys.fail_writes(false);

            // B's reset codes now say they were dropped for that code, not
            // that a reset elsewhere came later.
            for code in [&second, &first] {
                enter(&mut device, code.reveal());
                device.visible(DROPPED);
                device.fits_width();
                assert!(device.settle().text.iter().all(|(text, _, _)| text != SUPERSEDED));
                assert_eq!(device.status(), Some(E2eeStatus::Locked));
            }
            enter(&mut device, k3.reveal());
            device.until("unlocked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert_eq!(device.code(), Some(k3.clone()), "the newest code is kept");
            assert!(device.settle().text.iter().all(|(text, _, _)| text != DROPPED));
            owner.until("A still on its code", |f| f.status() == Some(E2eeStatus::On) && f.sync.upload_hold().is_none());
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    /// Pull answers wait until `released`, once `free` pulls have passed.
    fn gated_server(service: &Arc<SyncService>, device: &str, free: usize) -> (Server, Arc<AtomicBool>) {
        let released = Arc::new(AtomicBool::new(false));
        let (gate, free, reply) = (released.clone(), std::sync::atomic::AtomicUsize::new(free), account_reply(service, "fixture-account", device));
        let server = Server::start(move |request| {
            let mut answer = reply(request);
            if request.path.starts_with("/v1/sync/pull?") && free.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_err() {
                answer.gate = Some(gate.clone());
            }
            answer
        });
        (server, released)
    }

    #[test]
    fn unlock_and_reset_wait_for_the_server_check_and_the_code_field_stays_capped() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut owner = EncryptionFixture::new(&service, "device-a", size);
            owner.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            let code = owner.turn_on(&service);

            // B's first page names the account's marker; the check past it is held.
            let (server, released) = gated_server(&service, "device-b", 1);
            let mut device = EncryptionFixture::with_keys(server, size, Box::new(MemoryKeyStore::default()));
            let url = device._server.url.clone();
            device.sync.resume_fixture(&url, "fixture-account");
            device.until("checking", |f| f.status() == Some(E2eeStatus::Locked) && f.sync.checking_encryption());
            device.visible(CHECKING_LOCKED);
            device.fits_width();

            // The field never grows past its reserved buffer, yet a selection
            // can still be replaced when it is full.
            let field = Id::unique("settings-recovery-code");
            let reserved = device.state.e2ee_code.capacity();
            device.focus_id(field);
            device.frame(vec![egui::Event::Paste("X".repeat(400))]);
            device.frame(vec![egui::Event::Text("Y".repeat(10))]);
            assert_eq!(device.state.e2ee_code.len(), RECOVERY_FIELD_BYTES);
            assert_eq!(device.state.e2ee_code.capacity(), reserved, "the code's buffer was reallocated");
            device.key(egui::Key::A, egui::Modifiers::COMMAND);
            device.frame(vec![egui::Event::Text(code.reveal().clone())]);
            assert_eq!(device.typed(), code, "the field holds the code");
            // Unlock during the check queues the code for it (AA1): the field
            // is wiped and nothing unlocks before the check finishes.
            device.click("Unlock");
            assert_eq!(device.status(), Some(E2eeStatus::Locked), "Unlock ran before the check finished");
            assert!(device.state.e2ee_error.is_none() && device.state.e2ee_code.is_empty() && device.sync.unlock_waiting());

            released.store(true, Ordering::SeqCst);
            device.until("unlocked once checked", |f| f.status() == Some(E2eeStatus::On) && f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(device.settle().text.iter().all(|(text, _, _)| text != CHECKING_LOCKED));
            assert!(device.state.e2ee_code.is_empty());

            // A reset waits the same way until the server's stream is read to its end.
            released.store(false, Ordering::SeqCst);
            device.click("Reset Encryption…");
            device.visible(CHECKING_RESET);
            device.fits_width();
            device.click("I saved this recovery code");
            device.click("Reset Encryption");
            assert_eq!(device.status(), Some(E2eeStatus::Resetting), "Reset ran before the check finished");
            released.store(true, Ordering::SeqCst);
            device.until("the reset checked the server", |f| f.sync.reset_ready());
            assert!(device.settle().text.iter().all(|(text, _, _)| text != CHECKING_RESET));
            device.click("Reset Encryption");
            assert_eq!(device.status(), Some(E2eeStatus::On));
            device.until("sealed again", |f| f.lib.store.dirty_count().unwrap() == 0 && f.sync.upload_hold().is_none());
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
        }
    }

    #[test]
    fn an_encrypted_device_says_its_changes_wait_until_it_catches_up() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let failing = Arc::new(AtomicBool::new(false));
            let (flag, reply) = (failing.clone(), account_reply(&service, "fixture-account", "device-a"));
            let server = Server::start(move |request| {
                if request.path.starts_with("/v1/sync/pull?") && flag.load(Ordering::SeqCst) {
                    Reply::json(500, r#"{"error":{"code":"internal","message":"fixture pull failure"}}"#)
                } else { reply(request) }
            });
            let keys = MemoryKeyStore::default();
            let mut fixture = EncryptionFixture::with_keys(server, size, Box::new(keys.clone()));
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&service);

            // Relaunched while pulls fail: another device may have reset the
            // key meanwhile, so the edit waits for a complete pull.
            failing.store(true, Ordering::SeqCst);
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            fixture.lib.update_space(&space, |s| s.name = "Fixture Secret Space".into()).unwrap();
            let rows = service.rows();
            fixture.restart(Box::new(keys.clone()));
            fixture.until("held upload", |f| f.status() == Some(E2eeStatus::On) && f.sync.pending > 0 && f.sync.upload_hold().is_some());
            fixture.visible("Waiting for the first sync before uploading.");
            fixture.visible("Encryption is on");
            fixture.fits_width();
            assert_eq!(service.rows(), rows, "an encrypted device uploads nothing before it catches up");

            failing.store(false, Ordering::SeqCst);
            fixture.until_for("upload after catching up", std::time::Duration::from_secs(30), |f| f.lib.store.dirty_count().unwrap() == 0);
            assert!(EncryptionFixture::server_plaintext(&service).is_empty());
            assert!(!EncryptionFixture::plaintext_seen(&service));
            fixture.until("no pending changes", |f| f.sync.pending == 0);
            fixture.visible("Everything is up to date.");
        }
    }

    #[test]
    fn another_account_is_refused_on_a_device_holding_an_encrypted_library() {
        for size in sizes() {
            let a_service = Arc::new(SyncService::default());
            let b_service = Arc::new(SyncService::default());
            let b_server = account_server(&b_service, "fixture-other-account", "device-a");
            let mut fixture = EncryptionFixture::signed_out(account_server(&a_service, "fixture-account", "device-a"), size);
            fixture.sign_in();
            assert!(fixture.sync.signed_in());
            fixture.until("first sync", |f| f.sync.encryption_ready() && f.lib.store.dirty_count().unwrap() == 0);
            fixture.turn_on(&a_service);
            fixture.click("Sign Out");
            fixture.click("Sign Out");
            fixture.until("signed out", |f| !f.sync.signed_in());

            fixture.sync.set_base_url(&fixture.lib, &b_server.url).unwrap();
            fixture.sign_in();
            fixture.visible(OTHER_ACCOUNT);
            fixture.fits_width();
            assert!(!fixture.sync.signed_in());
            b_server.wait_for("/v1/auth/logout", 1);
            assert!(b_service.rows().is_empty(), "account B's server received nothing");
            assert!(!b_server.requests.lock().unwrap().iter().any(|r| r.path.starts_with("/v1/sync/")));
            fixture.visible("Get a Sign-in Code");

            // The encrypted account itself signs in again, Locked until its code.
            let a_url = fixture._server.url.clone();
            fixture.sync.set_base_url(&fixture.lib, &a_url).unwrap();
            fixture.sign_in();
            fixture.until("locked", |f| f.status() == Some(E2eeStatus::Locked));
            fixture.fits_width();
            assert!(EncryptionFixture::server_plaintext(&a_service).is_empty(), "signing in again never uploads plaintext");
            fixture.visible("Recovery code");
        }
    }

    #[test]
    fn local_changes_say_they_wait_for_the_first_sync_while_it_fails() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let failing = Arc::new(AtomicBool::new(true));
            let (flag, reply) = (failing.clone(), account_reply(&service, "fixture-account", "device-a"));
            let server = Server::start(move |request| {
                if request.path.starts_with("/v1/sync/pull?") && flag.load(Ordering::SeqCst) {
                    Reply::json(500, r#"{"error":{"code":"internal","message":"fixture pull failure"}}"#)
                } else { reply(request) }
            });
            let mut fixture = EncryptionFixture::signed_out(server, size);
            let url = fixture._server.url.clone();
            fixture.sync.resume_fixture(&url, "fixture-account");
            fixture.until("held upload", |f| f.sync.pending > 0 && f.sync.upload_hold().is_some());
            fixture.visible("Waiting for the first sync before uploading.");
            fixture.fits_width();
            assert!(service.rows().is_empty(), "nothing uploads before a complete pull");
            failing.store(false, Ordering::SeqCst);
            fixture.until_for("upload after the first sync", std::time::Duration::from_secs(30), |f| f.lib.store.dirty_count().unwrap() == 0 && !service.rows().is_empty());
            fixture.until("no pending changes", |f| f.sync.pending == 0);
            fixture.visible("Everything is up to date.");
        }
    }

    // -- deleting sync data --------------------------------------------------------------

    /// A device signed in through the real UI, synced.
    fn synced(service: &Arc<SyncService>, size: egui::Vec2) -> EncryptionFixture {
        let mut fixture = EncryptionFixture::signed_out(account_server(service, "fixture-account", "device-a"), size);
        fixture.sign_in();
        fixture.until("first upload", |f| f.lib.store.dirty_count().unwrap() == 0 && !service.rows().is_empty() && f.sync.upload_hold().is_none());
        fixture
    }

    pub(super) fn type_into(fixture: &mut EncryptionFixture, field: &str, text: &str) {
        fixture.focus_id(Id::unique(field));
        fixture.key(egui::Key::A, egui::Modifiers::COMMAND);
        fixture.frame(vec![egui::Event::Text(text.into())]);
    }

    pub(super) fn danger() -> Color32 {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        Ink::new(pal.panel, pal.accent).danger
    }

    fn shows(shown: &PreferenceFrame, label: &str) -> bool {
        shown.text.iter().any(|(text, _, _)| text == label)
    }

    #[test]
    fn delete_sync_data_names_the_account_and_needs_its_email_typed() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = synced(&service, size);
            let (rows, spaces) = (service.rows(), fixture.lib.spaces_ordered().len());
            assert_eq!(fixture.visible(DELETE_SYNC_DATA).color(DELETE_SYNC_DATA), danger(), "a destructive action is drawn in the danger colour");
            fixture.click(DELETE_SYNC_DATA);
            fixture.visible("Delete sync data for fixture@example.com?");
            for fact in DELETE_FACTS { fixture.visible(fact); }
            fixture.visible("Type fixture@example.com to confirm.");
            fixture.fits_width();

            // Without the email typed, or with another one, Delete does nothing.
            fixture.click("Delete Sync Data");
            assert!(!fixture.sync.deleting);
            type_into(&mut fixture, "settings-delete-email", "someone@example.com");
            fixture.click("Delete Sync Data");
            assert!(!fixture.sync.deleting);
            assert_eq!(service.rows(), rows);

            // Cancel closes it and changes nothing.
            fixture.click("Cancel");
            let shown = fixture.settle();
            assert!(!DELETE_FACTS.iter().any(|fact| shows(&shown, fact)));
            assert!(fixture.state.delete_email.is_empty());
            assert_eq!(service.rows(), rows);
            assert!(fixture.sync.sync_off.is_none());

            // The account's email, in any case and with spaces around it.
            fixture.click(DELETE_SYNC_DATA);
            assert!(fixture.state.delete_email.is_empty(), "a reopened sheet starts empty");
            type_into(&mut fixture, "settings-delete-email", " Fixture@Example.COM ");
            assert_eq!(fixture.visible("Delete Sync Data").color("Delete Sync Data"), danger());
            fixture.click("Delete Sync Data");
            fixture.until("verified delete", |f| f.sync.sync_off.is_some());
            assert!(service.rows().is_empty());
            assert_eq!(fixture.lib.spaces_ordered().len(), spaces);
            fixture.visible(crate::sync_glue::SYNC_OFF_HERE);
            fixture.visible("Turn Sync On…");
            let shown = fixture.settle();
            for gone in [DELETE_SYNC_DATA, "Sync Now", "Turn On Encryption…"] {
                assert!(!shows(&shown, gone), "{gone} is offered while sync is off");
            }
            fixture.fits_width();
        }
    }

    #[test]
    fn a_delete_the_server_cant_do_says_so_and_keeps_the_sheet() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = synced(&service, size);
            let rows = service.rows();
            service.lack_delete();
            fixture.click(DELETE_SYNC_DATA);
            type_into(&mut fixture, "settings-delete-email", "fixture@example.com");
            fixture.click("Delete Sync Data");
            fixture.until("the server's answer", |f| !f.sync.deleting);
            fixture.visible(crate::sync_glue::CANT_DELETE_YET);
            fixture.fits_width();
            // Retry is safe: the same answer, and nothing changed.
            fixture.click("Delete Sync Data");
            fixture.until("the server's answer", |f| !f.sync.deleting);
            fixture.visible(crate::sync_glue::CANT_DELETE_YET);
            assert_eq!(service.rows(), rows);
            assert!(fixture.sync.sync_off.is_none());
            assert_eq!(fixture.state.delete_email, "fixture@example.com");
        }
    }

    #[test]
    fn turning_sync_on_asks_first_and_an_encrypted_library_waits_for_encryption() {
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = synced(&service, size);
            fixture.turn_on(&service);
            fixture.sync.delete_sync_data(&fixture.lib);
            fixture.until("verified delete", |f| f.sync.sync_off.is_some());

            fixture.click("Turn Sync On…");
            fixture.visible(TURN_ON_FRESH);
            fixture.visible(TURN_ON_ENCRYPTED);
            fixture.fits_width();
            fixture.click("Cancel");
            assert!(fixture.sync.sync_off.is_some());
            assert!(!shows(&fixture.settle(), TURN_ON_FRESH));

            fixture.click("Turn Sync On…");
            fixture.click("Turn Sync On");
            fixture.until("sync on", |f| f.sync.sync_off.is_none() && f.sync.encryption_ready());
            fixture.visible(ENCRYPTION_HOLD);
            fixture.visible("Turn On Encryption…");
            fixture.fits_width();
            for _ in 0..90 { fixture.frame(vec![]); std::thread::sleep(std::time::Duration::from_millis(10)); }
            assert!(service.rows().is_empty(), "the library uploaded before the user chose");

            fixture.click("Upload Without Encryption");
            fixture.until("plaintext upload", |f| f.lib.store.dirty_count().unwrap() == 0 && !service.rows().is_empty());
            assert!(service.rows().iter().all(|r| r["enc"] != "xchacha20_v1"));
            assert!(!shows(&fixture.settle(), ENCRYPTION_HOLD));
        }
    }

    #[test]
    fn a_restored_library_says_sync_is_off_and_what_turning_it_on_does() {
        use crossbow_core::backup::{self, Reason};
        for size in sizes() {
            let service = Arc::new(SyncService::default());
            let mut fixture = synced(&service, size);
            let space = fixture.lib.spaces_ordered()[0].0.clone();
            let kept = fixture.lib.add_pinned(&space, None, "https://example.test/kept", "Pinned before the backup").unwrap();
            fixture.until("pin uploaded", |f| f.lib.store.dirty_count().unwrap() == 0);
            let chosen = backup::create(&fixture.lib.store, Reason::Daily).unwrap().unwrap();
            fixture.lib.update_item(&kept, |item| item.title = "Renamed after the backup".into()).unwrap();
            fixture.until("rename uploaded", |f| f.lib.store.dirty_count().unwrap() == 0 && service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("Renamed after the backup")));
            backup::restore(&fixture.lib.store, &chosen).unwrap();
            fixture.relaunch_restored(Box::new(MemoryKeyStore::default()));

            fixture.visible(crate::sync_glue::SYNC_OFF_RESTORED);
            assert_eq!(fixture.lib.items[&kept].title, "Pinned before the backup");
            for _ in 0..60 { fixture.frame(vec![]); std::thread::sleep(std::time::Duration::from_millis(10)); }
            assert!(service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("Renamed after the backup")), "nothing uploads by itself");

            fixture.click("Turn Sync On…");
            fixture.visible(TURN_ON_RESTORED);
            assert!(!shows(&fixture.settle(), TURN_ON_FRESH), "a restore isn't a fresh start");
            fixture.fits_width();
            fixture.click("Turn Sync On");
            fixture.until("restored pin uploaded", |f| f.sync.sync_off.is_none() && f.lib.store.dirty_count().unwrap() == 0
                && service.rows().iter().any(|r| r["id"] == kept.as_str() && r["payload"].as_str().unwrap().contains("Pinned before the backup")));
            assert_eq!(fixture.lib.items[&kept].title, "Pinned before the backup", "the restored item won");
        }
    }
}

#[cfg(test)]
#[path = "account_browser_tests.rs"]
mod account_browser_tests;
#[cfg(test)]
#[path = "account_delete_tests.rs"]
mod account_delete_tests;
