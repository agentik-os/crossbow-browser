//! First-run setup is a resumable path, not a carousel. Back follows the path
//! actually taken; optional steps do not manufacture success or require an account.

use crate::app::{Action, CrossbowApp};
use crate::sync_glue::{AuthStep, valid_email};
use crate::ui::{anim, glass, icons::{self, Icon}, style::{self, Button, Ink, Kind, Size, weight}, theme, theme_editor};
use crossbow_core::model::Theme;
use egui::{Align2, Color32, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

const CHECKPOINT: &str = "onboarding_progress_v2";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Step {
    #[default]
    Welcome,
    Account,
    Confirmed,
    Space,
    Favorites,
    Import,
    DefaultBrowser,
    Keys,
    Ready,
}

impl Step {
    fn progress(self) -> usize {
        match self {
            Self::Welcome => 0,
            Self::Account | Self::Confirmed => 1,
            Self::Space => 2,
            Self::Favorites => 3,
            Self::Import => 4,
            Self::DefaultBrowser => 5,
            Self::Keys | Self::Ready => 6,
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome to Crossbow",
            Self::Account => "Your browser, everywhere.",
            Self::Confirmed => "You're signed in.",
            Self::Space => "Make it yours.",
            Self::Favorites => "Keep your everyday close.",
            Self::Import => "Bring your world with you.",
            Self::DefaultBrowser => "Make yourself at home.",
            Self::Keys => "A few keys. A lot less friction.",
            Self::Ready => "A fresh start, made yours.",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Welcome => "A calmer home for the web.",
            Self::Account => "Sign in to sync spaces, pinned tabs and favorites. Or keep everything on this computer.",
            Self::Confirmed => "Your library will merge with your account. Browsing history and cookies stay on this computer.",
            Self::Space => "Choose the colour, texture and mood of your first space.",
            Self::Favorites => "Pick the sites you reach for most. They'll stay at the top of your sidebar.",
            Self::Import => "Copy your saved sites into Crossbow. Your other browser stays untouched.",
            Self::DefaultBrowser => "Let links from other apps open here. This is optional, and you can change it later.",
            Self::Keys => "Try these shortcuts here. They won't change your tabs during setup.",
            Self::Ready => "Your space is ready. Everything here can be changed in Settings.",
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Progress {
    step: Step,
    history: Vec<Step>,
    space_id: String,
    shortcuts: [bool; 3],
    import_summary: Option<String>,
}

impl Progress {
    fn forward(&mut self, step: Step) {
        if self.step != step {
            self.history.push(self.step);
            self.step = step;
        }
    }
    fn back(&mut self) -> bool {
        if let Some(step) = self.history.pop() {
            self.step = step;
            true
        } else {
            false
        }
    }
}

pub struct Onboarding {
    progress: Progress,
    hydrated: bool,
    changed_at: f64,
    backwards: bool,
    keyboard_transition: bool,
    space_name: String,
    email: String,
    code: String,
    resend_at: f64,
    focus_pending: bool,
    source: Option<bool>, // true = Arc, false = Chrome
    import_pending: bool,
    settings_opened: bool,
    error: Option<String>,
}

impl Onboarding {
    pub fn new(ctx: &egui::Context) -> Self {
        let now = ctx.input(|i| i.time);
        Self {
            progress: Progress::default(), hydrated: false, changed_at: now,
            backwards: false, keyboard_transition: false,
            space_name: String::new(), email: String::new(), code: String::new(),
            resend_at: 0.0, focus_pending: true, source: None, import_pending: false,
            settings_opened: false, error: None,
        }
    }

    fn hydrate(&mut self, app: &CrossbowApp) {
        if self.hydrated { return; }
        self.hydrated = true;
        if let Ok(Some(saved)) = app.lib.store.kv_get(CHECKPOINT) {
            if let Ok(progress) = serde_json::from_str::<Progress>(&saved) {
                self.progress = progress;
            }
        }
        if !app.lib.spaces.contains_key(&self.progress.space_id) {
            self.progress.space_id = app.active_space.clone();
        }
        self.space_name = app.lib.spaces.get(&self.progress.space_id).map(|s| s.name.clone()).unwrap_or_else(|| "Personal".into());
        if self.progress.step == Step::Confirmed && !app.sync.signed_in() {
            self.progress.step = Step::Account;
            self.progress.history.retain(|s| *s != Step::Account);
        }
    }

    fn save(&mut self, app: &CrossbowApp) -> bool {
        match serde_json::to_string(&self.progress).map_err(|e| e.to_string())
            .and_then(|s| app.lib.store.kv_set(CHECKPOINT, &s).map_err(|e| e.to_string())) {
            Ok(()) => true,
            Err(e) => { self.error = Some(format!("Couldn't save setup. Check free disk space and try again. {e}")); false }
        }
    }

    fn navigate(&mut self, app: &CrossbowApp, next: Option<Step>, now: f64, keyboard: bool) {
        let before = self.progress.clone();
        self.backwards = next.is_none();
        if let Some(step) = next { self.progress.forward(step); } else {
            self.progress.back();
            if self.progress.step == Step::Account && app.sync.signed_in() { self.progress.back(); }
        }
        if self.save(app) {
            self.changed_at = now;
            self.keyboard_transition = keyboard;
            self.focus_pending = true;
            self.error = None;
        } else {
            self.progress = before;
        }
    }
}

pub fn show(app: &mut CrossbowApp, ui: &mut Ui) {
    let Some(mut ob) = app.ui.onboarding.take() else { return };
    ob.hydrate(app);
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let full = ui.max_rect();
    let current_theme = app.lib.spaces.get(&ob.progress.space_id).map(|s| s.theme.clone()).unwrap_or_else(|| Theme::preset(0));
    let pal = theme::palette(&current_theme, app.system_dark);
    let tint = crate::window_appearance::WindowChrome::onboarding_tint_opacity(app.material_state.transparent);
    let space_color = style::mix(pal.stops[0], *pal.stops.last().unwrap(), 0.5);
    let backdrop = if app.material_state.transparent { style::mix(pal.panel, space_color, tint) } else { space_color };
    let p = ui.painter().clone();
    // Native AppKit supplies the desktop blur; egui only adds the shared space tint.
    theme::paint_gradient(&p, full, &pal.stops, current_theme.angle_deg, tint);
    theme::paint_texture(&p, full, &app.texture_bank, current_theme.texture, current_theme.grain * tint, ctx.pixels_per_point());

    let strip = Rect::from_min_size(full.min, vec2(full.width(), 36.0));
    if ui.interact(strip, Id::unique("setup-window-drag"), Sense::click_and_drag()).drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let step = ob.progress.step;
    let busy = app.sync.busy || ob.import_pending;
    let back_key = take_back_key(&ctx, busy);
    let enter = ctx.input(|i| i.key_pressed(Key::Enter));
    let account_enter = enter && ctx.memory(|m| m.has_focus(Id::unique("setup-email")) || m.has_focus(Id::unique("setup-code")));
    let mut back = back_key;
    let mut next = None;
    let mut finish = false;

    if step == Step::Welcome {
        let ink = Ink::new(backdrop, pal.accent);
        style::apply_visuals(ui, &ink);
        let texture = app.app_icon.preview_texture(&ctx);
        if welcome(ui, full, &ink, texture.id(), ob.error.as_deref()).clicked() || enter {
            next = Some(if app.sync.signed_in() { Step::Confirmed } else { Step::Account });
        }
    } else {
        let margin = if full.width() < 780.0 { 20.0 } else { 48.0 };
        let desired_height = match step {
            Step::Account if matches!(app.sync.auth, AuthStep::Code { .. }) => 530.0,
            Step::Account => 470.0,
            Step::Confirmed | Step::Ready => 440.0,
            Step::Space => 680.0,
            Step::Favorites => 540.0,
            Step::Import => 550.0,
            _ => 520.0,
        };
        let height = (full.height() - 100.0).min(desired_height);
        let height = if ob.keyboard_transition { height } else { anim::spring(&ctx, Id::unique("setup-sheet-height"), height, 600.0) };
        let card = Rect::from_center_size(full.center() + vec2(0.0, 8.0), vec2((full.width() - margin * 2.0).min(720.0), height));
        let ink = glass::sheet(&p, card, pal.dark, pal.accent, backdrop);
        let pad = if card.width() < 620.0 { 28.0 } else { 42.0 };
        let inner = card.shrink2(vec2(pad, 30.0));
        let footer = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 44.0), inner.right_bottom());
        let body = Rect::from_min_max(inner.min, pos2(inner.right(), footer.top() - 20.0));
        let t = if ob.keyboard_transition { 1.0 } else { anim::ease_out_strong(anim::progress(&ctx, ob.changed_at, style::motion::ENTER)) };
        let dx = if anim::reduced_motion() { 0.0 } else { (1.0 - t) * 16.0 * if ob.backwards { -1.0 } else { 1.0 } };
        let mut body_ui = ui.new_child(egui::UiBuilder::new().id_salt(("setup-body", step as u8)).max_rect(body.translate(vec2(dx, 0.0))));
        body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
        body_ui.multiply_opacity(0.25 + 0.75 * t);
        style::apply_visuals(&mut body_ui, &ink);
        egui::ScrollArea::vertical().id_salt(("setup-scroll", step as u8)).auto_shrink([false, false]).show(&mut body_ui, |ui| {
            ui.label(style::text(step.title(), if card.width() < 620.0 { 28.0 } else { 32.0 }, weight::SEMIBOLD).color(ink.text));
            ui.add_space(5.0);
            ui.label(style::text(step.description(), 15.0, weight::REGULAR).color(ink.secondary));
            ui.add_space(24.0);
            match step {
                Step::Account => account(app, &mut ob, ui, &ink, now, account_enter, &mut next),
                Step::Confirmed => {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 80.0), Sense::hover());
                    style::check_badge(ui.painter(), pos2(r.left() + 26.0, r.center().y), 24.0, ink.primary, ink.on_primary, t);
                    if let Some(session) = &app.sync.session {
                        style::paint(ui.painter(), pos2(r.left() + 68.0, r.center().y), Align2::LEFT_CENTER, &session.email, 17.0, weight::SEMIBOLD, ink.text);
                    }
                    ui.add_space(12.0);
                    ui.label(style::text("Sync runs in the background. You can sign out at any time in Settings → Account & Sync.", 14.0, weight::REGULAR).color(ink.secondary));
                    if let Some(error) = &app.sync.error { ui.label(style::text(error, 14.0, weight::REGULAR).color(ink.danger)); }
                }
                Step::Space => {
                    style::field_label(ui, &ink, "Space name");
                    let name_width = ui.available_width();
                    let response = style::field(ui, &ink, Id::unique("setup-space-name"), &mut ob.space_name, "Personal", name_width, false);
                    if response.changed() {
                        let name = ob.space_name.trim();
                        if !name.is_empty() {
                            ob.error = app.lib.update_space(&ob.progress.space_id, |s| s.name = name.chars().take(80).collect()).err().map(|e| format!("Couldn't save the space name. {e}"));
                        }
                    }
                    ui.add_space(18.0);
                    let mut draft = current_theme.clone();
                    if theme_editor::show(ui, &mut draft, app.system_dark, &app.texture_bank) {
                        ob.error = app.lib.update_space(&ob.progress.space_id, |s| s.theme = draft).err().map(|e| format!("Couldn't save this theme. {e}"));
                    }
                }
                Step::Favorites => favorites(app, &mut ob, ui, &ink),
                Step::Import => import(app, &mut ob, ui, &ink),
                Step::DefaultBrowser => default_browser(&mut ob, ui, &ink),
                Step::Keys => keys(&mut ob, ui, &ink),
                Step::Ready => {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
                    style::check_badge(ui.painter(), r.center(), 35.0, ink.primary, ink.on_primary, t);
                    ui.add_space(16.0);
                    let space = app.lib.spaces.get(&ob.progress.space_id);
                    let name = space.map(|s| s.name.as_str()).unwrap_or("Personal");
                    let count = space.map(|s| app.lib.favorites(&s.profile_id).len()).unwrap_or(0);
                    ui.vertical_centered(|ui| {
                        ui.label(style::text(format!("{name} · {count} favorite{}", if count == 1 { "" } else { "s" }), 17.0, weight::SEMIBOLD).color(ink.text));
                        ui.add_space(8.0);
                        let status = if app.sync.signed_in() { "Account connected. Sync status is available in Settings." } else { "Local mode. Sign in later if you want to sync." };
                        ui.label(style::text(status, 14.0, weight::REGULAR).color(ink.secondary));
                    });
                }
                Step::Welcome => {}
            }
            if let Some(error) = &ob.error {
                ui.add_space(12.0);
                ui.label(style::text(error, 14.0, weight::MEDIUM).color(ink.danger));
            }
        });

        let mut footer_ui = ui.new_child(egui::UiBuilder::new().id_salt("setup-footer").max_rect(footer));
        style::apply_visuals(&mut footer_ui, &ink);
        if Button::new("Back").icon(Icon::Back).kind(Kind::Plain).size(Size::Large).enabled(!busy && !ob.progress.history.is_empty())
            .show_at(&mut footer_ui, &ink, Rect::from_min_size(footer.min, vec2(94.0, 44.0))).clicked() { back = true; }
        let (label, target, enabled) = match step {
            Step::Account => ("Continue without an account", Some(Step::Space), !busy),
            Step::Confirmed => ("Make It Yours", Some(Step::Space), true),
            Step::Space => ("Continue", Some(Step::Favorites), !ob.space_name.trim().is_empty() && ob.error.is_none()),
            Step::Favorites => ("Continue", Some(Step::Import), ob.error.is_none()),
            Step::Import => (if ob.progress.import_summary.is_some() { "Continue" } else { "Not Now" }, Some(Step::DefaultBrowser), !ob.import_pending),
            Step::DefaultBrowser => (if ob.settings_opened { "Continue" } else { "Not Now" }, Some(Step::Keys), true),
            Step::Keys => ("Continue", Some(Step::Ready), true),
            Step::Ready => ("Start Browsing", None, true),
            Step::Welcome => unreachable!(),
        };
        let width = if step == Step::Account { 258.0 } else { 172.0 };
        let r = Rect::from_min_size(pos2(footer.right() - width, footer.top()), vec2(width, 44.0));
        let click = Button::new(label).kind(if step == Step::Account { Kind::Secondary } else { Kind::Primary }).size(Size::Large).enabled(enabled)
            .show_at(&mut footer_ui, &ink, r).clicked();
        // Enter on the account page submits the form, never silently skips it.
        // Elsewhere, a focused control owns Enter; tab focus must not advance setup.
        let enter_primary = enter && step != Step::Account && !ctx.memory(|m| m.focused().is_some());
        if enabled && (click || enter_primary) {
            if step == Step::Ready { finish = true; } else { next = target; }
        }
        progress(&p, full, card.top() - 22.0, step, &Ink::new(backdrop, pal.accent));
    }

    if step == Step::Account && app.sync.signed_in() { next = Some(Step::Confirmed); }
    if back && !busy {
        if step == Step::Account && matches!(app.sync.auth, AuthStep::Code { .. }) {
            app.sync.cancel_auth();
            ob.code.clear();
            ob.focus_pending = true;
        } else {
            ob.navigate(app, None, now, back_key);
        }
    } else if let Some(step) = next {
        if ob.progress.step == Step::Account && !app.sync.busy { app.sync.cancel_auth(); }
        ob.navigate(app, Some(step), now, enter);
    }
    if finish {
        // Persist first. A failed write leaves the user on the recoverable final step.
        match app.lib.store.kv_set("onboarded", "1") {
            Ok(()) => {
                let _ = app.lib.store.kv_delete(CHECKPOINT);
                app.active_space = ob.progress.space_id.clone();
                app.actions.push(Action::OpenCommandBar { edit_current: false });
                return;
            }
            Err(e) => ob.error = Some(format!("Couldn't finish setup. Free some disk space and try again. {e}")),
        }
    }
    app.ui.onboarding = Some(ob);
}

