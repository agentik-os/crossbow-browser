//! Settings → Links → Open a Little Window from anywhere: one system-wide
//! chord, off by default and kept on this device only. macOS takes it with
//! Carbon's RegisterEventHotKey, which needs no Accessibility permission and
//! sees only that chord. It is taken only while the switch is on and
//! Crossbow runs.

use crate::quit::SaveFailure;
use crate::shortcuts::{Chord, REGISTRY, Shortcuts, refusal};
use crossbow_core::store::Store;
use egui::{Key, Modifiers};

const KEY: &str = "little_window_hotkey";
pub const TITLE: &str = "Open a Little Window from anywhere";

/// ⌃⌥⌘N: New Little Window's ⌃⌘N plus ⌥. No browser shortcut and no
/// standard macOS shortcut uses it.
pub const DEFAULT_CHORD: Chord = Chord { key: Key::N, mods: Modifiers { alt: true, ctrl: true, shift: false, mac_cmd: false, command: true } };

/// Takes one chord from every app: Carbon in the app, a fake in tests.
pub trait HotkeySystem {
    /// Take `chord`, replacing any chord taken before. Err says why not.
    fn register(&mut self, chord: Chord) -> Result<(), String>;
    fn unregister(&mut self);
    /// Whether the chord was pressed since the last call.
    fn take_pressed(&mut self) -> bool;
}

/// The switch and the chord, saved together in one write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub on: bool,
    pub chord: Option<Chord>,
}

impl Choice {
    const DEFAULT: Choice = Choice { on: false, chord: Some(DEFAULT_CHORD) };

    fn encode(self) -> String {
        format!("{} {}", if self.on { "on" } else { "off" }, self.chord.map_or("none".to_string(), Chord::encode))
    }

    fn decode(raw: &str) -> Option<Choice> {
        let (state, chord) = raw.split_once(' ')?;
        let on = match state {
            "on" => true,
            "off" => false,
            _ => return None,
        };
        let chord = if chord == "none" { None } else { Some(Chord::decode(chord)?) };
        Some(Choice { on: on && chord.is_some(), chord })
    }
}

pub struct GlobalHotkey {
    system: Box<dyn HotkeySystem>,
    saved: Choice,
    /// The chord taken from the system now.
    registered: Option<Chord>,
    /// Why the hotkey is off or kept its chord; cleared by the next change.
    pub notice: Option<String>,
    pub failure: Option<SaveFailure<Choice>>,
}

impl GlobalHotkey {
    /// Read this device's choice and take its chord if it is on. If the
    /// system refuses, the switch shows off and says why; the saved choice
    /// is kept for the next launch.
    pub fn load(store: &Store, system: Box<dyn HotkeySystem>) -> Self {
        const UNREADABLE: &str = "The saved shortcut for opening a Little Window from anywhere could not be read, so it is off. Changing it saves over it.";
        let (saved, notice) = match store.kv_get(KEY) {
            Ok(None) => (Choice::DEFAULT, None),
            Ok(Some(raw)) => match Choice::decode(&raw) {
                Some(choice) => (choice, None),
                None => (Choice::DEFAULT, Some(UNREADABLE.to_string())),
            },
            Err(_) => (Choice::DEFAULT, Some(UNREADABLE.to_string())),
        };
        let mut hotkey = GlobalHotkey { system, saved, registered: None, notice, failure: None };
        if saved.on && let Some(chord) = saved.chord && let Err(reason) = hotkey.take(chord) {
            hotkey.notice = Some(format!("{} can't be used: {reason}. The shortcut stays off.", chord.label()));
        }
        hotkey
    }

    /// On only while the chord is taken.
    pub fn enabled(&self) -> bool {
        self.registered.is_some()
    }

    /// The chord taken from every app now.
    pub fn taken(&self) -> Option<Chord> {
        self.registered
    }

    pub fn chord(&self) -> Option<Chord> {
        self.saved.chord
    }

    pub fn set_enabled(&mut self, store: &Store, on: bool) {
        if on && self.saved.chord.is_none() {
            self.notice = Some("Record a shortcut first.".into());
            return;
        }
        self.apply(store, Choice { on, chord: self.saved.chord });
    }

