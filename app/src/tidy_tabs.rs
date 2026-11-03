//! Tidy Tab Titles and Tidy Today (Arc's Tidy Tabs), two opt-in browser
//! features on the shared consent in `ai_features`. They send tab titles and
//! host names only, never full addresses, page contents, cookies or form data,
//! and only to the saved loopback provider after the server still lists the
//! saved model. Answers are parsed strictly and change nothing until the user
//! accepts them. Nothing typed or returned is logged.

use crate::ai_features::{self, Checked, Pending, Provider, Ready};
use crate::tabs::Tab;
use crossbow_ai::local::LocalError;
use crossbow_core::backup::{self, Reason};
use crossbow_core::library::{Library, LibraryError};
use crossbow_core::model::ItemKind;
use serde::Deserialize;

pub const TITLES: &str = "tidy_titles";
pub const TITLES_NAME: &str = "Tidy Tab Titles";
pub const TODAY: &str = "tidy_today";
pub const TODAY_NAME: &str = "Tidy Today";

/// The longest suggested title, in characters.
pub const MAX_TITLE_CHARS: usize = 40;
/// The longest proposed folder name, in characters.
pub const MAX_FOLDER_NAME_CHARS: usize = 32;
/// Tidy Today asks only for this many Today tabs or more…
pub const MIN_TABS: usize = 3;
/// …and at most this many, so the prompt stays within the chat limits.
pub const MAX_TABS: usize = 40;
pub const MAX_FOLDERS: usize = 6;
pub const MAX_FOLDER_TABS: usize = 20;

/// The production providers, with each answer bounded to what it can use.
pub const TITLE_PROVIDER: Checked = Checked { max_tokens: 48 };
pub const TODAY_PROVIDER: Checked = Checked { max_tokens: 768 };

// Per-tab bytes in a prompt. 40 tabs stay well under chat::MAX_PROMPT_BYTES.
const SENT_TITLE_BYTES: usize = 160;
const SENT_HOST_BYTES: usize = 80;

const BAD_TITLE: &str = "The model's answer wasn't a short one-line title.";
const BAD_GROUPS: &str = "The model's answer wasn't a valid list of folders.";

// -- Prompts and answers ------------------------------------------------------

/// Everything a prompt may say about one tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabFacts {
    pub title: String,
    pub host: String,
}

fn facts(title: &str, url: &str) -> TabFacts {
    TabFacts { title: title.into(), host: crossbow_core::search::host_of(url).unwrap_or_default() }
}

/// One proposed folder: its name and zero-based indexes into the tabs sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub tabs: Vec<usize>,
}

pub fn title_prompt(tab: &TabFacts) -> String {
    format!(
        "Suggest a shorter title for a pinned browser tab.\n\
         Reply with only the title: one line, at most {MAX_TITLE_CHARS} characters, plain text, no quotes and no markdown.\n\
         Tab title: {}\nSite: {}",
        sent_title(&tab.title, &tab.host), sent_host(&tab.host),
    )
}

pub fn groups_prompt(tabs: &[TabFacts]) -> Option<String> {
    if !(MIN_TABS..=MAX_TABS).contains(&tabs.len()) { return None; }
    let mut prompt = format!(
        "Group these browser tabs into folders by topic.\n\
         Reply with only JSON in exactly this shape: {{\"folders\":[{{\"name\":\"Folder name\",\"tabs\":[1,2]}}]}}\n\
         Use at most {MAX_FOLDERS} folders and at most {MAX_FOLDER_TABS} tabs per folder. Use each tab number at most once, and leave out tabs that fit no folder. \
         Folder names are plain text of at most {MAX_FOLDER_NAME_CHARS} characters.\nTabs:\n"
    );
    for (i, tab) in tabs.iter().enumerate() {
        prompt.push_str(&format!("{}. {} — {}\n", i + 1, sent_title(&tab.title, &tab.host), sent_host(&tab.host)));
    }
    Some(prompt)
}

/// `ai_features::one_line`, and nothing else that reads as markdown: no
/// emphasis or strike marks, setext rule or ordered-list marker.
fn plain(answer: &str, max_chars: usize) -> Option<String> {
    let line = ai_features::one_line(answer, max_chars).ok()?;
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let listed = digits > 0 && line[digits..].starts_with(['.', ')']) && line[digits + 1..].starts_with(' ');
    (!listed && !line.starts_with('=') && !["__", "~~"].iter().any(|mark| line.contains(mark))).then_some(line)
}

