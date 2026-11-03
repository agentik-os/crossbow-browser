//! Content blocking in the app: each profile's Block Ads and Trackers and
//! Block Cookie Banners switches, kept on this device only, the lists that
//! come with the app (EasyList and EasyPrivacy, each on until a profile
//! turns it off) and the filter lists the person adds from files. Nothing
//! is downloaded.
//!
//! Lists open and compile, and their element hiding is built, off the UI
//! thread. Until a list is ready, the requests only it would block load and
//! aren't counted; nothing waits for it. A bundled list is compiled once
//! for every profile that uses it, and profiles with the same lists share
//! their element hiding. The engine gets a profile's rules whenever they
//! change, and asks them on its IO thread before each request.
//!
//! Each profile can turn blocking off on sites: every request made while a
//! tab shows such a site then loads, its frames' requests included, and
//! nothing in it is hidden.
//!
//! Element hiding comes from the same lists: each document asks for its
//! sheet when its script context starts, and gets the rules for every page
//! and its own host's, never another host's.

use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crossbow_core::content_blocking::bundled::{self, Bundled};
use crossbow_core::content_blocking::exceptions::{self, Exceptions};
use crossbow_core::content_blocking::filters::RequestType;
use crossbow_core::content_blocking::hiding::Hiding;
use crossbow_core::content_blocking::lists::{self, AddError, ListEntry, ListKind, Saved};
use crossbow_core::content_blocking::matcher::{self, CompiledList};
use crossbow_core::library::Library;
use crossbow_core::store::Store;
use crossbow_engine::{ContentRules, HidingSheet, ResourceKind, ResourceRequest};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub const ADD_LIST: &str = "Add List from File…";
pub const NO_LISTS: &str = "Add a list to start blocking.";
pub const LOADING: &str = "Loading… requests it would block still load until it's ready.";
pub const SITES_OFF: &str = "Sites with blocking off";
/// The shield, while no list a switch uses is ready yet.
pub const LOADING_LISTS: &str = "Lists are still loading, so nothing is blocked yet";
/// A bundled list's row while its switch is off.
pub const INCLUDED: &str = "Included with Crossbow";
/// A bundled list's row once this profile turned it off.
pub const TURNED_OFF: &str = "Off for this profile";

/// A list that comes with the app, for Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundledRow {
    pub id: &'static str,
    pub name: &'static str,
    pub credit: &'static str,
    /// Whether this profile uses it while the switch is on.
    pub on: bool,
    pub state: ListState,
    /// Its rules and unsupported rules, once compiled.
    pub counts: Option<(usize, usize)>,
}

/// "3 cosmetic rules skipped: too many".
pub fn skipped_note(skipped: usize) -> String {
    format!("{} cosmetic {} skipped: too many", grouped(skipped), if skipped == 1 { "rule" } else { "rules" })
}

/// Requests matching the profile's ad and tracker lists don't load, and
/// the elements they name are hidden.
#[derive(Debug)]
pub struct BlockAds;

impl SwitchSpec for BlockAds {
    const KEY: &'static str = "block_ads";
    const DEFAULT: bool = false;
    const TITLE: &'static str = "Block ads and trackers";
    const DESCRIPTION: &'static str = "Requests that match this profile's ad and tracker lists don't load, and elements they name are hidden. Third-party rules use a simplified site rule, so sites on a shared host such as github.io count as one site. A page can read the hiding rules it gets, so a site could learn some of the rules you use.";
    const STILL_OFF: &'static str = "Ads and trackers still load";
    const STILL_ON: &'static str = "Ads and trackers are still blocked";
    const UNREADABLE: &'static str = "The saved choice for blocking ads and trackers could not be read, so nothing is blocked. Changing it saves over it.";
}

/// Requests matching the profile's cookie banner lists don't load, and the
/// banners they name are hidden. Nothing answers a banner.
#[derive(Debug)]
pub struct BlockCookieBanners;