fn take_back_key(ctx: &egui::Context, busy: bool) -> bool {
    // A colour picker owns Escape while it is open. Do not navigate away from
    // the space editor when the user only meant to dismiss its popup.
    !busy && !egui::Popup::is_any_open(ctx) && ctx.input_mut(|i| {
        i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::COMMAND, Key::OpenBracket)
    })
}

fn account(app: &mut CrossbowApp, ob: &mut Onboarding, ui: &mut Ui, ink: &Ink, now: f64, enter: bool, next: &mut Option<Step>) {
    let has_error = app.sync.error.is_some();
    match app.sync.auth.clone() {
        // A build without a sync server: Continue without an account is the way on.
        _ if !app.sync.has_server() => {
            ui.label(style::text(crossbow_core::sync::server::NO_SERVER, 15.0, weight::MEDIUM));
        }
        AuthStep::Email => {
            style::field_label(ui, ink, "Email address");
            let r = style::field(ui, ink, Id::unique("setup-email"), &mut ob.email, "you@example.com", ui.available_width(), has_error);
            if ob.focus_pending { r.request_focus(); ob.focus_pending = false; }
            ui.add_space(16.0);
            let valid = valid_email(&ob.email);
            if (Button::new("Get a Sign-in Code").kind(Kind::Primary).size(Size::Large).enabled(valid && !app.sync.busy).loading(app.sync.busy).show(ui, ink).clicked() || enter && valid) && !app.sync.busy {
                app.sync.start_auth(&ob.email);
                ob.resend_at = now + 30.0;
                ob.focus_pending = true;
            }
            ui.add_space(18.0);
            ui.label(style::text("No password. Development servers show a test code instead of sending email.", 13.0, weight::REGULAR).color(ink.secondary));
        }
        AuthStep::Code { challenge_id, email, dev_code } => {
            ui.label(style::text(app.sync.auth.instruction(has_error), 15.0, weight::MEDIUM));
            ui.add_space(18.0);
            let (r, complete) = style::code_input(ui, ink, Id::unique("setup-code"), &mut ob.code, 6, has_error);
            if ob.focus_pending { r.request_focus(); ob.focus_pending = false; }
            if let Some(code) = dev_code {
                ui.add_space(10.0);
                ui.label(style::text(format!("Development server · test code {code}"), 12.0, weight::MEDIUM).color(ink.secondary));
            }
            ui.add_space(18.0);
            let ready = complete && !challenge_id.is_empty() && !app.sync.busy;
            if Button::new("Verify & Sign In").kind(Kind::Primary).size(Size::Large).enabled(ready).loading(app.sync.busy).show(ui, ink).clicked() || enter && ready {
                app.sync.verify(&ob.code);
            }
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if Button::new("Use a different email").kind(Kind::Plain).enabled(!app.sync.busy).show(ui, ink).clicked() {
                    app.sync.cancel_auth();
                    ob.code.clear();
                    ob.focus_pending = true;
                }
                let wait = (ob.resend_at - now).ceil().max(0.0) as u32;
                let label = if wait > 0 { format!("Resend in {wait}s") } else { "Resend code".into() };
                if Button::new(&label).kind(Kind::Plain).enabled(!app.sync.busy && (wait == 0 || challenge_id.is_empty() && has_error)).show(ui, ink).clicked() {
                    app.sync.start_auth(&email);
                    ob.code.clear();
                    ob.resend_at = now + 30.0;
                    ob.focus_pending = true;
                }
                if wait > 0 { ui.ctx().request_repaint_after(std::time::Duration::from_secs(1)); }
            });
        }
    }
    if let Some(error) = &app.sync.error {
        ui.add_space(12.0);
        ui.label(style::text(error, 14.0, weight::MEDIUM).color(ink.danger));
    }
    if app.sync.signed_in() { *next = Some(Step::Confirmed); }
}