pub fn parse_title(answer: &str) -> Result<String, &'static str> {
    plain(answer, MAX_TITLE_CHARS).ok_or(BAD_TITLE)
}

/// Exactly `{"folders":[{"name":…,"tabs":[…]}]}` with one-based numbers of
/// the `count` tabs sent, each used at most once. Anything else is refused.
pub fn parse_groups(answer: &str, count: usize) -> Result<Vec<Group>, &'static str> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Folder { name: String, tabs: Vec<u64> }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Answer { folders: Vec<Folder> }
    let answer: Answer = serde_json::from_str(answer.trim()).map_err(|_| BAD_GROUPS)?;
    if answer.folders.is_empty() || answer.folders.len() > MAX_FOLDERS { return Err(BAD_GROUPS); }
    let mut seen = vec![false; count];
    let mut names: Vec<String> = Vec::new();
    let mut groups = Vec::new();
    for folder in answer.folders {
        let name = plain(&folder.name, MAX_FOLDER_NAME_CHARS).ok_or(BAD_GROUPS)?;
        let key = name.to_lowercase();
        if names.contains(&key) || folder.tabs.is_empty() || folder.tabs.len() > MAX_FOLDER_TABS { return Err(BAD_GROUPS); }
        let mut tabs = Vec::new();
        for number in folder.tabs {
            let index = usize::try_from(number).ok().and_then(|n| n.checked_sub(1)).filter(|&i| i < count).ok_or(BAD_GROUPS)?;
            if std::mem::replace(&mut seen[index], true) { return Err(BAD_GROUPS); }
            tabs.push(index);
        }
        names.push(key);
        groups.push(Group { name, tabs });
    }
    Ok(groups)
}

fn sent_title(title: &str, host: &str) -> String {
    let title: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if title.is_empty() { sent_host(host) } else { title };
    truncate(&title, SENT_TITLE_BYTES)
}

/// Host names only, so a hostile title or address can't break the line format.
fn sent_host(host: &str) -> String {
    let host: String = host.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']')).collect();
    truncate(&host, SENT_HOST_BYTES)
}

fn truncate(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].to_owned()
}

// -- Consent and requests -------------------------------------------------------

/// The destination a request for `feature` may use now, or a short reason.
/// Reads storage; call it on an action, not every frame.
pub fn allowed(lib: &Library, profile: &str, feature: &str, name: &str) -> Result<Ready, String> {
    if ai_features::private_profile(profile) { return Err(format!("{name} is off in private profiles.")); }
    ai_features::allowed(lib, profile, feature, name)
}

/// Whether the profile turned `feature` on, without checking the provider.
pub fn turned_on(lib: &Library, profile: &str, feature: &str) -> bool {
    ai_features::consented(lib, profile, feature).is_ok_and(|consent| consent.is_some())
}

/// A short reason for a notice; never the prompt or the answer.
fn failure(error: &LocalError) -> String {
    match error {
        LocalError::Cancelled => "Cancelled.".into(),
        LocalError::Timeout => "The local model didn't answer in time.".into(),
        LocalError::Busy => "Another AI request is running. Try again when it finishes.".into(),
        LocalError::InvalidModel => "The server no longer lists the saved model.".into(),
        LocalError::Unavailable => "Couldn't reach the local AI server.".into(),
        LocalError::Http(code) => format!("The local AI server returned HTTP {code}."),
        _ => "The model's answer couldn't be used.".into(),
    }
}

/// What a title suggestion was made from; it applies only while this holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleBasis {
    pub item: String,
    pub url: String,
    pub title: String,
    profile: String,
}

/// A pin that may get a tidy title: never one whose title was set by hand.
fn title_basis(lib: &Library, item_id: &str) -> Result<TitleBasis, String> {
    let item = lib.items.get(item_id).ok_or("This tab is no longer pinned.")?;
    if item.kind != ItemKind::Pinned { return Err("Only pinned tabs get tidy titles.".into()); }
    if item.custom_title.as_deref().is_some_and(|title| !title.is_empty()) { return Err("You renamed this tab, so its title stays.".into()); }
    Ok(TitleBasis { item: item_id.into(), url: item.url.clone(), title: item.title.clone(), profile: item.profile_id.clone() })
}

struct TitleRequest {
    basis: TitleBasis,
    job: Box<dyn Pending>,
}

