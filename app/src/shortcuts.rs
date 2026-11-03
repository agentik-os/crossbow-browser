//! Keyboard shortcuts: the registry of browser actions Crossbow dispatches,
//! their default chords, this device's overrides and chord dispatch.
//! Overrides are a device-local preference in the store's key/value table,
//! so they never enter account sync.

use crossbow_core::store::Store;
use egui::{Key, Modifiers};
use std::collections::BTreeMap;

const KEY: &str = "keyboard_shortcuts";
const VERSION: u64 = 1;
const PRIORITY_KEY: &str = "shortcut_priority";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    NewTab,
    EditUrl,
    CloseTab,
    ReopenClosed,
    /// A Little Window: one page in a small window of its own.
    NewLittleWindow,
    ShowLittleWindows,
    CloseLittleWindows,
    /// A new tab at the profile's document app.
    NewDocument,
    PinTab,
    Favorite,
    CopyUrl,
    NextTab,
    PreviousTab,
    /// Tabs 1–8 in sidebar order.
    Tab(u8),
    LastTab,
    ClearToday,
    SplitView,
    Back,
    Forward,
    Reload,
    HardReload,
    Find,
    ToggleSidebar,
    NewFolder,
    PreviousSpace,
    NextSpace,
    /// Spaces 1–9 in sidebar order.
    Space(u8),
    ZoomIn,
    ZoomOut,
    ActualSize,
    DevTools,
    ViewSource,
    Print,
    /// Select part of the page and copy it as an image.
    Capture,
    Settings,
    CloseOverlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Tabs,
    Navigation,
    Spaces,
    View,
    Page,
    App,
}

impl Category {
    pub const ALL: [Category; 6] = [Category::Tabs, Category::Navigation, Category::Spaces, Category::View, Category::Page, Category::App];

    pub fn title(self) -> &'static str {
        match self {
            Category::Tabs => "Tabs",
            Category::Navigation => "Navigation",
            Category::Spaces => "Sidebar & Spaces",
            Category::View => "Zoom",
            Category::Page => "Page",
            Category::App => "App",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    Primary,
    Alternate,
}

impl Slot {
    pub const BOTH: [Slot; 2] = [Slot::Primary, Slot::Alternate];

    pub fn name(self) -> &'static str {
        match self {
            Slot::Primary => "primary",
            Slot::Alternate => "alternate",
        }
    }
}

/// One key with modifiers, compared as dispatch compares them: `command` is
/// ⌘ on macOS and Ctrl elsewhere; `ctrl` is only the macOS Control key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub mods: Modifiers,
}

const fn mods(command: bool, shift: bool, alt: bool, ctrl: bool) -> Modifiers {
    Modifiers { alt, ctrl, shift, mac_cmd: false, command }
}

const NONE: Modifiers = mods(false, false, false, false);
const CMD: Modifiers = mods(true, false, false, false);
const CMD_SHIFT: Modifiers = mods(true, true, false, false);
const CMD_ALT: Modifiers = mods(true, false, true, false);
const CTRL: Modifiers = mods(false, false, false, true);
const CTRL_SHIFT: Modifiers = mods(false, true, false, true);
const CTRL_CMD: Modifiers = mods(true, false, false, true);
const MAC: bool = cfg!(target_os = "macos");

const fn chord(key: Key, mods: Modifiers) -> Option<Chord> {
    Some(Chord { key, mods })
}

impl Chord {
    /// The chord a key event spells.
    pub fn from_event(key: Key, m: Modifiers) -> Chord {
        Chord { key, mods: mods(m.command, m.shift, m.alt, MAC && m.ctrl) }
    }

    /// Event forms this chord takes. "+" is Shift+"=" on many layouts, so a
    /// ⌘+ binding also takes ⌘⇧=, as Zoom In always has.
    fn forms(self) -> impl Iterator<Item = Chord> {
        let shifted = (self.key == Key::Plus && !self.mods.shift).then_some(Chord { key: Key::Equals, mods: Modifiers { shift: true, ..self.mods } });
        std::iter::once(self).chain(shifted)
    }

    pub fn overlaps(self, other: Chord) -> bool {
        self.forms().any(|a| other.forms().any(|b| a == b))
    }

    /// Caret and deletion chords stay with a focused text field or editable page.
    fn yields_to_text(self) -> bool {
        self.mods.command && !self.mods.alt && !self.mods.ctrl
            && matches!(self.key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End | Key::Backspace | Key::Delete)
    }

    fn function_key(self) -> bool {
        self.key.name().strip_prefix('F').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }

    pub fn label(self) -> String {
        let key = match self.key {
            Key::ArrowLeft => "←",
            Key::ArrowRight => "→",
            Key::ArrowUp => "↑",
            Key::ArrowDown => "↓",
            Key::Escape => "Esc",
            Key::Enter if MAC => "Return",
            Key::PageUp => "Page Up",
            Key::PageDown => "Page Down",
            key => key.symbol_or_name(),
        };
        crate::input::shortcut_label(self.mods.command, self.mods.shift, self.mods.alt, self.mods.ctrl, key)
    }

    pub(crate) fn encode(self) -> String {
        let mut text = String::new();
        for (on, name) in [(self.mods.command, "cmd"), (self.mods.ctrl, "ctrl"), (self.mods.alt, "alt"), (self.mods.shift, "shift")] {
            if on {
                text.push_str(name);
                text.push('+');
            }
        }
        text + self.key.name()
    }

    pub(crate) fn decode(text: &str) -> Option<Chord> {
        let (prefix, name) = text.rsplit_once('+').unwrap_or(("", text));
        let mut m = NONE;
        for part in prefix.split('+').filter(|part| !part.is_empty()) {
            let flag = match part {
                "cmd" => &mut m.command,
                "ctrl" => &mut m.ctrl,
                "alt" => &mut m.alt,
                "shift" => &mut m.shift,
                _ => return None,
            };
            if std::mem::replace(flag, true) { return None; }
        }
        let key = Key::from_name(name).filter(|key| key.name() == name)?;
        Some(Chord { key, mods: m })
    }
}

/// Chords the system or Crossbow's shell owns: (chord, who uses it).
fn reserved() -> Vec<(Chord, &'static str)> {
    let mut list = vec![
        (Chord { key: Key::Comma, mods: CMD }, "Crossbow opens Settings with it"),
        (Chord { key: Key::A, mods: CMD }, "text fields use it to select all"),
        (Chord { key: Key::C, mods: CMD }, "text fields use it to copy"),
        (Chord { key: Key::V, mods: CMD }, "text fields use it to paste"),
        (Chord { key: Key::X, mods: CMD }, "text fields use it to cut"),
        (Chord { key: Key::Z, mods: CMD }, "text fields use it to undo"),
        (Chord { key: Key::Z, mods: CMD_SHIFT }, "text fields use it to redo"),
    ];
    if MAC {
        list.extend([
            (Chord { key: Key::Q, mods: CMD }, "Crossbow quits with it"),
            (Chord { key: Key::H, mods: CMD }, "macOS hides Crossbow with it"),
            (Chord { key: Key::H, mods: CMD_ALT }, "macOS hides other apps with it"),
            (Chord { key: Key::M, mods: CMD }, "macOS minimizes the window with it"),
            (Chord { key: Key::Tab, mods: CMD }, "macOS switches apps with it"),
            (Chord { key: Key::Tab, mods: CMD_SHIFT }, "macOS switches apps with it"),
            (Chord { key: Key::Backtick, mods: CMD }, "macOS switches windows with it"),
            (Chord { key: Key::Backtick, mods: CMD_SHIFT }, "macOS switches windows with it"),
            (Chord { key: Key::Space, mods: CMD }, "macOS opens Spotlight with it"),
            (Chord { key: Key::Space, mods: CTRL_CMD }, "macOS opens the Character Viewer with it"),
            (Chord { key: Key::Q, mods: CTRL_CMD }, "macOS locks the screen with it"),
            (Chord { key: Key::F, mods: CTRL_CMD }, "macOS enters full screen with it"),
            (Chord { key: Key::Num3, mods: CMD_SHIFT }, "macOS takes screenshots with it"),
            (Chord { key: Key::Num4, mods: CMD_SHIFT }, "macOS takes screenshots with it"),
            (Chord { key: Key::Num5, mods: CMD_SHIFT }, "macOS takes screenshots with it"),
        ]);
    } else {
        list.push((Chord { key: Key::F4, mods: mods(false, false, true, false) }, "the system closes the window with it"));
    }
    list
}

/// Why `chord` can't be bound to any action, if it can't.
pub fn refusal(chord: Chord) -> Option<String> {
    if chord.key == Key::Escape {
        return Some("Esc always closes overlays and cancels, so it can't be assigned.".into());
    }
    if !chord.mods.command && !chord.mods.ctrl && !chord.function_key() {
        let needs = if MAC { "⌘ or ⌃" } else { "Ctrl" };
        return Some(format!("{} needs {needs}; only function keys work alone.", chord.label()));
    }
    reserved().into_iter().find(|(owned, _)| owned.overlaps(chord)).map(|(_, owner)| format!("{} is reserved: {owner}.", chord.label()))
}

pub struct Spec {
    pub command: Command,
    /// Stable id in saved overrides.
    pub id: &'static str,
    pub label: &'static str,
    pub category: Category,
    defaults: [Option<Chord>; 2],
    /// Listed but not rebindable.
    pub fixed: bool,
}

impl Spec {
    pub fn default_binding(&self, slot: Slot) -> Option<Chord> {
        self.defaults[slot as usize]
    }
}

const fn action(command: Command, id: &'static str, label: &'static str, category: Category, primary: Option<Chord>, alternate: Option<Chord>) -> Spec {
    Spec { command, id, label, category, defaults: [primary, alternate], fixed: false }
}

const fn fixed(command: Command, id: &'static str, label: &'static str, primary: Option<Chord>) -> Spec {
    Spec { command, id, label, category: Category::App, defaults: [primary, None], fixed: true }
}

const fn tab(n: u8, key: Key, id: &'static str, label: &'static str) -> Spec {
    action(Command::Tab(n), id, label, Category::Tabs, chord(key, CMD), None)
}

/// Control and a digit selects a Space on macOS; elsewhere Ctrl is ⌘ and selects tabs.
const fn space(n: u8, key: Key, id: &'static str, label: &'static str) -> Spec {
    action(Command::Space(n), id, label, Category::Spaces, if MAC { chord(key, CTRL) } else { None }, None)
}

