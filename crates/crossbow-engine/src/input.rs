//! Keyboard and mouse translation into CEF events.

/// Modifier state, independent of the UI toolkit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// ⌘ on macOS, the Windows/Super key elsewhere.
    pub command: bool,
}

// cef_event_flags_t bits.
const FLAG_SHIFT: u32 = 1 << 1;
const FLAG_CONTROL: u32 = 1 << 2;
const FLAG_ALT: u32 = 1 << 3;
pub(crate) const FLAG_LEFT_MOUSE: u32 = 1 << 4;
pub(crate) const FLAG_MIDDLE_MOUSE: u32 = 1 << 5;
pub(crate) const FLAG_RIGHT_MOUSE: u32 = 1 << 6;
const FLAG_COMMAND: u32 = 1 << 7;
const FLAG_IS_REPEAT: u32 = 1 << 13;

impl Modifiers {
    pub(crate) fn cef_flags(self) -> u32 {
        let mut f = 0;
        if self.shift {
            f |= FLAG_SHIFT;
        }
        if self.ctrl {
            f |= FLAG_CONTROL;
        }
        if self.alt {
            f |= FLAG_ALT;
        }
        if self.command {
            f |= FLAG_COMMAND;
        }
        f
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Keys the browser cares about. Letters are `'A'..='Z'`, digits `'0'..='9'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Letter(char),
    Digit(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Space,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    F(u8),
    Minus,
    Equals,
    OpenBracket,
    CloseBracket,
    Backslash,
    Semicolon,
    Quote,
    Backtick,
    Comma,
    Period,
    Slash,
}

/// One key press or release, with the text it produced (if any).
#[derive(Clone, Debug)]
pub struct KeyInput {
    pub key: NamedKey,
    pub pressed: bool,
    pub repeat: bool,
    pub modifiers: Modifiers,
    /// Character produced by this press (already shifted / layout-mapped).
    pub text: Option<char>,
}

impl NamedKey {
    /// Windows virtual-key code (what Chromium calls `windows_key_code`).
    pub(crate) fn vk(self) -> i32 {
        use NamedKey::*;
        match self {
            Letter(c) | Digit(c) => c.to_ascii_uppercase() as i32,
            Enter => 0x0D,
            Tab => 0x09,
            Backspace => 0x08,
            Escape => 0x1B,
            Space => 0x20,
            Delete => 0x2E,
            Insert => 0x2D,
            Home => 0x24,
            End => 0x23,
            PageUp => 0x21,
            PageDown => 0x22,
            ArrowLeft => 0x25,
            ArrowUp => 0x26,
            ArrowRight => 0x27,
            ArrowDown => 0x28,
            F(n) => 0x6F + n as i32,
            Minus => 0xBD,
            Equals => 0xBB,
            OpenBracket => 0xDB,
            CloseBracket => 0xDD,
            Backslash => 0xDC,
            Semicolon => 0xBA,
            Quote => 0xDE,
            Backtick => 0xC0,
            Comma => 0xBC,
            Period => 0xBE,
            Slash => 0xBF,
        }
    }

    /// macOS virtual key code (`kVK_*`), used by CEF to rebuild the NSEvent.
    pub fn mac_code(self) -> i32 {
        use NamedKey::*;
        const LETTERS: [i32; 26] = [
            0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F, 0x23,
            0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
        ];
        const DIGITS: [i32; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
        const FKEYS: [i32; 12] = [0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F];
        match self {
            Letter(c) => LETTERS[(c.to_ascii_uppercase() as u8 - b'A') as usize],
            Digit(c) => DIGITS[(c as u8 - b'0') as usize],
            Enter => 0x24,
            Tab => 0x30,
            Backspace => 0x33,
            Escape => 0x35,
            Space => 0x31,
            Delete => 0x75,
            Insert => 0x72,
            Home => 0x73,
            End => 0x77,
            PageUp => 0x74,
            PageDown => 0x79,
            ArrowLeft => 0x7B,
            ArrowRight => 0x7C,
            ArrowDown => 0x7D,
            ArrowUp => 0x7E,
            F(n) => FKEYS.get(n.saturating_sub(1) as usize).copied().unwrap_or(0x7A),
            Minus => 0x1B,
            Equals => 0x18,
            OpenBracket => 0x21,
            CloseBracket => 0x1E,
            Backslash => 0x2A,
            Semicolon => 0x29,
            Quote => 0x27,
            Backtick => 0x32,
            Comma => 0x2B,
            Period => 0x2F,
            Slash => 0x2C,
        }
    }

    /// The character AppKit reports for keys that produce no text.
    pub(crate) fn control_char(self) -> Option<u16> {
        use NamedKey::*;
        let mac = cfg!(target_os = "macos");
        Some(match self {
            Enter => 0x0D,
            Tab => 0x09,
            Backspace => {
                if mac {
                    0x7F
                } else {
                    0x08
                }
            }
            Escape => 0x1B,
            Space => 0x20,
            ArrowUp if mac => 0xF700,
            ArrowDown if mac => 0xF701,
            ArrowLeft if mac => 0xF702,
            ArrowRight if mac => 0xF703,
            F(n) if mac => 0xF703 + n as u16,
            Insert if mac => 0xF727,
            Delete if mac => 0xF728,
            Home if mac => 0xF729,
            End if mac => 0xF72B,
            PageUp if mac => 0xF72C,
            PageDown if mac => 0xF72D,
            _ => return None,
        })
    }

    /// Keys whose press inserts text into an editable field.
    pub(crate) fn emits_char_event(self) -> bool {
        matches!(self, NamedKey::Enter | NamedKey::Tab | NamedKey::Space | NamedKey::Backspace)
            || matches!(self, NamedKey::Letter(_) | NamedKey::Digit(_))
            || matches!(
                self,
                NamedKey::Minus
                    | NamedKey::Equals
                    | NamedKey::OpenBracket
                    | NamedKey::CloseBracket
                    | NamedKey::Backslash
                    | NamedKey::Semicolon
                    | NamedKey::Quote
                    | NamedKey::Backtick
                    | NamedKey::Comma
                    | NamedKey::Period
                    | NamedKey::Slash
            )
    }
}

pub(crate) fn key_events(input: &KeyInput) -> Vec<cef::KeyEvent> {
    use cef::{KeyEvent, KeyEventType};
    let mut flags = input.modifiers.cef_flags();
    if input.repeat {
        flags |= FLAG_IS_REPEAT;
    }
    let native = if cfg!(target_os = "macos") { input.key.mac_code() } else { 0 };
    let ch: u16 = input
        .text
        .and_then(|c| {
            let mut b = [0u16; 2];
            c.encode_utf16(&mut b).first().copied()
        })
        .or_else(|| input.key.control_char())
        .unwrap_or(0);
    let base = KeyEvent {
        size: std::mem::size_of::<KeyEvent>(),
        modifiers: flags,
        windows_key_code: input.key.vk(),
        native_key_code: native,
        is_system_key: 0,
        character: ch,
        unmodified_character: ch,
        focus_on_editable_field: 0,
        ..Default::default()
    };
    if !input.pressed {
        return vec![KeyEvent { type_: KeyEventType::KEYUP, ..base }];
    }
    let mut out = vec![KeyEvent { type_: KeyEventType::RAWKEYDOWN, ..base.clone() }];
    // Text-producing presses also send a CHAR, unless a shortcut modifier is held.
    let shortcut = input.modifiers.command || (input.modifiers.ctrl && !cfg!(target_os = "macos"));
    if !shortcut && ch != 0 && (input.text.is_some() || input.key.emits_char_event()) {
        let windows_key_code = if input.text.is_some() { ch as i32 } else { base.windows_key_code };
        out.push(KeyEvent { type_: KeyEventType::CHAR, windows_key_code, ..base });
    }
    out
}

/// How long Crossbow waits for a page to leave a tracked press unhandled.
/// A later report runs nothing.
pub(crate) const UNHANDLED_KEY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

const SHORTCUT_FLAGS: u32 = FLAG_SHIFT | FLAG_CONTROL | FLAG_ALT | FLAG_COMMAND;

/// How CEF identifies a press when it reports it back: the key code it rebuilds
/// the event from (the macOS virtual key code there, the Windows key code
/// elsewhere) and the shortcut modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeySignature {
    code: i32,
    modifiers: u32,
}

impl KeySignature {
    pub(crate) fn of(event: &cef::KeyEvent) -> KeySignature {
        let code = if cfg!(target_os = "macos") { event.native_key_code } else { event.windows_key_code };
        KeySignature { code, modifiers: event.modifiers & SHORTCUT_FLAGS }
    }
}

/// Presses sent with [`crate::WebView::key_tracked`], oldest first. CEF's
/// keyboard handler hears only about events the page did not consume, so a
/// consumed press is never reported and ages out after [`UNHANDLED_KEY_WINDOW`].
#[derive(Default)]
pub(crate) struct UnhandledKeys {
    pending: std::collections::VecDeque<(KeySignature, u64, std::time::Instant)>,
    reported: Vec<u64>,
}

impl UnhandledKeys {
    const LIMIT: usize = 32;

    pub(crate) fn track(&mut self, signature: KeySignature, ticket: u64, now: std::time::Instant) {
        self.expire(now);
        if self.pending.len() == Self::LIMIT {
            self.pending.pop_front();
        }
        self.pending.push_back((signature, ticket, now));
    }

    /// Record CEF's report of an unhandled event. True when it answers a
    /// tracked press, whose ticket is then reported once.
    pub(crate) fn report(&mut self, event: &cef::KeyEvent, now: std::time::Instant) -> bool {
        // Key-up and the CHAR that follows a press never run anything.
        if event.type_ != cef::KeyEventType::RAWKEYDOWN && event.type_ != cef::KeyEventType::KEYDOWN {
            return false;
        }
        self.expire(now);
        let signature = KeySignature::of(event);
        let Some(index) = self.pending.iter().position(|(pending, ..)| *pending == signature) else { return false };
        if let Some((_, ticket, _)) = self.pending.remove(index) {
            self.reported.push(ticket);
        }
        true
    }

    pub(crate) fn take(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.reported)
    }

    fn expire(&mut self, now: std::time::Instant) {
        self.pending.retain(|(_, _, sent)| now.saturating_duration_since(*sent) <= UNHANDLED_KEY_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cef::KeyEventType;
    use std::time::{Duration, Instant};

    fn press(key: NamedKey, modifiers: Modifiers) -> KeyInput {
        KeyInput { key, pressed: true, repeat: false, modifiers, text: None }
    }

    const CMD: Modifiers = Modifiers { shift: false, ctrl: false, alt: false, command: true };

    /// The RAWKEYDOWN Crossbow sends for `input`, as CEF reports it back.
    fn sent(input: &KeyInput) -> cef::KeyEvent {
        let events = key_events(input);
        assert_eq!(events[0].type_, KeyEventType::RAWKEYDOWN);
        events[0].clone()
    }

    fn reported(event: &cef::KeyEvent, type_: KeyEventType) -> cef::KeyEvent {
        cef::KeyEvent { type_, ..event.clone() }
    }

    #[test]
    fn an_unhandled_tracked_press_reports_its_ticket_once() {
        let now = Instant::now();
        let mut keys = UnhandledKeys::default();
        let reload = sent(&press(NamedKey::Letter('R'), CMD));
        keys.track(KeySignature::of(&reload), 7, now);
        // CEF adds flags of its own (caps lock, left/right keys); only shortcut modifiers count.
        let echoed = cef::KeyEvent { modifiers: reload.modifiers | (1 << 0) | (1 << 10), ..reload.clone() };
        assert!(keys.report(&echoed, now + Duration::from_millis(30)));
        assert_eq!(keys.take(), vec![7]);
        assert!(!keys.report(&echoed, now + Duration::from_millis(40)), "a press is answered once");
        assert!(keys.take().is_empty());
    }

    #[test]
    fn key_up_char_and_untracked_reports_run_nothing() {
        let now = Instant::now();
        let mut keys = UnhandledKeys::default();
        let space = press(NamedKey::Digit('1'), Modifiers { ctrl: true, ..Default::default() });
        let events = key_events(&space);
        keys.track(KeySignature::of(&events[0]), 1, now);
        for type_ in [KeyEventType::KEYUP, KeyEventType::CHAR] {
            assert!(!keys.report(&reported(&events[0], type_), now), "{type_:?}");
        }
        assert!(!keys.report(&sent(&press(NamedKey::Letter('K'), CMD)), now), "an untracked chord is not Crossbow's");
        assert!(!keys.report(&sent(&press(NamedKey::Digit('1'), CMD)), now), "other modifiers are another chord");
        assert!(keys.take().is_empty());
        assert!(keys.report(&events[0], now));
        assert_eq!(keys.take(), vec![1]);
    }

    #[test]
    fn a_consumed_press_never_answers_for_a_later_one_and_ages_out() {
        let now = Instant::now();
        let mut keys = UnhandledKeys::default();
        let find = sent(&press(NamedKey::Letter('F'), CMD));
        let print = sent(&press(NamedKey::Letter('P'), CMD));
        // The page consumes ⌘F (no report) and leaves ⌘P unhandled.
        keys.track(KeySignature::of(&find), 1, now);
        keys.track(KeySignature::of(&print), 2, now);
        assert!(keys.report(&print, now + Duration::from_millis(20)));
        assert_eq!(keys.take(), vec![2]);
        // Past the window the consumed ⌘F is forgotten; a fresh press answers for itself.
        let later = now + UNHANDLED_KEY_WINDOW + Duration::from_millis(1);
        keys.track(KeySignature::of(&find), 3, later);
        assert!(keys.report(&find, later));
        assert_eq!(keys.take(), vec![3]);
        // A report after the window runs nothing.
        keys.track(KeySignature::of(&print), 4, later);
        assert!(!keys.report(&print, later + UNHANDLED_KEY_WINDOW + Duration::from_millis(1)));
        assert!(keys.take().is_empty());
    }
}