const SITES: [(&str, &str); 12] = [
    ("Gmail", "https://mail.google.com/"), ("YouTube", "https://www.youtube.com/"), ("GitHub", "https://github.com/"),
    ("Notion", "https://www.notion.so/"), ("Figma", "https://www.figma.com/"), ("Linear", "https://linear.app/"),
    ("Slack", "https://app.slack.com/"), ("Calendar", "https://calendar.google.com/"), ("Spotify", "https://open.spotify.com/"),
    ("Reddit", "https://www.reddit.com/"), ("Wikipedia", "https://www.wikipedia.org/"), ("ChatGPT", "https://chatgpt.com/"),
];

fn favorites(app: &mut CrossbowApp, ob: &mut Onboarding, ui: &mut Ui, ink: &Ink) {
    let Some(profile) = app.lib.spaces.get(&ob.progress.space_id).map(|s| s.profile_id.clone()) else { return };
    let existing = app.lib.favorites(&profile);
    let columns = if ui.available_width() >= 520.0 { 3 } else { 2 };
    let gap = 10.0;
    let width = (ui.available_width() - gap * (columns - 1) as f32) / columns as f32;
    egui::Grid::new("setup-favorites").spacing(vec2(gap, gap)).num_columns(columns).show(ui, |ui| {
        for (i, (name, url)) in SITES.iter().enumerate() {
            let selected = existing.iter().find(|(_, item)| item.url.trim_end_matches('/') == url.trim_end_matches('/'));
            let (r, resp) = ui.allocate_exact_size(vec2(width, 62.0), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, selected.is_some(), *name));
            let p = ui.painter();
            p.rect_filled(r, style::radius::MD, if selected.is_some() { ink.fill_strong } else if resp.hovered() { ink.fill_hover } else { ink.fill });
            if resp.has_focus() { p.rect_stroke(r, style::radius::MD, Stroke::new(2.0, ink.accent), egui::StrokeKind::Inside); }
            icons::draw(p, Rect::from_center_size(pos2(r.left() + 23.0, r.center().y), Vec2::splat(21.0)), Icon::Globe, ink.secondary);
            style::paint(p, pos2(r.left() + 44.0, r.center().y), Align2::LEFT_CENTER, name, 14.0, weight::MEDIUM, ink.text);
            if selected.is_some() {
                icons::draw(p, Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), Vec2::splat(13.0)), Icon::Check, ink.text);
            }
            if resp.clicked() {
                let result = if let Some((id, _)) = selected { app.lib.delete_item(id) } else { app.lib.add_favorite(&profile, url, name).map(|_| ()) };
                ob.error = result.err().map(|e| format!("Couldn't save this favorite. {e}"));
            }
            if (i + 1) % columns == 0 { ui.end_row(); }
        }
    });
    ui.add_space(16.0);
    ui.label(style::text("Nothing selected? That's fine. You can add any site while browsing.", 13.0, weight::REGULAR).color(ink.secondary));
}