impl SwitchSpec for BlockCookieBanners {
    const KEY: &'static str = "block_cookie_banners";
    const DEFAULT: bool = false;
    const TITLE: &'static str = "Block cookie banners";
    const DESCRIPTION: &'static str = "Requests that match this profile's cookie banner lists don't load, and banners they name are hidden. Crossbow never answers a banner, so a hidden banner gives no consent. A page can read the hiding rules it gets, so a site could learn some of the rules you use.";
    const STILL_OFF: &'static str = "Cookie banners still load";
    const STILL_ON: &'static str = "Cookie banners are still blocked";
    const UNREADABLE: &'static str = "The saved choice for blocking cookie banners could not be read, so nothing is blocked. Changing it saves over it.";
}

/// "12,345 rules · 321 unsupported, skipped".
pub fn counts(rules: usize, unsupported: usize) -> String {
    let mut text = format!("{} {}", grouped(rules), if rules == 1 { "rule" } else { "rules" });
    if unsupported > 0 { text.push_str(&format!(" · {} unsupported, skipped", grouped(unsupported))); }
    text
}

/// "12,345".
pub(crate) fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 { text.push(','); }
        text.push(digit);
    }
    text
}

/// A profile's ready lists, their element hiding and its sites with
/// blocking off, as the engine asks them.
pub struct Rules {
    lists: Vec<Arc<CompiledList>>,
    sites_off: BTreeSet<String>,
    hiding: Arc<Hiding>,
}

impl Rules {
    /// `hiding` is built from `lists`, or from the lists before them while
    /// it is being rebuilt.
    pub fn new(lists: Vec<Arc<CompiledList>>, sites_off: BTreeSet<String>, hiding: Arc<Hiding>) -> Self {
        Rules { lists, sites_off, hiding }
    }

    /// Whether blocking is off on the site of the tab's top page, or of
    /// `document` when the top page isn't known.
    fn off_here(&self, document: &str, top: &str) -> bool {
        let page = if top.is_empty() { document } else { top };
        !self.sites_off.is_empty() && exceptions::site_of(page).is_some_and(|site| self.sites_off.contains(&site))
    }
}

impl ContentRules for Rules {
    fn blocks(&self, request: &ResourceRequest<'_>) -> bool {
        if self.off_here(request.context, request.top) { return false; }
        let kind = match request.kind {
            ResourceKind::Subdocument => RequestType::Subdocument,
            ResourceKind::Script => RequestType::Script,
            ResourceKind::Image => RequestType::Image,
            ResourceKind::Stylesheet => RequestType::Stylesheet,
            ResourceKind::Font => RequestType::Font,
            ResourceKind::Object => RequestType::Object,
            ResourceKind::Media => RequestType::Media,
            ResourceKind::Xhr => RequestType::Xhr,
            ResourceKind::Ping => RequestType::Ping,
            ResourceKind::Other => RequestType::Other,
        };
        matcher::Request::new(request.url, kind, request.context).is_some_and(|request| matcher::blocks(&self.lists, &request))
    }

    fn hiding(&self, document: &str, top: &str) -> Option<HidingSheet> {
        if self.off_here(document, top) { return None; }
        let sheet = self.hiding.sheet(document)?;
        Some(HidingSheet { specific: sheet.specific, generic: sheet.generic, generation: sheet.generation })
    }
}

/// Where a profile's rules go: the engine, or a test's record of them.
pub trait Install {
    fn install(&mut self, profile: &str, rules: Option<Arc<dyn ContentRules>>);
}

pub struct Engine;

impl Install for Engine {
    fn install(&mut self, profile: &str, rules: Option<Arc<dyn ContentRules>>) {
        crossbow_engine::set_content_rules(profile, rules);
    }
}

/// Asks which file to add. The production one is the system open panel.
pub trait FilePanel {
    /// The receiver gets the chosen path, or None on Cancel.
    fn choose(&mut self) -> Receiver<Result<Option<PathBuf>, String>>;
}

/// How one list is doing, for Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListState {
    /// Its switch is off.
    Off,
    Loading,
    Ready,
    Failed(String),
}

type Opened = Vec<(String, Result<Arc<CompiledList>, String>)>;

