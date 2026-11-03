//! Settings → Advanced → Window & Feedback: dragging the window from the top
//! of pages, UI sound effects and haptic feedback when reordering tabs. Each
//! choice is kept on this device only. Sounds are short tones made here and
//! played through AppKit; nothing is bundled.

use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crossbow_core::library::{Library, PinnedNode};
use crossbow_core::store::Store;

/// A thin band at the top of each page moves the window. Off by default:
/// Arc's key, `contentWindowDraggingEnabled`, has no registered default, so
/// it reads as off.
#[derive(Debug)]
pub struct PageTopDrag;

impl SwitchSpec for PageTopDrag {
    const KEY: &'static str = "page_top_window_drag";
    const DEFAULT: bool = false;
    const TITLE: &'static str = "Allow window dragging from the top of webpages";
    const DESCRIPTION: &'static str = "Drag the top edge of a page to move the window, or double-click it to zoom. Not in full screen. This device only.";
    const STILL_OFF: &'static str = "The top of webpages still belongs to the page";
    const STILL_ON: &'static str = "The top of webpages still moves the window";
    const UNREADABLE: &'static str = "The saved choice for dragging from the top of webpages could not be read, so it is off. Changing it saves over it.";
}

/// Short sounds for a few finished actions. Off by default: our sources do
/// not show Arc's default, so nothing new plays until it is chosen.
#[derive(Debug)]
pub struct UiSounds;

impl SwitchSpec for UiSounds {
    const KEY: &'static str = "ui_sound_effects";
    const DEFAULT: bool = false;
    const TITLE: &'static str = "Play sound effects";
    const DESCRIPTION: &'static str = "A short sound when you switch Spaces, close a tab, copy a link or move a Little Window into a Space. Silent while “Play user interface sound effects” is off in System Settings → Sound. This device only.";
    const STILL_OFF: &'static str = "Crossbow still plays no sounds";
    const STILL_ON: &'static str = "Crossbow still plays sounds";
    const UNREADABLE: &'static str = "The saved choice for sound effects could not be read, so they are off. Changing it saves over it.";
}

/// A tap on a Force Touch trackpad when a drop reorders the sidebar. On by
/// default: Arc keeps `tabReorderingHapticsDisabled`, which reads as false.
#[derive(Debug)]
pub struct ReorderHaptics;

impl SwitchSpec for ReorderHaptics {
    const KEY: &'static str = "reorder_haptics";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Haptic feedback when reordering tabs";
    const DESCRIPTION: &'static str = "A Force Touch trackpad taps when a dragged tab, pinned tab, folder or favorite lands in a new place. This device only.";
    const STILL_OFF: &'static str = "Reordering still gives no feedback";
    const STILL_ON: &'static str = "Reordering still gives feedback";
    const UNREADABLE: &'static str = "The saved choice for haptic feedback could not be read, so it is on. Changing it saves over it.";
}

/// The three Window & Feedback choices.
#[derive(Debug)]
pub struct FeelPreferences {
    pub page_drag: DeviceSwitch<PageTopDrag>,
    pub sounds: DeviceSwitch<UiSounds>,
    pub haptics: DeviceSwitch<ReorderHaptics>,
}

impl FeelPreferences {
    pub fn load(store: &Store) -> Self {
        FeelPreferences { page_drag: DeviceSwitch::load(store), sounds: DeviceSwitch::load(store), haptics: DeviceSwitch::load(store) }
    }
}

/// A finished action with a sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cue {
    SpaceSwitched,
    TabClosed,
    LinkCopied,
    /// Open in Space moved a Little Window's page into a Space.
    MovedToSpace,
}

impl Cue {
    #[cfg(test)]
    pub const ALL: [Cue; 4] = [Cue::SpaceSwitched, Cue::TabClosed, Cue::LinkCopied, Cue::MovedToSpace];

