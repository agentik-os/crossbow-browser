//! Settings → Passwords: the local vault preview, in the shared Settings row,
//! section, field and button vocabulary. Passwords are masked or hidden until
//! the user asks; nothing here is logged.

use crate::passwords::{COPY_UNAVAILABLE, Editor, FIELD_IDS, OFFER_TO_SAVE, OFFER_TO_SAVE_APPLIES, OFFER_TO_SAVE_PREVIEW, PageState, Passwords, RESET_ELSEWHERE_KEPT, ResetScope, SEARCH_CHARS, Section, field_id};
use crate::passwords::{NUMBER, card_field_id};
use crate::ui::anim;
use crate::ui::settings::{row, section, segmented, separator, toggle};
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use crossbow_core::vault::generator::{MAX_LENGTH, MIN_LENGTH};
use crossbow_core::vault::{EntrySummary, IDLE_TIMEOUT, KEYCHAIN_SERVICE, KeyLocation, MAX_NOTES_BYTES, MAX_ORIGIN_BYTES, MAX_PASSWORD_BYTES, MAX_USERNAME_BYTES, VaultError, VaultStatus, qa_key_file};
use egui::{Align2, FontId, Id, Rect, Sense, Stroke, Ui, vec2};

const BANNER: &str = "Preview — not security reviewed. Use test passwords only.";
const QA_BANNER: &str = "QA key file mode (CROSSBOW_VAULT_KEY_FILE=1): the vault key is a plain file beside the vault, not in the Keychain. Anyone with a copy of the data folder can open these passwords.";
const QA_BADGE: &str = "QA key file";
const OS_USER: &str = if cfg!(target_os = "macos") { "this macOS user" } else { "this system user" };
/// How profiles divide what pages are offered.
const PROFILES: &str = "Pages are offered and filled only with passwords saved in their own profile.";
const NEVER_SAVED_DESC: &str = "Crossbow won't offer to save passwords or addresses for these sites in this profile.";
/// The text fields that can hold a secret.
const SECRET_FIELDS: [&str; 2] = [FIELD_IDS[3], FIELD_IDS[4]];

/// Where the vault key is kept, in words.
fn key_store(location: KeyLocation) -> &'static str {
    match location {
        KeyLocation::QaFile => "a plain QA key file beside the vault",
        KeyLocation::SecureStorage if cfg!(target_os = "macos") => "the macOS Keychain",
        KeyLocation::SecureStorage => "your system's secure storage",
    }
}

/// What a click asked for, applied after the list is drawn.
enum Action {
    Reveal(String),
    Copy(String),
    Edit(String),
    Delete(String),
    ConfirmDelete,
    CancelDelete,
    Generate,
    Save,
    CancelEditor,
    UseInProfile(String),
}