/// Element hiding by the ids of its lists, while a profile uses it. Shared
/// with the threads that build it.
type Hidings = Arc<Mutex<HashMap<Vec<String>, Weak<Hiding>>>>;

/// What a profile's background work hands the UI: the lists it opened, and
/// the element hiding for every list then ready, by their ids.
struct Work {
    opened: Opened,
    hiding: Option<(Vec<String>, Arc<Hiding>)>,
}

/// A live copy of the element hiding for `ids`, when a profile uses one.
fn cached_hiding(hidings: &Hidings, ids: &[String]) -> Option<Arc<Hiding>> {
    hidings.lock().unwrap_or_else(PoisonError::into_inner).get(ids).and_then(Weak::upgrade)
}

/// The element hiding for `lists`, named by `ids`: the copy a profile uses,
/// or a new one. Takes milliseconds per list, so never on the UI thread.
fn shared_hiding(hidings: &Hidings, ids: &[String], lists: &[Arc<CompiledList>]) -> Arc<Hiding> {
    if let Some(hiding) = cached_hiding(hidings, ids) { return hiding; }
    let built = Arc::new(Hiding::new(lists));
    #[cfg(test)]
    tests::HIDINGS_BUILT_HERE.with(|built| built.set(built.get() + 1));
    let mut hidings = hidings.lock().unwrap_or_else(PoisonError::into_inner);
    hidings.retain(|_, hiding| hiding.strong_count() > 0);
    // Another profile's work may have built the same one meanwhile.
    if let Some(theirs) = hidings.get(ids).and_then(Weak::upgrade) { return theirs; }
    hidings.insert(ids.to_vec(), Arc::downgrade(&built));
    built
}

/// A list a switch that is on uses.
#[derive(Clone)]
enum Wanted {
    Bundled(&'static Bundled),
    File(ListEntry),
}

impl Wanted {
    fn id(&self) -> &str {
        match self {
            Wanted::Bundled(list) => list.id,
            Wanted::File(entry) => &entry.id,
        }
    }
}

/// A bundled list whose opening stopped short.
const BUNDLED_FAILED: &str = "Couldn't load this list. Quit and reopen Crossbow to try again.";

/// One profile's switches and lists.
pub struct ProfileBlocking {
    pub ads: DeviceSwitch<BlockAds>,
    pub cookies: DeviceSwitch<BlockCookieBanners>,
    saved: Saved,
    /// The lists that come with the app.
    bundled: &'static [&'static Bundled],
    /// Each open list by id: compiled, or why it couldn't be read.
    opened: HashMap<String, Result<Arc<CompiledList>, String>>,
    /// Lists being opened, or element hiding being built, off the UI thread.
    opening: Option<Receiver<Work>>,
    /// The list being added: its kind, file name and result.
    adding: Option<(ListKind, String, Receiver<Result<(ListEntry, Arc<CompiledList>), AddError>>)>,
    /// The last result, and whether it is an error.
    pub message: Option<(String, bool)>,
    /// Whose lists the message is about, so it shows beside them.
    message_kind: Option<ListKind>,
    /// Sites with blocking off, and why none could be read.
    exceptions: Exceptions,
    /// Why the last change to them from Settings failed.
    pub sites_error: Option<String>,
    /// Element hiding for the ready lists, by their ids, kept while they
    /// stay the same so pages keep their copy of the shared part.
    hiding: Option<(Vec<String>, Arc<Hiding>)>,
}