/// Every action keyboard dispatch can run, in Settings order.
pub static REGISTRY: &[Spec] = &[
    action(Command::NewTab, "new-tab", "New Tab", Category::Tabs, chord(Key::T, CMD), None),
    action(Command::EditUrl, "edit-url", "Edit Current URL", Category::Tabs, chord(Key::L, CMD), None),
    action(Command::CloseTab, "close-tab", "Close Tab or Peek", Category::Tabs, chord(Key::W, CMD), None),
    action(Command::ReopenClosed, "reopen-tab", "Reopen Closed Tab", Category::Tabs, chord(Key::T, CMD_SHIFT), None),
    // Arc uses ⌥⌘N, which is New Folder here.
    action(Command::NewLittleWindow, "new-little-window", "New Little Window", Category::Tabs, if MAC { chord(Key::N, CTRL_CMD) } else { None }, None),
    // No default chord; people can set one in Settings.
    action(Command::ShowLittleWindows, "show-little-windows", "Show All Little Windows", Category::Tabs, None, None),
    action(Command::CloseLittleWindows, "close-little-windows", "Close All Little Windows", Category::Tabs, None, None),
    action(Command::NewDocument, "new-document", "New Document", Category::Tabs, None, None),
    action(Command::PinTab, "pin-tab", "Pin or Unpin Tab", Category::Tabs, chord(Key::D, CMD), None),
    action(Command::Favorite, "add-favorite", "Add to Favorites", Category::Tabs, chord(Key::D, CMD_SHIFT), None),
    action(Command::CopyUrl, "copy-url", "Copy URL", Category::Tabs, chord(Key::C, CMD_SHIFT), None),
    action(Command::SplitView, "split-view", "Add Split View", Category::Tabs, chord(Key::Equals, CTRL_SHIFT), None),
    action(Command::NextTab, "next-tab", "Next Tab", Category::Tabs, chord(Key::Tab, CTRL), None),
    action(Command::PreviousTab, "previous-tab", "Previous Tab", Category::Tabs, chord(Key::Tab, CTRL_SHIFT), None),
    tab(1, Key::Num1, "tab-1", "Go to Tab 1"),
    tab(2, Key::Num2, "tab-2", "Go to Tab 2"),
    tab(3, Key::Num3, "tab-3", "Go to Tab 3"),
    tab(4, Key::Num4, "tab-4", "Go to Tab 4"),
    tab(5, Key::Num5, "tab-5", "Go to Tab 5"),
    tab(6, Key::Num6, "tab-6", "Go to Tab 6"),
    tab(7, Key::Num7, "tab-7", "Go to Tab 7"),
    tab(8, Key::Num8, "tab-8", "Go to Tab 8"),
    action(Command::LastTab, "last-tab", "Go to Last Tab", Category::Tabs, chord(Key::Num9, CMD), None),
    action(Command::ClearToday, "clear-unpinned", "Clear Unpinned Tabs", Category::Tabs, chord(Key::K, CMD_SHIFT), None),
    action(Command::Back, "back", "Back", Category::Navigation, chord(Key::OpenBracket, CMD), chord(Key::ArrowLeft, CMD)),
    action(Command::Forward, "forward", "Forward", Category::Navigation, chord(Key::CloseBracket, CMD), chord(Key::ArrowRight, CMD)),
    action(Command::Reload, "reload", "Reload", Category::Navigation, chord(Key::R, CMD), None),
    action(Command::HardReload, "hard-reload", "Hard Reload", Category::Navigation, chord(Key::R, CMD_SHIFT), None),
    action(Command::Find, "find", "Find on Page", Category::Navigation, chord(Key::F, CMD), None),
    action(Command::ToggleSidebar, "toggle-sidebar", "Toggle Sidebar", Category::Spaces, chord(Key::S, CMD), None),
    action(Command::NewFolder, "new-folder", "New Folder", Category::Spaces, chord(Key::N, CMD_ALT), None),
    action(Command::PreviousSpace, "previous-space", "Previous Space", Category::Spaces, chord(Key::ArrowLeft, CMD_ALT), None),
    action(Command::NextSpace, "next-space", "Next Space", Category::Spaces, chord(Key::ArrowRight, CMD_ALT), None),
    space(1, Key::Num1, "space-1", "Go to Space 1"),
    space(2, Key::Num2, "space-2", "Go to Space 2"),
    space(3, Key::Num3, "space-3", "Go to Space 3"),
    space(4, Key::Num4, "space-4", "Go to Space 4"),
    space(5, Key::Num5, "space-5", "Go to Space 5"),
    space(6, Key::Num6, "space-6", "Go to Space 6"),
    space(7, Key::Num7, "space-7", "Go to Space 7"),
    space(8, Key::Num8, "space-8", "Go to Space 8"),
    space(9, Key::Num9, "space-9", "Go to Space 9"),
    action(Command::ZoomIn, "zoom-in", "Zoom In", Category::View, chord(Key::Equals, CMD), chord(Key::Plus, CMD)),
    action(Command::ZoomOut, "zoom-out", "Zoom Out", Category::View, chord(Key::Minus, CMD), None),
    action(Command::ActualSize, "actual-size", "Actual Size", Category::View, chord(Key::Num0, CMD), None),
    action(Command::DevTools, "developer-tools", "Developer Tools", Category::Page, chord(Key::I, CMD_ALT), None),
    action(Command::ViewSource, "view-source", "View Page Source", Category::Page, chord(Key::U, CMD_ALT), None),
    action(Command::Print, "print", "Print", Category::Page, chord(Key::P, CMD), None),
    // ⌘⇧3, ⌘⇧4 and ⌘⇧5 are macOS's own screenshots.
    action(Command::Capture, "capture", "Capture", Category::Page, chord(Key::Num2, CMD_SHIFT), None),
    fixed(Command::Settings, "settings", "Settings", chord(Key::Comma, CMD)),
    fixed(Command::CloseOverlay, "close-overlay", "Close Overlay or Cancel", chord(Key::Escape, NONE)),
];

pub fn index_of(command: Command) -> usize {
    REGISTRY.iter().position(|spec| spec.command == command).expect("every command is registered")
}

/// Slots whose binding differs from the default; `None` unsets one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides(BTreeMap<(usize, Slot), Option<Chord>>);

impl Overrides {
    pub fn binding(&self, index: usize, slot: Slot) -> Option<Chord> {
        self.0.get(&(index, slot)).copied().unwrap_or_else(|| REGISTRY[index].default_binding(slot))
    }

    pub fn set(&mut self, index: usize, slot: Slot, chord: Option<Chord>) {
        if chord == REGISTRY[index].default_binding(slot) {
            self.0.remove(&(index, slot));
        } else {
            self.0.insert((index, slot), chord);
        }
    }

    pub fn changed(&self, index: usize) -> bool {
        Slot::BOTH.iter().any(|slot| self.0.contains_key(&(index, *slot)))
    }

    /// How many actions differ from their defaults.
    pub fn changed_actions(&self) -> usize {
        (0..REGISTRY.len()).filter(|index| self.changed(*index)).count()
    }

    /// Every bound slot whose chord overlaps `chord`.
    pub fn owners(&self, chord: Chord) -> Vec<(usize, Slot)> {
        (0..REGISTRY.len()).flat_map(|index| Slot::BOTH.map(|slot| (index, slot)))
            .filter(|(index, slot)| self.binding(*index, *slot).is_some_and(|bound| bound.overlaps(chord)))
            .collect()
    }

    fn encode(&self) -> String {
        let rows: Vec<_> = self.0.iter().map(|((index, slot), chord)| serde_json::json!({
            "action": REGISTRY[*index].id,
            "slot": slot.name(),
            "chord": chord.map(Chord::encode),
        })).collect();
        serde_json::json!({ "version": VERSION, "overrides": rows }).to_string()
    }

    /// Saved overrides and how many rows were ignored. Unknown actions,
    /// unreadable or refused chords and duplicate rows are skipped; a chord
    /// that collides with another binding falls back to its default when free.
    fn decode(raw: &str) -> Result<(Overrides, usize), &'static str> {
        const UNREADABLE: &str = "Saved shortcuts could not be read, so the defaults are in use. Changing a shortcut saves over the unreadable record.";
        let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| UNREADABLE)?;
        match value["version"].as_u64() {
            Some(VERSION) => {}
            Some(version) if version > VERSION => return Err("Saved shortcuts are from a newer version of Crossbow, so the defaults are in use. Changing a shortcut here replaces them."),
            _ => return Err(UNREADABLE),
        }
        let rows = value["overrides"].as_array().ok_or(UNREADABLE)?;
        let mut ignored = 0;
        let mut read: BTreeMap<(usize, Slot), Option<Chord>> = BTreeMap::new();
        for row in rows {
            let index = REGISTRY.iter().position(|spec| !spec.fixed && row["action"].as_str() == Some(spec.id));
            let slot = Slot::BOTH.into_iter().find(|slot| row["slot"].as_str() == Some(slot.name()));
            let chord = match &row["chord"] {
                serde_json::Value::Null => Some(None),
                serde_json::Value::String(text) => Chord::decode(text).filter(|chord| refusal(*chord).is_none()).map(Some),
                _ => None,
            };
            match (index, slot, chord) {
                (Some(index), Some(slot), Some(chord)) if !read.contains_key(&(index, slot)) => { read.insert((index, slot), chord); }
                _ => ignored += 1,
            }
        }
        // Defaults nothing replaced keep their chords; saved chords claim theirs in registry order.
        let mut claimed: Vec<Chord> = (0..REGISTRY.len()).flat_map(|index| Slot::BOTH.map(|slot| (index, slot)))
            .filter(|key| !read.contains_key(key))
            .filter_map(|(index, slot)| REGISTRY[index].default_binding(slot))
            .collect();
        let mut overrides = Overrides::default();
        for ((index, slot), chord) in read {
            let free = |chord: &Chord| !claimed.iter().any(|other| other.overlaps(*chord));
            let chord = match chord {
                Some(chord) if free(&chord) => Some(chord),
                Some(_) => {
                    ignored += 1;
                    REGISTRY[index].default_binding(slot).filter(free)
                }
                None => None,
            };
            claimed.extend(chord);
            overrides.set(index, slot, chord);
        }
        Ok((overrides, ignored))
    }
}

/// What recording a chord into a slot would do.
#[derive(Debug, PartialEq)]
pub enum Proposal {
    Unchanged,
    Refused(String),
    Ready(Overrides),
    /// Other slots use an overlapping chord; `reassigned` also unsets them.
    Conflict { owners: Vec<(usize, Slot)>, reassigned: Overrides },
}

/// Who gets a browser shortcut first while a web page has keyboard focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Priority {
    /// Crossbow runs the action and the page never receives the press.
    #[default]
    Browser,
    /// The page receives the press; Crossbow runs the action only if the page
    /// leaves it unhandled. Protected actions stay with Crossbow.
    Websites,
    /// Press once for the website, twice for Crossbow: as `Websites`, except
    /// that a second press of the same shortcut within [`SECOND_PRESS`] runs
    /// Crossbow's action and the page never receives it.
    Twice,
}

