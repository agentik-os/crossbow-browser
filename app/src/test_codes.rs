//! Recovery codes in tests: held as [`Hidden`], compared by value and
//! printed only as `Hidden(..)`, so a failing assert never shows one. A
//! source test is the backstop that keeps the encryption tests in both
//! crates from revealing a code or key inside an assert or a message.

use crossbow_core::sync::e2ee::RecoveryCode;

#[path = "../../crates/crossbow-core/src/sync/test_secrets.rs"]
#[allow(dead_code)]
mod test_secrets;
pub(crate) use test_secrets::{Hidden, failure};

/// A recovery code's text, hidden, as tests compare it.
impl From<&RecoveryCode> for Hidden<String> {
    fn from(code: &RecoveryCode) -> Hidden<String> {
        Hidden::new(code.as_str().to_owned())
    }
}

/// Text on screen in a failure message: from "CB1-" on, a code's groups,
/// dashes and line break become "CB1-…".
pub(crate) fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.to_ascii_uppercase().find("CB1-") {
        out.push_str(&rest[..at]);
        out.push_str("CB1-…");
        let code = &rest[at + 4..];
        rest = &code[code.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '\n')).unwrap_or(code.len())..];
    }
    out.push_str(rest);
    out
}

/// Macros that print every argument.
const PRINTS_ALL: [&str; 13] = ["assert_eq!(", "assert_ne!(", "debug_assert_eq!(", "debug_assert_ne!(", "panic!(", "format!(", "print!(", "println!(", "eprint!(", "eprintln!(", "write!(", "writeln!(", "unreachable!("];
/// Macros that print all but their first argument, the condition.
const PRINTS_MESSAGE: [&str; 2] = ["assert!(", "debug_assert!("];
/// Calls that print their arguments with them, as a tuple does.
const TRANSPARENT: [&str; 3] = ["Some", "Ok", "Err"];
/// What a test may read off the typed field: never its text.
const DERIVED: [&str; 3] = [".len()", ".capacity()", ".is_empty()"];