pub struct Suggestion {
    pub basis: TitleBasis,
    pub text: String,
}

/// One Today tab as it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSnap {
    pub id: String,
    pub url: String,
    pub title: String,
    pub order: String,
}

fn snap(tab: &Tab) -> TabSnap {
    TabSnap { id: tab.id.clone(), url: tab.url.clone(), title: tab.display_title(), order: tab.order.clone() }
}

pub enum Stage {
    Asking,
    Review(Vec<Group>),
    Failed(String),
}

/// The Tidy Today sheet: one Space's Today tabs as sent, and the answer.
pub struct Sheet {
    pub space: String,
    pub profile: String,
    pub destination: String,
    pub tabs: Vec<TabSnap>,
    pub stage: Stage,
    job: Option<Box<dyn Pending>>,
}

impl Sheet {
    /// A tab sent was closed, moved, renamed, pinned or navigated since, or
    /// the Space changed profile. Tabs opened since don't count.
    pub fn stale(&self, lib: &Library, tabs: &[Tab]) -> bool {
        if lib.spaces.get(&self.space).map(|space| space.profile_id.as_str()) != Some(self.profile.as_str()) { return true; }
        let now: Vec<TabSnap> = crate::tabs::today(tabs, &self.space).into_iter()
            .filter(|tab| self.tabs.iter().any(|sent| sent.id == tab.id)).map(snap).collect();
        now != self.tabs
    }
}

/// What Accept made, so one Undo can remove it and put the tabs back.
pub struct Undo {
    pub space: String,
    pub summary: String,
    made: Vec<(String, Vec<String>)>,
    /// (tab, the pin it became, its Today order before).
    tabs: Vec<(String, String, String)>,
}

#[derive(Default)]
pub struct Tidy {
    title: Option<TitleRequest>,
    pub suggestion: Option<Suggestion>,
    queued: Option<String>,
    pub sheet: Option<Sheet>,
    pub undo: Option<Undo>,
    /// Short notices for toasts, drained by the app.
    pub notices: Vec<String>,
}

impl Tidy {
    /// Once per frame: start a new pin's request on `titles` and collect
    /// answers. Idle frames read no storage.
    pub fn tick(&mut self, lib: &Library, titles: &dyn Provider, ctx: &egui::Context) {
        if let Some(item) = self.queued.take() { self.tidy_title(lib, &item, titles, ctx); }
        self.poll_title(lib);
        self.poll_sheet(lib);
        if self.suggestion.as_ref().is_some_and(|suggestion| title_basis(lib, &suggestion.basis.item).as_ref() != Ok(&suggestion.basis)) {
            self.suggestion = None;
        }
    }

    // -- Tidy Tab Titles ------------------------------------------------------

    pub fn title_pending(&self) -> Option<&str> { self.title.as_ref().map(|request| request.basis.item.as_str()) }

    /// Why "Tidy Title" can't run for this pin, or what it would use.
    pub fn can_tidy_title(&self, lib: &Library, item_id: &str) -> Result<(TitleBasis, Ready), String> {
        if self.title.is_some() { return Err("Another title suggestion is in progress.".into()); }
        let basis = title_basis(lib, item_id)?;
        let ready = allowed(lib, &basis.profile, TITLES, TITLES_NAME)?;
        Ok((basis, ready))
    }

    pub fn tidy_title(&mut self, lib: &Library, item_id: &str, provider: &dyn Provider, ctx: &egui::Context) {
        let (basis, ready) = match self.can_tidy_title(lib, item_id) {
            Ok(request) => request,
            Err(why) => return self.notices.push(why),
        };
        match provider.start(&ready, title_prompt(&facts(&basis.title, &basis.url)), ctx) {
            Ok(job) => {
                self.suggestion = None;
                self.title = Some(TitleRequest { basis, job });
            }
            Err(error) => self.notices.push(format!("Title unchanged. {}", failure(&error))),
        }
    }

    /// A pin made while Tidy Tab Titles is on asks for a title next frame.
    /// While it is off, nothing is read or sent.
    pub fn pinned(&mut self, lib: &Library, item_id: &str) {
        let Some(profile) = lib.items.get(item_id).map(|item| item.profile_id.as_str()) else { return };
        if turned_on(lib, profile, TITLES) { self.queued = Some(item_id.into()); }
    }

    pub fn cancel_title(&mut self) {
        if let Some(request) = self.title.take() {
            request.job.cancel();
            self.notices.push("Title unchanged.".into());
        }
    }