impl ProfileBlocking {
    fn load(store: &Store, data_dir: &Path, profile: &str, bundled: &'static [&'static Bundled]) -> Self {
        ProfileBlocking {
            ads: DeviceSwitch::load_for(store, profile),
            cookies: DeviceSwitch::load_for(store, profile),
            saved: lists::load(data_dir, profile),
            bundled,
            opened: HashMap::new(),
            opening: None,
            adding: None,
            message: None,
            message_kind: None,
            exceptions: exceptions::load(store, profile),
            sites_error: None,
            hiding: None,
        }
    }

    pub fn enabled(&self, kind: ListKind) -> bool {
        match kind {
            ListKind::Ads => self.ads.enabled(),
            ListKind::CookieBanners => self.cookies.enabled(),
        }
    }

    /// `kind`'s lists, in the order they were added.
    pub fn lists(&self, kind: ListKind) -> Vec<(ListEntry, ListState)> {
        self.saved.lists.iter().filter(|entry| entry.kind == kind).map(|entry| {
            let state = match self.opened.get(&entry.id) {
                _ if !self.enabled(kind) => ListState::Off,
                Some(Ok(_)) => ListState::Ready,
                Some(Err(error)) => ListState::Failed(error.clone()),
                None => ListState::Loading,
            };
            (entry.clone(), state)
        }).collect()
    }

    /// Why saved lists aren't in use.
    pub fn notice(&self) -> Option<&str> {
        self.saved.notice.as_deref()
    }

    /// The last result about `kind`'s lists, and whether it is an error.
    pub fn message_for(&self, kind: ListKind) -> Option<(&str, bool)> {
        self.message.as_ref().filter(|_| self.message_kind == Some(kind)).map(|(message, error)| (message.as_str(), *error))
    }

    /// The kind and file name of the list being added.
    pub fn adding(&self) -> Option<(ListKind, &str)> {
        self.adding.as_ref().map(|(kind, name, _)| (*kind, name.as_str()))
    }

    /// Sites with blocking off, in order.
    pub fn sites_off(&self) -> Vec<String> {
        self.exceptions.sites.iter().cloned().collect()
    }

    /// Why the sites with blocking off couldn't be read.
    pub fn sites_notice(&self) -> Option<&str> {
        self.exceptions.notice.as_deref()
    }

    /// Whether either switch is on.
    pub fn any_enabled(&self) -> bool {
        self.ads.enabled() || self.cookies.enabled()
    }

    /// Cosmetic rules for every page left out of each sheet: too many.
    pub fn hiding_skipped(&self) -> usize {
        self.hiding.as_ref().map_or(0, |(_, hiding)| hiding.skipped)
    }

    /// `kind`'s lists that come with the app, in the app's order.
    pub fn bundled(&self, kind: ListKind) -> Vec<BundledRow> {
        self.bundled.iter().filter(|list| list.kind == kind).map(|list| {
            let on = !self.saved.bundled_off.contains(list.id);
            let used = on && self.enabled(kind);
            let opened = self.opened.get(list.id).filter(|_| used);
            let state = match opened {
                _ if !used => ListState::Off,
                Some(Ok(_)) => ListState::Ready,
                Some(Err(error)) => ListState::Failed(error.clone()),
                None => ListState::Loading,
            };
            let counts = opened.and_then(|opened| opened.as_ref().ok()).map(|compiled| (compiled.rules, compiled.unsupported));
            BundledRow { id: list.id, name: list.name, credit: list.credit, on, state, counts }
        }).collect()
    }

    /// Whether a list a switch uses is still opening.
    pub fn loading(&self) -> bool {
        self.wanted().any(|list| !self.opened.contains_key(list.id()))
    }

    /// Whether a list a switch uses is ready, so requests are checked.
    pub fn ready(&self) -> bool {
        self.wanted().any(|list| matches!(self.opened.get(list.id()), Some(Ok(_))))
    }

    /// Whether a switch that is on has a list.
    pub fn has_lists(&self) -> bool {
        self.wanted().next().is_some()
    }

    /// The lists its switches use: the bundled ones it didn't turn off,
    /// then those added from files.
    fn wanted(&self) -> impl Iterator<Item = Wanted> {
        let bundled = self.bundled.iter().filter(|list| self.enabled(list.kind) && !self.saved.bundled_off.contains(list.id)).map(|list| Wanted::Bundled(list));
        bundled.chain(self.saved.lists.iter().filter(|entry| self.enabled(entry.kind)).cloned().map(Wanted::File))
    }
}