/// The source with string and char literals emptied, so their text never
/// reads as code. Lengths and line breaks are kept.
fn without_literals(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        let raw = bytes[i] == b'r' && matches!(bytes.get(i + 1), Some(b'"' | b'#')) && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'));
        if raw {
            let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
            if bytes.get(i + 1 + hashes) == Some(&b'"') {
                let close = format!("\"{}", "#".repeat(hashes));
                let body = i + 2 + hashes;
                let end = source[body..].find(&close).map_or(source.len(), |at| body + at + close.len());
                out.push_str("r\"\"");
                out.extend(source[i..end].chars().filter(|&c| c == '\n'));
                i = end;
                continue;
            }
        }
        match bytes[i] {
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' { j += if bytes[j] == b'\\' { 2 } else { 1 }; }
                out.push_str("\"\"");
                out.extend(source[i..j.min(bytes.len())].chars().filter(|&c| c == '\n'));
                i = j + 1;
            }
            // A char literal, not a lifetime: 'x', '\n', '\''.
            b'\'' if bytes.get(i + 1) == Some(&b'\\') || bytes.get(i + 2) == Some(&b'\'') => {
                let end = source[i + 2..].find('\'').map_or(source.len(), |at| i + 2 + at + 1);
                out.push_str("' '");
                i = end;
            }
            _ => {
                let c = source[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The arguments of the call whose `(` is at `open`, split at top-level
/// commas, and where the call ends.
fn arguments(source: &str, open: usize) -> (Vec<&str>, usize) {
    let (mut depth, mut start, mut args) = (0i32, open + 1, Vec::new());
    for (at, c) in source[open..].char_indices().map(|(at, c)| (open + at, c)) {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    args.push(&source[start..at]);
                    return (args, at + 1);
                }
            }
            ',' if depth == 1 => {
                args.push(&source[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    (args, source.len())
}

/// What printing `expression` prints: a call's arguments are left out, since
/// only its result prints, except those of Some, Ok and Err.
fn printed_part(expression: &str) -> String {
    let mut out = String::with_capacity(expression.len());
    let mut rest = expression;
    while let Some(at) = rest.find('(') {
        let before = &rest[..at];
        let callee = before.rsplit(|c: char| !(is_word(c) || c == ':')).next().unwrap_or("");
        out.push_str(before);
        if callee.is_empty() || TRANSPARENT.contains(&callee) {
            out.push('(');
            rest = &rest[at + 1..];
        } else {
            out.push_str("()");
            rest = &rest[arguments(rest, at).1..];
        }
    }
    out.push_str(rest);
    out
}

/// Whether printing `argument` prints a code, a key or the typed field: a
/// revealed Hidden value, bytes, text read off something named for a code or
/// key, or the field's text.
fn prints_secret(argument: &str) -> bool {
    let printed = printed_part(argument);
    let named = printed.to_ascii_lowercase();
    printed.contains(".reveal()")
        || printed.contains(".as_bytes()")
        || (printed.contains(".as_str()") && (named.contains("code") || named.contains("key")))
        || printed.match_indices("e2ee_code").any(|(at, field)| !DERIVED.iter().any(|derived| printed[at + field.len()..].starts_with(derived)))
}

/// Each assert, panic or format in `module` that would print a code, a key
/// or the typed field, by the line it starts on.
fn printed_secrets(module: &str) -> Vec<(usize, String)> {
    let source = without_literals(module);
    let line_at = |at: usize| source[..at].matches('\n').count() + 1;
    let mut found = Vec::new();
    for (macros, skip) in [(&PRINTS_ALL[..], 0), (&PRINTS_MESSAGE[..], 1)] {
        for name in macros {
            for (at, _) in source.match_indices(name).filter(|&(at, _)| !source[..at].chars().next_back().is_some_and(is_word)) {
                let (args, _) = arguments(&source, at + name.len() - 1);
                if args.iter().skip(skip).any(|arg| prints_secret(arg)) {
                    found.push((line_at(at), source[at..].lines().next().unwrap_or("").to_string()));
                }
            }
        }
    }
    found.sort();
    found
}

/// The tests in `file` from its test module on.
fn test_module<'a>(file: &'a str, start: &str) -> &'a str {
    &file[file.find(start).unwrap_or_else(|| panic!("no {start:?}"))..]
}

/// The encryption tests in both crates print codes and keys only as Hidden
/// values. Hidden is the guard; this check is the backstop that keeps a
/// test from revealing one inside an assert or a message. A code revealed
/// into a variable first is not caught here.
#[test]
fn encryption_tests_never_print_a_revealed_code_or_key() {
    let (settings, glue, keys) = (include_str!("ui/settings.rs"), include_str!("sync_glue.rs"), include_str!("../../crates/crossbow-core/src/sync/key_store.rs"));
    let (e2ee, e2ee_sync) = (include_str!("../../crates/crossbow-core/src/sync/e2ee_tests.rs"), include_str!("../../crates/crossbow-core/src/sync/e2ee_sync_tests.rs"));
    let modules = [
        ("app/src/ui/settings.rs", settings, test_module(settings, "\nmod repair_tests {")),
        ("app/src/sync_glue.rs", glue, test_module(glue, "\nmod tests {")),
        ("crates/crossbow-core/src/sync/key_store.rs", keys, test_module(keys, "\nmod tests {")),
        ("crates/crossbow-core/src/sync/e2ee_tests.rs", e2ee, e2ee),
        ("crates/crossbow-core/src/sync/e2ee_sync_tests.rs", e2ee_sync, e2ee_sync),
    ];
    let mut printed = Vec::new();
    for (path, file, module) in modules {
        let first_line = file[..file.len() - module.len()].matches('\n').count();
        for (line, text) in printed_secrets(module) {
            printed.push(format!("{path}:{}: {text}", first_line + line));
        }
    }
    assert!(printed.is_empty(), "keep codes and keys Hidden in asserts and messages:\n{}", printed.join("\n"));
}

#[test]
fn the_source_check_finds_revealed_codes_and_keys_and_passes_hidden_ones() {
    let module = r#"
    fn a_test() {
        assert_eq!(code.reveal(), "x");
        assert_eq!(Some(code.reveal().as_str()), None);
        assert_ne!(device.sync.recovery_code().unwrap().as_str(), other);
        assert_ne!(*key.as_bytes(), [0; 32]);
        assert_eq!(device.state.e2ee_code.as_str(), "q");
        assert!(shown.contains(code.reveal()), "{}", code.reveal());
        panic!("{:?}", Hidden::new(key.reveal()).reveal());
        let text = format!("{}", key.as_bytes()[0]);
        debug_assert_eq!(new.reveal().replace('\n', "-"), old);
    }

    fn another_test() {
        assert_eq!(a.e2ee.unlock(k1.reveal(), &a.lib).unwrap_err(), USED_BEFORE);
        assert!(device.unlock(code.reveal()).is_ok());
        assert!(shown.contains(code.reveal()), "the code is on screen");
        assert_eq!(device.state.e2ee_code.capacity(), reserved, "{what}");
        assert!(device.state.e2ee_code.is_empty());
        assert_eq!((after.counter, after.node.as_str()), (1, "n"));
        assert_eq!(e.lib.spaces.get(&id).map(|s| s.name.as_str()), Some("x"));
        assert_eq!(fixture.code(), Some(code.clone()), "the code on screen");
        assert_eq!(RecoveryCode::parse(code.reveal()).unwrap().key_id(), id);
        assert_ne!(Hidden::new(*key.as_bytes()), other, "hidden again");
        eprintln!("{}", "a code.reveal() in a literal");
    }
"#;
    let found: Vec<usize> = printed_secrets(module).into_iter().map(|(line, _)| line).collect();
    assert_eq!(found, [3, 4, 5, 6, 7, 8, 9, 10, 11], "{:?}", printed_secrets(module));
}

/// A hidden code compares by value, and a failed assert or a message over
/// it prints only `Hidden(..)`.
#[test]
fn a_hidden_code_compares_by_value_and_never_prints() {
    let code = Hidden::new("CB1-ABCD-EFGH-JKLM");
    assert_eq!(code, Hidden::new(String::from("CB1-ABCD-EFGH-JKLM")));
    let messages = [
        failure(|| assert_eq!(code, Hidden::new("CB1-WXYZ-WXYZ-WXYZ"), "the codes")),
        failure(|| assert_ne!(Some(code.clone()), Some(code.clone()))),
        failure(|| panic!("{code} {code:?}")),
    ];
    for (n, message) in messages.iter().enumerate() {
        assert!(message.contains("Hidden(..)") && !message.contains("ABCD") && !message.contains("WXYZ"), "failure {n} printed a code");
    }
}

/// failure() quiets its own thread only: a panic on another thread while it
/// runs, or on any thread after it, still reaches the hook.
#[test]
fn failure_quiets_only_its_own_thread() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let original = Arc::new(std::panic::take_hook());
    let (record, forward) = (seen.clone(), original.clone());
    std::panic::set_hook(Box::new(move |info| {
        record.lock().unwrap().push(info.to_string());
        forward(info);
    }));
    let message = failure(|| {
        let _ = std::thread::spawn(|| panic!("another thread")).join();
        panic!("this thread");
    });
    let _ = std::thread::spawn(|| panic!("after")).join();
    drop(std::panic::take_hook());
    std::panic::set_hook(Box::new(move |info| original(info)));
    let seen = seen.lock().unwrap();
    let reported = |text: &str| seen.iter().any(|panic| panic.contains(text));
    assert_eq!(message, "this thread");
    assert!(reported("another thread") && reported("after") && !reported("this thread"), "{seen:?}");
}