pub fn show(state: &mut Passwords, ui: &mut Ui, pal: &Palette) {
    state.page_shown(ui.ctx());
    if user_activity(ui) { state.touch(); }
    // Copy and Cut never reach a secret field, even while it is shown: the
    // timed, concealed Copy button is the only way a password leaves the vault.
    if ui.memory(|memory| SECRET_FIELDS.iter().any(|name| memory.has_focus(field_id(name)))) {
        ui.input_mut(|input| input.events.retain(|event| !matches!(event, egui::Event::Copy | egui::Event::Cut)));
    }
    let ink = Ink::new(pal.panel, pal.accent);
    let location = state.key_location();
    banner(ui, &ink, None, BANNER);
    if location == KeyLocation::QaFile { banner(ui, &ink, Some(QA_BADGE), QA_BANNER); }
    let status = state.status();
    // In every state, until those files are moved or deleted.
    if let Some(left) = state.left_by_recover() {
        ui.label(style::text(left, 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    if let Some(set_aside) = state.set_aside_by_recover() {
        ui.label(style::text(set_aside, 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    match status {
        Err(error) => {
            message(ui, &error.to_string(), ink.danger);
            // Why a Reset of the unopened file failed, such as a read-only folder.
            if let Some(failed) = state.page.error.as_ref().filter(|failed| **failed != error) {
                ui.label(style::text(failed.to_string(), 12.5, style::weight::REGULAR).color(ink.danger));
            }
            // Nothing is offered that can't change the state, Try Again included.
            if error.can_open_without() {
                stopped_recover(state, !error.is_final(), ui, &ink);
            } else if !error.is_final() {
                recovery(state, error.needs_reset(), true, ui, &ink);
            }
            offer_to_save(state, ui, pal, &ink);
        }
        Ok(VaultStatus::Unlocked) => unlocked(state, ui, pal, &ink),
        Ok(status) => locked(state, status == VaultStatus::New, location, ui, pal, &ink),
    }
    // Only while Crossbow is the active app: held in the background, it would
    // block other apps' keystroke features.
    let typing_password = ui.input(|input| input.focused) && ui.memory(|memory| memory.has_focus(field_id(FIELD_IDS[3])) || memory.has_focus(card_field_id(NUMBER)));
    state.set_secure_input(typing_password);
}

/// Clicks, keys, typing and scrolling on the page postpone the idle lock.
fn user_activity(ui: &Ui) -> bool {
    ui.input(|input| input.events.iter().any(|event| matches!(event,
        egui::Event::Key { pressed: true, .. } | egui::Event::Text(_) | egui::Event::Paste(_)
        | egui::Event::PointerButton { pressed: true, .. } | egui::Event::MouseWheel { .. })))
}

fn banner(ui: &mut Ui, ink: &Ink, badge: Option<&str>, text: &str) {
    egui::Frame::new().fill(ink.danger.gamma_multiply(0.10)).stroke(Stroke::new(1.0, ink.danger.gamma_multiply(0.45))).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if let Some(badge) = badge {
            egui::Frame::new().fill(ink.danger).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.label(style::text(badge, 11.5, style::weight::SEMIBOLD).color(ink.surface));
            });
        }
        ui.label(style::text(text, 12.5, style::weight::SEMIBOLD).color(ink.danger));
    });
}

/// A notice or error, scrolled into view the first time it appears.
fn message(ui: &mut Ui, text: &str, color: egui::Color32) {
    let response = ui.label(style::text(text, 12.5, style::weight::REGULAR).color(color));
    let key = Id::unique("passwords-message");
    if ui.is_sizing_pass() || ui.data(|data| data.get_temp::<String>(key)).as_deref() == Some(text) { return; }
    ui.data_mut(|data| data.insert_temp(key, text.to_owned()));
    response.scroll_to_me(None);
}

fn messages(page: &PageState, anchor: Option<&str>, ui: &mut Ui, ink: &Ink) {
    if page.anchor.as_deref() != anchor { return; }
    if let Some(error) = &page.error {
        // Only a draft kept through a reset survives a lock.
        let kept = *error == VaultError::ResetElsewhere && page.editor.is_some();
        let text = if kept { RESET_ELSEWHERE_KEPT.to_owned() } else { error.to_string() };
        message(ui, &text, ink.danger);
    }
    if let Some(notice) = &page.notice { message(ui, notice, ink.secondary); }
}

fn locked(state: &mut Passwords, new: bool, location: KeyLocation, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let created = if new { ", which the first unlock creates" } else { "" };
    let desc = format!("Stored only on this device and encrypted with a key kept in {}{created}. Every Crossbow account and profile for {OS_USER} shares it. {PROFILES} Passwords and saved addresses aren't synced.", key_store(location));
    let mut unlock = false;
    row(ui, pal, "Passwords are locked", &desc, |ui| {
        unlock = Button::new("Unlock").kind(Kind::Primary).show(ui, ink).clicked();
    });
    if unlock { state.unlock(); }
    messages(&state.page, None, ui, ink);
    // A draft kept through a reset elsewhere stays hidden until Unlock.
    if state.kept_draft() && Button::new("Discard Entry").kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked() { state.discard_draft(); }
    let needs_reset = state.page.error.as_ref().is_some_and(VaultError::needs_reset);
    recovery(state, needs_reset, false, ui, ink);
    key_details(state, new, ui, ink);
    offer_to_save(state, ui, pal, ink);
    never_saved(state, ui, pal, ink);
}

/// Which item holds this vault's key, so it can be told apart from items left
/// by earlier resets or other data folders. The vault id isn't secret.
fn key_details(state: &mut Passwords, new: bool, ui: &mut Ui, ink: &Ink) {
    let open = state.page.key_details;
    if Button::new(if open { "Hide Key Details" } else { "Key Details" }).kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked() {
        state.page.key_details = !open;
    }
    if !state.page.key_details { return; }
    let text = match state.live_key() {
        Some((location, id)) => key_item(location, &id, new),
        None => "The vault file couldn't be read to name its key.".into(),
    };
    ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.secondary));
}