    fn poll_title(&mut self, lib: &Library) {
        let Some(result) = self.title.as_ref().and_then(|request| request.job.poll()) else { return };
        let basis = self.title.take().unwrap().basis;
        if title_basis(lib, &basis.item).as_ref() != Ok(&basis) {
            return self.notices.push("The tab changed, so its title stays.".into());
        }
        if allowed(lib, &basis.profile, TITLES, TITLES_NAME).is_err() {
            return self.notices.push("Tidy Tab Titles is off, so the title stays.".into());
        }
        match result.map_err(|error| failure(&error)).and_then(|answer| parse_title(&answer).map_err(str::to_owned)) {
            Ok(text) if text == basis.title => self.notices.push("The title is already tidy.".into()),
            Ok(text) => self.suggestion = Some(Suggestion { basis, text }),
            Err(why) => self.notices.push(format!("Title unchanged. {why}")),
        }
    }

    /// Saves the suggestion as the pin's title, like a rename, so it persists
    /// and syncs. Never replaces a title set by hand meanwhile.
    pub fn accept_suggestion(&mut self, lib: &mut Library) {
        let Some(suggestion) = self.suggestion.take() else { return };
        if title_basis(lib, &suggestion.basis.item).as_ref() != Ok(&suggestion.basis) {
            return self.notices.push("The tab changed, so its title stays.".into());
        }
        let text = suggestion.text.clone();
        if lib.update_item(&suggestion.basis.item, |item| item.custom_title = Some(text)).is_err() {
            self.notices.push("Could not save the title. Try again.".into());
            self.suggestion = Some(suggestion);
        }
    }

    /// Hands the suggestion to the rename field: (pin, text).
    pub fn edit_suggestion(&mut self) -> Option<(String, String)> {
        self.suggestion.take().map(|suggestion| (suggestion.basis.item, suggestion.text))
    }

    /// The pin keeps its page title.
    pub fn revert_suggestion(&mut self) { self.suggestion = None; }

    // -- Tidy Today -----------------------------------------------------------

    /// Why "Tidy Today" can't run for this Space, or what it would use.
    pub fn can_tidy_today(&self, lib: &Library, tabs: &[Tab], space: &str) -> Result<(String, Ready), String> {
        if self.sheet.is_some() { return Err("Tidy Today is already open.".into()); }
        let profile = crate::chat::target_profile(lib, space).ok_or("This Space has no available profile.")?;
        let ready = allowed(lib, profile, TODAY, TODAY_NAME)?;
        let count = crate::tabs::today(tabs, space).len();
        if count < MIN_TABS { return Err(format!("Tidy Today needs at least {MIN_TABS} Today tabs.")); }
        if count > MAX_TABS { return Err(format!("Tidy Today works with up to {MAX_TABS} Today tabs.")); }
        Ok((profile.to_owned(), ready))
    }

    pub fn tidy_today(&mut self, lib: &Library, tabs: &[Tab], space: &str, provider: &dyn Provider, ctx: &egui::Context) {
        let (profile, ready) = match self.can_tidy_today(lib, tabs, space) {
            Ok(request) => request,
            Err(why) => return self.notices.push(why),
        };
        let sent: Vec<TabSnap> = crate::tabs::today(tabs, space).into_iter().map(snap).collect();
        let Some(prompt) = groups_prompt(&sent.iter().map(|tab| facts(&tab.title, &tab.url)).collect::<Vec<_>>()) else { return };
        let (stage, job) = match provider.start(&ready, prompt, ctx) {
            Ok(job) => (Stage::Asking, Some(job)),
            Err(error) => (Stage::Failed(failure(&error)), None),
        };
        self.sheet = Some(Sheet { space: space.into(), profile, destination: ready.destination(), tabs: sent, stage, job });
    }

    /// Asks again with the Space's current Today tabs.
    pub fn try_again(&mut self, lib: &Library, tabs: &[Tab], provider: &dyn Provider, ctx: &egui::Context) {
        let Some(space) = self.sheet.as_ref().map(|sheet| sheet.space.clone()) else { return };
        self.close_sheet();
        self.tidy_today(lib, tabs, &space, provider, ctx);
    }

    /// Cancel: a pending request is cancelled and nothing moves.
    pub fn close_sheet(&mut self) {
        if let Some(job) = self.sheet.take().and_then(|sheet| sheet.job) { job.cancel(); }
    }