pub struct ContentBlocking {
    data_dir: PathBuf,
    /// The lists that come with the app.
    bundled: &'static [&'static Bundled],
    profiles: HashMap<String, ProfileBlocking>,
    /// Element hiding in use, by the ids of its lists.
    hidings: Hidings,
    /// What pages get before a profile's first element hiding is built.
    no_hiding: Arc<Hiding>,
    install: Box<dyn Install>,
    panel: Box<dyn FilePanel>,
    /// The open file panel: for which profile and kind.
    choosing: Option<(String, ListKind, Receiver<Result<Option<PathBuf>, String>>)>,
    /// Settings → Privacy shows Content Blocking from its top the next time
    /// it is drawn, rather than where the page was left.
    pub reveal: bool,
}

impl ContentBlocking {
    /// No lists come with it until [`Self::bundled`].
    pub fn new(data_dir: PathBuf, install: Box<dyn Install>, panel: Box<dyn FilePanel>) -> Self {
        ContentBlocking { data_dir, bundled: &[], profiles: HashMap::new(), hidings: Hidings::default(), no_hiding: Arc::new(Hiding::new(&[])), install, panel, choosing: None, reveal: false }
    }

    /// With `lists` coming with the app: each is on for every profile until
    /// the profile turns it off. Before any profile is read.
    pub fn bundled(mut self, lists: &'static [&'static Bundled]) -> Self {
        self.bundled = lists;
        self
    }

    /// The engine, the system open panel, and EasyList and EasyPrivacy.
    pub fn system(data_dir: PathBuf) -> Self {
        ContentBlocking::new(data_dir, Box::new(Engine), system_panel()).bundled(&bundled::LISTS)
    }

    /// Turns a bundled list on or off for `profile`. It is saved first: when
    /// that fails, nothing changes and the message says why. Not while a
    /// list is being added, which rewrites the same file.
    pub fn set_bundled(&mut self, profile: &str, id: &str, on: bool, ctx: &egui::Context) {
        let Some(blocking) = self.profiles.get_mut(profile) else { return };
        let Some(list) = blocking.bundled.iter().find(|list| list.id == id).copied() else { return };
        if blocking.adding.is_some() { return; }
        let mut off = blocking.saved.bundled_off.clone();
        let changed = if on { off.remove(id) } else { off.insert(id.to_owned()) };
        if !changed { return; }
        blocking.message_kind = Some(list.kind);
        match lists::set_bundled_off(&self.data_dir, profile, &off) {
            Ok(()) => {
                // The new manifest replaced any that couldn't be read.
                blocking.saved.notice = None;
                blocking.saved.bundled_off = off;
                blocking.message = Some((format!("Turned {} “{}”.", if on { "on" } else { "off" }, list.name), false));
                self.apply(profile, ctx);
            }
            Err(error) => blocking.message = Some((error, true)),
        }
    }

    /// Reads every profile's switches and starts opening the lists they use.
    pub fn launch(&mut self, lib: &Library, ctx: &egui::Context) {
        for profile in lib.profiles.keys() {
            self.profile(&lib.store, profile);
            self.apply(profile, ctx);
        }
    }

    /// `profile`'s switches and lists, read when first needed.
    pub fn profile(&mut self, store: &Store, profile: &str) -> &mut ProfileBlocking {
        let (data_dir, bundled) = (&self.data_dir, self.bundled);
        self.profiles.entry(profile.to_owned()).or_insert_with(|| ProfileBlocking::load(store, data_dir, profile, bundled))
    }

    /// Gives the engine `profile`'s rules for the switches that are on, and
    /// starts opening any list they need. Until a list is ready, the rules
    /// leave it out. Element hiding for the ready lists is built on the same
    /// thread; until it arrives, pages keep the last one.
    pub fn apply(&mut self, profile: &str, ctx: &egui::Context) {
        let Some(blocking) = self.profiles.get_mut(profile) else { return };
        let wanted: Vec<Wanted> = blocking.wanted().collect();
        // A list whose switch is off is closed, so it holds no memory.
        blocking.opened.retain(|id, _| wanted.iter().any(|list| list.id() == id));
        let missing: Vec<Wanted> = wanted.iter().filter(|list| !blocking.opened.contains_key(list.id())).cloned().collect();
        let ready: Vec<(String, Arc<CompiledList>)> = wanted.iter().filter_map(|list| Some((list.id().to_owned(), blocking.opened.get(list.id())?.as_ref().ok()?.clone()))).collect();
        let ids: Vec<String> = ready.iter().map(|(id, _)| id.clone()).collect();
        if ready.is_empty() {
            blocking.hiding = None;
        } else if blocking.hiding.as_ref().is_none_or(|(built, _)| *built != ids) {
            // Ids name the lists' text, so profiles with the same ids share one.
            if let Some(hiding) = cached_hiding(&self.hidings, &ids) { blocking.hiding = Some((ids.clone(), hiding)); }
        }
        let unbuilt = !ready.is_empty() && blocking.hiding.as_ref().is_none_or(|(built, _)| *built != ids);
        if blocking.opening.is_none() && (!missing.is_empty() || unbuilt) {
            let (send, receive) = mpsc::channel();
            let (data_dir, profile, wake, hidings) = (self.data_dir.clone(), profile.to_owned(), ctx.clone(), self.hidings.clone());
            let (order, ready) = (wanted.iter().map(|list| list.id().to_owned()).collect::<Vec<_>>(), ready.clone());
            std::thread::spawn(move || {
                let opened: Opened = missing.iter().map(|list| match list {
                    // When another profile is compiling it, this waits for that copy.
                    Wanted::Bundled(list) => (list.id.to_owned(), Ok(list.compiled())),
                    Wanted::File(entry) => (entry.id.clone(), lists::open(&data_dir, &profile, entry)),
                }).collect();
                // Every list ready now, in the order `apply` reads them.
                let (ids, lists): (Vec<String>, Vec<Arc<CompiledList>>) = order.into_iter().filter_map(|id| {
                    let list = ready.iter().find(|(ready, _)| *ready == id).map(|(_, list)| list.clone());
                    let list = list.or_else(|| opened.iter().find(|(opened, _)| *opened == id)?.1.as_ref().ok().cloned())?;
                    Some((id, list))
                }).unzip();
                let hiding = (!lists.is_empty()).then(|| { let hiding = shared_hiding(&hidings, &ids, &lists); (ids, hiding) });
                let _ = send.send(Work { opened, hiding });
                wake.request_repaint();
            });
            blocking.opening = Some(receive);
        }
        let lists: Vec<Arc<CompiledList>> = ready.into_iter().map(|(_, list)| list).collect();
        let rules = (!lists.is_empty()).then(|| {
            let hiding = blocking.hiding.as_ref().map_or_else(|| self.no_hiding.clone(), |(_, hiding)| hiding.clone());
            Arc::new(Rules::new(lists, blocking.exceptions.sites.clone(), hiding)) as Arc<dyn ContentRules>
        });
        self.install.install(profile, rules);
    }

    /// Opens the file panel to add a list of `kind` to `profile`, once
    /// [`Self::profile`] has read it. One panel and one added list at a time.
    pub fn add_from_file(&mut self, profile: &str, kind: ListKind) {
        if self.choosing.is_some() || self.profiles.get(profile).is_none_or(|blocking| blocking.adding.is_some()) { return; }
        self.choosing = Some((profile.to_owned(), kind, self.panel.choose()));
    }

    /// Whether the file panel is open.
    pub fn choosing(&self) -> bool {
        self.choosing.is_some()
    }

    /// Removes a list. Not while one is being added, which rewrites the
    /// same file.
    pub fn remove(&mut self, profile: &str, id: &str, ctx: &egui::Context) {
        let Some(blocking) = self.profiles.get_mut(profile) else { return };
        if blocking.adding.is_some() { return; }
        let Some((name, kind)) = blocking.saved.lists.iter().find(|entry| entry.id == id).map(|entry| (entry.name.clone(), entry.kind)) else { return };
        blocking.message_kind = Some(kind);
        match lists::remove(&self.data_dir, profile, id) {
            Ok(()) => {
                blocking.saved.lists.retain(|entry| entry.id != id);
                blocking.opened.remove(id);
                blocking.message = Some((format!("Removed “{name}”."), false));
                self.apply(profile, ctx);
            }
            Err(error) => blocking.message = Some((error, true)),
        }
    }

    /// The site of `page` and whether `profile` blocks on it; None for a page
    /// that isn't on the web.
    pub fn site_blocking(&mut self, store: &Store, profile: &str, page: &str) -> Option<(String, bool)> {
        let site = exceptions::site_of(page)?;
        let on = !self.profile(store, profile).exceptions.sites.contains(&site);
        Some((site, on))
    }

    /// Turns blocking on or off on `site` for `profile`. It is saved first:
    /// when that fails, nothing changes and the error says so. Pages ask the
    /// new rules from their next request.
    pub fn set_site_blocking(&mut self, store: &Store, profile: &str, site: &str, on: bool, ctx: &egui::Context) -> Result<(), String> {
        let blocking = self.profile(store, profile);
        let mut sites = blocking.exceptions.sites.clone();
        let changed = if on { sites.remove(site) } else { sites.insert(site.to_owned()) };
        // An unreadable record is saved over even when nothing changed.
        if !changed && blocking.exceptions.notice.is_none() { return Ok(()); }
        exceptions::save(store, profile, &sites)?;
        blocking.exceptions = Exceptions { sites, notice: None };
        self.apply(profile, ctx);
        Ok(())
    }

    /// Drops a deleted profile's lists and rules.
    pub fn forget(&mut self, profile: &str) {
        self.profiles.remove(profile);
        self.choosing.take_if(|(chosen, _, _)| chosen == profile);
        self.install.install(profile, None);
    }

    /// Takes finished work: a chosen file, an added list, opened lists.
    /// True while anything is still waiting.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        self.poll_panel(ctx);
        let mut changed = Vec::new();
        for (profile, blocking) in &mut self.profiles {
            if let Some(receive) = &blocking.opening {
                let work: Option<(Work, bool)> = match receive.try_recv() {
                    Err(TryRecvError::Empty) => None,
                    // The work stopped short: what it didn't open isn't tried again until relaunch.
                    Err(TryRecvError::Disconnected) => Some((Work { opened: blocking.wanted().filter(|list| !blocking.opened.contains_key(list.id())).map(|list| {
                        let error = if matches!(list, Wanted::Bundled(_)) { BUNDLED_FAILED } else { lists::CHANGED };
                        (list.id().to_owned(), Err(error.to_owned()))
                    }).collect(), hiding: None }, true)),
                    Ok(work) => Some((work, false)),
                };
                if let Some((work, stopped)) = work {
                    blocking.opening = None;
                    // A list removed or turned off meanwhile stays closed.
                    for (id, opened) in work.opened {
                        if blocking.wanted().any(|list| list.id() == id) { blocking.opened.insert(id, opened); }
                    }
                    if let Some(hiding) = work.hiding {
                        blocking.hiding = Some(hiding);
                    } else if stopped {
                        // Nor was the hiding built: pages keep theirs until the lists change.
                        let ids = blocking.wanted().filter(|list| matches!(blocking.opened.get(list.id()), Some(Ok(_)))).map(|list| list.id().to_owned()).collect();
                        let hiding = blocking.hiding.take().map_or_else(|| self.no_hiding.clone(), |(_, hiding)| hiding);
                        blocking.hiding = Some((ids, hiding));
                    }
                    changed.push(profile.clone());
                }
            }
            if let Some((kind, name, receive)) = &blocking.adding {
                let added = match receive.try_recv() {
                    Err(TryRecvError::Empty) => continue,
                    Err(TryRecvError::Disconnected) => Err(AddError::Unreadable),
                    Ok(added) => added,
                };
                let (kind, name) = (*kind, name.clone());
                blocking.adding = None;
                blocking.message_kind = Some(kind);
                match added {
                    Ok((entry, compiled)) => {
                        // The new manifest replaced any that couldn't be read.
                        blocking.saved.notice = None;
                        if blocking.enabled(kind) { blocking.opened.insert(entry.id.clone(), Ok(compiled)); }
                        blocking.saved.lists.push(entry);
                        blocking.message = Some((format!("Added “{name}”."), false));
                        changed.push(profile.clone());
                    }
                    Err(error) => blocking.message = Some((error.to_string(), true)),
                }
            }
        }
        for profile in changed { self.apply(&profile, ctx); }
        self.choosing.is_some() || self.profiles.values().any(|blocking| blocking.opening.is_some() || blocking.adding.is_some())
    }

    /// A chosen file is read, checked, saved and compiled off the UI thread.
    fn poll_panel(&mut self, ctx: &egui::Context) {
        let Some((_, _, receive)) = &self.choosing else { return };
        let chosen = match receive.try_recv() {
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("The file panel closed unexpectedly. Try Add List from File… again.".to_owned()),
            Ok(chosen) => chosen,
        };
        let (profile, kind, _) = self.choosing.take().expect("checked above");
        let Some(blocking) = self.profiles.get_mut(&profile) else { return };
        let path = match chosen {
            Ok(Some(path)) => path,
            // Cancel changes nothing and says nothing.
            Ok(None) => return,
            Err(error) => {
                blocking.message = Some((error, true));
                blocking.message_kind = Some(kind);
                return;
            }
        };
        let name = path.file_name().map_or_else(|| "list".to_owned(), |name| name.to_string_lossy().into_owned());
        let (send, receive) = mpsc::channel();
        let (data_dir, wake, file) = (self.data_dir.clone(), ctx.clone(), name.clone());
        std::thread::spawn(move || {
            let added = lists::read_file(&path).and_then(|bytes| lists::add(&data_dir, &profile, kind, &file, &bytes, crossbow_core::now_ms()));
            let _ = send.send(added);
            wake.request_repaint();
        });
        blocking.message = None;
        blocking.adding = Some((kind, name, receive));
    }
}