fn key_item(location: KeyLocation, id: &str, new: bool) -> String {
    let others = "Items under other accounts belong to earlier resets or other data folders.";
    let made = if new { ", which the first unlock creates" } else { "" };
    match location {
        KeyLocation::QaFile => format!("QA key file {}, in the data folder.", qa_key_file(id)),
        KeyLocation::SecureStorage if cfg!(target_os = "macos") => format!("Keychain item “{KEYCHAIN_SERVICE}”, account {id}{made}. This vault uses only this item. {others}"),
        KeyLocation::SecureStorage => format!("Secure storage entry “{KEYCHAIN_SERVICE}”, account {id}{made}. This vault uses only this entry. {others}"),
    }
}

/// Try Again comes first and changes nothing. For a damaged file whose key
/// is available, Recover Passwords… comes next: it keeps every entry whose
/// sealed cells all still open, and deletes nothing. Reset is offered only
/// for the errors it resolves, last, and only after a confirmation that says
/// what it does: it empties the file, or moves a damaged one aside, and is
/// reported only once the file it leaves was checked.
/// `open_failed` means the vault file itself couldn't be opened.
fn recovery(state: &mut Passwords, needs_reset: bool, open_failed: bool, ui: &mut Ui, ink: &Ink) {
    if state.page.confirm_reset { return reset_prompt(state, ui, ink); }
    if state.page.confirm_recover { return recover_prompt(state, ui, ink); }
    if !needs_reset && !open_failed { return; }
    let can_recover = state.can_recover();
    let (mut retry, mut recover, mut reset) = (false, false, false);
    ui.horizontal_wrapped(|ui| {
        retry = Button::new("Try Again").show(ui, ink).clicked();
        if can_recover { recover = Button::new("Recover Passwords…").show(ui, ink).clicked(); }
        if needs_reset { reset = Button::new("Reset Vault…").kind(Kind::Plain).show(ui, ink).clicked(); }
    });
    if retry {
        if open_failed { state.retry_open(); } else { state.unlock(); }
    }
    if recover { state.page.confirm_recover = true; }
    if reset { state.page.confirm_reset = true; }
}

/// While a Recover Passwords that stopped keeps the vault closed: Try Again
/// first, unless it can't change anything, then Open Without Them…, which
/// asks first. Cancel is the question's default: it takes focus when the
/// question appears, so Return confirms only once focus is moved to Open
/// Without Them, and Escape cancels, as does leaving the page.
fn stopped_recover(state: &mut Passwords, can_retry: bool, ui: &mut Ui, ink: &Ink) {
    if state.page.confirm_open_without { return open_without_prompt(state, ui, ink); }
    let (mut retry, mut ask) = (false, false);
    ui.horizontal_wrapped(|ui| {
        if can_retry { retry = Button::new("Try Again").show(ui, ink).clicked(); }
        ask = Button::new("Open Without Them…").show(ui, ink).clicked();
    });
    if retry { state.retry_open(); }
    if ask {
        state.page.confirm_open_without = true;
        state.page.focus_cancel = true;
    }
}

fn open_without_prompt(state: &mut Passwords, ui: &mut Ui, ink: &Ink) {
    let question = state.open_without_question();
    let escape = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let Some(question) = question.filter(|_| !escape) else {
        state.page.confirm_open_without = false;
        return;
    };
    ui.label(style::text(question, 12.5, style::weight::REGULAR).color(ink.text));
    let (mut open, mut cancel) = (false, false);
    ui.horizontal_wrapped(|ui| {
        open = Button::new("Open Without Them").kind(Kind::Primary).show(ui, ink).clicked();
        let response = Button::new("Cancel").kind(Kind::Plain).show(ui, ink);
        if std::mem::take(&mut state.page.focus_cancel) { response.request_focus(); }
        cancel = response.clicked();
    });
    if open { state.open_without(); }
    if cancel { state.page.confirm_open_without = false; }
}