    /// Why `chord` can't open a Little Window from anywhere: the system or
    /// Crossbow's shell owns it, or a browser shortcut uses it.
    pub fn check(chord: Chord, shortcuts: &Shortcuts) -> Result<(), String> {
        if let Some(reason) = refusal(chord) {
            return Err(reason);
        }
        if let Some((index, _)) = shortcuts.saved().owners(chord).first() {
            return Err(format!("{} is the shortcut for {}. Choose another, or change that one in Settings → Shortcuts first.", chord.label(), REGISTRY[*index].label));
        }
        Ok(())
    }

    /// Record a new chord, or clear it with None, which turns the hotkey off.
    /// A refused chord changes nothing.
    pub fn set_chord(&mut self, store: &Store, shortcuts: &Shortcuts, chord: Option<Chord>) -> Result<(), String> {
        if let Some(chord) = chord {
            Self::check(chord, shortcuts)?;
        }
        self.apply(store, Choice { on: self.enabled() && chord.is_some(), chord });
        Ok(())
    }

    /// Save again what failed to save.
    pub fn retry(&mut self, store: &Store) {
        if let Some(failure) = self.failure.take() {
            self.apply(store, failure.wanted);
        }
    }

    /// Take the chord `choice` asks for, then save it. If the system refuses
    /// the chord or the save fails, the chord taken before stays in use.
    fn apply(&mut self, store: &Store, choice: Choice) {
        let previous = self.registered;
        let next = if choice.on { choice.chord } else { None };
        if next != previous {
            self.give_back();
            if let Some(chord) = next && let Err(reason) = self.take(chord) {
                self.restore(previous);
                let kept = previous.map_or("The shortcut stays off.".to_string(), |chord| format!("{} still opens a Little Window.", chord.label()));
                self.notice = Some(format!("{} can't be used: {reason}. {kept}", chord.label()));
                return;
            }
        }
        match store.kv_set(KEY, &choice.encode()) {
            Ok(()) => {
                self.saved = choice;
                self.notice = None;
                self.failure = None;
            }
            Err(error) => {
                if next != previous {
                    self.give_back();
                    self.restore(previous);
                }
                self.failure = Some(SaveFailure { wanted: choice, error: error.to_string(), reveal: true });
            }
        }
    }

    fn take(&mut self, chord: Chord) -> Result<(), String> {
        self.system.register(chord)?;
        self.registered = Some(chord);
        Ok(())
    }

    fn give_back(&mut self) {
        if self.registered.take().is_some() { self.system.unregister(); }
    }

    /// Take back the chord in use before a change that did not happen.
    fn restore(&mut self, previous: Option<Chord>) {
        if let Some(chord) = previous && let Err(reason) = self.take(chord) {
            self.notice = Some(format!("{} can't be used: {reason}. The shortcut stays off.", chord.label()));
        }
    }

    /// Whether the hotkey was pressed since the last call.
    pub fn take_pressed(&mut self) -> bool {
        let pressed = self.system.take_pressed();
        pressed && self.registered.is_some()
    }

    /// Quitting: give the chord back to the system.
    pub fn release(&mut self) {
        self.give_back();
    }
}

/// The system's hotkeys: Carbon on macOS.
#[cfg(target_os = "macos")]
pub fn system(ctx: &egui::Context) -> Box<dyn HotkeySystem> {
    Box::new(carbon::Carbon::new(ctx))
}

#[cfg(not(target_os = "macos"))]
pub fn system(_ctx: &egui::Context) -> Box<dyn HotkeySystem> {
    struct Unavailable;
    impl HotkeySystem for Unavailable {
        fn register(&mut self, _chord: Chord) -> Result<(), String> {
            Err("shortcuts that work in every app are only available on macOS".into())
        }
        fn unregister(&mut self) {}
        fn take_pressed(&mut self) -> bool {
            false
        }
    }
    Box::new(Unavailable)
}

