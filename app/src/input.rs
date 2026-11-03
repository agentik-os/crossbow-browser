//! Translating egui input into web-view input and app shortcuts.

use crossbow_engine::{KeyInput, Modifiers, NamedKey};
use egui::Key;

pub fn modifiers(m: egui::Modifiers) -> Modifiers {
    Modifiers { shift: m.shift, ctrl: m.ctrl, alt: m.alt, command: m.mac_cmd }
}

const LETTERS: [(Key, char); 26] = [
    (Key::A, 'A'),
    (Key::B, 'B'),
    (Key::C, 'C'),
    (Key::D, 'D'),
    (Key::E, 'E'),
    (Key::F, 'F'),
    (Key::G, 'G'),
    (Key::H, 'H'),
    (Key::I, 'I'),
    (Key::J, 'J'),
    (Key::K, 'K'),
    (Key::L, 'L'),
    (Key::M, 'M'),
    (Key::N, 'N'),
    (Key::O, 'O'),
    (Key::P, 'P'),
    (Key::Q, 'Q'),
    (Key::R, 'R'),
    (Key::S, 'S'),
    (Key::T, 'T'),
    (Key::U, 'U'),
    (Key::V, 'V'),
    (Key::W, 'W'),
    (Key::X, 'X'),
    (Key::Y, 'Y'),
    (Key::Z, 'Z'),
];

const DIGITS: [(Key, char); 10] = [
    (Key::Num0, '0'),
    (Key::Num1, '1'),
    (Key::Num2, '2'),
    (Key::Num3, '3'),
    (Key::Num4, '4'),
    (Key::Num5, '5'),
    (Key::Num6, '6'),
    (Key::Num7, '7'),
    (Key::Num8, '8'),
    (Key::Num9, '9'),
];

pub fn named_key(k: Key) -> Option<NamedKey> {
    if let Some((_, c)) = LETTERS.iter().find(|(key, _)| *key == k) {
        return Some(NamedKey::Letter(*c));
    }
    if let Some((_, c)) = DIGITS.iter().find(|(key, _)| *key == k) {
        return Some(NamedKey::Digit(*c));
    }
    Some(match k {
        Key::Enter => NamedKey::Enter,
        Key::Tab => NamedKey::Tab,
        Key::Backspace => NamedKey::Backspace,
        Key::Escape => NamedKey::Escape,
        Key::Space => NamedKey::Space,
        Key::Delete => NamedKey::Delete,
        Key::Insert => NamedKey::Insert,
        Key::Home => NamedKey::Home,
        Key::End => NamedKey::End,
        Key::PageUp => NamedKey::PageUp,
        Key::PageDown => NamedKey::PageDown,
        Key::ArrowLeft => NamedKey::ArrowLeft,
        Key::ArrowRight => NamedKey::ArrowRight,
        Key::ArrowUp => NamedKey::ArrowUp,
        Key::ArrowDown => NamedKey::ArrowDown,
        Key::F1 => NamedKey::F(1),
        Key::F2 => NamedKey::F(2),
        Key::F3 => NamedKey::F(3),
        Key::F4 => NamedKey::F(4),
        Key::F5 => NamedKey::F(5),
        Key::F6 => NamedKey::F(6),
        Key::F7 => NamedKey::F(7),
        Key::F8 => NamedKey::F(8),
        Key::F9 => NamedKey::F(9),
        Key::F10 => NamedKey::F(10),
        Key::F11 => NamedKey::F(11),
        Key::F12 => NamedKey::F(12),
        Key::Minus => NamedKey::Minus,
        Key::Equals | Key::Plus => NamedKey::Equals,
        Key::OpenBracket | Key::OpenCurlyBracket => NamedKey::OpenBracket,
        Key::CloseBracket | Key::CloseCurlyBracket => NamedKey::CloseBracket,
        Key::Backslash | Key::Pipe => NamedKey::Backslash,
        Key::Semicolon | Key::Colon => NamedKey::Semicolon,
        Key::Quote => NamedKey::Quote,
        Key::Backtick => NamedKey::Backtick,
        Key::Comma => NamedKey::Comma,
        Key::Period => NamedKey::Period,
        Key::Slash | Key::Questionmark => NamedKey::Slash,
        _ => return None,
    })
}