fn recover_prompt(state: &mut Passwords, ui: &mut Ui, ink: &Ink) {
    let prompt = "Copy the passwords, addresses, cards and never-save sites that can still be read into a new vault file under the same key? The damaged file is kept beside it in Crossbow's data folder.";
    ui.label(style::text(prompt, 12.5, style::weight::REGULAR).color(ink.text));
    ui.horizontal_wrapped(|ui| {
        if Button::new("Recover Passwords").kind(Kind::Primary).show(ui, ink).clicked() { state.recover(); }
        if Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked() { state.page.confirm_recover = false; }
    });
}

fn reset_prompt(state: &mut Passwords, ui: &mut Ui, ink: &Ink) {
    let kept = "No existing key is deleted, since a copy of this vault, such as a backup, may still need it. This can't be undone.";
    let (prompt, confirm) = match state.reset_scope() {
        ResetScope::Entries { passwords, addresses, cards } => {
            let count = |n: usize, one: &str, many: &str| format!("{n} saved {}", if n == 1 { one } else { many });
            let mut items = vec![count(passwords, "password", "passwords")];
            if addresses > 0 { items.push(count(addresses, "address", "addresses")); }
            if cards > 0 { items.push(count(cards, "card", "cards")); }
            let listed = match items.split_last() {
                Some((last, [])) => last.clone(),
                Some((last, first)) => format!("{} and {last}", first.join(", ")),
                None => String::new(),
            };
            (format!("Delete {listed} from this vault file and start a new, empty vault with a new key? {kept}"), "Delete All and Reset")
        }
        // Nothing is deleted: the file is moved aside, for the key Reset keeps.
        ResetScope::DamagedFile => (
            "Move the damaged vault file aside and start a new, empty vault with a new key? The old file stays in Crossbow's data folder, and no existing key is deleted.".to_owned(),
            "Move Aside and Reset",
        ),
    };
    ui.label(style::text(prompt, 12.5, style::weight::REGULAR).color(ink.text));
    ui.horizontal_wrapped(|ui| {
        if Button::new(confirm).kind(Kind::Primary).show(ui, ink).clicked() { state.reset(); }
        if Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked() { state.page.confirm_reset = false; }
    });
}