impl Priority {
    pub const ALL: [Priority; 3] = [Priority::Browser, Priority::Websites, Priority::Twice];

    pub fn label(self) -> &'static str {
        match self {
            Priority::Browser => "Crossbow first",
            Priority::Websites => "Websites first",
            Priority::Twice => "Press twice",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Priority::Browser => "browser",
            Priority::Websites => "websites",
            Priority::Twice => "press-twice",
        }
    }
}

/// In [`Priority::Twice`], how soon after a first press the same shortcut
/// pressed again is Crossbow's, in seconds: about a double-click.
pub const SECOND_PRESS: f64 = 0.5;

/// The page a first press went to: its view and how many navigations it had
/// made. A different view or a navigation starts the count again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageMark {
    pub view: u64,
    pub navigations: u64,
}

/// Press once for the website, twice for Crossbow. Holds the last first
/// press a page received until its second press, another key, a focus
/// change, a navigation or [`SECOND_PRESS`] ends it. Only ever in memory.
#[derive(Debug, Default)]
pub struct FirstPress {
    on: bool,
    now: f64,
    armed: Option<Armed>,
    /// A second press Crossbow took, until its key goes up: its auto-repeat
    /// stays with Crossbow, as in browser-first mode.
    held: Option<Chord>,
}

#[derive(Debug)]
struct Armed {
    chord: Chord,
    at: f64,
    page: PageMark,
    /// The engine's ticket for the first press.
    ticket: u64,
}

impl FirstPress {
    /// Begin a frame. `on`: press-twice is the chosen priority. `page`: the
    /// page with the keyboard, None while Crossbow's own controls, an overlay
    /// or nothing has it. The window losing focus to another app counts as
    /// None too, and egui's frame time is the clock.
    pub fn follow(&mut self, ctx: &egui::Context, on: bool, page: Option<PageMark>) {
        let (now, focused) = ctx.input(|input| (input.time, input.focused));
        let page = page.filter(|_| focused);
        self.on = on;
        self.now = now;
        if !self.armed.as_ref().is_some_and(|armed| on && page == Some(armed.page) && now - armed.at <= SECOND_PRESS) {
            self.armed = None;
        }
        if !on || page.is_none() {
            self.held = None;
        }
    }

    /// The page received `press` as a first press, tracked as `ticket`.
    pub fn arm(&mut self, press: &Deferred, page: PageMark, ticket: u64) {
        if self.on {
            self.armed = Some(Armed { chord: press.chord, at: self.now, page, ticket });
        }
    }

    /// The page left `ticket` unhandled, so its action ran: the next press is
    /// a first press again.
    pub fn reported(&mut self, ticket: u64) {
        if self.armed.as_ref().is_some_and(|armed| armed.ticket == ticket) {
            self.armed = None;
        }
    }

    /// For a second press of `chord`: the first press's ticket, whose report
    /// must now run nothing.
    fn second(&mut self, chord: Chord) -> Option<u64> {
        let now = self.now;
        let armed = self.armed.take_if(|armed| armed.chord == chord && now - armed.at <= SECOND_PRESS)?;
        self.held = Some(chord);
        Some(armed.ticket)
    }

    /// Any other press ends the sequence.
    fn reset(&mut self) {
        self.armed = None;
        self.held = None;
    }
}

impl Command {
    /// Actions that never go to a page first, so a page can't keep anyone in
    /// it: Chromium reserves the same tab commands, plus the address field,
    /// Settings and Escape.
    pub fn protected(self) -> bool {
        matches!(self, Command::NewTab | Command::EditUrl | Command::CloseTab | Command::ReopenClosed | Command::NextTab | Command::PreviousTab | Command::Settings | Command::CloseOverlay)
    }
}

#[derive(Clone)]
pub struct Shortcuts {
    saved: Overrides,
    /// Why saved shortcuts were not fully read; cleared by the next save.
    pub notice: Option<String>,
    priority: Priority,
    /// Why the saved priority was not read; cleared by the next priority save.
    pub priority_notice: Option<String>,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Shortcuts { saved: Overrides::default(), notice: None, priority: Priority::Browser, priority_notice: None }
    }
}

impl Shortcuts {
    pub fn defaults() -> Self {
        Self::default()
    }

    /// Read this device's overrides. A bad record never blocks the defaults
    /// and is left untouched until the next save.
    pub fn load(store: &Store) -> Self {
        const UNREADABLE: &str = "The saved shortcut priority could not be read, so Crossbow's shortcuts come first. Choosing an option saves over it.";
        let (priority, priority_notice) = match store.kv_get(PRIORITY_KEY) {
            Ok(None) => (Priority::Browser, None),
            Ok(Some(raw)) => match Priority::ALL.into_iter().find(|priority| priority.name() == raw) {
                Some(priority) => (priority, None),
                None => (Priority::Browser, Some(UNREADABLE.to_string())),
            },
            Err(_) => (Priority::Browser, Some(UNREADABLE.to_string())),
        };
        Shortcuts { priority, priority_notice, ..Self::load_overrides(store) }
    }

    fn load_overrides(store: &Store) -> Self {
        let raw = match store.kv_get(KEY) {
            Ok(raw) => raw,
            Err(_) => return Shortcuts { notice: Some("Saved shortcuts could not be read, so the defaults are in use. Reopen Crossbow to try again; changing a shortcut saves over them.".into()), ..Self::defaults() },
        };
        let Some(raw) = raw else { return Self::defaults() };
        match Overrides::decode(&raw) {
            Ok((saved, 0)) => Shortcuts { saved, ..Self::defaults() },
            Ok((saved, _)) => Shortcuts { saved, notice: Some("Some saved shortcuts were unreadable or conflicted with another, so they were ignored. Changing a shortcut saves the list shown here.".into()), ..Self::defaults() },
            Err(notice) => Shortcuts { notice: Some(notice.into()), ..Self::defaults() },
        }
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Save this device's priority. On failure the previous one stays in use.
    pub fn save_priority(&mut self, store: &Store, priority: Priority) -> Result<(), String> {
        store.kv_set(PRIORITY_KEY, priority.name()).map_err(|error| error.to_string())?;
        self.priority = priority;
        self.priority_notice = None;
        Ok(())
    }

    pub fn saved(&self) -> &Overrides {
        &self.saved
    }

    pub fn binding(&self, command: Command, slot: Slot) -> Option<Chord> {
        self.saved.binding(index_of(command), slot)
    }

    /// The chord shown beside a menu item or command: primary, else alternate.
    pub fn hint(&self, command: Command) -> String {
        Slot::BOTH.into_iter().find_map(|slot| self.binding(command, slot)).map(Chord::label).unwrap_or_default()
    }

    /// Write `candidate` in one statement. On failure nothing changes here.
    pub fn save(&mut self, store: &Store, candidate: Overrides) -> Result<(), String> {
        store.kv_set(KEY, &candidate.encode()).map_err(|error| error.to_string())?;
        self.saved = candidate;
        self.notice = None;
        Ok(())
    }

    pub fn propose(&self, index: usize, slot: Slot, chord: Option<Chord>) -> Proposal {
        let spec = &REGISTRY[index];
        if spec.fixed {
            return Proposal::Refused(format!("{} can't be changed.", spec.label));
        }
        if chord == self.saved.binding(index, slot) {
            return Proposal::Unchanged;
        }
        let mut candidate = self.saved.clone();
        candidate.set(index, slot, chord);
        let Some(chord) = chord else { return Proposal::Ready(candidate) };
        if let Some(reason) = refusal(chord) {
            return Proposal::Refused(reason);
        }
        let owners: Vec<_> = self.saved.owners(chord).into_iter().filter(|owner| *owner != (index, slot)).collect();
        if owners.is_empty() {
            return Proposal::Ready(candidate);
        }
        for (other, other_slot) in &owners {
            candidate.set(*other, *other_slot, None);
        }
        Proposal::Conflict { owners, reassigned: candidate }
    }

    /// Restore one action's defaults. Slots of other actions holding one of
    /// those chords now are returned and lose it in the same candidate.
    pub fn propose_reset(&self, index: usize) -> (Overrides, Vec<(usize, Slot)>) {
        let mut candidate = self.saved.clone();
        let mut owners = Vec::new();
        for slot in Slot::BOTH {
            candidate.set(index, slot, REGISTRY[index].default_binding(slot));
            for owner in REGISTRY[index].default_binding(slot).map(|chord| self.saved.owners(chord)).unwrap_or_default() {
                if owner.0 != index && !owners.contains(&owner) {
                    owners.push(owner);
                    candidate.set(owner.0, owner.1, None);
                }
            }
        }
        (candidate, owners)
    }

    /// Take each bound chord pressed this frame, at most once per binding,
    /// in registry order. Escape is left for overlay dismissal.
    pub fn take(&self, ctx: &egui::Context, editing_text: bool) -> Vec<Command> {
        let mut taken = Vec::new();
        for (index, spec) in REGISTRY.iter().enumerate() {
            if spec.command == Command::CloseOverlay { continue; }
            for slot in Slot::BOTH {
                let Some(chord) = self.saved.binding(index, slot) else { continue };
                if editing_text && chord.yields_to_text() { continue; }
                if chord.forms().any(|form| crate::app::take_shortcut(ctx, form.mods, form.key)) {
                    taken.push(spec.command);
                }
            }
        }
        taken
    }

    /// Route this frame's presses. `page_first` means websites receive
    /// shortcuts first and a web page has the keyboard; `first` is the
    /// press-twice sequence, inert unless that mode is on.
    pub fn route(&self, ctx: &egui::Context, editing_text: bool, page_first: bool, first: &mut FirstPress) -> Routed {
        if !page_first {
            return Routed { run: self.take(ctx, editing_text), ..Routed::default() };
        }
        let mut routed = Routed::default();
        // Protected actions keep their presses, exactly as in browser-first mode.
        for (index, spec) in REGISTRY.iter().enumerate().filter(|(_, spec)| spec.command.protected() && spec.command != Command::CloseOverlay) {
            for slot in Slot::BOTH {
                let Some(chord) = self.saved.binding(index, slot) else { continue };
                if editing_text && chord.yields_to_text() { continue; }
                if chord.forms().any(|form| crate::app::take_shortcut(ctx, form.mods, form.key)) {
                    routed.run.push(spec.command);
                }
            }
        }
        // A protected press ends a press-twice sequence, like any other key.
        if !routed.run.is_empty() {
            first.reset();
        }
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::Key { key, physical_key, pressed, repeat, modifiers } = *event else { return true };
                // Modifier and lock keys alone are no press: holding ⌘ again for
                // the second ⌘R must not end the sequence.
                if modifier_only(key) {
                    return true;
                }
                let chord = Chord::from_event(key, modifiers);
                if !pressed {
                    // A key-up is never a press; it ends a held second press.
                    if first.held.is_some_and(|held| held.key == key) {
                        first.held = None;
                    }
                    return true;
                }
                let Some(command) = self.command_for(chord, editing_text) else {
                    if !repeat {
                        first.reset();
                    }
                    return true;
                };
                match crate::input::web_key(key, physical_key) {
                    // No page can receive this key, so the browser has it now.
                    None => {
                        first.reset();
                        routed.run.push(command);
                        false
                    }
                    // An auto-repeat stays with whoever had the press: the page,
                    // or Crossbow after a second press, which repeats as in
                    // browser-first mode.
                    Some(_) if repeat => {
                        if first.held != Some(chord) {
                            return true;
                        }
                        routed.run.push(command);
                        false
                    }
                    Some(named) => {
                        // A second press is Crossbow's; the first press's report runs nothing.
                        if let Some(ticket) = first.second(chord) {
                            routed.run.push(command);
                            routed.withdrawn.push(ticket);
                            return false;
                        }
                        first.reset();
                        routed.deferred.push(Deferred { command, chord, key: named, mods: crate::input::modifiers(modifiers) });
                        true
                    }
                }
            });
        });
        routed
    }

    /// The unprotected action bound to `chord`, if any.
    fn command_for(&self, chord: Chord, editing_text: bool) -> Option<Command> {
        REGISTRY.iter().enumerate().filter(|(_, spec)| !spec.command.protected()).find_map(|(index, spec)| {
            Slot::BOTH.into_iter().filter_map(|slot| self.saved.binding(index, slot))
                .any(|bound| !(editing_text && bound.yields_to_text()) && bound.forms().any(|form| form == chord))
                .then_some(spec.command)
        })
    }

    /// What always stays with Crossbow, with the current shortcut, for Settings.
    pub fn protected(&self) -> Vec<(&'static str, String)> {
        let mut listed: Vec<_> = REGISTRY.iter().filter(|spec| spec.command.protected() && spec.command != Command::CloseOverlay)
            .map(|spec| (spec.label, self.hint(spec.command)))
            .collect();
        if MAC {
            listed.push(("Quit Crossbow", Chord { key: Key::Q, mods: CMD }.label()));
        }
        listed.push(("Esc while a Crossbow overlay is open", String::new()));
        listed
    }
}