fn import(app: &mut CrossbowApp, ob: &mut Onboarding, ui: &mut Ui, ink: &Ink) {
    if ob.import_pending {
        // Run only after the progress state has been painted in the preceding frame.
        let result = if ob.source == Some(true) { crate::import::import_arc(&mut app.lib) } else { crate::import::import_chrome_bookmarks(&mut app.lib) };
        ob.import_pending = false;
        match result {
            Ok(report) => {
                ob.progress.import_summary = Some(report.summary());
                ob.error = None;
                ob.save(app);
                // Import may have reused the initial empty space.
                if let Some(space) = app.lib.spaces.get(&ob.progress.space_id) { ob.space_name = space.name.clone(); }
            }
            Err(error) => ob.error = Some(format!("Import couldn't finish. {error}")),
        }
    }
    if let Some(summary) = &ob.progress.import_summary {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
        style::check_badge(ui.painter(), pos2(r.left() + 23.0, r.center().y), 22.0, ink.primary, ink.on_primary, 1.0);
        style::paint(ui.painter(), pos2(r.left() + 60.0, r.center().y), Align2::LEFT_CENTER, "Your saved sites are here.", 17.0, weight::SEMIBOLD, ink.text);
        ui.add_space(10.0);
        ui.label(style::text(summary, 14.0, weight::REGULAR).color(ink.secondary));
        ui.add_space(18.0);
        ui.label(style::text("Import another browser later from Settings → General.", 13.0, weight::REGULAR).color(ink.secondary));
        return;
    }
    let arc = crate::import::arc_available();
    let chrome = crate::import::chrome_available();
    for (source, name, available, details) in [
        (true, "Arc", arc, "Spaces, themes, folders, pinned tabs and favorites"),
        (false, "Google Chrome", chrome, "Bookmarks and folders from the default profile"),
    ] {
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 76.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, available, ob.source == Some(source), name));
        let p = ui.painter();
        p.rect_filled(r, style::radius::MD, if ob.source == Some(source) { ink.fill_strong } else if resp.hovered() && available { ink.fill_hover } else { ink.fill });
        let c = pos2(r.left() + 26.0, r.center().y);
        icons::draw(p, Rect::from_center_size(c, Vec2::splat(26.0)), Icon::Globe, ink.secondary);
        style::paint(p, pos2(r.left() + 52.0, r.top() + 24.0), Align2::LEFT_CENTER, name, 16.0, weight::SEMIBOLD, ink.text);
        style::paint(p, pos2(r.left() + 52.0, r.top() + 50.0), Align2::LEFT_CENTER, if available { details } else { "No supported data found on this computer" }, 12.0, weight::REGULAR, ink.secondary);
        if resp.has_focus() { p.rect_stroke(r, style::radius::MD, Stroke::new(2.0, ink.accent), egui::StrokeKind::Inside); }
        if ob.source == Some(source) { icons::draw(p, Rect::from_center_size(pos2(r.right() - 22.0, r.center().y), Vec2::splat(18.0)), Icon::Check, ink.text); }
        if resp.clicked() && available { ob.source = Some(source); ob.error = None; }
        ui.add_space(10.0);
    }
    ui.add_space(8.0);
    ui.label(style::text("Passwords, cookies and extensions aren't imported. Arc's Today tabs go to the Archive; split views become individual pinned tabs.", 13.0, weight::REGULAR).color(ink.secondary));
    ui.add_space(18.0);
    let available = ob.source.map(|s| if s { arc } else { chrome }).unwrap_or(false);
    if Button::new("Import Selected Browser").icon(Icon::Download).kind(Kind::Primary).size(Size::Large).enabled(available).loading(ob.import_pending).show(ui, ink).clicked() {
        ob.import_pending = true;
        ui.ctx().request_repaint();
    }
    if !arc && !chrome {
        ui.add_space(12.0);
        ui.label(style::text("You can start fresh. Safari and Firefox import are not available in this build.", 13.0, weight::REGULAR).color(ink.secondary));
    }
}