fn unlocked(state: &mut Passwords, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let mut lock = false;
    let desc = format!("Locks after {} minutes without activity and when you sign out. Signing out doesn't hide it from other Crossbow accounts for {OS_USER}.", IDLE_TIMEOUT.as_secs() / 60);
    row(ui, pal, "Unlocked", &desc, |ui| {
        lock = Button::new("Lock Now").show(ui, ink).clicked();
    });
    if lock { return state.lock(Some("Locked.")); }
    messages(&state.page, None, ui, ink);
    key_details(state, false, ui, ink);
    if !state.unlocked() { return; }

    let mut shown = state.page.section;
    let sections = [(Section::Passwords, "Passwords"), (Section::Addresses, "Addresses"), (Section::Cards, "Cards")];
    if ui.push_id("passwords-section", |ui| segmented(ui, pal, &mut shown, &sections)).inner {
        state.show_section(shown);
    }
    if state.page.section == Section::Cards {
        card_settings::cards(state, ui, pal, ink);
        return;
    }
    if state.page.section == Section::Addresses {
        address_settings::addresses(state, ui, pal, ink);
        offer_to_save(state, ui, pal, ink);
        never_saved(state, ui, pal, ink);
        return;
    }

    let mut add = false;
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 150.0).clamp(160.0, 360.0);
        field(ui, ink, field_id(FIELD_IDS[0]), &mut state.page.search, "Search websites and usernames", width, false, SEARCH_CHARS);
        add = Button::new("Add Password").kind(Kind::Primary).show(ui, ink).clicked();
    });
    forget_undo(ui.ctx(), field_id(FIELD_IDS[0]));
    if add { state.start_add(); }
    let can_copy = state.can_copy();
    if !can_copy {
        ui.label(style::text(COPY_UNAVAILABLE, 12.0, style::weight::REGULAR).color(ink.secondary));
    }

    let names: Vec<(String, String)> = state.profile_names().to_vec();
    let tag_here = !state.current_profile().is_empty();
    let profile_label = |id: Option<&str>| match id {
        Some(id) => format!("Profile: {}", names.iter().find(|(profile, _)| profile == id).map_or("another profile", |(_, name)| name.as_str())),
        None => "In no profile, so never offered on pages".to_owned(),
    };
    let mut actions = Vec::new();
    let (vault, page) = state.parts();
    if let Some(editor) = page.editor.as_mut().filter(|editor| editor.id.is_none()) {
        editor_form(editor, ui, pal, ink, &mut actions);
    }
    let Some(vault) = vault else { return };
    let shown = vault.search(&page.search);
    if vault.entries().is_empty() {
        ui.label(style::text("No saved passwords yet.", 12.5, style::weight::REGULAR).color(ink.secondary));
    } else if shown.is_empty() {
        ui.label(style::text("No passwords match this search.", 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    for entry in shown {
        let id = entry.id.as_str();
        ui.push_id(id, |ui| {
            entry_header(entry, page.revealed.get(id).map(|password| password.as_str()), &profile_label(entry.profile.as_deref()), ui, pal, ink);
            if page.confirm_delete.as_deref() == Some(id) {
                delete_prompt(entry, ui, ink, &mut actions);
            } else if let Some(editor) = page.editor.as_mut().filter(|editor| editor.id.as_deref() == Some(id)) {
                editor_form(editor, ui, pal, ink, &mut actions);
            } else {
                entry_actions(entry, page.revealed.contains_key(id), can_copy, tag_here, ui, ink, &mut actions);
            }
            messages(page, Some(id), ui, ink);
        });
        separator(ui, pal);
    }

    for action in actions {
        match action {
            Action::Reveal(id) => state.toggle_reveal(&id),
            Action::Copy(id) => state.copy(&id),
            Action::Edit(id) => state.start_edit(&id),
            Action::Delete(id) => state.request_delete(&id),
            Action::ConfirmDelete => state.confirm_delete(),
            Action::CancelDelete => state.page.confirm_delete = None,
            Action::Generate => state.generate(),
            Action::Save => state.save_editor(),
            Action::CancelEditor => state.close_editor(),
            Action::UseInProfile(id) => state.use_in_current_profile(&id),
        }
    }
    offer_to_save(state, ui, pal, ink);
    never_saved(state, ui, pal, ink);
}

/// "Offer to save passwords from websites", for the active profile, whatever
/// the vault's state: off, pages get no submit listener at all.
fn offer_to_save(state: &mut Passwords, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let profile = state.current_profile().to_owned();
    if profile.is_empty() { return; }
    section(ui, pal, "Saving from websites");
    ui.label(style::text(OFFER_TO_SAVE_PREVIEW, 12.0, style::weight::REGULAR).color(ink.danger));
    let mut on = state.offer_to_save(&profile);
    let mut changed = false;
    ui.push_id("offer-to-save", |ui| row(ui, pal, OFFER_TO_SAVE, OFFER_TO_SAVE_APPLIES, |ui| {
        changed = in_view(toggle(ui, &mut on, pal.accent)).changed();
    }));
    if changed { state.set_offer_to_save(&profile, on); }
    if let Some(error) = state.offer_to_save_error(&profile) { message(ui, error, ink.danger); }
}

/// Sites this profile chose Never for This Site on, readable while locked.
fn never_saved(state: &mut Passwords, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let profile = state.current_profile().to_owned();
    if profile.is_empty() { return; }
    let sites = state.never_saved(&profile);
    if sites.is_empty() { return; }
    section(ui, pal, "Never saved");
    ui.label(style::text(NEVER_SAVED_DESC, 12.0, style::weight::REGULAR).color(ink.secondary));
    let mut remove = None;
    for site in &sites {
        ui.push_id(site, |ui| row(ui, pal, site, "", |ui| {
            if Button::new("Remove").size(Size::Small).show(ui, ink).clicked() { remove = Some(site.clone()); }
        }));
    }
    if let Some(site) = remove { state.remove_never_save(&profile, &site); }
}

fn entry_header(entry: &EntrySummary, revealed: Option<&str>, profile: &str, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    if !entry.readable() {
        ui.label(style::text("Damaged entry", 13.5, style::weight::REGULAR).color(pal.panel_text));
        ui.label(style::text(VaultError::Unreadable.to_string(), 12.0, style::weight::REGULAR).color(ink.danger));
        return;
    }
    ui.label(style::text(entry.origin.as_str(), 13.5, style::weight::REGULAR).color(pal.panel_text));
    let username = if entry.username.is_empty() { "No username" } else { entry.username.as_str() };
    ui.label(style::text(username, 12.0, style::weight::REGULAR).color(pal.panel_dim));
    ui.label(style::text(format!("{profile} · {}", last_used(entry.used_ms, crossbow_core::now_ms())), 12.0, style::weight::REGULAR).color(pal.panel_dim));
    if let Some(password) = revealed {
        // Not selectable: Copy is the only way out, so the clipboard timer applies.
        ui.add(egui::Label::new(egui::RichText::new(password).monospace().size(13.0).color(ink.text)).selectable(false));
    }
}

/// When an entry was last filled into a page, counted in whole days, as the library counts them.
fn last_used(used_ms: Option<u64>, now_ms: u64) -> String {
    let Some(used) = used_ms else { return "Never used on a page".into() };
    match now_ms.saturating_sub(used) / 86_400_000 {
        0 => "Last used today".into(),
        1 => "Last used yesterday".into(),
        days => format!("Last used {days} days ago"),
    }
}

fn entry_actions(entry: &EntrySummary, revealed: bool, can_copy: bool, tag_here: bool, ui: &mut Ui, ink: &Ink, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        if entry.readable() {
            if Button::new(if revealed { "Hide" } else { "Reveal" }).size(Size::Small).show(ui, ink).clicked() { actions.push(Action::Reveal(entry.id.clone())); }
            if Button::new("Copy").size(Size::Small).enabled(can_copy).show(ui, ink).clicked() { actions.push(Action::Copy(entry.id.clone())); }
            if Button::new("Edit").size(Size::Small).show(ui, ink).clicked() { actions.push(Action::Edit(entry.id.clone())); }
            // Entries from before profiles are filled nowhere until put in one.
            if entry.profile.is_none() && tag_here && Button::new("Use in Current Profile").size(Size::Small).show(ui, ink).clicked() {
                actions.push(Action::UseInProfile(entry.id.clone()));
            }
        }
        if Button::new("Delete…").size(Size::Small).kind(Kind::Plain).show(ui, ink).clicked() { actions.push(Action::Delete(entry.id.clone())); }
    });
}