pub fn system_panel() -> Box<dyn FilePanel> {
    #[cfg(target_os = "macos")]
    { Box::new(mac::Panel) }
    #[cfg(not(target_os = "macos"))]
    { Box::new(NoPanel) }
}

#[cfg(not(target_os = "macos"))]
struct NoPanel;

#[cfg(not(target_os = "macos"))]
impl FilePanel for NoPanel {
    fn choose(&mut self) -> Receiver<Result<Option<PathBuf>, String>> {
        let (send, receive) = std::sync::mpsc::channel();
        let _ = send.send(Err("Adding a list is available on macOS only in this preview.".into()));
        receive
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::FilePanel;
    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::NSString;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver};

    pub struct Panel;

    impl FilePanel for Panel {
        fn choose(&mut self) -> Receiver<Result<Option<PathBuf>, String>> {
            let (send, receive) = mpsc::channel();
            let Some(main) = MainThreadMarker::new() else {
                let _ = send.send(Err("Add List from File… opens from the main window. Try again there.".into()));
                return receive;
            };
            let panel = NSOpenPanel::openPanel(main);
            panel.setTitle(Some(&NSString::from_str("Add a filter list")));
            panel.setPrompt(Some(&NSString::from_str("Add")));
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            panel.setResolvesAliases(true);
            let chosen = panel.clone();
            let completion = RcBlock::new(move |response| {
                let _ = send.send(if response == NSModalResponseOK {
                    chosen.URL().and_then(|url| url.to_file_path()).map(Some).ok_or_else(|| "Choose a list file on this Mac and try again.".into())
                } else if response == NSModalResponseCancel {
                    Ok(None)
                } else {
                    Err("The file panel couldn't open. Try Add List from File… again.".into())
                });
            });
            if let Some(window) = NSApplication::sharedApplication(main).keyWindow() {
                panel.beginSheetModalForWindow_completionHandler(&window, &completion);
            } else {
                panel.beginWithCompletionHandler(&completion);
            }
            receive
        }
    }
}

#[cfg(test)]
#[path = "content_blocking_tests.rs"]
pub(crate) mod tests;