fn default_browser(ob: &mut Onboarding, ui: &mut Ui, ink: &Ink) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 100.0), Sense::hover());
    icons::draw(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(62.0)), Icon::Globe, ink.text);
    ui.add_space(12.0);
    ui.label(style::text("Choose Crossbow as your default web browser in your system's settings. Opening settings does not change your default.", 15.0, weight::REGULAR).color(ink.secondary));
    ui.add_space(24.0);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if Button::new("Open System Settings").kind(Kind::Primary).size(Size::Large).trailing(Icon::Expand).show(ui, ink).clicked() {
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.Desktop-Settings.extension").status();
        #[cfg(target_os = "windows")]
        let result = std::process::Command::new("cmd").args(["/C", "start", "", "ms-settings:defaultapps"]).status();
        match result {
            Ok(status) if status.success() => { ob.settings_opened = true; ob.error = None; }
            _ => ob.error = Some("Couldn't open system settings. You can set the default manually or choose Not Now.".into()),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    ui.label(style::text("Open your desktop's Default Applications settings and select Crossbow under Web Browser.", 14.0, weight::REGULAR).color(ink.secondary));
    if ob.settings_opened {
        ui.add_space(16.0);
        ui.label(style::text("Return here when you're ready. We haven't assumed that you changed the default.", 13.0, weight::REGULAR).color(ink.secondary));
    }
}

fn keys(ob: &mut Onboarding, ui: &mut Ui, ink: &Ink) {
    let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
    for (i, (key, label, detail, icon)) in [
        (Key::T, "Open anything", "Search the web, enter a URL or switch to a tab.", Icon::Search),
        (Key::S, "Give the page more room", "Hide or show the sidebar.", Icon::Sidebar),
        (Key::D, "Keep a tab", "Pin the current tab in this space.", Icon::Pin),
    ].iter().enumerate() {
        if ui.ctx().input_mut(|input| input.consume_key(Modifiers::COMMAND, *key)) {
            ob.progress.shortcuts[i] = true;
        }
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 82.0), Sense::hover());
        let p = ui.painter();
        let key_rect = Rect::from_min_size(pos2(r.left(), r.top() + 13.0), vec2(64.0, 42.0));
        style::keycap(p, ink, key_rect, &format!("{cmd} {}", key.name()), if ob.progress.shortcuts[i] { 1.0 } else { 0.0 });
        style::paint(p, pos2(r.left() + 84.0, r.top() + 25.0), Align2::LEFT_CENTER, label, 16.0, weight::SEMIBOLD, ink.text);
        style::paint(p, pos2(r.left() + 84.0, r.top() + 50.0), Align2::LEFT_CENTER, detail, 13.0, weight::REGULAR, ink.secondary);
        icons::draw(p, Rect::from_center_size(pos2(r.right() - 12.0, r.top() + 34.0), Vec2::splat(19.0)), if ob.progress.shortcuts[i] { Icon::Check } else { *icon }, ink.secondary);
    }
    ui.add_space(12.0);
    ui.label(style::text("Prefer the mouse? Every action is available through the sidebar and menus too.", 13.0, weight::REGULAR).color(ink.secondary));
}