    /// Each tone glides from one pitch to another, in Hz, over its length in ms.
    fn tones(self) -> &'static [(f32, f32, u32)] {
        match self {
            Cue::SpaceSwitched => &[(660.0, 880.0, 90)],
            Cue::TabClosed => &[(520.0, 330.0, 80)],
            Cue::LinkCopied => &[(1320.0, 1320.0, 45)],
            Cue::MovedToSpace => &[(587.0, 587.0, 60), (880.0, 880.0, 80)],
        }
    }

    /// The cue as a 16-bit mono WAV file: quiet sine tones that start and end
    /// at silence, so they never click.
    pub fn wav(self) -> Vec<u8> {
        let mut samples = Vec::new();
        for &(from, to, ms) in self.tones() {
            let count = (RATE * ms / 1000) as usize;
            let attack = (RATE / 250) as usize;
            let mut phase = 0.0f32;
            for i in 0..count {
                let t = i as f32 / count as f32;
                phase += std::f32::consts::TAU * (from + (to - from) * t) / RATE as f32;
                let envelope = (i as f32 / attack as f32).min(1.0) * (1.0 - t) * (1.0 - t);
                samples.push((phase.sin() * envelope * PEAK * f32::from(i16::MAX)) as i16);
            }
        }
        let data = (samples.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        // PCM, one channel, 16-bit samples.
        for (value, width) in [(16u32, 4), (1, 2), (1, 2), (RATE, 4), (RATE * 2, 4), (2, 2), (16, 2)] {
            wav.extend_from_slice(&value.to_le_bytes()[..width]);
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }
}

const RATE: u32 = 44_100;
/// Loudest sample, as a share of full scale.
const PEAK: f32 = 0.2;

/// Plays cues: AppKit in the app, a fake in tests.
pub trait SoundSystem {
    fn play(&mut self, cue: Cue) -> Result<(), String>;
    /// macOS's "Play user interface sound effects"; None when it can't be read.
    fn ui_sounds(&self) -> Option<bool>;
}

/// Taps the trackpad: AppKit in the app, a fake in tests.
pub trait HapticSystem {
    fn perform(&mut self) -> Result<(), String>;
}

/// Sounds and haptics for finished actions, as Settings chooses. Failures
/// are silent and logged once.
pub struct Feel {
    pub prefs: FeelPreferences,
    sounds: Box<dyn SoundSystem>,
    haptics: Box<dyn HapticSystem>,
    sound_logged: bool,
    haptic_logged: bool,
}

impl Feel {
    pub fn load(store: &Store, sounds: Box<dyn SoundSystem>, haptics: Box<dyn HapticSystem>) -> Self {
        Feel { prefs: FeelPreferences::load(store), sounds, haptics, sound_logged: false, haptic_logged: false }
    }

    /// Play `cue` once for an action that just happened. `held`: a key's
    /// auto-repeat ran it again, which plays nothing.
    pub fn play(&mut self, cue: Cue, held: bool) {
        if held || !self.prefs.sounds.enabled() { return; }
        match self.sounds.ui_sounds() {
            Some(false) => return,
            Some(true) => {}
            // macOS's own default is on.
            None => self.log_sound_once("Could not read macOS's user interface sound setting; playing Crossbow's sounds as chosen in Settings."),
        }
        if let Err(error) = self.sounds.play(cue) {
            self.log_sound_once(&format!("Could not play a sound effect: {error}"));
        }
    }

    fn log_sound_once(&mut self, message: &str) {
        if !std::mem::replace(&mut self.sound_logged, true) { log::warn!("{message}"); }
    }

    /// A sidebar drop finished: tap once if it moved something.
    pub fn dropped(&mut self, before: &SidebarOrder, after: &SidebarOrder) {
        if before == after || !self.prefs.haptics.enabled() { return; }
        if let Err(error) = self.haptics.perform() && !std::mem::replace(&mut self.haptic_logged, true) {
            log::warn!("Could not give haptic feedback: {error}");
        }
    }
}

/// The sidebar of one Space as a person sees it: favorites, the pinned tree
/// and Today, in order. Equal before and after a drop means nothing moved.
#[derive(Debug, PartialEq, Eq)]
pub struct SidebarOrder(Vec<String>);

impl SidebarOrder {
    pub fn of(lib: &Library, space: &str, profile: &str, today: impl IntoIterator<Item = String>) -> Self {
        fn walk(lib: &Library, space: &str, folder: Option<&str>, depth: usize, out: &mut Vec<String>) {
            for node in lib.pinned_children(space, folder) {
                out.push(format!("pinned {depth} {}", node.id()));
                if let PinnedNode::Folder(id, _) = node { walk(lib, space, Some(&id), depth + 1, out); }
            }
        }
        let mut out: Vec<String> = lib.favorites(profile).into_iter().map(|(id, _)| format!("favorite {id}")).collect();
        walk(lib, space, None, 0, &mut out);
        out.extend(today.into_iter().map(|id| format!("today {id}")));
        SidebarOrder(out)
    }
}

#[cfg(target_os = "macos")]
pub fn systems() -> (Box<dyn SoundSystem>, Box<dyn HapticSystem>) {
    (Box::new(appkit::Sounds::default()), Box::new(appkit::Haptics))
}

#[cfg(not(target_os = "macos"))]
pub fn systems() -> (Box<dyn SoundSystem>, Box<dyn HapticSystem>) {
    struct Unavailable;
    impl SoundSystem for Unavailable {
        fn play(&mut self, _cue: Cue) -> Result<(), String> { Err("sound effects need macOS".into()) }
        fn ui_sounds(&self) -> Option<bool> { None }
    }
    impl HapticSystem for Unavailable {
        fn perform(&mut self) -> Result<(), String> { Err("haptic feedback needs macOS".into()) }
    }
    (Box::new(Unavailable), Box::new(Unavailable))
}

#[cfg(target_os = "macos")]
mod appkit {
    use super::{Cue, HapticSystem, SoundSystem};
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSString;
    use std::collections::HashMap;

    /// NSHapticFeedbackPatternAlignment: the tap macOS gives when something
    /// snaps into place.
    const ALIGNMENT: isize = 1;
    /// NSHapticFeedbackPerformanceTimeNow.
    const NOW: usize = 1;

    /// One NSSound per cue, made on first use and kept, so a sound is not
    /// released while it plays.
    #[derive(Default)]
    pub struct Sounds(HashMap<Cue, Retained<AnyObject>>);

    impl Sounds {
        pub(super) fn sound(cue: Cue) -> Result<Retained<AnyObject>, String> {
            let wav = cue.wav();
            // SAFETY: NSData copies the bytes, which outlive the call.
            let data: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSData), dataWithBytes: wav.as_ptr().cast::<std::ffi::c_void>(), length: wav.len()] };
            let data = data.ok_or("no memory for the sound")?;
            // SAFETY: alloc, then init with an NSData, as NSSound documents.
            let sound: Allocated<AnyObject> = unsafe { msg_send![class!(NSSound), alloc] };
            let sound: Option<Retained<AnyObject>> = unsafe { msg_send![sound, initWithData: &*data] };
            sound.ok_or_else(|| "macOS could not read the sound".to_string())
        }
    }

    impl SoundSystem for Sounds {
        fn play(&mut self, cue: Cue) -> Result<(), String> {
            if !self.0.contains_key(&cue) {
                let sound = Sounds::sound(cue)?;
                self.0.insert(cue, sound);
            }
            let sound = &self.0[&cue];
            // A cue still playing starts again rather than being skipped.
            // SAFETY: an NSSound this module made.
            let _: bool = unsafe { msg_send![&**sound, stop] };
            let played: bool = unsafe { msg_send![&**sound, play] };
            if played { Ok(()) } else { Err("macOS did not play it".into()) }
        }

        /// com.apple.systemsound's com.apple.sound.uiaudio.enabled; missing
        /// means it was never turned off.
        fn ui_sounds(&self) -> Option<bool> {
            let (domain, key) = (NSString::from_str("com.apple.systemsound"), NSString::from_str("com.apple.sound.uiaudio.enabled"));
            // SAFETY: Foundation calls with NSString arguments; the value is
            // checked for boolValue before it is asked.
            unsafe {
                let defaults: Option<Retained<AnyObject>> = msg_send![class!(NSUserDefaults), standardUserDefaults];
                let settings: Option<Retained<AnyObject>> = msg_send![&*defaults?, persistentDomainForName: &*domain];
                let Some(settings) = settings else { return Some(true) };
                let value: Option<Retained<AnyObject>> = msg_send![&*settings, objectForKey: &*key];
                let Some(value) = value else { return Some(true) };
                let readable: bool = msg_send![&*value, respondsToSelector: sel!(boolValue)];
                if !readable { return None; }
                let on: bool = msg_send![&*value, boolValue];
                Some(on)
            }
        }
    }

    pub struct Haptics;

    impl HapticSystem for Haptics {
        /// Without a Force Touch trackpad, macOS does nothing.
        fn perform(&mut self) -> Result<(), String> {
            let manager = AnyClass::get(c"NSHapticFeedbackManager").ok_or("this macOS has no haptic feedback")?;
            // SAFETY: AppKit's documented class method and pattern values.
            let performer: Option<Retained<AnyObject>> = unsafe { msg_send![manager, defaultPerformer] };
            let performer = performer.ok_or("macOS has no haptic performer")?;
            let _: () = unsafe { msg_send![&*performer, performFeedbackPattern: ALIGNMENT, performanceTime: NOW] };
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// AppKit reads every cue, at its length, without playing one.
        #[test]
        fn appkit_reads_every_cue_and_has_a_haptic_performer() {
            for cue in Cue::ALL {
                let sound = Sounds::sound(cue).unwrap();
                let seconds: f64 = unsafe { msg_send![&*sound, duration] };
                let expected = cue.tones().iter().map(|(_, _, ms)| f64::from(*ms) / 1000.0).sum::<f64>();
                assert!((seconds - expected).abs() < 0.005, "{cue:?} lasts {seconds}s, not {expected}s");
            }
            let manager = AnyClass::get(c"NSHapticFeedbackManager").unwrap();
            let performer: Option<Retained<AnyObject>> = unsafe { msg_send![manager, defaultPerformer] };
            assert!(performer.is_some());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shortcuts::tests::Fixture;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    pub(crate) struct FakeState {
        pub played: Vec<Cue>,
        pub taps: usize,
        /// None: the system setting can't be read.
        pub ui_sounds: Option<bool>,
        pub fail: bool,
    }

    /// Records what would play or tap.
    #[derive(Clone)]
    pub(crate) struct Fake(pub Rc<RefCell<FakeState>>);

    impl Default for Fake {
        fn default() -> Self {
            Fake(Rc::new(RefCell::new(FakeState { ui_sounds: Some(true), ..Default::default() })))
        }
    }

    impl Fake {
        pub(crate) fn played(&self) -> Vec<Cue> {
            std::mem::take(&mut self.0.borrow_mut().played)
        }

        pub(crate) fn taps(&self) -> usize {
            std::mem::take(&mut self.0.borrow_mut().taps)
        }
    }

    impl SoundSystem for Fake {
        fn play(&mut self, cue: Cue) -> Result<(), String> {
            let mut state = self.0.borrow_mut();
            if state.fail { return Err("no output device".into()); }
            state.played.push(cue);
            Ok(())
        }

        fn ui_sounds(&self) -> Option<bool> {
            self.0.borrow().ui_sounds
        }
    }

    impl HapticSystem for Fake {
        fn perform(&mut self) -> Result<(), String> {
            let mut state = self.0.borrow_mut();
            if state.fail { return Err("no performer".into()); }
            state.taps += 1;
            Ok(())
        }
    }

    pub(crate) fn feel(store: &Store) -> (Feel, Fake) {
        let fake = Fake::default();
        (Feel::load(store, Box::new(fake.clone()), Box::new(fake.clone())), fake)
    }

    #[test]
    fn defaults_follow_arcs_keys_and_choices_survive_reopening() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let prefs = FeelPreferences::load(&store);
        assert_eq!((prefs.page_drag.enabled(), prefs.sounds.enabled(), prefs.haptics.enabled()), (false, false, true));
        let mut prefs = prefs;
        prefs.page_drag.choose(&store, true);
        prefs.sounds.choose(&store, true);
        prefs.haptics.choose(&store, false);
        let reopened = FeelPreferences::load(&fixture.store());
        assert_eq!((reopened.page_drag.enabled(), reopened.sounds.enabled(), reopened.haptics.enabled()), (true, true, false));

        // A failed save keeps each previous choice and offers the new one again.
        let read_only = fixture.read_only();
        let mut prefs = reopened;
        prefs.page_drag.choose(&read_only, false);
        prefs.sounds.choose(&read_only, false);
        prefs.haptics.choose(&read_only, true);
        assert_eq!((prefs.page_drag.enabled(), prefs.sounds.enabled(), prefs.haptics.enabled()), (true, true, false));
        assert_eq!([&prefs.page_drag.failure, &prefs.sounds.failure, &prefs.haptics.failure].map(|failure| failure.as_ref().map(|failure| failure.wanted)), [Some(false), Some(false), Some(true)]);
        let reopened = FeelPreferences::load(&fixture.store());
        assert_eq!((reopened.page_drag.enabled(), reopened.sounds.enabled(), reopened.haptics.enabled()), (true, true, false));

        // Unreadable values keep the defaults and say so.
        for key in [PageTopDrag::KEY, UiSounds::KEY, ReorderHaptics::KEY] { store.kv_set(key, "maybe").unwrap(); }
        let prefs = FeelPreferences::load(&store);
        assert_eq!((prefs.page_drag.enabled(), prefs.sounds.enabled(), prefs.haptics.enabled()), (false, false, true));
        assert_eq!([prefs.page_drag.notice, prefs.sounds.notice, prefs.haptics.notice].map(|notice| notice.unwrap()), [PageTopDrag::UNREADABLE, UiSounds::UNREADABLE, ReorderHaptics::UNREADABLE]);
    }

    #[test]
    fn each_cue_plays_once_per_action_and_never_while_held_off_or_muted_by_macos() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (mut feel, fake) = feel(&store);
        for cue in Cue::ALL { feel.play(cue, false); }
        assert!(fake.played().is_empty(), "off by default");

        feel.prefs.sounds.choose(&store, true);
        for cue in Cue::ALL { feel.play(cue, false); }
        assert_eq!(fake.played(), Cue::ALL, "one sound per action");
        // A held ⌘W closes tab after tab; only the first press plays.
        feel.play(Cue::TabClosed, false);
        for _ in 0..5 { feel.play(Cue::TabClosed, true); }
        assert_eq!(fake.played(), [Cue::TabClosed]);

        fake.0.borrow_mut().ui_sounds = Some(false);
        feel.play(Cue::LinkCopied, false);
        assert!(fake.played().is_empty(), "macOS's setting mutes them");
        fake.0.borrow_mut().ui_sounds = None;
        feel.play(Cue::LinkCopied, false);
        assert_eq!(fake.played(), [Cue::LinkCopied], "an unreadable macOS setting follows Settings alone");

        fake.0.borrow_mut().fail = true;
        feel.play(Cue::SpaceSwitched, false);
        feel.play(Cue::SpaceSwitched, false);
        assert!(fake.played().is_empty() && feel.sound_logged, "failures are silent and logged once");

        feel.prefs.sounds.choose(&store, false);
        fake.0.borrow_mut().fail = false;
        fake.0.borrow_mut().ui_sounds = Some(true);
        feel.play(Cue::SpaceSwitched, false);
        assert!(fake.played().is_empty(), "off plays nothing");
    }

    #[test]
    fn cues_are_short_quiet_distinct_wav_files_that_start_and_end_silent() {
        let mut seen = Vec::new();
        for cue in Cue::ALL {
            let wav = cue.wav();
            let u32_at = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
            let u16_at = |at: usize| u16::from_le_bytes(wav[at..at + 2].try_into().unwrap());
            assert_eq!((&wav[0..4], &wav[8..16], &wav[36..40]), (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..]));
            assert_eq!((u32_at(4) as usize, u32_at(40) as usize), (wav.len() - 8, wav.len() - 44));
            assert_eq!((u32_at(16), u16_at(20), u16_at(22), u32_at(24), u32_at(28), u16_at(32), u16_at(34)), (16, 1, 1, RATE, RATE * 2, 2, 16), "16-bit mono PCM");
            let samples: Vec<i16> = wav[44..].chunks(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
            let ms = samples.len() as u32 * 1000 / RATE;
            assert!((40..=150).contains(&ms), "{cue:?} lasts {ms} ms");
            let peak = samples.iter().map(|sample| sample.unsigned_abs()).max().unwrap();
            assert!(f32::from(peak) <= PEAK * f32::from(i16::MAX) + 1.0 && f32::from(peak) > PEAK * f32::from(i16::MAX) * 0.5, "{cue:?} peaks at {peak}");
            assert!(samples[0].unsigned_abs() < 200 && samples[samples.len() - 1].unsigned_abs() < 200, "{cue:?} starts and ends at silence");
            assert!(!seen.contains(&wav), "{cue:?} sounds like another cue");
            seen.push(wav);
        }
    }

    #[test]
    fn a_drop_taps_once_only_when_it_moved_something_and_failures_stay_silent() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (mut feel, fake) = feel(&store);
        let order = |ids: &[&str]| SidebarOrder(ids.iter().map(|id| id.to_string()).collect());
        feel.dropped(&order(&["a", "b"]), &order(&["a", "b"]));
        assert_eq!(fake.taps(), 0, "a drop back in place moves nothing");
        feel.dropped(&order(&["a", "b"]), &order(&["b", "a"]));
        assert_eq!(fake.taps(), 1);
        feel.prefs.haptics.choose(&store, false);
        feel.dropped(&order(&["a", "b"]), &order(&["b", "a"]));
        assert_eq!(fake.taps(), 0, "off gives nothing");
        feel.prefs.haptics.choose(&store, true);
        fake.0.borrow_mut().fail = true;
        feel.dropped(&order(&["a", "b"]), &order(&["b", "a"]));
        feel.dropped(&order(&["a", "b"]), &order(&["b", "a"]));
        assert!(fake.taps() == 0 && feel.haptic_logged, "unsupported hardware is a silent no-op, logged once");
    }
}