/// The key a web page receives for an egui key event, if it receives one.
pub fn web_key(key: Key, physical_key: Option<Key>) -> Option<NamedKey> {
    physical_key.and_then(named_key).or_else(|| named_key(key))
}

/// Turn egui key/text events into web-view key inputs. A `Text` event right
/// after a key press is folded into that press as its character.
pub fn web_key_inputs(events: &[egui::Event]) -> Vec<WebInput> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            egui::Event::Key { key, physical_key, pressed, repeat, modifiers: m } => {
                let k = web_key(*key, *physical_key);
                if let Some(k) = k {
                    let mut text = None;
                    if *pressed {
                        if let Some(egui::Event::Text(t)) = events.get(i + 1) {
                            let mut chars = t.chars();
                            if let (Some(c), None) = (chars.next(), chars.next()) {
                                text = Some(c);
                                i += 1;
                            }
                        }
                    }
                    out.push(WebInput::Key(KeyInput {
                        key: k,
                        pressed: *pressed,
                        repeat: *repeat,
                        modifiers: modifiers(*m),
                        text,
                    }));
                }
            }
            egui::Event::Text(t) => out.push(WebInput::Text(t.clone())),
            egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => out.push(WebInput::Composition(text.clone())),
            egui::Event::Ime(egui::ImeEvent::Commit(t)) => out.push(WebInput::Text(t.clone())),
            egui::Event::Copy => out.push(WebInput::Copy),
            egui::Event::Cut => out.push(WebInput::Cut),
            egui::Event::Paste(_) => out.push(WebInput::Paste),
            _ => {}
        }
        i += 1;
    }
    out
}

#[derive(Clone, Debug)]
pub enum WebInput {
    Key(KeyInput),
    /// Text with no key press of its own (IME commit, emoji picker, dictation).
    Text(String),
    Composition(String),
    Copy,
    Cut,
    Paste,
}

/// Deliver one keyboard input to a page. The primary modifier with A, Z and
/// Shift-Z runs the page's edit commands; with C, X and V it is skipped,
/// because copy, cut and paste arrive as their own inputs.
pub fn send(view: &crossbow_engine::WebView, input: &WebInput) {
    match input {
        WebInput::Key(k) => {
            let primary = if cfg!(target_os = "macos") { k.modifiers.command } else { k.modifiers.ctrl };
            if primary && k.pressed {
                match k.key {
                    NamedKey::Letter('A') => return view.select_all(),
                    NamedKey::Letter('Z') => return if k.modifiers.shift { view.redo() } else { view.undo() },
                    NamedKey::Letter('C' | 'X' | 'V') => return,
                    _ => {}
                }
            }
            view.key(k);
        }
        WebInput::Text(t) => view.ime_commit(t),
        WebInput::Composition(t) => view.ime_set_composition(t),
        WebInput::Copy => view.copy(),
        WebInput::Cut => view.cut(),
        WebInput::Paste => view.paste(),
    }
}

/// The primary shortcut modifier: ⌘ on macOS, Ctrl elsewhere.
pub fn primary(m: &egui::Modifiers) -> bool {
    m.command
}

pub fn shortcut_label(primary: bool, shift: bool, alt: bool, ctrl: bool, key: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let mut s = String::new();
    if mac {
        if ctrl {
            s.push('⌃');
        }
        if alt {
            s.push('⌥');
        }
        if shift {
            s.push('⇧');
        }
        if primary {
            s.push('⌘');
        }
        s.push_str(key);
    } else {
        let mut parts = Vec::new();
        if primary || ctrl {
            parts.push("Ctrl");
        }
        if alt {
            parts.push("Alt");
        }
        if shift {
            parts.push("Shift");
        }
        parts.push(key);
        s = parts.join("+");
    }
    s
}