/// What one frame's presses do.
#[derive(Debug, Default, PartialEq)]
pub struct Routed {
    /// Actions to run now; their presses were taken from the input.
    pub run: Vec<Command>,
    /// Presses left in the input for the web page to have first.
    pub deferred: Vec<Deferred>,
    /// Tickets of first presses whose second press ran the action instead.
    pub withdrawn: Vec<u64>,
}

/// Keys egui reports on their own when a modifier or lock key goes down or up.
fn modifier_only(key: Key) -> bool {
    matches!(key, Key::ShiftLeft | Key::ShiftRight | Key::ControlLeft | Key::ControlRight | Key::AltLeft | Key::AltRight | Key::SuperLeft | Key::SuperRight | Key::CapsLock | Key::NumLock | Key::Fn)
}

/// A browser shortcut press sent to the web page first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deferred {
    pub command: Command,
    chord: Chord,
    key: crossbow_engine::NamedKey,
    mods: crossbow_engine::Modifiers,
}

/// The deferral for `input`, taken from `deferred` so it is tracked once.
/// Key-up and auto-repeat are never tracked.
pub fn claim(deferred: &mut Vec<Deferred>, input: &crossbow_engine::KeyInput) -> Option<Deferred> {
    if !input.pressed || input.repeat { return None; }
    let index = deferred.iter().position(|deferred| deferred.key == input.key && deferred.mods == input.modifiers)?;
    Some(deferred.remove(index))
}

/// Deferred actions sent to a page, by the engine's ticket for the press.
#[derive(Default)]
pub struct Waiting(Vec<(u64, Command)>);

impl Waiting {
    /// Presses a page consumed are never answered; keep only the latest.
    const LIMIT: usize = 64;

    pub fn wait(&mut self, ticket: u64, command: Command) {
        if self.0.len() == Self::LIMIT { self.0.remove(0); }
        self.0.push((ticket, command));
    }

    /// The action to run for a press its page left unhandled, at most once.
    /// `same_page`: the page that reported it still has the keyboard; after a
    /// switch to another tab, Peek or Crossbow's own controls nothing runs.
    pub fn unhandled(&mut self, ticket: u64, same_page: bool) -> Option<Command> {
        let index = self.0.iter().position(|(waiting, _)| *waiting == ticket)?;
        let (_, command) = self.0.remove(index);
        same_page.then_some(command)
    }