fn delete_prompt(entry: &EntrySummary, ui: &mut Ui, ink: &Ink, actions: &mut Vec<Action>) {
    let prompt = if !entry.readable() {
        "Delete this damaged entry? This can't be undone.".to_owned()
    } else if entry.username.is_empty() {
        format!("Delete the password for {}? This can't be undone.", entry.origin.as_str())
    } else {
        format!("Delete the password for {} on {}? This can't be undone.", entry.username.as_str(), entry.origin.as_str())
    };
    ui.label(style::text(prompt, 12.5, style::weight::REGULAR).color(ink.text));
    ui.horizontal_wrapped(|ui| {
        if Button::new("Delete").kind(Kind::Primary).size(Size::Small).show(ui, ink).clicked() { actions.push(Action::ConfirmDelete); }
        if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked() { actions.push(Action::CancelDelete); }
    });
}

fn editor_form(editor: &mut Editor, ui: &mut Ui, pal: &Palette, ink: &Ink, actions: &mut Vec<Action>) {
    let width = ui.available_width().min(440.0);
    section(ui, pal, if editor.id.is_some() { "Edit password" } else { "New password" });
    let mut enter = false;
    style::field_label(ui, ink, "Website");
    let origin = field(ui, ink, field_id(FIELD_IDS[1]), &mut editor.origin, "https://example.com", width, false, MAX_ORIGIN_BYTES);
    if std::mem::take(&mut editor.focus) {
        origin.request_focus();
        origin.scroll_to_me(None);
    }
    enter |= submitted(ui, &origin);
    style::field_label(ui, ink, "Username");
    let username = field(ui, ink, field_id(FIELD_IDS[2]), &mut editor.username, "Optional", width, false, MAX_USERNAME_BYTES);
    enter |= submitted(ui, &username);
    style::field_label(ui, ink, "Password");
    let password = field(ui, ink, field_id(FIELD_IDS[3]), &mut editor.password, "Required", width, !editor.show_password, MAX_PASSWORD_BYTES);
    enter |= submitted(ui, &password);
    ui.horizontal_wrapped(|ui| {
        if Button::new(if editor.show_password { "Hide Password" } else { "Show Password" }).size(Size::Small).show(ui, ink).clicked() { editor.show_password = !editor.show_password; }
        if Button::new("Generate").size(Size::Small).show(ui, ink).clicked() { actions.push(Action::Generate); }
    });
    ui.horizontal_wrapped(|ui| {
        let label = ui.label(style::text("Length", 12.5, style::weight::REGULAR).color(ink.text));
        ui.add(egui::DragValue::new(&mut editor.rules.length).range(MIN_LENGTH..=MAX_LENGTH)).labelled_by(label.id);
        ui.checkbox(&mut editor.rules.lowercase, "a–z");
        ui.checkbox(&mut editor.rules.uppercase, "A–Z");
        ui.checkbox(&mut editor.rules.digits, "0–9");
        ui.checkbox(&mut editor.rules.symbols, "Symbols");
        ui.checkbox(&mut editor.rules.avoid_ambiguous, "Avoid look-alikes");
    });
    style::field_label(ui, ink, "Notes");
    ui.add(egui::TextEdit::multiline(&mut *editor.notes).id(field_id(FIELD_IDS[4])).hint_text("Optional").char_limit(MAX_NOTES_BYTES).desired_rows(3).desired_width(width));
    for name in &FIELD_IDS[1..] { forget_undo(ui.ctx(), field_id(name)); }
    if let Some(error) = &editor.error {
        ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
    }
    ui.horizontal_wrapped(|ui| {
        if Button::new("Save").kind(Kind::Primary).show(ui, ink).clicked() || enter { actions.push(Action::Save); }
        if Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked() { actions.push(Action::CancelEditor); }
    });
}