fn progress(p: &egui::Painter, full: Rect, y: f32, step: Step, ink: &Ink) {
    let width = 26.0;
    for i in 1..=6 {
        let x = full.center().x + (i as f32 - 3.5) * (width + 7.0);
        p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(width, 3.0)), 1.5, if i <= step.progress() { ink.text.gamma_multiply(0.8) } else { ink.text.gamma_multiply(0.20) });
    }
}

fn welcome(ui: &mut Ui, full: Rect, ink: &Ink, icon: egui::TextureId, error: Option<&str>) -> egui::Response {
    let compact = full.height() < 580.0;
    let icon_side = if compact { 88.0 } else { 112.0 };
    let title_size = if full.width() < 760.0 { 35.0 } else { 44.0 };
    let title_height = style::measure(ui.painter(), Step::Welcome.title(), title_size, weight::SEMIBOLD).y;
    let icon_gap = if compact { 20.0 } else { 24.0 };
    let action_gap = if compact { 24.0 } else { 32.0 };
    let feedback_height = if error.is_some() { 80.0 } else { 0.0 };
    let height = icon_side + icon_gap + title_height + 8.0 + 25.0 + action_gap + 44.0 + feedback_height;
    let top = (full.center().y - height / 2.0).max(full.top() + 48.0);
    let width = (full.width() - 48.0).min(480.0);
    let center_x = full.center().x;
    let mark = Rect::from_center_size(pos2(center_x, top + icon_side / 2.0), Vec2::splat(icon_side));
    let p = ui.painter();
    p.image(icon, mark, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    let title_y = mark.bottom() + icon_gap;
    style::paint(p, pos2(center_x, title_y), Align2::CENTER_TOP, Step::Welcome.title(), title_size, weight::SEMIBOLD, ink.text);
    let description_y = title_y + title_height + 8.0;
    style::paint_wrapped(p, pos2(center_x - width / 2.0, description_y), width, true, Step::Welcome.description(), 17.0, weight::REGULAR, ink.secondary, 25.0);
    let action = Rect::from_min_size(pos2(center_x - 94.0, description_y + 25.0 + action_gap), vec2(188.0, 44.0));
    let response = Button::new("Get Started").kind(Kind::Primary).size(Size::Large).trailing(Icon::Forward).show_at(ui, ink, action);
    if let Some(error) = error {
        let feedback = Rect::from_min_max(pos2(center_x - width / 2.0, action.bottom() + 12.0), pos2(center_x + width / 2.0, (action.bottom() + feedback_height).min(full.bottom() - 16.0)));
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("setup-welcome-error").max_rect(feedback));
        child.set_clip_rect(feedback.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().id_salt("setup-welcome-error-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
            ui.label(style::text(error, 13.0, weight::MEDIUM).color(ink.danger));
        });
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_uses_the_icon_without_slogan_and_fits_compact_recovery() {
        for size in [vec2(1360.0, 860.0), vec2(640.0, 420.0)] {
            for dark in [false, true] {
                for error in [None, Some("Couldn't save setup. Check free disk space and try again.")] {
                    let ctx = egui::Context::default();
                    crate::ui::window::install_fonts(&ctx);
                    let full = Rect::from_min_size(Pos2::ZERO, size);
                    let ink = Ink::new(if dark { Color32::from_gray(24) } else { Color32::from_gray(240) }, Color32::from_rgb(80, 90, 200));
                    let icon = egui::TextureId::User(42);
                    let mut action = Rect::NOTHING;
                    for frame in 0..3 {
                        let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(full), time: Some(frame as f64), ..Default::default() }, |ui| {
                            style::apply_visuals(ui, &ink);
                            action = welcome(ui, full, &ink, icon, error).rect;
                        });
                        assert_eq!(action.size(), vec2(188.0, 44.0));
                        assert_eq!(action.center().x, full.center().x);
                        assert!(full.shrink(16.0).contains_rect(action));
                        let mut texts = Vec::new();
                        let mut icon_count = 0;
                        for shape in &output.shapes {
                            if let egui::Shape::Text(text) = &shape.shape {
                                texts.push(text.galley.job.text.as_str());
                                assert!(full.shrink(12.0).contains_rect(shape.shape.visual_bounding_rect()), "text outside welcome: {}", text.galley.job.text);
                            }
                            if let egui::Shape::Mesh(mesh) = &shape.shape {
                                if mesh.texture_id == icon {
                                    icon_count += 1;
                                    assert!(mesh.vertices.iter().all(|v| v.color == Color32::WHITE));
                                }
                            }
                        }
                        assert_eq!(icon_count, 1);
                        assert!(texts.contains(&Step::Welcome.title()));
                        assert!(texts.contains(&Step::Welcome.description()));
                        assert!(texts.contains(&"Get Started"));
                        assert!(!texts.iter().any(|text| text.contains("Your choice to sync")));
                        if let Some(error) = error { assert!(texts.contains(&error)); }
                        output.textures_delta.clear();
                    }
                }
            }
        }
    }

    #[test]
    fn welcome_primary_action_supports_pointer_and_keyboard_focus() {
        for size in [vec2(1360.0, 860.0), vec2(640.0, 420.0)] {
            for key in [None, Some(Key::Space), Some(Key::Enter)] {
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let full = Rect::from_min_size(Pos2::ZERO, size);
                let ink = Ink::new(Color32::from_gray(240), Color32::from_rgb(80, 90, 200));
                let mut action = Rect::NOTHING;
                let mut focused = false;
                let mut clicks = 0;
                for frame in 0..8 {
                    let mut events = Vec::new();
                    if key.is_some() && frame == 2 {
                        for pressed in [true, false] { events.push(egui::Event::Key { key: Key::Tab, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }); }
                    }
                    if frame == 4 || frame == 5 {
                        if let Some(key) = key {
                            events.push(egui::Event::Key { key, physical_key: None, pressed: frame == 4, repeat: false, modifiers: Modifiers::NONE });
                        } else {
                            events.push(egui::Event::PointerMoved(action.center()));
                            events.push(egui::Event::PointerButton { pos: action.center(), button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Modifiers::NONE });
                        }
                    }
                    ctx.run_ui(egui::RawInput { screen_rect: Some(full), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                        style::apply_visuals(ui, &ink);
                        let response = welcome(ui, full, &ink, egui::TextureId::User(42), None);
                        action = response.rect;
                        if frame == 3 { focused = response.has_focus(); }
                        clicks += usize::from(response.clicked());
                    }).textures_delta.clear();
                }
                assert_eq!(clicks, 1, "activation at {size:?} by {key:?}");
                if key.is_some() { assert!(focused, "Tab must focus Get Started"); }
            }
        }
    }

    #[test]
    fn compact_welcome_keeps_retry_action_visible_while_long_errors_scroll() {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let full = Rect::from_min_size(Pos2::ZERO, vec2(640.0, 420.0));
        let ink = Ink::new(Color32::from_gray(240), Color32::from_rgb(80, 90, 200));
        let error = "Couldn't save setup. Check free disk space and try again. ".repeat(30);
        let mut action = Rect::NOTHING;
        let mut first_y = None;
        let mut last_y = None;
        for frame in 0..16 {
            let pointer = pos2(full.center().x, action.bottom() + 30.0);
            let mut events = Vec::new();
            if frame >= 3 { events.push(egui::Event::PointerMoved(pointer)); }
            if frame == 5 {
                events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -120.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE });
            }
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(full), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                style::apply_visuals(ui, &ink);
                action = welcome(ui, full, &ink, egui::TextureId::User(42), Some(&error)).rect;
                assert!(full.shrink(16.0).contains_rect(action));
            });
            for shape in &output.shapes {
                if let egui::Shape::Text(text) = &shape.shape {
                    if text.galley.job.text == error {
                        if frame == 3 { first_y = Some(text.pos.y); }
                        if frame == 15 { last_y = Some(text.pos.y); }
                        assert!(shape.clip_rect.top() >= action.bottom());
                        assert!(shape.clip_rect.bottom() <= full.bottom() - 16.0);
                    }
                }
            }
            output.textures_delta.clear();
        }
        assert!(last_y.unwrap() < first_y.unwrap() - 30.0, "long save errors must scroll independently of Get Started");
    }

    #[test]
    fn back_follows_the_signed_out_branch() {
        let mut p = Progress::default();
        for step in [Step::Account, Step::Space, Step::Favorites, Step::Import, Step::DefaultBrowser, Step::Keys, Step::Ready] { p.forward(step); }
        for expected in [Step::Keys, Step::DefaultBrowser, Step::Import, Step::Favorites, Step::Space, Step::Account, Step::Welcome] {
            assert!(p.back());
            assert_eq!(p.step, expected);
        }
        assert!(!p.back());
    }

    #[test]
    fn confirmation_only_exists_on_the_signed_in_path() {
        let mut p = Progress::default();
        p.forward(Step::Account);
        p.forward(Step::Confirmed);
        p.forward(Step::Space);
        assert!(p.back());
        assert_eq!(p.step, Step::Confirmed);
    }

    #[test]
    fn checkpoint_round_trips_without_auth_secrets() {
        let mut p = Progress::default();
        p.forward(Step::Account);
        p.forward(Step::Space);
        let saved = serde_json::to_string(&p).unwrap();
        let restored: Progress = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.step, Step::Space);
        assert_eq!(restored.history, p.history);
        assert!(!saved.contains("email") && !saved.contains("code") && !saved.contains("token"));
    }

    #[test]
    fn popup_owns_escape_before_setup_navigation() {
        let ctx = egui::Context::default();
        let popup = Id::unique("test-colour-popup");
        egui::Popup::open_id(&ctx, popup);
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Escape, physical_key: None, pressed: true,
                repeat: false, modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let ctx = ui.ctx();
            assert!(!take_back_key(ctx, false));
            assert!(ctx.input(|i| i.key_pressed(Key::Escape)));
            egui::Popup::close_id(ctx, popup);
            assert!(take_back_key(ctx, false));
            assert!(!take_back_key(ctx, false));
        }).textures_delta.clear();
    }

    #[test]
    fn busy_step_does_not_consume_back() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Escape, physical_key: None, pressed: true,
                repeat: false, modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let ctx = ui.ctx();
            assert!(!take_back_key(ctx, true));
            assert!(ctx.input(|i| i.key_pressed(Key::Escape)));
        }).textures_delta.clear();
    }

    #[test]
    fn email_is_checked_before_submission() {
        assert!(valid_email("person@example.com"));
        for invalid in ["", "person", "person@", "@example.com", "a@@b.com", "a@.com", "a@b.", "a b@example.com"] { assert!(!valid_email(invalid), "{invalid}"); }
    }
}