#[cfg(target_os = "macos")]
mod carbon {
    use super::{Chord, HotkeySystem};
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};

    type OsStatus = i32;
    type Handle = *mut c_void;
    type Handler = extern "C" fn(Handle, Handle, *mut c_void) -> OsStatus;

    #[repr(C)]
    struct EventTypeSpec {
        class: u32,
        kind: u32,
    }

    #[repr(C)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    const KEYBOARD_EVENTS: u32 = u32::from_be_bytes(*b"keyb");
    const HOT_KEY_PRESSED: u32 = 5;
    const SIGNATURE: u32 = u32::from_be_bytes(*b"CRSB");
    /// Fail when another app already has the chord.
    const EXCLUSIVE: u32 = 1;
    const HOT_KEY_EXISTS: OsStatus = -9878;
    const COMMAND: u32 = 1 << 8;
    const SHIFT: u32 = 1 << 9;
    const OPTION: u32 = 1 << 11;
    const CONTROL: u32 = 1 << 12;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> Handle;
        fn InstallEventHandler(target: Handle, handler: Handler, count: usize, list: *const EventTypeSpec, data: *mut c_void, out: *mut Handle) -> OsStatus;
        fn RegisterEventHotKey(code: u32, modifiers: u32, id: EventHotKeyId, target: Handle, options: u32, out: *mut Handle) -> OsStatus;
        fn UnregisterEventHotKey(hotkey: Handle) -> OsStatus;
    }

    static PRESSED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static WAKE: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    }

    /// Carbon calls this on the main thread for each press.
    extern "C" fn pressed(_call: Handle, _event: Handle, _data: *mut c_void) -> OsStatus {
        PRESSED.store(true, Ordering::Release);
        if let Some(ctx) = WAKE.with(|wake| wake.borrow().clone()) {
            // AppKit may call this while egui holds its context lock.
            let _ = std::thread::Builder::new().name("crossbow-hotkey-wake".into()).spawn(move || ctx.request_repaint_of(egui::ViewportId::ROOT));
        }
        0
    }

    pub(super) struct Carbon {
        handler: Option<Handle>,
        hotkey: Option<Handle>,
    }

    impl Carbon {
        pub(super) fn new(ctx: &egui::Context) -> Self {
            WAKE.with(|wake| *wake.borrow_mut() = Some(ctx.clone()));
            Carbon { handler: None, hotkey: None }
        }

        fn install(&mut self) -> Result<(), String> {
            if self.handler.is_some() { return Ok(()); }
            let spec = EventTypeSpec { class: KEYBOARD_EVENTS, kind: HOT_KEY_PRESSED };
            let mut handler = std::ptr::null_mut();
            // SAFETY: Carbon on the main thread; `pressed` has the handler's
            // signature and lives for the whole program.
            let status = unsafe { InstallEventHandler(GetApplicationEventTarget(), pressed, 1, &spec, std::ptr::null_mut(), &mut handler) };
            if status != 0 {
                return Err(format!("macOS refused to report it (error {status})"));
            }
            self.handler = Some(handler);
            Ok(())
        }
    }

    impl HotkeySystem for Carbon {
        fn register(&mut self, chord: Chord) -> Result<(), String> {
            self.unregister();
            let code = crate::input::named_key(chord.key).map(|key| key.mac_code()).ok_or("its key has no key code")?;
            self.install()?;
            let modifiers = [(chord.mods.command, COMMAND), (chord.mods.shift, SHIFT), (chord.mods.alt, OPTION), (chord.mods.ctrl, CONTROL)]
                .into_iter().filter(|(on, _)| *on).fold(0, |flags, (_, flag)| flags | flag);
            let mut hotkey = std::ptr::null_mut();
            // SAFETY: Carbon on the main thread with a valid out pointer.
            let status = unsafe { RegisterEventHotKey(code as u32, modifiers, EventHotKeyId { signature: SIGNATURE, id: 1 }, GetApplicationEventTarget(), EXCLUSIVE, &mut hotkey) };
            match status {
                0 => {
                    self.hotkey = Some(hotkey);
                    Ok(())
                }
                HOT_KEY_EXISTS => Err("another app already uses it".into()),
                status => Err(format!("macOS refused it (error {status})")),
            }
        }

        fn unregister(&mut self) {
            if let Some(hotkey) = self.hotkey.take() {
                // SAFETY: a handle RegisterEventHotKey returned, released once.
                let status = unsafe { UnregisterEventHotKey(hotkey) };
                if status != 0 { log::warn!("Could not release the Little Window shortcut (error {status})"); }
            }
        }

        fn take_pressed(&mut self) -> bool {
            PRESSED.swap(false, Ordering::AcqRel)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_press_is_reported_once() {
            let mut carbon = Carbon { handler: None, hotkey: None };
            carbon.take_pressed();
            assert!(!carbon.take_pressed());
            assert_eq!(pressed(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), 0);
            assert!(carbon.take_pressed(), "the press is reported");
            assert!(!carbon.take_pressed(), "once");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shortcuts::tests::Fixture;
    use crate::shortcuts::{Slot, index_of, Command};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The system's hotkeys as a log, with chords another app owns.
    #[derive(Clone, Default)]
    pub(crate) struct Fake(pub(crate) Rc<RefCell<FakeState>>);

    #[derive(Default)]
    pub(crate) struct FakeState {
        pub(crate) taken: Option<Chord>,
        pub(crate) owned_elsewhere: Vec<Chord>,
        pub(crate) log: Vec<String>,
        pub(crate) pressed: bool,
    }

    impl Fake {
        pub(crate) fn taken(&self) -> Option<Chord> {
            self.0.borrow().taken
        }

        pub(crate) fn log(&self) -> Vec<String> {
            std::mem::take(&mut self.0.borrow_mut().log)
        }

        pub(crate) fn press(&self) {
            self.0.borrow_mut().pressed = true;
        }

        pub(crate) fn owned_elsewhere(&self, chord: Chord) {
            self.0.borrow_mut().owned_elsewhere.push(chord);
        }
    }

    impl HotkeySystem for Fake {
        fn register(&mut self, chord: Chord) -> Result<(), String> {
            let mut state = self.0.borrow_mut();
            assert!(state.taken.is_none(), "one chord at a time");
            state.log.push(format!("register {}", chord.label()));
            if state.owned_elsewhere.contains(&chord) {
                return Err("another app already uses it".into());
            }
            state.taken = Some(chord);
            Ok(())
        }

        fn unregister(&mut self) {
            let mut state = self.0.borrow_mut();
            if let Some(chord) = state.taken.take() { state.log.push(format!("unregister {}", chord.label())); }
        }

        fn take_pressed(&mut self) -> bool {
            std::mem::take(&mut self.0.borrow_mut().pressed)
        }
    }

    pub(crate) fn chord(key: Key, command: bool, shift: bool, alt: bool, ctrl: bool) -> Chord {
        Chord { key, mods: Modifiers { alt, ctrl, shift, mac_cmd: false, command } }
    }

    fn load(store: &Store) -> (GlobalHotkey, Fake) {
        let fake = Fake::default();
        (GlobalHotkey::load(store, Box::new(fake.clone())), fake)
    }

    #[test]
    fn the_default_chord_is_free_in_the_registry_and_in_macos() {
        assert_eq!(DEFAULT_CHORD.label(), "⌃⌥⌘N");
        assert_eq!(GlobalHotkey::check(DEFAULT_CHORD, &Shortcuts::defaults()), Ok(()));
        assert_eq!(Choice::decode(&Choice::DEFAULT.encode()), Some(Choice::DEFAULT));
    }

    #[test]
    fn it_is_off_by_default_and_on_takes_the_chord_until_turned_off_or_quit() {
        let fixture = Fixture::new();
        let (mut hotkey, fake) = load(&fixture.store());
        assert!(!hotkey.enabled() && hotkey.chord() == Some(DEFAULT_CHORD) && fake.log().is_empty(), "off by default; nothing is taken");
        fake.press();
        assert!(!hotkey.take_pressed(), "an off hotkey opens nothing");

        hotkey.set_enabled(&fixture.store(), true);
        assert!(hotkey.enabled() && hotkey.notice.is_none() && hotkey.failure.is_none());
        assert_eq!((fake.taken(), fake.log()), (Some(DEFAULT_CHORD), vec!["register ⌃⌥⌘N".to_string()]));
        fake.press();
        assert!(hotkey.take_pressed() && !hotkey.take_pressed(), "each press opens one window");

        // Quit, then launch again: the chord is taken at launch.
        hotkey.release();
        assert_eq!((fake.taken(), fake.log()), (None, vec!["unregister ⌃⌥⌘N".to_string()]));
        let (mut hotkey, fake) = load(&fixture.store());
        assert!(hotkey.enabled(), "read back after reopening");
        assert_eq!(fake.taken(), Some(DEFAULT_CHORD));

        hotkey.set_enabled(&fixture.store(), false);
        assert!(!hotkey.enabled() && fake.taken().is_none(), "off gives the chord back");
        let (hotkey, fake) = load(&fixture.store());
        assert!(!hotkey.enabled() && fake.taken().is_none() && hotkey.chord() == Some(DEFAULT_CHORD), "off survives reopening");
    }

    #[test]
    fn changing_the_chord_moves_it_and_clearing_turns_the_hotkey_off() {
        let fixture = Fixture::new();
        let shortcuts = Shortcuts::defaults();
        let (mut hotkey, fake) = load(&fixture.store());
        let other = chord(Key::M, true, false, true, true);
        // Off: the chord is saved, nothing is taken.
        assert_eq!(hotkey.set_chord(&fixture.store(), &shortcuts, Some(other)), Ok(()));
        assert!(!hotkey.enabled() && hotkey.chord() == Some(other) && fake.log().is_empty());
        hotkey.set_enabled(&fixture.store(), true);
        assert_eq!(fake.taken(), Some(other));
        fake.log();

        assert_eq!(hotkey.set_chord(&fixture.store(), &shortcuts, Some(DEFAULT_CHORD)), Ok(()));
        assert!(hotkey.enabled());
        assert_eq!(fake.log(), ["unregister ⌃⌥⌘M", "register ⌃⌥⌘N"], "the old chord is released first");
        let (reopened, _) = load(&fixture.store());
        assert!(reopened.enabled() && reopened.chord() == Some(DEFAULT_CHORD), "the new chord is read back");
        drop(reopened);

        assert_eq!(hotkey.set_chord(&fixture.store(), &shortcuts, None), Ok(()));
        assert!(!hotkey.enabled() && hotkey.chord().is_none() && fake.taken().is_none(), "clearing gives the chord back and turns it off");
        let (mut reopened, reopened_fake) = load(&fixture.store());
        assert!(!reopened.enabled() && reopened.chord().is_none() && reopened_fake.taken().is_none());
        reopened.set_enabled(&fixture.store(), true);
        assert!(!reopened.enabled() && reopened.notice.as_deref() == Some("Record a shortcut first."), "no chord, nothing to turn on");
    }

    #[test]
    fn chords_the_system_shell_or_a_browser_shortcut_use_are_refused_and_change_nothing() {
        let fixture = Fixture::new();
        let mut shortcuts = Shortcuts::defaults();
        let (mut hotkey, fake) = load(&fixture.store());
        hotkey.set_enabled(&fixture.store(), true);
        fake.log();
        let new_tab = chord(Key::T, true, false, false, false);
        assert_eq!(hotkey.set_chord(&fixture.store(), &shortcuts, Some(new_tab)), Err("⌘T is the shortcut for New Tab. Choose another, or change that one in Settings → Shortcuts first.".into()));
        assert!(hotkey.set_chord(&fixture.store(), &shortcuts, Some(chord(Key::Q, true, false, false, false))).unwrap_err().contains("Crossbow quits with it"));
        assert!(hotkey.set_chord(&fixture.store(), &shortcuts, Some(chord(Key::N, false, false, true, false))).unwrap_err().contains("needs ⌘ or ⌃"));
        // A chord set on the Shortcuts page counts too.
        let custom = chord(Key::M, true, false, true, true);
        let mut candidate = shortcuts.saved().clone();
        candidate.set(index_of(Command::ShowLittleWindows), Slot::Primary, Some(custom));
        shortcuts.save(&fixture.store(), candidate).unwrap();
        assert!(hotkey.set_chord(&fixture.store(), &shortcuts, Some(custom)).unwrap_err().starts_with("⌃⌥⌘M is the shortcut for Show All Little Windows."));
        assert!(fake.log().is_empty() && hotkey.enabled() && hotkey.chord() == Some(DEFAULT_CHORD), "a refused chord changes nothing");
        assert_eq!(load(&fixture.store()).0.chord(), Some(DEFAULT_CHORD));
    }

    #[test]
    fn a_chord_another_app_owns_keeps_the_switch_off_or_the_previous_chord() {
        let fixture = Fixture::new();
        let shortcuts = Shortcuts::defaults();
        let (mut hotkey, fake) = load(&fixture.store());
        fake.owned_elsewhere(DEFAULT_CHORD);
        hotkey.set_enabled(&fixture.store(), true);
        assert!(!hotkey.enabled() && fake.taken().is_none());
        assert_eq!(hotkey.notice.as_deref(), Some("⌃⌥⌘N can't be used: another app already uses it. The shortcut stays off."));
        assert!(!load(&fixture.store()).0.enabled(), "nothing was saved");

        let free = chord(Key::L, true, false, true, true);
        hotkey.set_chord(&fixture.store(), &shortcuts, Some(free)).unwrap();
        hotkey.set_enabled(&fixture.store(), true);
        assert!(hotkey.enabled() && hotkey.notice.is_none());
        fake.log();
        hotkey.set_chord(&fixture.store(), &shortcuts, Some(DEFAULT_CHORD)).unwrap();
        assert_eq!(fake.log(), ["unregister ⌃⌥⌘L", "register ⌃⌥⌘N", "register ⌃⌥⌘L"], "the previous chord is taken again");
        assert!(hotkey.enabled() && hotkey.chord() == Some(free) && fake.taken() == Some(free));
        assert_eq!(hotkey.notice.as_deref(), Some("⌃⌥⌘N can't be used: another app already uses it. ⌃⌥⌘L still opens a Little Window."));

        // At launch: off, with the reason, and the saved choice kept for the next launch.
        fixture.store().kv_set(KEY, &Choice { on: true, chord: Some(DEFAULT_CHORD) }.encode()).unwrap();
        let fake = Fake::default();
        fake.owned_elsewhere(DEFAULT_CHORD);
        let hotkey = GlobalHotkey::load(&fixture.store(), Box::new(fake.clone()));
        assert!(!hotkey.enabled() && fake.taken().is_none());
        assert_eq!(hotkey.notice.as_deref(), Some("⌃⌥⌘N can't be used: another app already uses it. The shortcut stays off."));
        assert_eq!(fixture.store().kv_get(KEY).unwrap().as_deref(), Some("on cmd+ctrl+alt+N"));
    }

    #[test]
    fn a_failed_save_keeps_the_chord_in_use_and_retry_saves_it() {
        let fixture = Fixture::new();
        let (mut hotkey, fake) = load(&fixture.store());
        hotkey.set_enabled(&fixture.read_only(), true);
        assert!(!hotkey.enabled() && fake.taken().is_none(), "the chord is given back when the save fails");
        assert_eq!(fake.log(), ["register ⌃⌥⌘N", "unregister ⌃⌥⌘N"]);
        assert_eq!(hotkey.failure.as_ref().map(|failure| failure.wanted), Some(Choice { on: true, chord: Some(DEFAULT_CHORD) }));
        hotkey.retry(&fixture.read_only());
        assert!(hotkey.failure.is_some() && !hotkey.enabled(), "still failing");
        hotkey.retry(&fixture.store());
        assert!(hotkey.failure.is_none() && hotkey.enabled() && load(&fixture.store()).0.enabled(), "Retry saves it");
        fake.log();

        // Turning off without a save keeps it on.
        hotkey.set_enabled(&fixture.read_only(), false);
        assert!(hotkey.enabled() && fake.taken() == Some(DEFAULT_CHORD) && hotkey.failure.is_some());
        assert_eq!(fake.log(), ["unregister ⌃⌥⌘N", "register ⌃⌥⌘N"]);
        hotkey.failure = None;
        assert!(hotkey.enabled(), "Cancel leaves it on");

        // An unreadable choice is explained, and the next save replaces it.
        fixture.store().kv_set(KEY, "sometimes").unwrap();
        let (mut hotkey, fake) = load(&fixture.store());
        assert!(!hotkey.enabled() && fake.taken().is_none() && hotkey.notice.is_some());
        hotkey.set_enabled(&fixture.store(), true);
        assert!(hotkey.enabled() && hotkey.notice.is_none() && load(&fixture.store()).0.enabled());
    }
}