    /// A press whose action already ran another way: its report runs nothing.
    pub fn withdraw(&mut self, ticket: u64) {
        self.0.retain(|(waiting, _)| *waiting != ticket);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every chord `CrossbowApp::handle_shortcuts` took before the registry,
    /// read from its `take(...)` calls (aliases included), and the action it ran.
    pub(crate) fn inventory() -> Vec<(Key, Modifiers, Command)> {
        let mut chords = vec![
            (Key::T, CMD_SHIFT, Command::ReopenClosed),
            (Key::T, CMD, Command::NewTab),
            (Key::L, CMD, Command::EditUrl),
            (Key::W, CMD, Command::CloseTab),
            (Key::R, CMD_SHIFT, Command::HardReload),
            (Key::R, CMD, Command::Reload),
            (Key::OpenBracket, CMD, Command::Back),
            (Key::CloseBracket, CMD, Command::Forward),
            (Key::ArrowLeft, CMD_ALT, Command::PreviousSpace),
            (Key::ArrowRight, CMD_ALT, Command::NextSpace),
            (Key::ArrowLeft, CMD, Command::Back),
            (Key::ArrowRight, CMD, Command::Forward),
            (Key::S, CMD, Command::ToggleSidebar),
            (Key::D, CMD_SHIFT, Command::Favorite),
            (Key::D, CMD, Command::PinTab),
            (Key::N, CMD_ALT, Command::NewFolder),
            (Key::C, CMD_SHIFT, Command::CopyUrl),
            (Key::F, CMD, Command::Find),
            (Key::Equals, CMD, Command::ZoomIn),
            (Key::Plus, CMD, Command::ZoomIn),
            // Zoom In took ⌘⇧= first; Split View's later take never saw it.
            (Key::Equals, CMD_SHIFT, Command::ZoomIn),
            (Key::Minus, CMD, Command::ZoomOut),
            (Key::Num0, CMD, Command::ActualSize),
            (Key::Comma, CMD, Command::Settings),
            (Key::I, CMD_ALT, Command::DevTools),
            (Key::U, CMD_ALT, Command::ViewSource),
            (Key::P, CMD, Command::Print),
            (Key::Num2, CMD_SHIFT, Command::Capture),
            (Key::K, CMD_SHIFT, Command::ClearToday),
            (Key::Equals, CTRL_SHIFT, Command::SplitView),
            (Key::Tab, CTRL, Command::NextTab),
            (Key::Tab, CTRL_SHIFT, Command::PreviousTab),
        ];
        let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (i, key) in digits.into_iter().enumerate() {
            chords.push((key, CMD, if i == 8 { Command::LastTab } else { Command::Tab(i as u8 + 1) }));
            if cfg!(target_os = "macos") {
                chords.push((key, CTRL, Command::Space(i as u8 + 1)));
            }
        }
        // Added with the registry: New Little Window has a default on macOS only.
        if MAC {
            chords.push((Key::N, CTRL_CMD, Command::NewLittleWindow));
        }
        chords
    }

    pub(crate) fn press(key: Key, modifiers: Modifiers) -> egui::RawInput {
        egui::RawInput {
            events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
            ..Default::default()
        }
    }

    /// Commands taken for one press, and whether the press was left for others.
    pub(crate) fn dispatch(shortcuts: &Shortcuts, key: Key, modifiers: Modifiers, editing_text: bool) -> (Vec<Command>, bool) {
        let ctx = egui::Context::default();
        let mut result = (Vec::new(), false);
        ctx.run_ui(press(key, modifiers), |ui| {
            result.0 = shortcuts.take(ui.ctx(), editing_text);
            result.1 = ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { .. })));
        }).textures_delta.clear();
        result
    }

    #[test]
    fn default_registry_dispatches_every_chord_handled_before_the_registry() {
        let shortcuts = Shortcuts::defaults();
        let mut wrong = Vec::new();
        for (key, modifiers, expected) in inventory() {
            let (taken, left) = dispatch(&shortcuts, key, modifiers, false);
            if taken != [expected] || left {
                wrong.push(format!("{key:?} {modifiers:?}: expected {expected:?}, got {taken:?} (left for others: {left})"));
            }
        }
        assert!(wrong.is_empty(), "{} of {} chords changed:\n{}", wrong.len(), inventory().len(), wrong.join("\n"));
    }

    #[test]
    fn command_arrows_stay_with_text_fields_and_unbound_chords_pass_through() {
        let shortcuts = Shortcuts::defaults();
        for key in [Key::ArrowLeft, Key::ArrowRight] {
            assert_eq!(dispatch(&shortcuts, key, CMD, true), (vec![], true), "⌘{key:?} moves the caret while editing");
            assert_eq!(dispatch(&shortcuts, key, CMD_ALT, true).0.len(), 1, "⌘⌥{key:?} still switches Spaces");
        }
        for (key, modifiers) in [(Key::K, CMD), (Key::N, CMD), (Key::N, CMD_ALT | Modifiers::SHIFT), (Key::Escape, Modifiers::NONE), (Key::A, CMD), (Key::Equals, CTRL)] {
            assert_eq!(dispatch(&shortcuts, key, modifiers, false), (vec![], true), "{key:?} {modifiers:?} was never a browser shortcut");
        }
    }

    fn at(command: Command, slot: Slot) -> (usize, Slot) {
        (index_of(command), slot)
    }

    fn set(shortcuts: &mut Shortcuts, store: &Store, command: Command, slot: Slot, chord: Option<Chord>) {
        let Proposal::Ready(candidate) = shortcuts.propose(index_of(command), slot, chord) else { panic!("{command:?} {slot:?} should be ready") };
        shortcuts.save(store, candidate).unwrap();
    }

    /// A disposable store under TMPDIR.
    pub(crate) struct Fixture {
        dir: std::path::PathBuf,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("crossbow-shortcuts-{}", crossbow_core::new_id()));
            std::fs::create_dir(&dir).unwrap();
            Fixture { dir }
        }

        pub(crate) fn path(&self) -> std::path::PathBuf {
            self.dir.join("fixture.sqlite")
        }

        pub(crate) fn store(&self) -> Store {
            Store::open(&self.path()).unwrap()
        }

        /// URI mode=ro makes the real write fail without mocks.
        pub(crate) fn read_only(&self) -> Store {
            let path = self.path().to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
            Store::open(std::path::Path::new(&format!("file:{path}?mode=ro"))).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn registry_ids_are_stable_unique_and_defaults_never_collide() {
        for (index, spec) in REGISTRY.iter().enumerate() {
            assert_eq!(index_of(spec.command), index, "{:?} is registered once", spec.command);
            assert_eq!(REGISTRY.iter().filter(|other| other.id == spec.id).count(), 1, "{} is unique", spec.id);
            for slot in Slot::BOTH {
                let Some(chord) = spec.default_binding(slot) else { continue };
                assert_eq!(refusal(chord).is_some(), spec.fixed, "{} {slot:?}: only fixed actions own reserved chords", spec.id);
                let owners = Overrides::default().owners(chord);
                assert_eq!(owners, vec![(index, slot)], "{} {slot:?} {} collides", spec.id, chord.label());
            }
        }
        let listed: Vec<(Key, Modifiers, Command)> = inventory();
        for spec in REGISTRY.iter().filter(|spec| !spec.fixed) {
            assert!(listed.iter().any(|(.., command)| *command == spec.command) || (!MAC && matches!(spec.command, Command::Space(_) | Command::NewLittleWindow))
                || matches!(spec.command, Command::ShowLittleWindows | Command::CloseLittleWindows | Command::NewDocument), "{} runs from a chord", spec.id);
        }
    }

    #[test]
    fn show_and_close_all_little_windows_are_registered_without_default_chords() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        for (command, id, label, key) in [(Command::ShowLittleWindows, "show-little-windows", "Show All Little Windows", Key::F6), (Command::CloseLittleWindows, "close-little-windows", "Close All Little Windows", Key::F7), (Command::NewDocument, "new-document", "New Document", Key::F8)] {
            let spec = REGISTRY.iter().find(|spec| spec.command == command).unwrap_or_else(|| panic!("{id} is a registry action"));
            assert_eq!((spec.id, spec.label, spec.fixed), (id, label, false));
            assert!(Slot::BOTH.into_iter().all(|slot| spec.default_binding(slot).is_none()), "{id} has no default chord");
            assert!(!inventory().iter().any(|(.., listed)| *listed == command), "no chord ran it before");
            set(&mut shortcuts, &store, command, Slot::Primary, Some(Chord { key, mods: NONE }));
            assert_eq!(dispatch(&shortcuts, key, NONE, false), (vec![command], false), "a chord the user sets runs {id} once");
            assert_eq!(dispatch(&Shortcuts::load(&fixture.store()), key, NONE, false).0, vec![command], "and is read back");
        }
    }

    #[test]
    fn chords_round_trip_through_their_saved_text() {
        for key in Key::ALL {
            for m in [NONE, CMD, CMD_SHIFT, CMD_ALT, CTRL, CTRL_SHIFT, CTRL_CMD, mods(true, true, true, true)] {
                let chord = Chord { key: *key, mods: m };
                assert_eq!(Chord::decode(&chord.encode()), Some(chord), "{}", chord.encode());
            }
        }
        for bad in ["", "cmd+", "cmd+cmd+T", "hyper+T", "cmd+Digit1", "cmd+t", "T+cmd"] {
            assert_eq!(Chord::decode(bad), None, "{bad:?}");
        }
        assert_eq!(Chord { key: Key::Equals, mods: CTRL_SHIFT }.label(), if MAC { "⌃⇧=" } else { "Shift+=" });
    }

    #[test]
    fn rebinding_runs_the_new_chord_once_and_the_old_chord_no_longer() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        set(&mut shortcuts, &store, Command::Reload, Slot::Primary, Some(Chord { key: Key::K, mods: CMD }));
        assert_eq!(dispatch(&shortcuts, Key::K, CMD, false), (vec![Command::Reload], false));
        assert_eq!(dispatch(&shortcuts, Key::R, CMD, false), (vec![], true), "the old chord is free again");
        // An alternate runs the same action, each chord exactly once.
        set(&mut shortcuts, &store, Command::Reload, Slot::Alternate, Some(Chord { key: Key::F5, mods: NONE }));
        assert_eq!(dispatch(&shortcuts, Key::F5, NONE, false).0, vec![Command::Reload]);
        assert_eq!(dispatch(&shortcuts, Key::K, CMD, false).0, vec![Command::Reload]);
        // Unsetting one mapping disables only that mapping.
        set(&mut shortcuts, &store, Command::Reload, Slot::Primary, None);
        assert_eq!(dispatch(&shortcuts, Key::K, CMD, false), (vec![], true));
        assert_eq!(dispatch(&shortcuts, Key::F5, NONE, false).0, vec![Command::Reload]);
        set(&mut shortcuts, &store, Command::ClearToday, Slot::Primary, None);
        assert_eq!(dispatch(&shortcuts, Key::K, CMD_SHIFT, false), (vec![], true), "an unset action does not run");
        // A rebound caret chord still yields to text fields.
        set(&mut shortcuts, &store, Command::Print, Slot::Primary, Some(Chord { key: Key::ArrowUp, mods: CMD }));
        assert_eq!(dispatch(&shortcuts, Key::ArrowUp, CMD, true), (vec![], true));
        assert_eq!(dispatch(&shortcuts, Key::ArrowUp, CMD, false).0, vec![Command::Print]);
        // Dispatch after reopening agrees with what was saved.
        drop(store);
        let reopened = Shortcuts::load(&fixture.store());
        assert_eq!(reopened.saved(), shortcuts.saved());
        assert_eq!(dispatch(&reopened, Key::F5, NONE, false).0, vec![Command::Reload]);
        assert_eq!(dispatch(&reopened, Key::R, CMD, false).0, vec![]);
    }

    #[test]
    fn conflicts_name_the_other_action_and_reassign_atomically() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        let reload = index_of(Command::Reload);
        let Proposal::Conflict { owners, reassigned } = shortcuts.propose(reload, Slot::Primary, Some(Chord { key: Key::W, mods: CMD })) else { panic!("⌘W belongs to Close Tab") };
        assert_eq!(owners, vec![at(Command::CloseTab, Slot::Primary)]);
        assert_eq!(store.kv_get(KEY).unwrap(), None, "asking writes nothing");
        shortcuts.save(&store, reassigned).unwrap();
        assert_eq!(shortcuts.binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
        assert_eq!(shortcuts.binding(Command::CloseTab, Slot::Primary), None);
        assert_eq!(dispatch(&shortcuts, Key::W, CMD, false).0, vec![Command::Reload]);
        // ⌘⇧= is the same "+" key as Zoom In's ⌘+ alternate.
        let Proposal::Conflict { owners, .. } = shortcuts.propose(reload, Slot::Alternate, Some(Chord { key: Key::Equals, mods: CMD_SHIFT })) else { panic!("⌘⇧= is Zoom In's") };
        assert_eq!(owners, vec![at(Command::ZoomIn, Slot::Alternate)]);
        let reopened = Shortcuts::load(&fixture.store());
        assert_eq!(reopened.binding(Command::CloseTab, Slot::Primary), None);
        assert_eq!(reopened.binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
    }

    #[test]
    fn reserved_bare_and_escape_chords_are_refused_and_change_nothing() {
        let shortcuts = Shortcuts::defaults();
        let reload = index_of(Command::Reload);
        let mut refused = vec![
            Chord { key: Key::Comma, mods: CMD },
            Chord { key: Key::C, mods: CMD },
            Chord { key: Key::Escape, mods: CMD },
            Chord { key: Key::K, mods: NONE },
            Chord { key: Key::K, mods: mods(false, true, false, false) },
            Chord { key: Key::Tab, mods: mods(false, false, true, false) },
        ];
        if MAC {
            refused.extend([
                Chord { key: Key::Q, mods: CMD }, Chord { key: Key::H, mods: CMD }, Chord { key: Key::H, mods: CMD_ALT },
                Chord { key: Key::M, mods: CMD }, Chord { key: Key::Tab, mods: CMD }, Chord { key: Key::Backtick, mods: CMD },
                Chord { key: Key::Space, mods: CMD }, Chord { key: Key::Q, mods: CTRL_CMD }, Chord { key: Key::F, mods: CTRL_CMD },
            ]);
        }
        for chord in refused {
            assert!(matches!(shortcuts.propose(reload, Slot::Primary, Some(chord)), Proposal::Refused(_)), "{} must be refused", chord.label());
        }
        assert!(matches!(shortcuts.propose(index_of(Command::Settings), Slot::Primary, Some(Chord { key: Key::J, mods: CMD })), Proposal::Refused(_)), "Settings is fixed");
        assert!(matches!(shortcuts.propose(reload, Slot::Alternate, Some(Chord { key: Key::F5, mods: NONE })), Proposal::Ready(_)), "function keys work alone");
        assert!(matches!(shortcuts.propose(reload, Slot::Primary, Some(Chord { key: Key::R, mods: CMD })), Proposal::Unchanged));
        let Proposal::Refused(reason) = shortcuts.propose(reload, Slot::Primary, Some(Chord { key: Key::Q, mods: CMD })) else { unreachable!() };
        if MAC { assert_eq!(reason, "⌘Q is reserved: Crossbow quits with it."); }
        assert_eq!(shortcuts.saved(), &Overrides::default());
    }

    #[test]
    fn resetting_one_action_takes_its_chords_back_and_reset_all_restores_defaults() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        let Proposal::Conflict { reassigned, .. } = shortcuts.propose(index_of(Command::Reload), Slot::Primary, Some(Chord { key: Key::W, mods: CMD })) else { unreachable!() };
        shortcuts.save(&store, reassigned).unwrap();
        set(&mut shortcuts, &store, Command::Print, Slot::Alternate, Some(Chord { key: Key::F6, mods: NONE }));
        let (candidate, owners) = shortcuts.propose_reset(index_of(Command::CloseTab));
        assert_eq!(owners, vec![at(Command::Reload, Slot::Primary)]);
        shortcuts.save(&store, candidate).unwrap();
        assert_eq!(shortcuts.binding(Command::CloseTab, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
        assert_eq!(shortcuts.binding(Command::Reload, Slot::Primary), None, "the other action visibly loses the chord");
        assert_eq!(shortcuts.binding(Command::Print, Slot::Alternate), Some(Chord { key: Key::F6, mods: NONE }), "other overrides stay");
        let (candidate, owners) = shortcuts.propose_reset(index_of(Command::Reload));
        assert!(owners.is_empty());
        shortcuts.save(&store, candidate).unwrap();
        assert_eq!(shortcuts.saved().changed_actions(), 1);
        shortcuts.save(&store, Overrides::default()).unwrap();
        let reopened = Shortcuts::load(&fixture.store());
        assert_eq!(reopened.saved(), &Overrides::default());
        for (key, modifiers, command) in inventory() {
            assert_eq!(dispatch(&reopened, key, modifiers, false).0, vec![command]);
        }
    }

    #[test]
    fn a_failed_write_keeps_the_previous_shortcuts_and_retry_saves_once() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        set(&mut shortcuts, &store, Command::Reload, Slot::Primary, Some(Chord { key: Key::K, mods: CMD }));
        let before = shortcuts.saved().clone();
        let raw = store.kv_get(KEY).unwrap();
        let read_only = fixture.read_only();
        let Proposal::Conflict { reassigned, .. } = shortcuts.propose(index_of(Command::Reload), Slot::Primary, Some(Chord { key: Key::W, mods: CMD })) else { unreachable!() };
        assert!(shortcuts.save(&read_only, reassigned.clone()).is_err());
        assert_eq!(shortcuts.saved(), &before, "neither action changed");
        assert_eq!(store.kv_get(KEY).unwrap(), raw);
        assert_eq!(dispatch(&shortcuts, Key::K, CMD, false).0, vec![Command::Reload]);
        assert_eq!(dispatch(&shortcuts, Key::W, CMD, false).0, vec![Command::CloseTab]);
        assert!(shortcuts.save(&read_only, Overrides::default()).is_err(), "a failed reset all leaves no half-reset");
        assert_eq!(Shortcuts::load(&store).saved(), &before);
        shortcuts.save(&store, reassigned.clone()).unwrap();
        assert_eq!(Shortcuts::load(&fixture.store()).saved(), &reassigned);
    }

    #[test]
    fn unreadable_or_conflicting_saved_rows_keep_defaults_and_the_record() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let row = |action: &str, slot: &str, chord: serde_json::Value| serde_json::json!({ "action": action, "slot": slot, "chord": chord });
        let raw = serde_json::json!({ "version": 1, "overrides": [
            row("reload", "primary", "cmd+K".into()),
            row("reload", "primary", "cmd+J".into()),
            row("teleport", "primary", "cmd+J".into()),
            row("settings", "primary", "cmd+J".into()),
            row("print", "primary", "cmd+Q".into()),
            row("print", "alternate", "nonsense".into()),
            row("find", "middle", serde_json::Value::Null),
            row("copy-url", "primary", "cmd+W".into()),
            row("hard-reload", "alternate", "cmd+K".into()),
            row("zoom-out", "alternate", serde_json::Value::Null),
            serde_json::json!("not a row"),
        ]}).to_string();
        store.kv_set(KEY, &raw).unwrap();
        let shortcuts = Shortcuts::load(&store);
        assert!(shortcuts.notice.is_some());
        assert_eq!(shortcuts.binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::K, mods: CMD }), "the first row wins");
        assert_eq!(shortcuts.binding(Command::Print, Slot::Primary), Some(Chord { key: Key::P, mods: CMD }), "reserved chords never load");
        assert_eq!(shortcuts.binding(Command::CopyUrl, Slot::Primary), Some(Chord { key: Key::C, mods: CMD_SHIFT }), "a saved chord cannot steal a default");
        assert_eq!(shortcuts.binding(Command::CloseTab, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
        assert_eq!(shortcuts.binding(Command::HardReload, Slot::Alternate), None, "a duplicate of another saved chord is dropped");
        assert_eq!(shortcuts.binding(Command::Settings, Slot::Primary), Some(Chord { key: Key::Comma, mods: CMD }));
        assert_eq!(dispatch(&shortcuts, Key::Comma, CMD, false).0, vec![Command::Settings], "Settings stays reachable");
        assert_eq!(store.kv_get(KEY).unwrap().as_deref(), Some(raw.as_str()), "loading never rewrites the record");
        for (raw, newer) in [("not json", false), ("{\"version\":2,\"overrides\":[]}", true), ("{\"version\":1}", false), ("[]", false)] {
            store.kv_set(KEY, raw).unwrap();
            let shortcuts = Shortcuts::load(&store);
            assert_eq!(shortcuts.saved(), &Overrides::default(), "{raw}");
            assert_eq!(shortcuts.notice.as_deref().is_some_and(|notice| notice.contains("newer version")), newer, "{raw}");
            assert_eq!(store.kv_get(KEY).unwrap().as_deref(), Some(raw));
        }
    }

    #[test]
    fn priority_is_saved_on_this_device_and_a_failed_save_keeps_the_previous_one() {
        let fixture = Fixture::new();
        let mut lib = crossbow_core::library::Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let dirty = lib.store.dirty_count().unwrap();
        let mut shortcuts = Shortcuts::load(&lib.store);
        assert_eq!(shortcuts.priority(), Priority::Browser, "browser shortcuts win by default");
        shortcuts.save_priority(&lib.store, Priority::Websites).unwrap();
        set(&mut shortcuts, &lib.store, Command::Reload, Slot::Primary, Some(Chord { key: Key::K, mods: CMD }));
        let reopened = Shortcuts::load(&fixture.store());
        assert_eq!(reopened.priority(), Priority::Websites);
        assert_eq!(reopened.binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::K, mods: CMD }), "priority and bindings are saved apart");
        assert_eq!(lib.store.dirty_count().unwrap(), dirty, "no sync change is queued");
        let raw = lib.store.kv_get(PRIORITY_KEY).unwrap();
        assert!(shortcuts.save_priority(&fixture.read_only(), Priority::Browser).is_err());
        assert_eq!(shortcuts.priority(), Priority::Websites, "a failed save keeps the previous choice in use");
        assert_eq!(lib.store.kv_get(PRIORITY_KEY).unwrap(), raw);
        assert_eq!(Shortcuts::load(&fixture.store()).priority(), Priority::Websites);
        // Press twice is saved, read back and kept on a failed save the same way.
        shortcuts.save_priority(&lib.store, Priority::Twice).unwrap();
        assert_eq!(lib.store.kv_get(PRIORITY_KEY).unwrap().as_deref(), Some("press-twice"));
        assert_eq!(Shortcuts::load(&fixture.store()).priority(), Priority::Twice);
        assert!(shortcuts.save_priority(&fixture.read_only(), Priority::Websites).is_err());
        assert_eq!(shortcuts.priority(), Priority::Twice);
        assert_eq!(Shortcuts::load(&fixture.store()).priority(), Priority::Twice);
        assert_eq!(lib.store.dirty_count().unwrap(), dirty, "no sync change is queued");
        // An unreadable or newer value falls back to browser-first and is kept until the next save.
        lib.store.kv_set(PRIORITY_KEY, "per-shortcut").unwrap();
        let unreadable = Shortcuts::load(&lib.store);
        assert_eq!(unreadable.priority(), Priority::Browser);
        assert!(unreadable.priority_notice.is_some());
        assert_eq!(unreadable.binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::K, mods: CMD }));
        assert_eq!(lib.store.kv_get(PRIORITY_KEY).unwrap().as_deref(), Some("per-shortcut"));
    }

    /// A web page with keyboard focus. It consumes presses of `consumes` and,
    /// like CEF, reports every other tracked press as unhandled.
    #[derive(Default)]
    struct Page {
        /// One context per page, so egui marks a held key's presses as repeats.
        ctx: egui::Context,
        consumes: Vec<crossbow_engine::NamedKey>,
        /// (key, pressed, repeat) of every key event the page received.
        received: Vec<(crossbow_engine::NamedKey, bool, bool)>,
        unhandled: Vec<u64>,
        next_ticket: u64,
    }

    impl Page {
        fn pressed(&self) -> usize {
            self.received.iter().filter(|(_, pressed, _)| *pressed).count()
        }
    }

    /// One frame routed the way `CrossbowApp::handle_shortcuts` and
    /// `forward_keys` route it. Returns the actions run in this frame.
    fn route_frame(shortcuts: &Shortcuts, events: Vec<egui::Event>, editing_text: bool, page_first: bool, page: &mut Page, waiting: &mut Waiting) -> Vec<Command> {
        let ctx = page.ctx.clone();
        let mut run = Vec::new();
        ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
            let Routed { run: now, mut deferred, .. } = shortcuts.route(ui.ctx(), editing_text, page_first, &mut FirstPress::default());
            run = now;
            for input in crate::input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                let crate::input::WebInput::Key(key) = input else { continue };
                page.received.push((key.key, key.pressed, key.repeat));
                if let Some(press) = claim(&mut deferred, &key) {
                    page.next_ticket += 1;
                    waiting.wait(page.next_ticket, press.command);
                    if !page.consumes.contains(&key.key) { page.unhandled.push(page.next_ticket); }
                }
            }
            // A deferred press that never reached the page is the browser's.
            run.extend(deferred.into_iter().map(|deferred| deferred.command));
        }).textures_delta.clear();
        run
    }

    /// The page's unhandled reports arrive; `same_page` is whether it still has the keyboard.
    fn reports(page: &mut Page, waiting: &mut Waiting, same_page: bool) -> Vec<Command> {
        page.unhandled.drain(..).filter_map(|ticket| waiting.unhandled(ticket, same_page)).collect()
    }

    fn key_event(key: Key, modifiers: Modifiers, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed, repeat, modifiers }
    }

    fn tap(key: Key, modifiers: Modifiers) -> Vec<egui::Event> {
        vec![key_event(key, modifiers, true, false), key_event(key, modifiers, false, false)]
    }

    #[test]
    fn browser_first_runs_each_shortcut_once_and_the_page_never_receives_the_press() {
        let shortcuts = Shortcuts::defaults();
        for (key, modifiers, command) in inventory() {
            // Browser first, or websites first while Crossbow's own controls have the keyboard.
            let mut page = Page::default();
            let mut waiting = Waiting::default();
            assert_eq!(route_frame(&shortcuts, tap(key, modifiers), false, false, &mut page, &mut waiting), [command], "{key:?} {modifiers:?}");
            assert_eq!(page.pressed(), 0, "{key:?} {modifiers:?} never reaches the page");
            assert!(reports(&mut page, &mut waiting, true).is_empty(), "nothing waits for the page");
        }
    }

    #[test]
    fn websites_first_gives_the_page_each_shortcut_and_runs_it_only_when_unhandled() {
        let shortcuts = Shortcuts::defaults();
        let deferred: Vec<_> = inventory().into_iter().filter(|(.., command)| !command.protected()).collect();
        assert!(deferred.len() > 30, "most actions go to the page first");
        for (key, modifiers, command) in deferred {
            let mut page = Page::default();
            let mut waiting = Waiting::default();
            assert!(route_frame(&shortcuts, tap(key, modifiers), false, true, &mut page, &mut waiting).is_empty(), "{key:?} {modifiers:?} waits for the page");
            assert_eq!(page.pressed(), 1, "{key:?} {modifiers:?} reaches the page once");
            assert_eq!(reports(&mut page, &mut waiting, true), [command], "unhandled, it runs once");
            assert!(waiting.unhandled(page.next_ticket, true).is_none(), "a repeated report runs nothing");

            let mut page = Page { consumes: vec![page.received[0].0], ..Page::default() };
            assert!(route_frame(&shortcuts, tap(key, modifiers), false, true, &mut page, &mut waiting).is_empty());
            assert_eq!(page.pressed(), 1);
            assert!(reports(&mut page, &mut waiting, true).is_empty(), "{key:?} {modifiers:?}: a page that uses it keeps it");
        }
    }

    #[test]
    fn protected_shortcuts_stay_with_the_browser_in_both_modes() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        let protected: Vec<_> = inventory().into_iter().filter(|(.., command)| command.protected()).collect();
        let commands: Vec<Command> = protected.iter().map(|(.., command)| *command).collect();
        for command in [Command::NewTab, Command::EditUrl, Command::CloseTab, Command::Settings] {
            assert!(commands.contains(&command), "{command:?} is protected");
        }
        for page_first in [false, true] {
            for (key, modifiers, command) in &protected {
                let mut page = Page::default();
                let mut waiting = Waiting::default();
                assert_eq!(route_frame(&shortcuts, tap(*key, *modifiers), false, page_first, &mut page, &mut waiting), [*command]);
                assert_eq!(page.pressed(), 0, "{key:?} {modifiers:?} never reaches the page");
            }
            // Escape is left for the overlay chain, which sends it to the page only when nothing is open.
            let ctx = egui::Context::default();
            ctx.run_ui(press(Key::Escape, Modifiers::NONE), |ui| {
                assert_eq!(shortcuts.route(ui.ctx(), false, page_first, &mut FirstPress::default()), Routed::default());
                assert!(crate::app::take_shortcut(ui.ctx(), Modifiers::NONE, Key::Escape));
            }).textures_delta.clear();
        }
        // Protection follows the action: a rebound New Tab keeps its new chord, and the old one is the page's.
        set(&mut shortcuts, &store, Command::NewTab, Slot::Primary, Some(Chord { key: Key::K, mods: CMD }));
        let mut page = Page::default();
        let mut waiting = Waiting::default();
        assert_eq!(route_frame(&shortcuts, tap(Key::K, CMD), false, true, &mut page, &mut waiting), [Command::NewTab]);
        assert!(route_frame(&shortcuts, tap(Key::T, CMD), false, true, &mut page, &mut waiting).is_empty());
        assert_eq!(page.pressed(), 1, "⌘T is no shortcut now, so the page simply receives it");
        assert!(reports(&mut page, &mut waiting, true).is_empty());
        let listed = shortcuts.protected();
        assert!(listed.contains(&("New Tab", Chord { key: Key::K, mods: CMD }.label())), "{listed:?}");
        assert!(listed.iter().any(|(label, _)| *label == "Esc while a Crossbow overlay is open"));
        if MAC { assert!(listed.contains(&("Quit Crossbow", "⌘Q".to_string()))); }
    }

    #[test]
    fn key_up_and_auto_repeat_never_run_a_deferred_action() {
        let shortcuts = Shortcuts::defaults();
        let held = vec![
            key_event(Key::R, CMD, true, false),
            key_event(Key::R, CMD, true, true),
            key_event(Key::R, CMD, true, true),
            key_event(Key::R, CMD, false, false),
        ];
        let mut page = Page::default();
        let mut waiting = Waiting::default();
        assert!(route_frame(&shortcuts, held.clone(), false, true, &mut page, &mut waiting).is_empty());
        assert_eq!(page.received.len(), 4, "the page receives the press, its repeats and the release");
        assert_eq!(page.unhandled.len(), 1, "only the first press is tracked");
        assert_eq!(reports(&mut page, &mut waiting, true), [Command::Reload], "a held key runs the action once");
        // Held across frames: egui marks the later presses as repeats; they and the release run nothing.
        for event in held.clone() {
            assert!(route_frame(&shortcuts, vec![event], false, true, &mut page, &mut waiting).is_empty());
        }
        assert_eq!(page.received.iter().filter(|(_, pressed, repeat)| *pressed && *repeat).count(), 4);
        assert_eq!(reports(&mut page, &mut waiting, true), [Command::Reload]);
        assert!(route_frame(&shortcuts, vec![key_event(Key::R, CMD, false, false)], false, true, &mut page, &mut waiting).is_empty());
        assert!(reports(&mut page, &mut waiting, true).is_empty(), "a stray release runs nothing");
        // Browser first keeps today's behaviour: the release runs nothing and
        // each auto-repeat runs again, so holding ⌘= keeps zooming.
        let mut page = Page::default();
        for (event, expected) in held.into_iter().zip([1, 1, 1, 0]) {
            assert_eq!(route_frame(&shortcuts, vec![event], false, false, &mut page, &mut waiting).len(), expected);
        }
        assert_eq!(page.pressed(), 0);
    }

    #[test]
    fn a_focus_change_before_the_report_runs_nothing() {
        let shortcuts = Shortcuts::defaults();
        let mut page = Page::default();
        let mut waiting = Waiting::default();
        assert!(route_frame(&shortcuts, tap(Key::R, CMD), false, true, &mut page, &mut waiting).is_empty());
        let ticket = page.unhandled[0];
        // Another tab, Peek or Crossbow's own controls took the keyboard first.
        assert!(reports(&mut page, &mut waiting, false).is_empty());
        assert_eq!(waiting.unhandled(ticket, true), None, "a dropped report never runs later");
        // A press made after the switch runs normally.
        assert!(route_frame(&shortcuts, tap(Key::R, CMD), false, true, &mut page, &mut waiting).is_empty());
        assert_eq!(reports(&mut page, &mut waiting, true), [Command::Reload]);
    }

    #[test]
    fn caret_chords_stay_with_an_editable_page_and_unreachable_keys_run_at_once() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut shortcuts = Shortcuts::load(&store);
        let mut page = Page::default();
        let mut waiting = Waiting::default();
        // While a page field is focused ⌘← moves the caret, as in browser-first mode.
        assert!(route_frame(&shortcuts, tap(Key::ArrowLeft, CMD), true, true, &mut page, &mut waiting).is_empty());
        assert_eq!((page.pressed(), page.unhandled.len()), (1, 0), "the page receives it untracked");
        assert!(route_frame(&shortcuts, tap(Key::ArrowLeft, CMD_ALT), true, true, &mut page, &mut waiting).is_empty());
        assert_eq!(reports(&mut page, &mut waiting, true), [Command::PreviousSpace]);
        // A key no page can receive runs the browser action at once.
        set(&mut shortcuts, &store, Command::Reload, Slot::Alternate, Some(Chord { key: Key::F13, mods: NONE }));
        let mut page = Page::default();
        assert_eq!(route_frame(&shortcuts, tap(Key::F13, NONE), false, true, &mut page, &mut waiting), [Command::Reload]);
        assert_eq!(page.pressed(), 0);
    }

    #[test]
    fn overrides_are_device_local_and_never_enter_sync() {
        let fixture = Fixture::new();
        let mut lib = crossbow_core::library::Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let before = lib.store.dirty_count().unwrap();
        let mut shortcuts = Shortcuts::load(&lib.store);
        set(&mut shortcuts, &lib.store, Command::Reload, Slot::Primary, Some(Chord { key: Key::K, mods: CMD }));
        assert_eq!(lib.store.dirty_count().unwrap(), before, "no sync change is queued");
        assert!(lib.store.all_entities().unwrap().iter().all(|row| !row.kind.contains("shortcut")));
    }

    const PAGE: PageMark = PageMark { view: 1, navigations: 3 };
    const RELOAD: (Key, Modifiers) = (Key::R, CMD);

    /// Press-twice routing as `CrossbowApp` runs it, one frame at a time on an
    /// injected clock: `follow`, the page's reports from earlier frames,
    /// `route`, then delivery to the page with the keyboard.
    struct Session {
        shortcuts: Shortcuts,
        page: Page,
        waiting: Waiting,
        first: FirstPress,
        /// The page with the keyboard; None while Crossbow's own controls have it.
        mark: Option<PageMark>,
        /// The window has focus; false while another app is in front.
        focused: bool,
        /// The page answers presses only once this is false again.
        slow: bool,
    }

    impl Session {
        /// A page that uses ⌘R itself, so the chord conflicts.
        fn new(priority: Priority) -> Self {
            let shortcuts = Shortcuts { priority, ..Shortcuts::defaults() };
            let page = Page { consumes: vec![crate::input::web_key(Key::R, None).unwrap()], ..Page::default() };
            Session { shortcuts, page, waiting: Waiting::default(), first: FirstPress::default(), mark: Some(PAGE), focused: true, slow: false }
        }

        fn frame(&mut self, at: f64, events: Vec<egui::Event>) -> Vec<Command> {
            let input = egui::RawInput { time: Some(at), focused: self.focused, events, ..Default::default() };
            let mut run = Vec::new();
            self.page.ctx.clone().run_ui(input, |ui| {
                let ctx = ui.ctx();
                self.first.follow(ctx, self.shortcuts.priority() == Priority::Twice, self.mark);
                if !self.slow {
                    for ticket in std::mem::take(&mut self.page.unhandled) {
                        self.first.reported(ticket);
                        run.extend(self.waiting.unhandled(ticket, self.mark.is_some_and(|mark| mark.view == PAGE.view)));
                    }
                }
                let page_first = self.shortcuts.priority() != Priority::Browser && self.mark.is_some();
                let routed = self.shortcuts.route(ctx, false, page_first, &mut self.first);
                routed.withdrawn.iter().for_each(|ticket| self.waiting.withdraw(*ticket));
                run.extend(routed.run);
                let mut deferred = routed.deferred;
                if let Some(mark) = self.mark {
                    for input in crate::input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                        let crate::input::WebInput::Key(key) = input else { continue };
                        self.page.received.push((key.key, key.pressed, key.repeat));
                        if let Some(press) = claim(&mut deferred, &key) {
                            self.page.next_ticket += 1;
                            self.waiting.wait(self.page.next_ticket, press.command);
                            self.first.arm(&press, mark, self.page.next_ticket);
                            if !self.page.consumes.contains(&key.key) { self.page.unhandled.push(self.page.next_ticket); }
                        }
                    }
                }
                run.extend(deferred.into_iter().map(|deferred| deferred.command));
            }).textures_delta.clear();
            run
        }

        fn tap(&mut self, at: f64, (key, modifiers): (Key, Modifiers)) -> Vec<Command> {
            self.frame(at, tap(key, modifiers))
        }

        /// The page's presses, its auto-repeats included.
        fn presses(&self) -> usize {
            self.page.pressed()
        }
    }

    #[test]
    fn press_twice_gives_the_first_press_to_the_page_and_a_quick_second_press_to_crossbow() {
        // A single press is the page's, even once the window has passed.
        let mut s = Session::new(Priority::Twice);
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert!(s.frame(0.3, vec![]).is_empty() && s.frame(2.0, vec![]).is_empty());
        assert_eq!(s.presses(), 1);

        // A second press inside the window, up to and including SECOND_PRESS, is Crossbow's alone.
        for second in [0.15, 0.3, SECOND_PRESS] {
            let mut s = Session::new(Priority::Twice);
            assert!(s.tap(10.0, RELOAD).is_empty());
            assert_eq!(s.tap(10.0 + second, RELOAD), [Command::Reload], "{second} s after");
            assert_eq!(s.presses(), 1, "the second press never reaches the page");
            assert!(s.frame(10.0 + second + 0.1, vec![]).is_empty() && s.frame(15.0, vec![]).is_empty(), "nothing runs later");
        }

        // Just outside the window the press is a new first press.
        let mut s = Session::new(Priority::Twice);
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert!(s.tap(SECOND_PRESS + 0.001, RELOAD).is_empty());
        assert_eq!(s.presses(), 2);
        assert_eq!(s.tap(SECOND_PRESS + 0.3, RELOAD), [Command::Reload], "and its own second press is Crossbow's");

        // A triple press: page, Crossbow, then a new first press for the page.
        let mut s = Session::new(Priority::Twice);
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert_eq!(s.tap(0.2, RELOAD), [Command::Reload]);
        assert!(s.tap(0.4, RELOAD).is_empty(), "the third press starts again");
        assert_eq!(s.presses(), 2);
        assert_eq!(s.tap(0.6, RELOAD), [Command::Reload], "the fourth is a second press again");
    }

    #[test]
    fn another_key_a_focus_change_or_a_navigation_starts_again() {
        let find = (Key::F, CMD);
        // ⌘R, ⌘F, ⌘R: every press is a first press.
        let mut s = Session::new(Priority::Twice);
        s.page.consumes.push(crate::input::web_key(Key::F, None).unwrap());
        for (at, chord) in [(0.0, RELOAD), (0.1, find), (0.2, RELOAD)] {
            assert!(s.tap(at, chord).is_empty(), "{chord:?} at {at}");
        }
        assert_eq!(s.presses(), 3);
        // Typing in between does too.
        let mut s = Session::new(Priority::Twice);
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert!(s.frame(0.1, vec![key_event(Key::X, NONE, true, false), egui::Event::Text("x".into()), key_event(Key::X, NONE, false, false)]).is_empty());
        assert!(s.tap(0.2, RELOAD).is_empty());

        let switches: [(&str, fn(&mut Session)); 5] = [
            ("another pane or Peek", |s| s.mark = Some(PageMark { view: 2, ..PAGE })),
            ("Crossbow's sidebar, field or an overlay", |s| s.mark = None),
            ("another app", |s| s.focused = false),
            ("a navigation", |s| s.mark = Some(PageMark { navigations: PAGE.navigations + 1, ..PAGE })),
            ("a reload of the same address", |s| s.mark = Some(PageMark { navigations: PAGE.navigations + 2, ..PAGE })),
        ];
        for (what, switch) in switches {
            let mut s = Session::new(Priority::Twice);
            assert!(s.tap(0.0, RELOAD).is_empty());
            switch(&mut s);
            s.frame(0.1, vec![]);
            // Back on a page with the keyboard, the next press is a first press.
            s.focused = true;
            s.mark = s.mark.or(Some(PAGE));
            assert!(s.tap(0.2, RELOAD).is_empty(), "after {what}");
            assert_eq!(s.presses(), 2, "after {what}");
            assert_eq!(s.tap(0.3, RELOAD), [Command::Reload], "the sequence works again after {what}");
        }
    }

    #[test]
    fn key_ups_auto_repeat_and_modifiers_alone_never_count_as_presses() {
        // A key-up alone never arms, and stray key-ups never end a sequence.
        let mut s = Session::new(Priority::Twice);
        assert!(s.frame(0.0, vec![key_event(Key::R, CMD, false, false)]).is_empty());
        assert!(s.frame(0.1, vec![key_event(Key::R, CMD, true, false)]).is_empty(), "the first real press is a first press");
        assert!(s.frame(0.2, vec![key_event(Key::R, CMD, false, false)]).is_empty());
        assert!(s.frame(0.25, vec![key_event(Key::R, CMD, false, false), key_event(Key::K, CMD, false, false)]).is_empty());
        assert_eq!(s.tap(0.3, RELOAD), [Command::Reload]);

        // Auto-repeat of a held first press goes to the page and is no second press.
        let mut s = Session::new(Priority::Twice);
        assert!(s.frame(0.0, vec![key_event(Key::R, CMD, true, false)]).is_empty());
        for at in [0.1, 0.15, 0.2] {
            assert!(s.frame(at, vec![key_event(Key::R, CMD, true, true)]).is_empty());
        }
        assert!(s.frame(0.25, vec![key_event(Key::R, CMD, false, false)]).is_empty());
        assert_eq!(s.page.received.iter().filter(|(_, pressed, repeat)| *pressed && *repeat).count(), 3, "the page has its repeats");
        // A held second press is Crossbow's, repeats included, as in browser-first mode.
        assert_eq!(s.frame(0.3, vec![key_event(Key::R, CMD, true, false)]), [Command::Reload]);
        let received = s.page.received.len();
        for at in [0.35, 0.4] {
            assert_eq!(s.frame(at, vec![key_event(Key::R, CMD, true, true)]), [Command::Reload]);
        }
        assert_eq!(s.page.received.len(), received, "its repeats never reach the page");
        assert!(s.frame(0.45, vec![key_event(Key::R, CMD, false, false)]).is_empty());
        assert!(s.frame(0.5, vec![key_event(Key::R, CMD, true, true)]).is_empty(), "after its key-up, a stray repeat is the page's");
        assert!(s.tap(0.6, RELOAD).is_empty(), "and the next press is a first press");

        // egui reports a modifier going down or up as a key event of its own,
        // plus ModifiersChanged. Such presses neither arm nor end a sequence.
        let modifier = |key: Key, pressed: bool, modifiers: Modifiers| vec![key_event(key, modifiers, pressed, false), egui::Event::ModifiersChanged(modifiers)];
        let mut s = Session::new(Priority::Twice);
        for (at, events) in [(0.0, modifier(Key::SuperLeft, true, CMD)), (0.05, modifier(Key::ShiftLeft, true, CMD_SHIFT)), (0.1, modifier(Key::ShiftLeft, false, CMD)), (0.15, modifier(Key::SuperLeft, false, NONE))] {
            assert!(s.frame(at, events).is_empty());
        }
        assert!(s.tap(0.2, RELOAD).is_empty(), "a first press");
        // Letting go of ⌘ and pressing it again for the second ⌘R, as people do.
        for (at, events) in [(0.25, modifier(Key::SuperLeft, false, NONE)), (0.3, modifier(Key::SuperRight, true, CMD)), (0.32, modifier(Key::CapsLock, true, CMD)), (0.34, modifier(Key::AltLeft, true, CMD_ALT)), (0.36, modifier(Key::AltLeft, false, CMD))] {
            assert!(s.frame(at, events).is_empty());
        }
        assert_eq!(s.tap(0.4, RELOAD), [Command::Reload]);
        assert_eq!(s.presses(), 1, "the page had one ⌘R press");
    }

    #[test]
    fn press_twice_keeps_protected_shortcuts_with_crossbow_and_runs_an_unused_press_at_once() {
        // Protected chords run at every press and end a sequence.
        let mut s = Session::new(Priority::Twice);
        assert_eq!(s.tap(0.0, (Key::T, CMD)), [Command::NewTab]);
        assert_eq!(s.tap(0.1, (Key::T, CMD)), [Command::NewTab]);
        assert_eq!(s.presses(), 0, "the page never receives them");
        assert!(s.tap(0.2, RELOAD).is_empty());
        assert_eq!(s.tap(0.3, (Key::T, CMD)), [Command::NewTab]);
        assert!(s.tap(0.4, RELOAD).is_empty(), "⌘T in between starts again");

        // A first press the page leaves unhandled runs Crossbow's action at
        // once, as in websites-first mode, and the sequence ends there: each
        // press runs it once.
        let mut s = Session::new(Priority::Twice);
        s.page.consumes.clear();
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert_eq!(s.frame(0.016, vec![]), [Command::Reload], "the report runs it");
        assert!(s.tap(0.2, RELOAD).is_empty(), "a quick press after that is a first press");
        assert_eq!(s.frame(0.216, vec![]), [Command::Reload]);
        assert_eq!(s.presses(), 2);
        // If the second press comes before the page answers, it runs the action, and the late report runs nothing.
        let mut s = Session::new(Priority::Twice);
        s.page.consumes.clear();
        s.slow = true;
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert_eq!(s.tap(0.1, RELOAD), [Command::Reload]);
        s.slow = false;
        assert!(s.frame(0.2, vec![]).is_empty(), "one action for two presses");
    }

    #[test]
    fn switching_modes_reopening_or_websites_first_never_continue_a_sequence() {
        // A mode switch mid-sequence starts again, both ways.
        let mut s = Session::new(Priority::Twice);
        assert!(s.tap(0.0, RELOAD).is_empty());
        s.shortcuts.priority = Priority::Websites;
        assert!(s.tap(0.1, RELOAD).is_empty(), "websites first: the page has every press");
        s.shortcuts.priority = Priority::Twice;
        assert!(s.tap(0.2, RELOAD).is_empty(), "the press before the switch back does not count");
        assert_eq!(s.tap(0.3, RELOAD), [Command::Reload]);
        let mut s = Session::new(Priority::Websites);
        for at in [0.0, 0.1, 0.2] {
            assert!(s.tap(at, RELOAD).is_empty());
        }
        assert_eq!(s.presses(), 3, "websites first never takes a second press");

        // The sequence lives in memory only: reopening starts afresh and nothing is written.
        let fixture = Fixture::new();
        let mut lib = crossbow_core::library::Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let mut shortcuts = Shortcuts::load(&lib.store);
        shortcuts.save_priority(&lib.store, Priority::Twice).unwrap();
        let dirty = lib.store.dirty_count().unwrap();
        let mut s = Session { shortcuts, ..Session::new(Priority::Twice) };
        assert!(s.tap(0.0, RELOAD).is_empty());
        assert_eq!(lib.store.kv_get(PRIORITY_KEY).unwrap().as_deref(), Some("press-twice"));
        assert_eq!(lib.store.dirty_count().unwrap(), dirty);
        let page = std::mem::take(&mut s.page);
        let mut reopened = Session { shortcuts: Shortcuts::load(&fixture.store()), page, ..Session::new(Priority::Browser) };
        assert_eq!(reopened.shortcuts.priority(), Priority::Twice);
        assert!(reopened.tap(0.1, RELOAD).is_empty(), "the press before reopening does not count");
    }
}