/// Keep a control the keyboard reached on screen: at the compact size a
/// long page otherwise leaves Tab's focus below the fold.
fn in_view(response: egui::Response) -> egui::Response {
    if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
    response
}

fn submitted(ui: &Ui, response: &egui::Response) -> bool {
    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

/// egui keeps every edit for undo and saves field state with the window's
/// memory. Vault fields keep no undo history, so Undo can't bring back a
/// cleared password, and their state is kept as temporary data, which eframe
/// never writes to its storage.
fn forget_undo(ctx: &egui::Context, id: Id) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        state.clear_undoer();
        ctx.data_mut(|data| data.insert_temp(id, state));
    }
}

/// The house text field, with masking and a character limit. `limit` is the
/// byte limit the vault enforces; the buffer reserves room for it.
fn field(ui: &mut Ui, ink: &Ink, id: Id, value: &mut String, hint: &str, width: f32, masked: bool, limit: usize) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 46.0), Sense::hover());
    let painter = ui.painter().clone();
    let focused = ui.memory(|memory| memory.has_focus(id));
    let f = anim::spring(ui.ctx(), id.with("focus"), if focused { 1.0 } else { 0.0 }, 900.0);
    painter.rect_filled(rect, style::radius::MD, style::mix(ink.fill, ink.fill_hover, f));
    if f > 0.01 {
        painter.rect_stroke(rect, style::radius::MD, Stroke::new(1.5, ink.accent.gamma_multiply(f)), egui::StrokeKind::Inside);
    }
    let inner: Rect = rect.shrink2(vec2(14.0, 0.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
    fn house<'t>(edit: egui::TextEdit<'t>, id: Id, ink: &Ink, hint: &str, width: f32, masked: bool, limit: usize) -> egui::TextEdit<'t> {
        edit.id(id)
            .frame(egui::Frame::NONE)
            .font(FontId::proportional(16.0))
            .text_color(ink.text)
            .hint_text(style::text(hint, 16.0, style::weight::REGULAR).color(ink.secondary))
            .desired_width(width)
            .align(Align2::LEFT_CENTER)
            .password(masked)
            .char_limit(limit)
    }
    // egui's TextEdit copies its whole text into Strings it never wipes, every
    // frame it is drawn. A masked field the user isn't typing in hands it a
    // stand-in of the same length, masked the same, under the same id, so a
    // click lands as on the real one and the password is copied only while
    // its field has focus.
    if masked && !focused {
        let stand_in = "*".repeat(value.chars().count());
        return child.add(house(egui::TextEdit::singleline(&mut stand_in.as_str()), id, ink, hint, inner.width(), masked, limit));
    }
    child.add(house(egui::TextEdit::singleline(value), id, ink, hint, inner.width(), masked, limit))
}

#[path = "address_settings.rs"]
mod address_settings;
#[path = "card_settings.rs"]
mod card_settings;

#[cfg(test)]
#[path = "passwords_tests.rs"]
pub(crate) mod tests;