    fn poll_sheet(&mut self, lib: &Library) {
        let Some(sheet) = self.sheet.as_mut() else { return };
        let Some(result) = sheet.job.as_ref().and_then(|job| job.poll()) else { return };
        sheet.job = None;
        sheet.stage = if allowed(lib, &sheet.profile, TODAY, TODAY_NAME).is_err() {
            Stage::Failed("Tidy Today was turned off or its provider changed.".into())
        } else {
            match result {
                Ok(answer) => match parse_groups(&answer, sheet.tabs.len()) {
                    Ok(groups) => Stage::Review(groups),
                    Err(why) => Stage::Failed(why.into()),
                },
                Err(error) => Stage::Failed(failure(&error)),
            }
        };
    }

    /// Applies the reviewed proposal in one commit: new folders at the end of
    /// the pinned area, the tabs pinned inside them in their Today order. A
    /// stale proposal or a failed save changes nothing.
    pub fn accept(&mut self, lib: &mut Library, tabs: &mut [Tab]) {
        let Some(sheet) = self.sheet.as_ref() else { return };
        let Stage::Review(groups) = &sheet.stage else { return };
        if sheet.stale(lib, tabs) { return; }
        let groups: Vec<(String, Vec<usize>)> = groups.iter().map(|group| {
            let mut order = group.tabs.clone();
            order.sort_unstable();
            (group.name.clone(), order)
        }).collect();
        let folders: Vec<(String, Vec<(String, String)>)> = groups.iter()
            .map(|(name, order)| (name.clone(), order.iter().map(|&i| (sheet.tabs[i].url.clone(), sheet.tabs[i].title.clone())).collect()))
            .collect();
        // A failed backup never stops the tidy; Restore Data lists it.
        backup::before(&lib.store, Reason::TidyToday);
        let made = match lib.tidy_into_folders(&sheet.space, &folders) {
            Ok(made) => made,
            Err(_) => return self.notices.push("Could not tidy Today. Nothing was changed. Try again.".into()),
        };
        let mut moved = Vec::new();
        for ((_, order), (_, items)) in groups.iter().zip(&made) {
            for (&i, item) in order.iter().zip(items) {
                let Some(tab) = tabs.iter_mut().find(|tab| tab.id == sheet.tabs[i].id) else { continue };
                moved.push((tab.id.clone(), item.clone(), tab.order.clone()));
                tab.item_id = Some(item.clone());
                tab.dirty = true;
            }
        }
        let summary = format!("Tidied {} tabs into {} {}.", moved.len(), made.len(), if made.len() == 1 { "folder" } else { "folders" });
        self.undo = Some(Undo { space: sheet.space.clone(), summary, made, tabs: moved });
        self.sheet = None;
    }

    /// One step back: removes the folders and pins Accept made and returns the
    /// tabs to Today in their exact order. Refused, changing nothing, once
    /// anything it made has changed.
    pub fn undo(&mut self, lib: &mut Library, tabs: &mut [Tab]) {
        let Some(undo) = self.undo.take() else { return };
        let linked = undo.tabs.iter().all(|(tab, item, _)| {
            tabs.iter().any(|t| t.id == *tab && t.item_id.as_deref() == Some(item.as_str()) && t.space_id == undo.space)
        });
        if !linked { return self.notices.push("The tidied tabs changed, so Undo is no longer available.".into()); }
        match lib.untidy_folders(&undo.space, &undo.made) {
            Ok(()) => {
                for (id, _, order) in &undo.tabs {
                    if let Some(tab) = tabs.iter_mut().find(|tab| tab.id == *id) {
                        tab.item_id = None;
                        // Defensive: Accept keeps each tab's order key, and only
                        // Today moves and unpinning rewrite one (unpinning
                        // refuses Undo above), so this matters only if a later
                        // change renumbers pinned tabs.
                        tab.order = order.clone();
                        tab.dirty = true;
                    }
                }
            }
            Err(LibraryError::Store(_) | LibraryError::Json(_)) => {
                self.notices.push("Could not undo. Nothing was changed. Try again.".into());
                self.undo = Some(undo);
            }
            Err(_) => self.notices.push("The tidied folders changed, so Undo is no longer available.".into()),
        }
    }
}

#[cfg(test)]
#[path = "tidy_tabs_tests.rs"]
pub(crate) mod tests;
