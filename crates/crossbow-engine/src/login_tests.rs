//! The browser-side rules for page logins, with plain frames. CEF can't run
//! here, so the renderer glue is checked by reading it; `login.js` also runs
//! in JavaScriptCore, in login_js_tests.rs.
//! Every username and password is a synthetic fixture.

use super::*;

const USER: &str = "fixture-user@example.test";
const PASSWORD: &str = "fixture-page-password-4K";

/// A frame tree node: an address, and whether it is the main frame.
#[derive(Clone)]
struct Fake {
    id: &'static str,
    url: &'static str,
    main: bool,
    parent: Option<Box<Fake>>,
}

impl FrameLike for Fake {
    fn id(&self) -> String {
        self.id.into()
    }

    fn address(&self) -> String {
        self.url.into()
    }

    fn main(&self) -> bool {
        self.main
    }

    fn parent_frame(&self) -> Option<Fake> {
        self.parent.as_deref().cloned()
    }
}

fn top(url: &'static str) -> Fake {
    Fake { id: "main", url, main: true, parent: None }
}

fn child(id: &'static str, url: &'static str, parent: Fake) -> Fake {
    Fake { id, url, main: false, parent: Some(Box::new(parent)) }
}

fn args(values: &[&str]) -> Vec<Secret> {
    values.iter().map(|value| Secret::new(value.to_string())).collect()
}

/// The id the fixture renderers give every document they report.
const DOC: &str = "fixture-document-1";

fn capture_args(origin: &str) -> Vec<Secret> {
    args(&[origin, DOC, USER, PASSWORD])
}

/// A follow, focus or unfilled note from the fixture document.
fn note(origin: &str) -> Vec<Secret> {
    args(&[origin, DOC])
}

/// A renderer's report of `document`, before it sent a card or address note there.
fn reported(origin: &str, document: &str) -> Vec<Secret> {
    args(&[origin, document, "0", "0"])
}

/// A view whose `frames` each reported installing `login.js` in their
/// current document, as their renderers do.
fn ready(frames: &[&Fake]) -> LoginState {
    let mut state = LoginState::default();
    for frame in frames {
        let Some(origin) = page_origin(frame.url) else { continue };
        receive(&mut state, *frame, INSTALLED, reported(&origin, DOC), Instant::now(), true);
    }
    state
}

#[test]
fn only_canonical_http_and_https_addresses_have_an_origin() {
    for (url, origin) in [
        ("https://login.example/session?next=/", Some("https://login.example")),
        ("http://login.example/", Some("http://login.example")),
        ("https://login.example:8443/a#b", Some("https://login.example:8443")),
        ("http://127.0.0.1:49152/login", Some("http://127.0.0.1:49152")),
        ("http://[::1]:8080/", Some("http://[::1]:8080")),
        ("https://xn--bcher-kva.example/", Some("https://xn--bcher-kva.example")),
        ("https://user:secret@login.example/", Some("https://login.example")),
        ("file:///Users/fixture/login.html", None),
        ("data:text/html,<form>", None),
        ("blob:https://login.example/1234", None),
        ("about:blank", None),
        ("chrome://settings/passwords", None),
        ("devtools://devtools/bundled/inspector.html", None),
        ("crossbow://boost", None),
        ("https:///path", None),
        ("https://Login.Example/", None),
        ("https://login.example:/", None),
        ("https://login.example:84a3/", None),
        ("https://[::1/", None),
        ("", None),
    ] {
        assert_eq!(page_origin(url).as_deref(), origin, "{url}");
    }
}

#[test]
fn logins_are_taken_only_from_the_top_frame_or_frames_same_origin_with_every_ancestor() {
    let page = top("https://login.example/");
    assert_eq!(eligible_origin(&page).as_deref(), Some("https://login.example"));
    let same = child("same", "https://login.example/embedded", page.clone());
    assert_eq!(eligible_origin(&same).as_deref(), Some("https://login.example"));
    assert_eq!(eligible_origin(&child("deeper", "https://login.example/deeper", same.clone())).as_deref(), Some("https://login.example"));
    for (case, frame) in [
        ("cross-origin iframe", child("other", "https://widget.example/login", page.clone())),
        ("same host, other port", child("port", "https://login.example:8443/", page.clone())),
        ("same host, http", child("http", "http://login.example/", page.clone())),
        ("subdomain", child("sub", "https://sso.login.example/", page.clone())),
        ("same origin below a cross-origin frame", child("inner", "https://login.example/x", child("mid", "https://widget.example/", page.clone()))),
        ("parent unknown, as for a cross-process parent", Fake { id: "orphan", url: "https://login.example/", main: false, parent: None }),
        ("srcdoc frame", child("srcdoc", "about:srcdoc", page.clone())),
        ("file page", top("file:///tmp/login.html")),
        ("data page", top("data:text/html,x")),
        ("internal page", top("chrome://version/")),
    ] {
        assert_eq!(eligible_origin(&frame), None, "{case}");
    }
}

#[test]
fn a_submitted_login_is_reported_once_after_a_request_follows() {
    let page = top("https://login.example/");
    let now = Instant::now();
    let mut state = ready(&[&page]);
    assert!(receive(&mut state, &page, CAPTURE, capture_args("https://login.example"), now, true).is_none(), "nothing before a follow");
    assert!(state.has_pending());
    let capture = receive(&mut state, &page, FOLLOWED, note("https://login.example"), now + Duration::from_secs(2), true).expect("offered");
    assert_eq!((capture.origin.as_str(), capture.username.expose(), capture.password.expose()), ("https://login.example", USER, PASSWORD));
    assert!(receive(&mut state, &page, FOLLOWED, note("https://login.example"), now + Duration::from_secs(3), true).is_none(), "exactly once");
    assert!(state.navigated("main", true, now + Duration::from_secs(4)).is_none(), "and not again on the navigation");
}

#[test]
fn a_navigation_of_the_submitting_frame_or_the_page_reports_the_login() {
    let page = top("https://login.example/");
    let frame = child("same", "https://login.example/embedded", page.clone());
    let now = Instant::now();
    for (case, nav_frame, main, offered) in [
        ("the page navigates", "main", true, true),
        ("the submitting frame navigates", "same", false, true),
        ("another frame loads", "ad", false, false),
    ] {
        let mut state = ready(&[&frame]);
        receive(&mut state, &frame, CAPTURE, capture_args("https://login.example"), now, true);
        assert_eq!(state.navigated(nav_frame, main, now + Duration::from_secs(1)).is_some(), offered, "{case}");
    }
}

#[test]
fn nothing_is_offered_without_a_follow_in_time_and_the_login_is_wiped() {
    let page = top("https://login.example/");
    let now = Instant::now();
    let mut state = ready(&[&page]);
    receive(&mut state, &page, CAPTURE, capture_args("https://login.example"), now, true);
    state.expire(now + FOLLOW_WINDOW - Duration::from_millis(1));
    assert!(state.has_pending());
    state.expire(now + FOLLOW_WINDOW);
    assert!(!state.has_pending(), "wiped after the follow window");
    assert!(state.navigated("main", true, now + FOLLOW_WINDOW).is_none());
    // A late follow finds nothing either.
    receive(&mut state, &page, CAPTURE, capture_args("https://login.example"), now, true);
    assert!(receive(&mut state, &page, FOLLOWED, note("https://login.example"), now + FOLLOW_WINDOW + Duration::from_secs(1), true).is_none());
}

#[test]
fn a_second_submit_replaces_the_first() {
    let page = top("https://login.example/");
    let now = Instant::now();
    let mut state = ready(&[&page]);
    receive(&mut state, &page, CAPTURE, args(&["https://login.example", DOC, "first", "fixture-first-password"]), now, true);
    receive(&mut state, &page, CAPTURE, args(&["https://login.example", DOC, "second", "fixture-second-password"]), now, true);
    let capture = state.navigated("main", true, now).unwrap();
    assert_eq!(capture.username.expose(), "second");
    assert!(state.navigated("main", true, now).is_none());
}

#[test]
fn forged_or_misdirected_messages_are_dropped() {
    let page = top("https://login.example/");
    let iframe = child("widget", "https://widget.example/", page.clone());
    let now = Instant::now();
    let dropped = |frame: &Fake, name: &str, values: Vec<Secret>| {
        // Each frame reported, so only the message itself is wrong.
        let mut state = ready(&[frame]);
        receive(&mut state, frame, name, values, now, true);
        !state.has_pending() && state.focus().is_none()
    };
    assert!(dropped(&page, CAPTURE, capture_args("https://bank.example")), "an origin the frame doesn't have");
    assert!(dropped(&page, CAPTURE, capture_args("https://login.example:443")), "a port spelled differently");
    assert!(dropped(&page, CAPTURE, capture_args("http://login.example")), "another scheme");
    assert!(dropped(&iframe, CAPTURE, capture_args("https://widget.example")), "a cross-origin iframe");
    assert!(dropped(&iframe, FOCUSED, note("https://widget.example")), "nor its focus");
    assert!(dropped(&top("file:///tmp/login.html"), CAPTURE, capture_args("file://")), "a file page");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", DOC, USER, ""])), "an empty password");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", DOC, USER])), "a missing argument");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", DOC, USER, PASSWORD, "extra"])), "an extra argument");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", DOC, USER, &"p".repeat(MAX_PASSWORD_BYTES + 1)])), "a password the vault can't hold");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", DOC, &"u".repeat(MAX_USERNAME_BYTES + 1), PASSWORD])), "a username the vault can't hold");
    assert!(dropped(&page, "crossbow.login.save", capture_args("https://login.example")), "a name that isn't ours");
    assert!(dropped(&page, CAPTURE, Vec::new()), "no arguments");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", "fixture-document-2", USER, PASSWORD])), "another document of the frame");
    assert!(dropped(&page, FOCUSED, args(&["https://login.example", "fixture-document-2"])), "nor its focus");
    assert!(dropped(&page, CAPTURE, args(&["https://login.example", USER, PASSWORD])), "no document");
    let mut unreported = LoginState::default();
    receive(&mut unreported, &page, CAPTURE, capture_args("https://login.example"), now, true);
    receive(&mut unreported, &page, FOCUSED, note("https://login.example"), now, true);
    assert!(!unreported.has_pending() && unreported.focus().is_none(), "a frame that never reported");
    // A forged follow without a submit offers nothing.
    let same = child("same", "https://login.example/embedded", page.clone());
    let mut state = ready(&[&page, &same]);
    assert!(receive(&mut state, &page, FOLLOWED, note("https://login.example"), now, true).is_none());
    // A follow from another frame doesn't release this frame's login.
    receive(&mut state, &same, CAPTURE, capture_args("https://login.example"), now, true);
    assert!(receive(&mut state, &page, FOLLOWED, note("https://login.example"), now, true).is_none());
    assert!(state.has_pending());
}

#[test]
fn focus_names_the_frame_to_fill_and_ends_with_the_page() {
    let page = top("https://login.example/");
    let same = child("same", "https://login.example/embedded", page.clone());
    let now = Instant::now();
    let mut state = ready(&[&page, &same]);
    receive(&mut state, &same, FOCUSED, note("https://login.example"), now, true);
    assert_eq!(state.focus(), Some(("same", "https://login.example")));
    assert!(state.navigated("same", false, now).is_none());
    state.document_started("ad", false);
    assert!(state.focus().is_some(), "a load in another frame keeps the page's focus");
    state.document_started("same", false);
    assert_eq!(state.focus(), None, "a new document in the focused frame forgets it: its field is gone");
    receive(&mut state, &same, FOCUSED, note("https://login.example"), now, true);
    assert_eq!(state.focus(), None, "and its report with it");
    let mut state = ready(&[&page, &same]);
    receive(&mut state, &same, FOCUSED, note("https://login.example"), now, true);
    state.navigated("main", true, now);
    assert_eq!(state.focus(), None, "a new page forgets it");
}

#[test]
fn a_fill_goes_only_to_a_frame_with_the_entry_origin() {
    let page = top("https://login.example/");
    let same = child("same", "https://login.example/embedded", page.clone());
    let widget = child("widget", "https://widget.example/", page.clone());
    let state = ready(&[&page, &same, &widget]);
    let id = |frame: Option<Fake>| frame.map(|frame| frame.id);
    assert_eq!(id(fill_target(&state, [Some(same.clone()), Some(same.clone()), Some(page.clone())], "https://login.example")), Some("same"), "the frame whose login field has focus first");
    assert_eq!(id(fill_target(&state, [Some(widget.clone()), None, Some(page.clone())], "https://login.example")), Some("main"), "a cross-origin focused frame is skipped");
    assert_eq!(id(fill_target(&state, [Some(widget.clone()), None, Some(page.clone())], "https://widget.example")), None, "and never filled, even for its own entry");
    assert_eq!(id(fill_target(&state, [None, None, Some(page.clone())], "http://login.example")), None, "an http entry on an https page");
    assert_eq!(id(fill_target(&state, [None, None, Some(top("http://login.example/"))], "https://login.example")), None, "an https entry on an http page");
    assert_eq!(id(fill_target(&state, [None, None, Some(page.clone())], "https://login.example:8443")), None, "another port");
    assert_eq!(id(fill_target(&state, [None, None, Some(page.clone())], "https://example")), None, "a parent domain");
    assert_eq!(id(fill_target(&state, [None, None, Some(top("https://sso.login.example/"))], "https://login.example")), None, "a subdomain");
    assert_eq!(id(fill_target(&state, [None, None, Some(top("file:///tmp/login.html"))], "https://login.example")), None);
}

#[test]
fn secrets_never_show_in_debug_output_and_are_zeroed_when_dropped() {
    let capture = LoginCapture { origin: "https://login.example".into(), username: Secret::new(USER.into()), password: Secret::new(PASSWORD.into()) };
    let shown = format!("{capture:?} {:?} {:?} {:#?}", capture.username, capture.password, crate::WebEvent::Credential(capture.clone()));
    assert!(!shown.contains(USER) && !shown.contains(PASSWORD), "{shown}");
    assert!(shown.contains("https://login.example"), "the origin isn't secret");
    // Drop wipes the string before it is freed: every byte it allocated is zeroed.
    let mut text = String::with_capacity(64);
    text.push_str(PASSWORD);
    let (ptr, capacity) = (text.as_ptr(), text.capacity());
    wipe(&mut text);
    assert!(text.is_empty() && text.capacity() == capacity && text.as_ptr() == ptr, "still the same allocation");
    // SAFETY: `text` still owns this allocation, and wipe wrote every byte of it.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, capacity) };
    assert!(bytes.iter().all(|byte| *byte == 0), "every byte, spare capacity included, is zero");
}

/// The part of `login.js` that runs after page scripts may have patched
/// built-ins: everything below the marker.
fn listener_body() -> &'static str {
    let marker = "// -- Everything below calls only the references captured above. --";
    let (_, body) = SCRIPT.split_once(marker).expect("login.js keeps its marker");
    body
}

#[test]
fn the_listener_calls_no_page_reachable_method_and_never_hands_out_the_native_functions() {
    let body = listener_body();
    let bytes = body.as_bytes();
    // A method call (`x.y(`) looks `y` up on an object the page can patch.
    // Only captured references, called bare or through the captured Reflect.apply, are allowed.
    let mut calls = Vec::new();
    for (i, _) in body.match_indices('.') {
        let name: String = body[i + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let after = bytes.get(i + 1 + name.len()).copied();
        if !name.is_empty() && after == Some(b'(') { calls.push(name); }
    }
    assert!(calls.is_empty(), "method calls after the prologue: {calls:?}");
    for forbidden in ["console", "eval(", "Function(", "postMessage", "localStorage", "sessionStorage", "document.cookie", "fetch(", "XMLHttpRequest"] {
        assert!(!body.contains(forbidden), "{forbidden} in the listener");
    }
    // The native functions are only ever called directly: never passed,
    // returned, stored or used as `this`, so no patched built-in receives one.
    for native in ["capture", "followed", "focused", "addressCapture", "addressFocused", "cardFocused", "addressFilled", "cardFilled"] {
        let uses: Vec<usize> = body.match_indices(native).map(|(i, _)| i).filter(|&i| {
            let before = body[..i].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            let after = body[i + native.len()..].chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            !before && !after
        }).collect();
        assert!(!uses.is_empty(), "{native} is used");
        for i in uses {
            let line = body[..i].rsplit('\n').next().unwrap_or("");
            if line.trim_start().starts_with("//") { continue; }
            assert_eq!(body.as_bytes().get(i + native.len()), Some(&b'('), "{native} is only called: {line}…");
        }
    }
}

#[test]
fn the_prologue_captures_every_built_in_the_listener_relies_on() {
    let (prologue, _) = SCRIPT.split_once("// -- Everything below").unwrap();
    let start = prologue.find("'use strict';").expect("strict mode");
    for captured in [
        "Reflect.apply", "Object.getOwnPropertyDescriptor", "Function.prototype[Symbol.hasInstance]", "EventTarget.prototype.addEventListener",
        "EventTarget.prototype.dispatchEvent", "Element.prototype.querySelectorAll", "Element.prototype.getAttribute", "String.prototype.includes",
        "NodeList.prototype.item", "HTMLCollection.prototype.item", "get(Event.prototype, 'target')", "get(Navigator.prototype, 'userActivation')",
        "get(UserActivation.prototype, 'isActive')", "gopd(InputCtor.prototype, 'value')", "Performance.prototype.now", "PerformanceObserverEntryList.prototype.getEntries",
    ] {
        assert!(prologue[start..].contains(captured), "{captured} is captured before any page script");
    }
    let body = listener_body();
    assert!(body.contains("gopd(event, 'isTrusted')"), "isTrusted is read from the event's own unforgeable property");
    assert!(body.contains("apply(isActive, apply(userActivation, nav, []), []) !== true"), "user activation is required");
    assert!(body.contains("password === ''"), "an empty password is never captured");
    assert!(body.contains("['submit', onSubmit, true]"), "the submit listener runs in the capture phase, before page listeners");
    assert!(!body.contains("'load'") && !body.contains("DOMContentLoaded"), "nothing fills on page load");
}

#[test]
fn nothing_on_the_login_path_logs_or_prints() {
    let view = include_str!("view.rs");
    let start = view.find("fn login_message").unwrap();
    let browser_side = &view[start..start + view[start..].find("\n    }\n").unwrap()];
    for (source, text) in [("login.rs", include_str!("login.rs")), ("view.rs login_message", browser_side)] {
        for token in ["log::", "println!", "eprintln!", "print!", "dbg!", "tracing", "{:?}", ":?}"] {
            assert!(!text.contains(token), "{token} in {source}");
        }
    }
    assert!(!SCRIPT.contains("console"), "the listener never writes to the page console");
}

#[test]
fn the_fill_hands_page_code_no_option_object_with_a_prototype() {
    let (_, body) = SCRIPT.split_once("// -- Everything below").unwrap();
    assert!(body.contains("new EventCtor(type, { __proto__: null, bubbles: true })"));
    assert_eq!(body.matches("new EventCtor(").count(), 1, "every event the fill fires uses those options");
}

// -- the preview gate: "Offer to save passwords from websites" -----------------------------

/// Off, the renderer installs `login.js` without its capture functions, and
/// the script adds no submit listener and no request observer: no password
/// can reach the browser process. The focus note and the fill stay, for the
/// key button.
#[test]
fn without_capture_functions_the_script_adds_no_submit_listener_or_request_observer() {
    let (prologue, body) = SCRIPT.split_once("// -- Everything below").unwrap();
    assert!(prologue.contains("const capturing = typeof capture === 'function' && typeof followed === 'function';"), "decided before any page script");
    let gate = body.find("if (capturing) {").expect("the capture listeners are gated");
    let end = gate + body[gate..].find("\n  }\n").expect("the gate closes");
    let gated = &body[gate..end];
    for listener in ["['submit', onSubmit, true]", "new Observer(onEntries)"] {
        assert_eq!(body.matches(listener).count(), 1, "{listener} is added once");
        assert!(gated.contains(listener), "{listener} only when capturing");
    }
    for always in ["['focusin', onFocus, true]", "const login = function (username, password, main)", "const address = function (main, ...values)", "return { __proto__: null, login: login, address: address, card: card };"] {
        assert!(body.contains(always) && !gated.contains(always), "{always} is not gated");
    }
    assert!(prologue.contains("const capturingAddresses = capturing && typeof addressCapture === 'function';"));
    assert!(body.contains("const addressSent = capturingAddresses && captureAddress(form);"), "an address is captured only by the gated submit listener, and only with its function");
    // The renderer passes the capture functions only when the switch is on.
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("let always = *name == \"focused\" || *name == \"addressFocused\" || *name == \"addressFilled\";\n            let wanted = if *name == \"cardFocused\" || *name == \"cardFilled\" { cards } else { !cards_only && (capture || always) };\n            if wanted { v8_value_create_function(Some(&CefString::from(*name)), Some(&mut NativeBuilder::new())) } else { v8_value_create_undefined() }"));
}

/// With the setting off for the view's profile, the browser process drops a
/// submitted login and its follow, so zero logins are reported even from a
/// renderer that missed the change; the focus note, for the key, still counts.
#[test]
fn with_capture_off_no_login_is_ever_reported_and_the_focus_note_still_counts() {
    let now = Instant::now();
    let page = top("https://login.example/");
    let mut state = ready(&[&page]);
    let mut reported = 0;
    for second in 0..3 {
        let at = now + Duration::from_secs(second);
        reported += receive(&mut state, &page, CAPTURE, capture_args("https://login.example"), at, false).into_iter().count();
        reported += receive(&mut state, &page, FOLLOWED, note("https://login.example"), at, false).into_iter().count();
        reported += state.navigated("main", true, at).into_iter().count();
    }
    assert_eq!(reported, 0);
    assert!(!state.has_pending(), "nothing is held for a later follow");
    receive(&mut state, &page, FOCUSED, note("https://login.example"), now, false);
    assert_eq!(state.focus(), Some(("main", "https://login.example")));
    // Turning it off drops a login already waiting for its follow.
    receive(&mut state, &page, CAPTURE, capture_args("https://login.example"), now, true);
    assert!(state.has_pending());
    state.forget_pending();
    assert!(receive(&mut state, &page, FOLLOWED, note("https://login.example"), now, true).is_none());
}

/// Profile ids no other test uses: the setting is one per process.
fn fresh_profile(name: &str) -> String {
    format!("{name}-{:?}", Instant::now()).replace([',', ':', ' '], "-")
}

#[test]
fn the_setting_is_off_by_default_per_profile_and_each_change_is_newer() {
    let (a, b) = (fresh_profile("fixture-profile-a"), fresh_profile("fixture-profile-b"));
    assert!(!login_capture(&a) && !login_capture(&b), "off until turned on");
    assert!(!set_login_capture(&a, false), "already off");
    let (before, _) = capture_setting(&a);
    assert!(set_login_capture(&a, true));
    let (on_at, on) = capture_setting(&a);
    assert!(on && on_at > before);
    assert!(!login_capture(&b), "another profile stays off");
    assert!(launch_switch().split_once(':').unwrap().1.split(',').any(|id| id == a), "a renderer launched now starts with it on");
    assert!(set_login_capture(&a, false));
    let (off_at, off) = capture_setting(&a);
    assert!(!off && off_at > on_at);
    assert!(!launch_switch().split_once(':').unwrap().1.split(',').any(|id| id == a));
}

#[test]
fn a_renderer_keeps_the_newest_value_and_a_tie_or_an_unknown_browser_is_off() {
    let mut gate = Gate::default();
    gate.launched("4:");
    assert!(!gate.allows(1), "a browser the renderer never saw created");
    // Created while off: nothing is installed.
    gate.browser_created(1, "profile-a", 4, false, true);
    assert!(!gate.allows(1));
    // Turned on, the message reaches this renderer: pages loaded afterwards capture.
    gate.learn("profile-a", 5, true);
    assert!(gate.allows(1));
    // Turned off mid-session: the next load installs no listener.
    gate.learn("profile-a", 6, false);
    assert!(!gate.allows(1));
    // An older message arriving late changes nothing.
    gate.learn("profile-a", 5, true);
    assert!(!gate.allows(1));
    // A tie between sources is off.
    gate.learn("profile-a", 7, true);
    gate.learn("profile-a", 7, false);
    assert!(!gate.allows(1));
    gate.browser_destroyed(1);
    assert!(!gate.allows(1));

    // A tab opened with it on moves to a renderer launched after it was turned
    // off: the launch value is newer than the tab's creation value.
    let mut fresh = Gate::default();
    fresh.launched("9:profile-b");
    fresh.browser_created(2, "profile-a", 3, true, true);
    assert!(!fresh.allows(2), "the newer launch value wins");
    fresh.browser_created(3, "profile-b", 3, false, true);
    assert!(fresh.allows(3), "on at launch, after the tab was created");
    // A browser with no profile, or a malformed launch value, is off.
    let mut odd = Gate::default();
    odd.launched("not a value");
    odd.browser_created(4, "", 99, true, true);
    odd.browser_created(5, "profile-c", 0, false, true);
    assert!(!odd.allows(4) && !odd.allows(5));
}

/// Turning the setting off reaches the open page at once: its installed
/// listener's native functions check the gate before sending anything.
#[test]
fn an_open_page_stops_sending_once_the_renderer_learns_the_setting_is_off() {
    let renderer = include_str!("login.rs");
    let native = &renderer[renderer.find("fn native(name: &str").unwrap()..];
    let native = &native[..native.find("\n}\n").unwrap()];
    let gate = native.find("if name != \"focused\" && name != \"addressFocused\" && name != \"cardFocused\" && name != \"addressFilled\" && name != \"cardFilled\" && !allowed(context.browser()) { return; }").expect("checked before any message is built");
    assert!(gate < native.find("outgoing(CAPTURE").unwrap() && gate < native.find("outgoing(ADDRESS_CAPTURE").unwrap() && gate < native.find("send_process_message").unwrap());
    let handler = &renderer[renderer.find("fn on_process_message_received(").unwrap()..];
    let (from_browser, learn) = (handler.find("if source_process != ProcessId::BROWSER { return 0; }").expect("only the browser process changes the setting"), handler.find("if setting(message) {").unwrap());
    assert!(from_browser < learn);
}

/// CEF can't run in unit tests, so the glue that carries the setting is
/// checked in the source: every new browser's `extra_info`, every new
/// renderer's launch switch, a message to every frame of an open page, and
/// the browser-side drop.
#[test]
fn the_browser_process_tells_every_renderer_the_setting_and_drops_capture_while_off() {
    let view = include_str!("view.rs");
    let create = &view[view.find("let mut info = login::creation_info(&shared.profile_id, shared.saves_logins);").expect("built before the browser")..];
    let create = &create[..create.find(")?;").unwrap()];
    assert!(create.contains("browser_host_create_browser_sync(") && create.contains("info.as_mut(),"), "passed as the browser's extra_info");
    assert!(view.contains("if login::carries_login(&name) && !(self.saves_logins && login::login_capture(&self.profile_id)) { return true; }"));
    assert!(view.contains("login::receive(&mut self.login.borrow_mut(), frame, &name, args, now, true)"));
    let refresh = &view[view.find("pub fn refresh_login_capture").unwrap()..];
    let refresh = &refresh[..refresh.find("\n    }\n").unwrap()];
    assert!(refresh.contains("forget_pending()") && refresh.contains("frame_identifiers") && refresh.contains("login::setting_message(&self.shared.profile_id)"));
    let lib = include_str!("lib.rs");
    let launch = &lib[lib.find("fn on_before_child_process_launch").unwrap()..];
    let launch = &launch[..launch.find("\n        }\n").unwrap()];
    assert!(launch.contains("!= \"renderer\" { return; }") && launch.contains("append_switch_with_value(Some(&login::SWITCH.into()), Some(&login::launch_switch().as_str().into()))"));
    let renderer = include_str!("login.rs");
    for step in ["gate.launched(&switch.unwrap_or_default());", "created(gate, browser.identifier(), extra_info.as_deref());", "let fills = install(context, allowed(browser), cards_only, cards);"] {
        assert!(renderer.contains(step), "{step}");
    }
}

// -- round 5: the document's own origin, hidden fields, the setting at load, Peek and Little Windows --

fn realm(origin: &str, secure: bool) -> Realm {
    Realm { origin: origin.to_owned(), secure }
}

/// WP1: a document whose own origin is opaque, as in a sandboxed frame or a
/// page served with `Content-Security-Policy: sandbox`, or isn't the origin
/// its address names, never gets the listener, so it never captures, notes
/// focus or is filled.
#[test]
fn only_a_document_whose_own_origin_is_its_address_origin_gets_the_listener() {
    for (url_origin, document, allowed, why) in [
        ("https://mail.example", Some(realm("https://mail.example", true)), true, "an ordinary page"),
        ("https://mail.example", Some(realm("null", true)), false, "an opaque origin: CSP sandbox, a sandboxed iframe, a popup that inherited the sandbox"),
        ("https://mail.example", Some(realm("null", false)), false, "opaque and insecure"),
        ("https://mail.example", Some(realm("https://other.example", true)), false, "another origin than the address's"),
        ("https://mail.example", Some(realm("https://mail.example:8443", true)), false, "another port"),
        ("https://mail.example", Some(realm("https://mail.example", false)), false, "an https address that isn't a secure context"),
        ("http://intranet.example", Some(realm("http://intranet.example", false)), true, "http is never a secure context, and was saved before"),
        ("http://intranet.example", Some(realm("null", false)), false, "an opaque http document"),
        ("https://mail.example", None, false, "a context whose origin can't be read"),
    ] {
        assert_eq!(realm_allows(url_origin, document.as_ref()), allowed, "{why}");
    }
}

/// WP1: the origin is read natively once, when the script context is
/// created and before any page script runs, and kept where the page can't
/// reach it. A page that later redefines `window.origin` or
/// `isSecureContext` changes nothing: nothing reads them again. CEF can't
/// run here, so this is checked in the source.
#[test]
fn a_page_that_redefines_window_origin_after_load_is_still_refused() {
    let renderer = include_str!("login.rs");
    let read = &renderer[renderer.find("fn realm(context: &V8Context)").expect("the native read")..];
    let read = &read[..read.find("\n}\n").unwrap()];
    assert!(read.contains("global.value_bykey(Some(&CefString::from(\"origin\")))") && read.contains("global.value_bykey(Some(&CefString::from(\"isSecureContext\")))"));
    // The third reads the fill functions off the null-prototype object login.js returns.
    assert_eq!(renderer.matches("value_bykey(").count(), 3, "nowhere else");
    assert!(renderer.contains("let function = |key: &str| fills.value_bykey(Some(&CefString::from(key))).filter(|fill| fill.is_function() == 1);"));
    assert!(SCRIPT.contains("return { __proto__: null, login: login, address: address, card: card };"));
    let inject = &renderer[renderer.find("fn inject(").unwrap()..];
    let inject = &inject[..inject.find("\n}\n").unwrap()];
    let (snapshot, install) = (inject.find("let snapshot = realm(context);").expect("read in inject"), inject.find("install(context, allowed(browser), cards_only, cards)").unwrap());
    assert!(snapshot < install, "read before login.js, which runs before any page script");
    assert!(inject.contains("if !realm_allows(&origin, snapshot.as_ref())"), "refused before anything is installed");
    assert_eq!(renderer.matches("realm(context)").count(), 1, "read once, at creation");
    // Later decisions use the snapshot kept with the listener, never the page.
    let native = &renderer[renderer.find("fn native(name: &str").unwrap()..];
    let native = &native[..native.find("\n}\n").unwrap()];
    assert!(native.contains("installed_for(&frame, &context, &origin)"));
    let fill = &renderer[renderer.find("fn fill(frame: &Frame").unwrap()..];
    let fill = &fill[..fill.find("\n}\n").unwrap()];
    assert!(fill.contains("filler.origin != origin.expose()"));
    for needle in [".origin", "isSecureContext", "location"] {
        assert!(!SCRIPT.contains(needle), "login.js never reads {needle}");
    }
    // A snapshot of "null" refuses the document whatever it later claims.
    let at_creation = realm("null", true);
    assert!(!realm_allows("https://mail.example", Some(&at_creation)));
}

/// WP1, browser side: a `Content-Security-Policy: sandbox` response without
/// allow-same-origin gives the document an opaque origin.
#[test]
fn a_sandbox_policy_without_allow_same_origin_makes_a_document_opaque() {
    for (policies, opaque) in [
        (vec!["sandbox"], true),
        (vec!["sandbox allow-scripts allow-forms"], true),
        (vec!["default-src 'self'; SANDBOX allow-scripts"], true),
        (vec!["sandbox allow-scripts allow-same-origin"], false),
        (vec!["sandbox Allow-Same-Origin"], false),
        (vec!["default-src 'self'"], false),
        (vec!["script-src 'none', sandbox allow-popups"], true),
        (vec!["sandbox allow-same-origin", "sandbox allow-scripts"], true),
        (vec!["sandbox allow-same-origin", "frame-ancestors 'none'"], false),
        (vec!["sandboxed-thing"], false),
        (vec![], false),
    ] {
        let policies: Vec<String> = policies.into_iter().map(str::to_owned).collect();
        assert_eq!(sandboxes_origin(&policies), opaque, "{policies:?}");
    }
}

/// WP1, ported from the review's probe: a sandboxed document at the site's
/// address neither notes focus, captures nor is filled, whether the browser
/// saw its sandbox policy or its renderer said its origin is opaque.
#[test]
fn opaque_origin_documents_hold_no_logins_in_the_browser_process() {
    let t0 = Instant::now();
    let main = top("https://mail.example/inbox");
    let body = child("sandboxed-body", "https://mail.example/message/42/body", main.clone());
    let mut state = ready(&[&main, &body]);
    assert!(state.origin(&body).is_some(), "reported");
    state.noted_opaque(&body);
    assert_eq!(state.origin(&body), None, "a note that its document is opaque ends its report");
    assert!(receive(&mut state, &body, FOCUSED, note("https://mail.example"), t0, true).is_none());
    assert_eq!(state.focus(), None, "no focus note counts");
    receive(&mut state, &body, CAPTURE, capture_args("https://mail.example"), t0, true);
    assert!(!state.has_pending(), "no capture");
    assert_eq!(fill_target(&state, [Some(body.clone()), None, None], "https://mail.example").map(|frame| frame.id), None, "never filled");
    assert_eq!(fill_target(&state, [Some(body.clone()), None, Some(main.clone())], "https://mail.example").map(|frame| frame.id), Some("main"), "the page itself still is");
    // A child of an opaque document inherits its sandbox.
    let inner = child("inner", "https://mail.example/message/42/part", body.clone());
    assert_eq!(state.origin(&inner), None);

    // A main frame served with `Content-Security-Policy: sandbox`.
    let attachment = top("https://mail.example/attachments/7/view");
    let mut state = ready(&[&main]);
    state.documents().lock().unwrap().response("https://mail.example/attachments/7/view", &["sandbox allow-scripts allow-forms".to_owned()]);
    assert_eq!(state.origin(&attachment), None);
    assert_eq!(state.origin(&top("https://mail.example/attachments/7/view#page=2")), None, "a fragment is the same document");
    receive(&mut state, &attachment, FOCUSED, note("https://mail.example"), t0, true);
    receive(&mut state, &attachment, CAPTURE, capture_args("https://mail.example"), t0, true);
    assert!(state.focus().is_none() && !state.has_pending());
    assert!(fill_target(&state, [None, None, Some(attachment.clone())], "https://mail.example").is_none());
    assert_eq!(state.origin(&main).as_deref(), Some("https://mail.example"), "other documents of the site are unaffected");
    // Served again without the sandbox, or with allow-same-origin, it is an ordinary page.
    state.documents().lock().unwrap().response("https://mail.example/attachments/7/view", &["sandbox allow-scripts allow-same-origin".to_owned()]);
    assert_eq!(state.origin(&attachment).as_deref(), Some("https://mail.example"));
    // Only http(s) documents are kept, and only the latest few hundred.
    let documents = state.documents();
    let mut documents = documents.lock().unwrap();
    for i in 0..1000 { documents.response(&format!("https://flood.example/{i}"), &["sandbox".to_owned()]); }
    documents.response("data:text/html,x", &["sandbox".to_owned()]);
    assert!(documents.len() <= 256);
}

/// WP2: with no login field focused, a fill goes only into a visible,
/// enabled, non-zero, on-screen password field and its username; hidden
/// fields are never filled; with none, the page is told nothing and the
/// app asks the user to click the field. Checked in the source.
#[test]
fn a_fill_never_goes_into_a_hidden_disabled_or_off_screen_field() {
    let (prologue, body) = SCRIPT.split_once("// -- Everything below").unwrap();
    // What the rule decides is tested in a real engine (login_js_tests.rs);
    // here, that each built-in it calls was captured before any page script.
    for captured in ["Element.prototype.checkVisibility", "Element.prototype.getBoundingClientRect", "get(InputCtor.prototype, 'disabled')", "get(InputCtor.prototype, 'readOnly')", "get(win, 'innerWidth')", "get(win, 'innerHeight')", "get(DOMRectReadOnly.prototype, name)",
        "win.getComputedStyle", "CSSStyleDeclaration.prototype.getPropertyValue", "Document.prototype.elementFromPoint", "get(Node.prototype, 'parentNode')", "get(Element.prototype, 'assignedSlot')", "get(RootCtor.prototype, 'host')", "Node.prototype.contains"] {
        assert!(prologue.contains(captured), "{captured} is captured");
    }
    let visible = &body[body.find("const visible = (input) => {").expect("one rule for every field seen")..];
    let visible = &visible[..visible.find("\n  };\n").unwrap()];
    for call in ["apply(computedStyle, win, [node])", "apply(elementFromPoint, doc, [x, y])", "apply(contains, input, [hit])", "node = flatParent(node)"] {
        assert!(visible.contains(call), "{call}");
    }
    assert!(visible.contains("node !== doc;") && visible.contains("!is(ElementCtor, node)) return false;"), "the walk reaches the document or refuses");
    assert!(body.contains("const fillable = (input) => !blocked(input) && visible(input);"), "one rule for every filled field");
    let parent = &body[body.find("const flatParent = (node) => {").expect("the walk follows the flat tree")..];
    let parent = &parent[..parent.find("\n  };\n").unwrap()];
    for call in ["apply(assignedSlot, node, [])", "apply(parentNode, node, [])", "apply(host, up, [])"] {
        assert!(parent.contains(call), "{call}");
    }
    let filler = &body[body.find("const login = function (username, password, main) {").unwrap()..];
    assert!(filler.contains("if (found === null) return false;"), "nothing fillable is reported");
    assert!(filler.contains("put(found.first, password)") && filler.contains("const found = fillFields(main);"));
    assert!(body.contains("if (filling && !fillable(input)) continue;"), "fields() leaves out what can't be filled");
    let target = &body[body.find("const fillFields = (main) => {").unwrap()..];
    let target = &target[..target.find("\n  };\n").unwrap()];
    assert_eq!(target.matches("fields(").count(), 3);
    assert!(target.contains("return form === null || fields(form, false) === null ? null : fields(form, true);") && target.contains("fields(apply(formAt, all, [i]), true)"), "only fillable fields, in the focused field's form or else the one that has one");
    assert!(target.contains("if (main !== true) return null;"), "with no field focused, only the main frame fills");
    assert!(!body.contains("fields(form)"), "every caller says whether it fills");
    // The renderer reports an unfilled login; the browser takes it only from
    // the frame a fill was just sent to.
    let renderer = include_str!("login.rs");
    let fill = &renderer[renderer.find("fn fill(frame: &Frame").unwrap()..];
    let fill = &fill[..fill.find("\n}\n").unwrap()];
    assert!(fill.contains("outgoing(unfilled, &[origin.expose(), document.expose()])") && fill.contains("FILL => (Fill::Login, UNFILLED, 2),"));
    let page = top("https://login.example/");
    let other = child("same", "https://login.example/embedded", page.clone());
    let t0 = Instant::now();
    let mut state = ready(&[&page, &other]);
    assert!(!state.unfilled(&page, note("https://login.example"), t0), "no fill was sent");
    state.fill_sent(&page, t0);
    assert!(!state.unfilled(&other, note("https://login.example"), t0), "another frame");
    assert!(!state.unfilled(&page, note("https://other.example"), t0), "another origin");
    assert!(!state.unfilled(&page, args(&["https://login.example", "fixture-document-2"]), t0), "another document");
    assert!(!state.unfilled(&page, note("https://login.example"), t0 + Duration::from_secs(6)), "too late");
    state.fill_sent(&page, t0);
    assert!(state.unfilled(&page, note("https://login.example"), t0 + Duration::from_secs(1)));
    assert!(!state.unfilled(&page, note("https://login.example"), t0 + Duration::from_secs(1)), "once per fill");
    state.fill_sent(&page, t0);
    state.document_started("main", true);
    assert!(!state.unfilled(&page, note("https://login.example"), t0), "a frame whose document changed since");
}

/// WP2: the fill goes to the frame whose login field last had focus while
/// the user's focus is still in it, else to the main frame, and only to one
/// of the entry's origin. A subframe the user's focus is in, without a
/// login field focused, gets nothing (the sixth recheck's remainder).
#[test]
fn a_fill_goes_to_the_focused_login_frame_only_while_focus_is_in_it_else_the_page() {
    let page = top("https://login.example/");
    let same = child("same", "https://login.example/embedded", page.clone());
    let widget = child("widget", "https://widget.example/", page.clone());
    let state = ready(&[&page, &same, &widget]);
    let id = |frame: Option<Fake>| frame.map(|frame| frame.id);
    assert_eq!(id(fill_target(&state, [Some(same.clone()), Some(same.clone()), Some(page.clone())], "https://login.example")), Some("same"));
    assert_eq!(id(fill_target(&state, [Some(same.clone()), Some(page.clone()), Some(page.clone())], "https://login.example")), Some("main"), "the user's focus left the frame");
    assert_eq!(id(fill_target(&state, [Some(same.clone()), None, Some(page.clone())], "https://login.example")), Some("main"), "no frame has the user's focus");
    assert_eq!(id(fill_target(&state, [None, Some(same.clone()), Some(page.clone())], "https://login.example")), Some("main"), "a focused frame without a login field focused");
    assert_eq!(id(fill_target(&state, [None, Some(widget.clone()), Some(page.clone())], "https://login.example")), Some("main"), "a cross-origin focused frame is skipped");
    let view = include_str!("view.rs");
    let fill_login = &view[view.find("pub fn fill_login").unwrap()..];
    let fill_login = &fill_login[..fill_login.find("\n    }\n").unwrap()];
    assert!(fill_login.contains("login::fill_target(&state, [focused, browser.focused_frame(), browser.main_frame()], origin)"));
    assert!(fill_login.contains("state.fill_sent(&frame, std::time::Instant::now())"));
    assert!(fill_login.contains("let Some((_, document)) = state.current(&frame) else { return false };") && fill_login.contains("login::outgoing(login::FILL, &[origin, &document, username, password])"), "the fill names the document the frame was chosen for");
}

/// WP3: every frame's renderer learns the setting when its document starts
/// loading, so a renderer launched while it was on, which missed the change,
/// knows it by the time of a submit; the renderer checks it at submit; the
/// browser drops a capture before reading its values, and the renderer never
/// sends a value the vault couldn't hold.
#[test]
fn every_loading_frame_learns_the_setting_and_a_capture_is_dropped_unread_while_off() {
    let view = include_str!("view.rs");
    let load_start = &view[view.find("fn on_load_start(").unwrap()..];
    let load_start = &load_start[..load_start.find("\n        }\n").unwrap()];
    assert!(load_start.contains("self.h.0.tell_login_setting(frame);"));
    let tell = &view[view.find("fn tell_login_setting(").unwrap()..];
    let tell = &tell[..tell.find("\n    }\n").unwrap()];
    assert!(tell.contains("login::setting_message(&self.profile_id)") && tell.contains("frame.send_process_message(ProcessId::RENDERER"));
    let message = &view[view.find("fn login_message(").unwrap()..];
    let message = &message[..message.find("\n    }\n").unwrap()];
    let (drop_at, decode_at) = (message.find("if login::carries_login(&name) && !(self.saves_logins && login::login_capture(&self.profile_id)) { return true; }").expect("dropped first"), message.find("login::string_args(message)").unwrap());
    assert!(drop_at < decode_at, "before any value is copied out of CEF's message");
    assert!(carries_login(CAPTURE) && carries_login(FOLLOWED) && !carries_login(FOCUSED) && !carries_login(OPAQUE) && !carries_login(UNFILLED) && !carries_login(INSTALLED));
    assert!(carries_login(ADDRESS_CAPTURE) && !carries_login(ADDRESS_FOCUSED) && !carries_login(ADDRESS_UNFILLED));
    let renderer = include_str!("login.rs");
    let native = &renderer[renderer.find("fn native(name: &str").unwrap()..];
    let native = &native[..native.find("\n}\n").unwrap()];
    let (gate, send) = (native.find("if name != \"focused\" && name != \"addressFocused\" && name != \"cardFocused\" && name != \"addressFilled\" && name != \"cardFilled\" && !allowed(context.browser()) { return; }").unwrap(), native.find("outgoing(CAPTURE").unwrap());
    assert!(gate < send, "checked at submit, with the newest generation this renderer holds");
    assert!(native.contains("username.len() > MAX_USERNAME_BYTES || password.len() > MAX_PASSWORD_BYTES"), "an over-long value never crosses");
}

/// WP5: Peek and Little Windows never get the capture functions, whatever
/// the setting, and the browser drops anything a renderer sends for them.
#[test]
fn views_that_never_offer_never_install_capture() {
    let profile = fresh_profile("fixture-surface");
    set_login_capture(&profile, true);
    let (generation, on) = capture_setting(&profile);
    let mut gate = Gate::default();
    gate.launched(&launch_switch());
    gate.browser_created(1, &profile, generation, on, true);
    gate.browser_created(2, &profile, generation, on, false);
    assert!(gate.allows(1), "a tab");
    assert!(!gate.allows(2), "Peek or a Little Window, though the profile is on at launch and creation");
    gate.learn(&profile, generation + 1, true);
    assert!(!gate.allows(2), "and after any later message");
    set_login_capture(&profile, false);
    let view = include_str!("view.rs");
    assert!(view.contains("let mut info = login::creation_info(&shared.profile_id, shared.saves_logins);"));
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("text(INFO_SAVES) == \"1\""), "a missing value never captures");
}

/// R5-2, the review's first escape: an opaque document never reports, so a
/// same-document change of its address (pushState or replaceState may change
/// the query) gives it nothing, whatever either record names.
#[test]
fn an_opaque_main_frame_that_changes_its_address_still_holds_no_logins() {
    let t0 = Instant::now();
    let attachment = top("https://mail.example/attachments/7/view");
    let mut state = LoginState::default();
    state.documents().lock().unwrap().response(attachment.url, &["sandbox allow-scripts".to_owned()]);
    receive(&mut state, &attachment, OPAQUE, args(&["https://mail.example"]), t0, true);
    for url in ["https://mail.example/attachments/7/view?x=1", "https://mail.example/attachments/7/other"] {
        let pushed = top(url);
        assert_eq!(state.origin(&pushed), None, "{url}");
        assert_eq!(fill_target(&state, [Some(pushed.clone()), Some(pushed.clone()), Some(pushed.clone())], "https://mail.example").map(|f| f.id), None, "{url}: never the fill target");
    }
}

/// R5-2, the second escape: pushing the opaque main frame's records out with
/// 129 sandboxed same-site parts uncovers nothing.
#[test]
fn flooding_the_records_never_uncovers_an_opaque_main_frame() {
    let t0 = Instant::now();
    let main = top("https://mail.example/attachments/7/view");
    let mut state = LoginState::default();
    state.documents().lock().unwrap().response(main.url, &["sandbox allow-scripts".to_owned()]);
    receive(&mut state, &main, OPAQUE, args(&["https://mail.example"]), t0, true);
    for n in 0..=DOCUMENTS_KEPT {
        let url: &'static str = Box::leak(format!("https://mail.example/attachments/7/view?part={n}").into_boxed_str());
        let id: &'static str = Box::leak(format!("part-{n}").into_boxed_str());
        state.documents().lock().unwrap().response(url, &["sandbox allow-scripts".to_owned()]);
        receive(&mut state, &child(id, url, main.clone()), OPAQUE, args(&["https://mail.example"]), t0, true);
    }
    assert_eq!(state.origin(&main), None, "the opaque main frame stays without logins");
    assert!(fill_target(&state, [None, None, Some(main.clone())], "https://mail.example").is_none());
    // Nor does reporting a page then flooding: a report is only ever lost.
    let page = top("https://mail.example/inbox");
    let mut state = ready(&[&page]);
    for n in 0..=FRAMES_KEPT {
        let id: &'static str = Box::leak(format!("part-{n}").into_boxed_str());
        receive(&mut state, &child(id, "https://mail.example/part", page.clone()), INSTALLED, reported("https://mail.example", DOC), t0, true);
    }
    assert_eq!(state.origin(&page), None, "the oldest report is pushed out, and the page holds no logins");
    assert!(state.origin(&child("part-128", "https://mail.example/part", page.clone())).is_some());
}

/// R5-2, the third escape: a frame sandboxed by its parent's attribute, in
/// its own process, can't see its parent. Its renderer installs nothing
/// for logins, and since round 11 (C10-4) defers only a card's ancestors
/// to the browser; its document's opaque origin still stops it before any
/// report (see `a_cross_site_frame_whose_parent_the_renderer_cant_see...`).
/// It is never the fill target.
#[test]
fn a_frame_whose_renderer_never_reports_is_never_filled() {
    let main = top("https://mail.example/inbox");
    let body = child("sandboxed-body", "https://mail.example/message/42/body", main.clone());
    let state = LoginState::default();
    assert_eq!(state.origin(&body), None);
    assert_eq!(fill_target(&state, [Some(body.clone()), Some(body.clone()), None], "https://mail.example").map(|f| f.id), None);
    // With its page reported, the fill goes to the page instead.
    let state = ready(&[&main]);
    assert_eq!(fill_target(&state, [Some(body.clone()), Some(body.clone()), Some(main.clone())], "https://mail.example").map(|f| f.id), Some("main"));
    let renderer = include_str!("login.rs");
    let inject = &renderer[renderer.find("fn inject(browser").unwrap()..];
    let inject = &inject[..inject.find("\n}\n").unwrap()];
    let at = |needle: &str| inject.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("let Some(origin) = renderer_listener_origin(frame) else { return };") < at("report(frame, &origin, &document);"), "a renderer with no origin for the frame returns before reporting");
    assert!(at("if !realm_allows(&origin, snapshot.as_ref()) {") < at("report(frame, &origin, &document);"), "and so does an opaque document, wherever its parent is");
}

/// R5-2: the allowlist. A frame holds logins only while the report its
/// renderer sent for its current document stands: for the origin the browser
/// sees, never for a document known to be opaque, and only until the next
/// document starts in the frame, or a new page in the view. A same-document
/// change of address keeps it, since it can't change the origin.
#[test]
fn a_frame_holds_logins_only_while_its_current_documents_report_stands() {
    let t0 = Instant::now();
    let page = top("https://login.example/");
    let same = child("same", "https://login.example/embedded", page.clone());
    let report = |state: &mut LoginState, frame: &Fake, values: &[&str]| {
        receive(state, frame, INSTALLED, args(&[values, &["0", "0"][..]].concat()), t0, true);
        state.current(frame)
    };
    let mut state = LoginState::default();
    assert_eq!(state.origin(&page), None, "nothing before a report");
    let long = "d".repeat(65);
    for (case, values) in [
        ("another origin", vec!["https://other.example", DOC]),
        ("http for an https page", vec!["http://login.example", DOC]),
        ("no document", vec!["https://login.example"]),
        ("an empty document id", vec!["https://login.example", ""]),
        ("an over-long document id", vec!["https://login.example", &long]),
        ("an extra argument", vec!["https://login.example", DOC, "x"]),
    ] {
        assert_eq!(report(&mut state, &page, &values), None, "{case}");
    }
    let long = "9".repeat(21);
    for (case, values) in [
        ("no note counts", vec!["https://login.example", DOC]),
        ("a note count that isn't digits", vec!["https://login.example", DOC, "-1", "0"]),
        ("an over-long note count", vec!["https://login.example", DOC, "0", &long]),
    ] {
        receive(&mut state, &page, INSTALLED, args(&values), t0, true);
        assert_eq!(state.current(&page), None, "{case}");
    }
    let held = Some(("https://login.example".to_owned(), DOC.to_owned()));
    assert_eq!(report(&mut state, &page, &["https://login.example", DOC]), held);
    // What CEF's OnAddressChange gives for pushState, replaceState or a fragment.
    assert!(state.navigated("main", true, t0).is_none());
    assert_eq!(state.current(&top("https://login.example/account?tab=2#security")), held, "the same document at a new address");
    // A wrong report ends a good one.
    assert_eq!(report(&mut state, &page, &["https://other.example", DOC]), None);
    assert_eq!(state.origin(&page), None, "fails closed");
    report(&mut state, &page, &["https://login.example", DOC]);
    report(&mut state, &same, &["https://login.example", "fixture-document-2"]);
    state.document_started("same", false);
    assert_eq!((state.origin(&same), state.origin(&page).is_some()), (None, true), "a load in a frame ends only its report");
    assert_eq!(report(&mut state, &same, &["https://login.example", "fixture-document-3"]).map(|(_, document)| document).as_deref(), Some("fixture-document-3"));
    state.document_started("main", true);
    assert_eq!((state.origin(&same), state.origin(&page)), (None, None), "a new page ends every report");
    // A document known to be opaque, or below one, is never taken.
    state.documents().lock().unwrap().response("https://login.example/", &["sandbox allow-scripts".to_owned()]);
    assert_eq!(report(&mut state, &page, &["https://login.example", DOC]), None);
    assert_eq!(report(&mut state, &same, &["https://login.example", DOC]), None, "a child inherits the sandbox");
}

/// R5-2, the renderer's half, checked in the source since CEF can't run
/// here: it reports a document only once `login.js` loaded in it, with an id
/// of its own; every message and fill carries that id; and it answers each
/// setting, which the browser sends after forgetting a frame's report at a
/// load, with its live listener's report.
#[test]
fn the_renderer_reports_each_document_it_installs_in_and_checks_its_id_on_a_fill() {
    let renderer = include_str!("login.rs");
    let section = |start: &str| { let rest = &renderer[renderer.find(start).unwrap()..]; &rest[..rest.find("\n}\n").unwrap()] };
    let inject = section("fn inject(browser");
    let at = |needle: &str| inject.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("fillers.remove(&id)") < at("let Some(origin) = renderer_listener_origin(frame) else { return };"), "a new context ends the frame's last listener, whatever it holds");
    assert!(at("if !realm_allows(&origin, snapshot.as_ref())") < at("let Some((fill, address, card)) = fills else { return };"));
    assert!(at("let Some((fill, address, card)) = fills else { return };") < at("report(frame, &origin, &document);"), "a script that didn't load is never reported");
    assert!(inject.contains("format!(\"{}.{}\", std::process::id(), next.get())"), "an id no other document of this renderer has");
    let native = section("fn native(name: &str");
    for message in ["outgoing(CAPTURE, &[&origin, &document, username.expose(), password.expose()])", "outgoing(FOLLOWED, &[&origin, &document])", "outgoing(FOCUSED, &[&origin, &document])"] {
        assert!(native.contains(message), "{message}");
    }
    let fill = section("fn fill(frame: &Frame");
    assert!(fill.contains("let [origin, document, rest @ ..] = args.as_slice() else { return true };") && fill.contains("if rest.len() != values ||"));
    assert!(fill.contains("if filler.origin != origin.expose() || filler.document != document.expose() { return None; }"), "only into the document the browser chose");
    assert!(fill.contains("v8_value_create_bool(frame.is_main())"), "login.js learns whether it fills the main frame, where only a focused login form is filled otherwise");
    let again = section("fn report_again(frame: &Frame)");
    assert!(again.contains("filler.context.is_valid() == 1") && again.contains("if renderer_listener_origin(frame).as_deref() == Some(origin.as_str()) { report(frame, &origin, &document); }"));
    let handler = &renderer[renderer.find("fn on_process_message_received(").unwrap()..];
    assert!(handler.contains("if setting(message) {\n                report_again(frame);"));
    let view = include_str!("view.rs");
    let load_start = &view[view.find("fn on_load_start(").unwrap()..];
    let load_start = &load_start[..load_start.find("\n        }\n").unwrap()];
    assert!(load_start.find("self.h.0.login_document_started(frame);").unwrap() < load_start.find("self.h.0.tell_login_setting(frame);").unwrap(), "forgotten before the setting the renderer answers");
    let address = &view[view.find("fn on_address_change(").unwrap()..];
    let address = &address[..address.find("\n        }\n").unwrap()];
    assert!(!address.contains("login_document_started"), "a same-document change keeps the report");
}

// ---------------------------------------------------------------------------
// Addresses (ASP-P17)
// ---------------------------------------------------------------------------

/// A synthetic address's parts, in `ADDRESS_PARTS` order.
const PARTS: [&str; ADDRESS_PARTS] = ["Fixture Person", "", "1 Test Street\nUnit 2", "Testville", "", "90000", "United States", "", ""];

fn address_args(origin: &str, parts: &[&str; ADDRESS_PARTS]) -> Vec<Secret> {
    let mut values = vec![origin, DOC];
    values.extend(parts);
    args(&values)
}

/// ASP-P17: a submitted address is reported once a navigation or a request
/// follows it, like a login, only from a frame that installed the script for
/// its origin, only with capture on, and only with a street and a city or
/// postal code within the vault's limits. Its Debug shows nothing.
#[test]
fn a_submitted_address_is_reported_once_after_a_follow_and_only_when_shaped() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let mut state = ready(&[&page]);
    assert!(receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", &PARTS), now, true).is_none());
    assert!(state.take_followed_address().is_none(), "nothing before a follow");
    receive(&mut state, &page, FOLLOWED, note("https://shop.example"), now + Duration::from_secs(1), true);
    let address = state.take_followed_address().expect("offered");
    assert_eq!((address.origin.as_str(), address.parts[2].expose(), address.parts[5].expose()), ("https://shop.example", "1 Test Street\nUnit 2", "90000"));
    assert_eq!(format!("{address:?}"), "PageAddress(..)", "no part, and not the origin, in Debug output");
    assert!(state.take_followed_address().is_none(), "exactly once");
    // A navigation delivers it too, and a new page's focus is forgotten.
    receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", &PARTS), now, true);
    state.navigated("main", true, now);
    assert!(state.take_followed_address().is_some());
    // Nothing that follows too late, and nothing with capture off.
    receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", &PARTS), now, true);
    receive(&mut state, &page, FOLLOWED, note("https://shop.example"), now + FOLLOW_WINDOW, true);
    assert!(state.take_followed_address().is_none(), "expired");
    receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", &PARTS), now, false);
    state.navigated("main", true, now);
    assert!(state.take_followed_address().is_none(), "capture off");
    // Turning capture off wipes one waiting for its follow.
    receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", &PARTS), now, true);
    state.forget_pending();
    state.navigated("main", true, now);
    assert!(state.take_followed_address().is_none(), "forgotten");
    let shapes: [(&str, [&str; ADDRESS_PARTS]); 4] = [
        ("no street", ["Fixture Person", "", " ", "Testville", "", "90000", "", "", ""]),
        ("no city or postal code", ["Fixture Person", "", "1 Test Street", "", "California", "", "", "", ""]),
        ("a street over the limit", ["", "", &"s".repeat(1025), "Testville", "", "", "", "", ""]),
        ("a name over the limit", [&"n".repeat(257), "", "1 Test Street", "Testville", "", "", "", "", ""]),
    ];
    for (case, parts) in &shapes {
        receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", parts), now, true);
        state.navigated("main", true, now);
        assert!(state.take_followed_address().is_none(), "{case}");
    }
    receive(&mut state, &page, ADDRESS_CAPTURE, address_args("https://shop.example", &["", "", "1 Test Street", "", "", "90000", "", "", ""]), now, true);
    state.navigated("main", true, now);
    assert!(state.take_followed_address().is_some(), "a street and a postal code are enough");
    assert!(MAX_ADDRESS_PART_BYTES[2] == 1024 && MAX_ADDRESS_PART_BYTES.iter().enumerate().all(|(i, limit)| i == 2 || *limit == 256));
}

/// ASP-P17: only a frame that installed the script for the origin the browser
/// sees counts. A cross-origin frame, a frame whose document is opaque, and a
/// message naming another origin or document never deliver an address or
/// set the address focus; nor does a frame's own follow deliver another's.
#[test]
fn an_address_is_taken_only_from_a_frame_that_installed_the_script() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let widget = child("widget", "https://pay.example/address", page.clone());
    let same = child("same", "https://shop.example/address", page.clone());
    let mut state = ready(&[&page, &widget, &same]);
    for (frame, origin, case) in [(&widget, "https://pay.example", "cross-origin frame"), (&page, "https://other.example", "another origin")] {
        receive(&mut state, frame, ADDRESS_FOCUSED, numbered(origin, 1), now, true);
        receive(&mut state, frame, ADDRESS_CAPTURE, address_args(origin, &PARTS), now, true);
        receive(&mut state, frame, FOLLOWED, note(origin), now, true);
        assert!(state.address_focus().is_none() && state.take_followed_address().is_none(), "{case}");
    }
    let mut other_document = address_args("https://shop.example", &PARTS);
    other_document[1] = Secret::new("fixture-document-2".into());
    receive(&mut state, &page, ADDRESS_CAPTURE, other_document, now, true);
    receive(&mut state, &page, FOLLOWED, note("https://shop.example"), now, true);
    assert!(state.take_followed_address().is_none(), "another document");
    receive(&mut state, &same, ADDRESS_CAPTURE, address_args("https://shop.example", &PARTS), now, true);
    receive(&mut state, &page, FOLLOWED, note("https://shop.example"), now, true);
    assert!(state.take_followed_address().is_none(), "the main frame's request doesn't follow a subframe's submit");
    receive(&mut state, &same, ADDRESS_FOCUSED, numbered("https://shop.example", 1), now, true);
    assert_eq!(state.address_focus(), Some(("same", "https://shop.example")));
    state.noted_opaque(&same);
    assert_eq!(state.address_focus(), None, "an opaque document's focus is forgotten");
    state.navigated("same", false, now);
    assert!(state.take_followed_address().is_none(), "and its submit");
}

/// ASP-P17: a fill of an address goes, like a login, to the frame whose
/// address field has focus while it still has CEF's focus, else the main
/// frame, whichever installed the script for the origin; never a
/// cross-origin frame, even for an address the user picked there. Only a
/// recent address fill, to that frame, document and origin, reports that
/// nothing was filled, and a login fill's report is told apart from it.
#[test]
fn a_fill_of_an_address_goes_only_to_a_frame_that_installed_it() {
    let t0 = Instant::now();
    let page = top("https://shop.example/");
    let same = child("same", "https://shop.example/address", page.clone());
    let widget = child("widget", "https://pay.example/address", page.clone());
    let mut state = ready(&[&page, &same, &widget]);
    receive(&mut state, &same, ADDRESS_FOCUSED, numbered("https://shop.example", 1), t0, true);
    let id = |frame: Option<Fake>| frame.map(|frame| frame.id);
    assert_eq!(id(fill_target(&state, [Some(same.clone()), Some(same.clone()), Some(page.clone())], "https://shop.example")), Some("same"));
    assert_eq!(id(fill_target(&state, [Some(same.clone()), None, Some(page.clone())], "https://shop.example")), Some("main"), "focus moved on: the main frame");
    assert_eq!(id(fill_target(&state, [Some(widget.clone()), Some(widget.clone()), Some(page.clone())], "https://pay.example")), None, "a cross-origin frame never");
    state.address_fill_sent(&same, t0);
    assert!(!state.unfilled(&same, note("https://shop.example"), t0), "a login fill wasn't sent");
    assert!(!state.address_unfilled(&page, note("https://shop.example"), t0), "another frame");
    assert!(!state.address_unfilled(&same, note("https://shop.example"), t0 + Duration::from_secs(6)), "too late");
    assert!(state.address_unfilled(&same, note("https://shop.example"), t0 + Duration::from_secs(1)));
    assert!(!state.address_unfilled(&same, note("https://shop.example"), t0 + Duration::from_secs(1)), "once");
}

/// ASP-P17: the values a fill sends are split in the browser, so `login.js`
/// never splits a saved value with a method the page could replace.
#[test]
fn an_address_fill_splits_the_name_and_street_in_the_browser() {
    let values = |parts: &[&str; ADDRESS_PARTS]| address_values(parts).iter().map(|value| value.expose().to_owned()).collect::<Vec<_>>();
    let full = ["Ada Fixture Person", "Org", "1 Test Street\n\n Unit 2 \nBuilding 3\nWing 4", "City", "Region", "00000", "Country", "+1", "a@example.test"];
    assert_eq!(values(&full), ["Ada Fixture Person", "Ada Fixture", "Person", "Org", "1 Test Street\nUnit 2\nBuilding 3\nWing 4", "1 Test Street, Unit 2, Building 3, Wing 4",
        "1 Test Street", "Unit 2", "Unit 2, Building 3, Wing 4", "Building 3, Wing 4", "City", "Region", "00000", "Country", "+1", "a@example.test"]);
    let short = ["Mononym", "", "1 Test Street", "", "", "", "", "", ""];
    assert_eq!(values(&short)[..10], ["Mononym", "Mononym", "", "", "1 Test Street", "1 Test Street", "1 Test Street", "", "", ""]);
}

/// ASP-P17, the renderer's half, read from the source: the address capture
/// is gated like a login's, the focus note is always installed, and a fill
/// of an address carries the document and only reaches its own function.
#[test]
fn the_renderer_gates_address_capture_like_a_login_and_reports_an_unfilled_address() {
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("let always = *name == \"focused\" || *name == \"addressFocused\" || *name == \"addressFilled\";"));
    assert!(renderer.contains("if name != \"focused\" && name != \"addressFocused\" && name != \"cardFocused\" && name != \"addressFilled\" && name != \"cardFilled\" && !allowed(context.browser()) { return; }"), "addressCapture is checked against the setting at each call");
    assert!(renderer.contains("let Some(parts) = parts.filter(|parts| address_shaped(parts)) else { return };"), "nothing the vault couldn't hold crosses");
    assert!(renderer.contains("FILL_ADDRESS => (Fill::Address, ADDRESS_UNFILLED, ADDRESS_VALUES),"));
    assert!(renderer.contains("let frame_origin = if kind == Fill::Card { renderer_card_origin(frame) } else { eligible_origin(frame) };\n    if rest.len() != values || frame_origin.as_deref() != Some(origin.expose()) { return true; }"));
    assert!(carries_login(ADDRESS_CAPTURE), "dropped by the browser while capture is off");
    let view = include_str!("view.rs");
    let fill = &view[view.find("pub fn fill_address_at(").unwrap()..];
    let fill = &fill[..fill.find("\n    }\n").unwrap()];
    assert!(fill.contains("login::address_frozen_frame(&state, browser.identifier(), [focused, browser.focused_frame(), browser.main_frame()], target)") && fill.contains("state.address_fill_sent(&frame, std::time::Instant::now())"));
    assert!(fill.contains("let mut args = vec![target.origin(), target.document(), note.as_str()];") && !view.contains("pub fn fill_address("), "no address fill re-derives its target at the choice");
    let aim = &renderer[renderer.find("fn address_aim<").unwrap()..];
    assert!(aim[..aim.find("\n}\n").unwrap()].contains("let frame = [here, main].into_iter().flatten().find(|frame| state.origin(frame).as_deref() == Some(origin.as_str()))?;\n    let (_, document) = state.current(&frame)?;"));
}

// -- Round 11: an address fill goes only where its menu was opened (C10-1) --------------------

/// C10-1 for addresses, whose menu re-derived its target every frame as the
/// card menu did. An address isn't bound to a site, so a page that
/// navigated to another site, whose own field then got focus, would have
/// had it. The address menu now freezes its fill the same way: the browser,
/// frame, document, origin, the frame of the latest address note and the
/// frame with focus; any change voids it and nothing is sent. Since round
/// 13 (C11-1) address notes are numbered as card notes are, so a newer
/// note in the same document, moving it to another of that page's own
/// forms, voids it too.
#[test]
fn an_address_fill_goes_only_where_its_menu_was_opened() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let same = child("same", "https://shop.example/address", page.clone());
    let billing = child("billing", "https://shop.example/billing", page.clone());
    let tree = [&page, &same, &billing];
    let opened = || {
        let mut state = ready(&tree);
        receive(&mut state, &same, ADDRESS_FOCUSED, numbered("https://shop.example", 1), now, true);
        state
    };
    let aimed = |state: &LoginState, focused: &Fake| {
        let noted = state.address_focus().and_then(|(id, _)| tree.iter().find(|frame| frame.id == id).map(|frame| (*frame).clone()));
        [noted, Some(focused.clone()), Some(page.clone())]
    };
    let state = opened();
    let target = address_menu_target(&state, 1, aimed(&state, &same)).expect("the menu opens on the frame's address form");
    assert_eq!(target.origin(), "https://shop.example");
    let goes = |state: &LoginState, frames: [Option<Fake>; 3]| address_frozen_frame(state, 1, frames, &target).map(|frame| frame.id);
    let elsewhere = top("https://elsewhere.example/");
    let mut navigated = opened();
    navigated.navigated("main", true, now);
    navigated.document_started("main", true);
    receive(&mut navigated, &elsewhere, INSTALLED, reported("https://elsewhere.example", "fixture-document-2"), now, true);
    receive(&mut navigated, &elsewhere, ADDRESS_FOCUSED, args(&["https://elsewhere.example", "fixture-document-2", "1"]), now, true);
    let mut reloaded = opened();
    reloaded.document_started("same", false);
    receive(&mut reloaded, &same, INSTALLED, reported("https://shop.example", "fixture-document-2"), now, true);
    receive(&mut reloaded, &same, ADDRESS_FOCUSED, args(&["https://shop.example", "fixture-document-2", "1"]), now, true);
    let mut moved = opened();
    receive(&mut moved, &billing, ADDRESS_FOCUSED, numbered("https://shop.example", 1), now, true);
    let mut within = opened();
    receive(&mut within, &same, ADDRESS_FOCUSED, numbered("https://shop.example", 2), now, true);
    let wrong: Vec<String> = [
        ("nothing changed: the address goes to the frame's form", goes(&state, aimed(&state, &same)), Some("same")),
        ("the page navigated to another site, whose own address field has focus", goes(&navigated, [Some(elsewhere.clone()), Some(elsewhere.clone()), Some(elsewhere.clone())]), None),
        ("a reload of the frame: the same origin, another document", goes(&reloaded, aimed(&reloaded, &same)), None),
        ("focus moved to another frame's address form, whose note follows", goes(&moved, aimed(&moved, &billing)), None),
        ("focus moved to another address form of the same frame, whose note follows", goes(&within, aimed(&within, &same)), None),
        ("focus moved to the page, before or without a note", goes(&state, aimed(&state, &page)), None),
        ("the frame was removed from the page", goes(&state, [None, None, Some(page.clone())]), None),
        ("the page's view was replaced by another browser", address_frozen_frame(&state, 2, aimed(&state, &same), &target).map(|frame| frame.id), None),
    ].into_iter().filter(|(_, got, want)| got != want).map(|(case, got, want)| format!("{case}: went to {got:?}, expected {want:?}")).collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

// -- payment cards (ASP-P16) -------------------------------------------------------------------

/// ASP-P16: a card may go only to an https frame, or an http one on this
/// computer's loopback address, whose ancestors are each one too. Unlike a
/// login, a frame of another site qualifies, as a payment processor's card
/// fields do; never an http page, a frame inside one, file: or data:.
#[test]
fn a_card_may_go_only_to_an_https_or_loopback_frame_inside_one() {
    let page = top("https://shop.example/checkout");
    let http = top("http://shop.example/checkout");
    for (frame, origin) in [
        (page.clone(), Some("https://shop.example")),
        (child("pay", "https://pay.example/fields", page.clone()), Some("https://pay.example")),
        (child("nested", "https://pay.example/inner", child("pay", "https://pay.example/fields", page.clone())), Some("https://pay.example")),
        (top("http://127.0.0.1:8080/fixture"), Some("http://127.0.0.1:8080")),
        (top("http://localhost:3000/"), Some("http://localhost:3000")),
        (top("http://[::1]:8443/"), Some("http://[::1]:8443")),
        (child("pay", "http://127.0.0.1:9090/fields", top("http://127.0.0.1:8080/")), Some("http://127.0.0.1:9090")),
        (http.clone(), None),
        (child("pay", "https://pay.example/fields", http.clone()), None),
        (child("pay", "http://127.0.0.1:9090/fields", http.clone()), None),
        (top("http://127.0.0.2/"), None),
        (top("http://localhost.example/"), None),
        (top("file:///tmp/checkout.html"), None),
        (top("data:text/html,checkout"), None),
        (Fake { id: "orphan", url: "https://pay.example/", main: false, parent: None }, None),
    ] {
        assert_eq!(card_origin(&frame).as_deref(), origin, "{}", frame.url);
    }
}

/// ASP-P16: the renderer gives `login.js` the card note only for a document
/// that may take a card: a secure context whose own origin, read before any
/// page script, is its frame's card origin. An http page, a document that
/// isn't a secure context, an opaque or another origin get none, and
/// login.js then never fills a card (see the JavaScriptCore test
/// `a_document_installed_without_the_card_note_never_takes_a_card`).
#[test]
fn a_card_goes_only_to_a_secure_https_or_loopback_document() {
    let page = top("https://shop.example/checkout");
    for (frame, document, takes, why) in [
        (page.clone(), Some(realm("https://shop.example", true)), true, "an https page"),
        (child("pay", "https://pay.example/fields", page.clone()), Some(realm("https://pay.example", true)), true, "a processor's frame"),
        (top("http://127.0.0.1:8080/"), Some(realm("http://127.0.0.1:8080", true)), true, "a loopback fixture"),
        (top("http://localhost:3000/"), Some(realm("http://localhost:3000", true)), true, "a loopback fixture by name"),
        (top("http://[::1]:8443/"), Some(realm("http://[::1]:8443", true)), true, "an IPv6 loopback fixture"),
        (top("http://shop.example/"), Some(realm("http://shop.example", false)), false, "an http page"),
        (top("http://shop.example/"), Some(realm("http://shop.example", true)), false, "an http page, whatever it says"),
        (page.clone(), Some(realm("https://shop.example", false)), false, "not a secure context"),
        (page.clone(), Some(realm("null", true)), false, "an opaque origin"),
        (page.clone(), Some(realm("https://other.example", true)), false, "another origin than the address's"),
        (top("http://127.0.0.2/"), Some(realm("http://127.0.0.2", true)), false, "not a loopback name Crossbow takes"),
        (top("http://localhost.example/"), Some(realm("http://localhost.example", true)), false, "a look-alike host"),
        (page.clone(), None, false, "an origin that can't be read"),
    ] {
        assert_eq!(takes_cards(&frame, document.as_ref()), takes, "{why}");
    }
}

/// ASP-P16: a frame of another site that installed the script counts for
/// cards only: its card note is taken, every login and address message from
/// it is still dropped, and it is never a login's fill target.
#[test]
fn another_sites_frame_reports_for_cards_only() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let mut state = ready(&[&page, &pay]);
    assert_eq!(state.current(&pay), None, "no logins or addresses there");
    assert_eq!(state.card_current(&pay), Some(("https://pay.example".into(), DOC.into())));
    receive(&mut state, &pay, FOCUSED, note("https://pay.example"), now, true);
    receive(&mut state, &pay, ADDRESS_FOCUSED, numbered("https://pay.example", 1), now, true);
    receive(&mut state, &pay, CAPTURE, capture_args("https://pay.example"), now, true);
    assert!(state.focus().is_none() && state.address_focus().is_none() && !state.has_pending());
    assert_eq!(fill_target(&state, [Some(pay.clone()), Some(pay.clone()), Some(page.clone())], "https://pay.example").map(|frame| frame.id), None);
    receive(&mut state, &pay, CARD_FOCUSED, card_note("https://shop.example"), now, true);
    assert_eq!(state.card_focus(), None, "a note naming another origin");
    receive(&mut state, &pay, CARD_FOCUSED, args(&["https://pay.example", "fixture-document-2", "1"]), now, true);
    assert_eq!(state.card_focus(), None, "a note naming another document");
    receive(&mut state, &pay, CARD_FOCUSED, card_note("https://pay.example"), now, true);
    assert_eq!(state.card_focus(), Some(("pay", "https://pay.example")));
    state.document_started("pay", false);
    assert_eq!(state.card_focus(), None, "a new document in the frame forgets it");
    // An http page's frames take no card at all.
    let http = top("http://shop.example/");
    let mut state = ready(&[&http]);
    receive(&mut state, &http, CARD_FOCUSED, card_note("http://shop.example"), now, true);
    assert_eq!((state.card_current(&http), state.card_focus()), (None, None));
    // Nor does an opaque document.
    let mut state = ready(&[&page, &pay]);
    state.noted_opaque(&pay);
    assert_eq!(state.card_current(&pay), None);
}

/// ASP-P16: a card fill goes to the frame with the user's focus while its
/// document has noted a card field, else the main frame (C11-2), and only
/// to one of the origin the menu named; a processor's frame that lost focus
/// gets nothing, and the page doesn't get the card in its place. Only a
/// recent card fill reports that nothing was filled, told apart from the
/// others.
#[test]
fn a_card_fill_goes_to_the_focused_card_frame_while_it_has_focus_else_the_page() {
    let t0 = Instant::now();
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let other = child("ad", "https://ads.example/", page.clone());
    let mut state = ready(&[&page, &pay, &other]);
    receive(&mut state, &pay, CARD_FOCUSED, numbered("https://pay.example", 1), t0, true);
    let id = |target: Option<(Fake, String)>| target.map(|(frame, document)| (frame.id, document));
    assert_eq!(id(card_fill_target(&state, [Some(pay.clone()), Some(pay.clone()), Some(page.clone())], "https://pay.example")), Some(("pay", DOC.into())));
    assert_eq!(id(card_fill_target(&state, [Some(pay.clone()), Some(page.clone()), Some(page.clone())], "https://pay.example")), None, "the user's focus left the processor's frame");
    assert_eq!(id(card_fill_target(&state, [Some(pay.clone()), None, Some(page.clone())], "https://pay.example")), None);
    assert_eq!(id(card_fill_target(&state, [None, Some(other.clone()), Some(page.clone())], "https://ads.example")), None, "a focused frame without a card field focused");
    assert_eq!(id(card_fill_target(&state, [Some(page.clone()), Some(page.clone()), Some(page.clone())], "https://shop.example")), Some(("main", DOC.into())));
    assert_eq!(id(card_fill_target(&state, [None, None, Some(page.clone())], "https://shop.example")), Some(("main", DOC.into())), "the page's one card form");
    let http = top("http://shop.example/");
    assert_eq!(id(card_fill_target(&ready(&[&http]), [None, None, Some(http.clone())], "http://shop.example")), None, "never an http page");
    let mut state = ready(&[&page, &pay]);
    state.card_fill_sent(&pay, t0);
    assert!(!state.address_unfilled(&pay, note("https://pay.example"), t0) && !state.unfilled(&pay, note("https://pay.example"), t0), "a login or address fill wasn't sent");
    assert!(!state.card_unfilled(&page, note("https://shop.example"), t0), "another frame");
    assert!(!state.card_unfilled(&pay, note("https://pay.example"), t0 + Duration::from_secs(6)), "too late");
    assert!(state.card_unfilled(&pay, note("https://pay.example"), t0 + Duration::from_secs(1)));
    assert!(!state.card_unfilled(&pay, note("https://pay.example"), t0 + Duration::from_secs(1)), "once");
}

/// ASP-P16: the values a card fill sends: the name and its given and family
/// parts, split in the browser, the digits, the month and the year. Never a
/// security code, which no saved card has.
#[test]
fn a_card_fill_splits_the_name_in_the_browser() {
    let values = |parts: &[&str; CARD_PARTS]| card_values(parts).iter().map(|value| value.expose().to_owned()).collect::<Vec<_>>();
    assert_eq!(values(&["Ada Fixture Cardholder", "4242424242424242", "07", "2034"]), ["Ada Fixture Cardholder", "Ada Fixture", "Cardholder", "4242424242424242", "07", "2034"]);
    assert_eq!(values(&["", "5555555555554444", "12", "2031"]), ["", "", "", "5555555555554444", "12", "2031"]);
    assert_eq!(CARD_PARTS, 4);
}

/// ASP-P16, the renderer's half, read from the source: the script is
/// installed in another site's frame with the card note alone; a card fill
/// reaches only a card frame's own card function with its document; and the
/// view sends the number in one message, only after the choice.
#[test]
fn the_renderer_installs_cards_only_in_another_sites_frame_and_fills_only_there() {
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("let wanted = if *name == \"cardFocused\" || *name == \"cardFilled\" { cards } else { !cards_only && (capture || always) };"));
    assert!(renderer.contains("let Some(origin) = (if name == \"cardFocused\" || name == \"cardFilled\" { renderer_card_origin(&frame) } else { eligible_origin(&frame) }) else { return };"),
        "another site's frame sends only the card note and the card fill's report");
    assert!(renderer.contains("let cards = takes_cards(frame, snapshot.as_ref());\n    if cards_only && !cards {"), "another site's frame that can't take a card gets nothing");
    assert!(renderer.contains("FILL_CARD => (Fill::Card, CARD_UNFILLED, CARD_VALUES),"));
    assert!(renderer.contains("let frame_origin = if kind == Fill::Card { renderer_card_origin(frame) } else { eligible_origin(frame) };"));
    assert!(renderer.contains("if rest.len() != values || frame_origin.as_deref() != Some(origin.expose()) { return true; }"));
    assert!(!carries_login(CARD_FOCUSED) && !carries_login(FILL_CARD), "a card never leaves a page");
    let view = include_str!("view.rs");
    let fill = &view[view.find("pub fn fill_card_at(").unwrap()..];
    let fill = &fill[..fill.find("\n    }\n").unwrap()];
    assert!(fill.contains("login::card_frozen_frame(&state, browser.identifier(), [focused, browser.focused_frame(), browser.main_frame()], target)") && fill.contains("state.card_fill_sent(&frame, std::time::Instant::now())"));
    assert_eq!(fill.matches("send_process_message").count(), 1, "the number is sent once");
    assert!(!view.contains("pub fn fill_card("), "no card fill re-derives its target at the choice");
}

// -- Round 11: the browser decides which frame may take a card (C10-4) ------------------------

/// A card note from the fixture document, its first.
fn card_note(origin: &str) -> Vec<Secret> {
    numbered(origin, 1)
}

/// The fixture document's card or address note `number`.
fn numbered(origin: &str, number: u64) -> Vec<Secret> {
    args(&[origin, DOC, &number.to_string()])
}

/// The frames a card fill is aimed with when `focused` has the user's
/// focus: the frame of the latest card note, that frame, and the page.
fn aimed(state: &LoginState, tree: &[&Fake], focused: &Fake, page: &Fake) -> [Option<Fake>; 3] {
    let noted = state.card_focus().and_then(|(id, _)| tree.iter().find(|frame| frame.id == id).map(|frame| (*frame).clone()));
    [noted, Some(focused.clone()), Some(page.clone())]
}

/// C10-4: under site isolation another site's frame runs in another process
/// than its parent, and its renderer reads that parent as missing. The
/// renderer no longer refuses a card for that: it defers the ancestors it
/// can't see to the browser, which installs nothing but takes the frame's
/// report and card note only for the card origin it computes over its own
/// whole frame tree. So a processor's frame is offered and filled.
#[test]
fn a_cross_site_frame_whose_parent_the_renderer_cant_see_is_offered_and_filled_as_the_browser_decides() {
    let now = Instant::now();
    // The processor's frame as its own renderer sees it: no parent.
    let seen = Fake { id: "pay", url: "https://pay.example/fields", main: false, parent: None };
    assert_eq!(renderer_card_origin(&seen).as_deref(), Some("https://pay.example"), "the renderer defers the ancestors it can't see");
    assert_eq!(renderer_listener_origin(&seen).as_deref(), Some("https://pay.example"));
    assert!(takes_cards(&seen, Some(&realm("https://pay.example", true))), "the renderer installs the card note");
    assert!(!takes_cards(&seen, Some(&realm("https://pay.example", false))), "not in a document that isn't a secure context");
    assert!(!realm_allows("https://pay.example", Some(&realm("null", true))) && !takes_cards(&seen, Some(&realm("null", true))), "nor in a sandboxed, opaque one in its own process");
    assert_eq!(eligible_origin(&seen), None, "never logins or addresses there");
    assert_eq!(card_origin(&seen), None, "the browser's own rule still fails closed on a missing parent");
    // The same frame in the browser's tree, with the renderer's report and note.
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let mut state = ready(&[&page]);
    let origin = renderer_card_origin(&seen).unwrap();
    receive(&mut state, &pay, INSTALLED, reported(&origin, DOC), now, true);
    receive(&mut state, &pay, CARD_FOCUSED, card_note(&origin), now, true);
    assert_eq!(state.card_focus(), Some(("pay", "https://pay.example")), "the browser takes the note");
    let frames = aimed(&state, &[&page, &pay], &pay, &page);
    assert_eq!(card_fill_origin(&state, frames.clone()).as_deref(), Some("https://pay.example"), "Fill Card is offered for the processor");
    assert_eq!(card_fill_target(&state, frames, "https://pay.example").map(|(frame, document)| (frame.id, document)), Some(("pay", DOC.to_owned())), "and the card goes there");
}

/// C10-4: an http frame anywhere above a card frame refuses a card: the
/// renderer refuses one it can see, and the browser refuses the rest from
/// its own tree, whatever the renderer reports.
#[test]
fn an_http_ancestor_anywhere_refuses_a_card() {
    let now = Instant::now();
    let https = top("https://shop.example/");
    let http = top("http://shop.example/");
    for (pay, tree, why) in [
        (child("pay", "https://pay.example/", http.clone()), vec![http.clone()], "an http page"),
        (child("pay", "https://pay.example/", child("mid", "http://mid.example/", https.clone())), vec![https.clone(), child("mid", "http://mid.example/", https.clone())], "an http frame between"),
        (child("pay", "https://pay.example/", child("mid", "https://mid.example/", http.clone())), vec![http.clone(), child("mid", "https://mid.example/", http.clone())], "an http page above an https frame"),
        (child("pay", "http://127.0.0.1:9090/", child("mid", "http://mid.example/", top("http://127.0.0.1:8080/"))), vec![top("http://127.0.0.1:8080/")], "an http frame between loopback ones"),
    ] {
        assert_eq!(renderer_card_origin(&pay), None, "{why}: the renderer sees it");
        assert_eq!(card_origin(&pay), None, "{why}: the browser sees it");
        // A renderer that can't see the ancestor reports anyway; the browser refuses.
        let mut state = ready(&tree.iter().collect::<Vec<_>>());
        let origin = page_origin(pay.url).unwrap();
        receive(&mut state, &pay, INSTALLED, reported(&origin, DOC), now, true);
        receive(&mut state, &pay, CARD_FOCUSED, card_note(&origin), now, true);
        assert_eq!((state.card_current(&pay), state.card_focus()), (None, None), "{why}");
        let page = tree[0].clone();
        assert_eq!(card_fill_target(&state, [Some(pay.clone()), Some(pay.clone()), Some(page)], &origin).map(|(frame, _)| frame.id), None, "{why}");
    }
}

/// C10-4: a frame whose address in the browser has another origin than its
/// renderer claims takes no card: its report and note are dropped, and the
/// card never goes there, whichever origin the menu names.
#[test]
fn a_frame_whose_browser_side_origin_differs_from_the_renderers_claim_takes_no_card() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://other.example/fields", page.clone());
    for claimed in ["https://pay.example", "https://shop.example", "http://127.0.0.1:9090"] {
        let mut state = ready(&[&page]);
        receive(&mut state, &pay, INSTALLED, reported(claimed, DOC), now, true);
        receive(&mut state, &pay, CARD_FOCUSED, card_note(claimed), now, true);
        assert_eq!((state.card_current(&pay), state.card_focus()), (None, None), "claimed {claimed}");
        for origin in [claimed, "https://other.example"] {
            assert_ne!(card_fill_target(&state, [Some(pay.clone()), Some(pay.clone()), Some(page.clone())], origin).map(|(frame, _)| frame.id), Some("pay"), "claimed {claimed}, menu {origin}");
        }
    }
}

/// C10-4: once a frame navigates, what its old document reports arrives too
/// late to count. Its card note names a document the browser no longer
/// sees there, after a reload too; after a move to another origin, its
/// report and its note name an origin the frame no longer has, so neither
/// counts and no card goes to the old origin.
#[test]
fn after_a_navigation_the_old_documents_card_reports_are_ignored() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let elsewhere = child("pay", "https://other.example/fields", page.clone());
    for (now_there, new_origin) in [(elsewhere.clone(), "https://other.example"), (pay.clone(), "https://pay.example")] {
        let mut state = ready(&[&page, &pay]);
        receive(&mut state, &pay, CARD_FOCUSED, card_note("https://pay.example"), now, true);
        assert_eq!(state.card_focus(), Some(("pay", "https://pay.example")));
        state.document_started("pay", false);
        assert_eq!(state.card_focus(), None, "{new_origin}: a new document forgets the note");
        receive(&mut state, &now_there, INSTALLED, reported(new_origin, "fixture-document-2"), now, true);
        receive(&mut state, &now_there, CARD_FOCUSED, card_note("https://pay.example"), now, true);
        assert_eq!(state.card_focus(), None, "{new_origin}: the old document's late note");
        assert_eq!(state.card_current(&now_there), Some((new_origin.to_owned(), "fixture-document-2".to_owned())), "{new_origin}: the new report stands");
    }
    let mut state = ready(&[&page, &pay]);
    state.document_started("pay", false);
    receive(&mut state, &elsewhere, INSTALLED, reported("https://pay.example", DOC), now, true);
    receive(&mut state, &elsewhere, CARD_FOCUSED, card_note("https://pay.example"), now, true);
    assert_eq!((state.card_current(&elsewhere), state.card_focus()), (None, None), "the old report and note, after the frame moved to another origin");
    let frames = [Some(elsewhere.clone()), Some(elsewhere.clone()), Some(page.clone())];
    assert_eq!(card_fill_target(&state, frames, "https://pay.example").map(|(frame, _)| frame.id), None, "no card goes to the old origin");
}

/// C10-4, read from the source: every renderer path that decides a card,
/// installing the listener, its card note and report, a fill and the
/// repeated report, uses the renderer's rule, which defers what it can't
/// see; the browser keeps the strict rule over its own frame tree for the
/// report, the note and the fill's target.
#[test]
fn the_renderer_defers_a_missing_parent_to_the_browser_which_decides_from_its_own_tree() {
    let source = include_str!("login.rs");
    let (browser, renderer) = source.split_once("// Renderer process\n").unwrap();
    for (rule, deferring) in [("card_origin(", "renderer_card_origin("), ("listener_origin(", "renderer_listener_origin(")] {
        assert!(renderer.contains(deferring), "the renderer uses {deferring}");
        assert_eq!(renderer.matches(rule).count(), renderer.matches(deferring).count(), "no renderer path uses the browser's {rule}");
    }
    assert!(renderer.contains("let Some(origin) = renderer_listener_origin(frame) else { return };"));
    assert!(renderer.contains("if renderer_listener_origin(frame).as_deref() == Some(origin.as_str()) { report(frame, &origin, &document); }"));
    let installed = &browser[browser.find("pub(crate) fn installed<F: FrameLike>").unwrap()..];
    assert!(installed[..installed.find("\n    }\n").unwrap()].contains("if listener_origin(frame).as_deref() != Some(origin.expose()) || self.covered(frame) { return; }"));
    assert!(browser.contains("self.reported(frame, card_origin(frame))"), "the browser's card origin comes from its own tree");
    let takes = &browser[browser.find("pub(crate) fn takes_cards<F: FrameLike>").unwrap()..];
    assert!(takes[..takes.find("\n}\n").unwrap()].contains("renderer_card_origin(frame)"), "the renderer's install check");
}

// -- Round 11: a card fill goes only where its menu was opened (C10-1) ------------------------

/// A checkout page with a processor's card frame and an ad frame, and the
/// browser's state once the user focused the processor's card number.
fn checkout() -> (Fake, Fake, Fake, LoginState) {
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let ad = child("ad", "https://ads.example/", page.clone());
    let mut state = ready(&[&page, &pay, &ad]);
    receive(&mut state, &pay, CARD_FOCUSED, card_note("https://pay.example"), Instant::now(), true);
    (page, pay, ad, state)
}

/// C10-1: the card menu freezes where its fill goes when it opens: the
/// browser, the frame, its document, the origin the browser computed, the
/// card note naming the focused field's form or group, and the frame with
/// focus. At the choice the card goes there only if nothing of it changed;
/// otherwise nothing is sent. Recheck 10's cases (a) and (d), and focus
/// moved to the top page or to another frame without a note.
#[test]
fn a_card_fill_goes_only_where_its_menu_was_opened() {
    let now = Instant::now();
    let (page, pay, ad, state) = checkout();
    let tree = [&page, &pay, &ad];
    let target = card_menu_target(&state, 1, aimed(&state, &tree, &pay, &page)).expect("the menu opens on the processor's field");
    assert_eq!(target.origin(), "https://pay.example");
    let goes = |state: &LoginState, focused: &Fake| card_frozen_frame(state, 1, aimed(state, &tree, focused, &page), &target).map(|frame| frame.id);
    let mut moved = checkout().3;
    receive(&mut moved, &page, CARD_FOCUSED, card_note("https://shop.example"), now, true);
    let mut third = checkout().3;
    receive(&mut third, &ad, CARD_FOCUSED, card_note("https://ads.example"), now, true);
    let mut again = checkout().3;
    receive(&mut again, &pay, CARD_FOCUSED, numbered("https://pay.example", 2), now, true);
    let wrong: Vec<String> = [
        ("nothing changed: the card goes to the processor's field", goes(&state, &pay), Some("pay")),
        ("(a) page script focuses the top page's own card field, whose note follows", goes(&moved, &page), None),
        ("focus on the top page's field, before or without its note", goes(&state, &page), None),
        ("focus in another frame, with no card field there", goes(&state, &ad), None),
        ("(d) the ad frame sends its own card note and has focus", goes(&third, &ad), None),
        ("(c) another frame's note while focus stays", goes(&third, &pay), None),
        ("another card form or group in the same frame, its next note", goes(&again, &pay), None),
        ("the page's view was replaced by another browser", card_frozen_frame(&state, 2, aimed(&state, &tree, &pay, &page), &target).map(|frame| frame.id), None),
        ("a default target", card_frozen_frame(&state, 1, aimed(&state, &tree, &pay, &page), &FillTarget::default()).map(|frame| frame.id), None),
    ].into_iter().filter(|(_, got, want)| got != want).map(|(case, got, want)| format!("{case}: went to {got:?}, expected {want:?}")).collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// C10-1: navigation, a reload and a detached frame void a frozen card fill.
/// The same frame navigating to another origin voids it at once, from the
/// frame's address in the browser, before its load start is reported.
#[test]
fn a_navigation_reload_or_detached_frame_voids_a_frozen_card_fill() {
    let now = Instant::now();
    let (page, pay, ad, state) = checkout();
    let target = card_menu_target(&state, 1, aimed(&state, &[&page, &pay, &ad], &pay, &page)).unwrap();
    let elsewhere = child("pay", "https://other.example/fields", page.clone());
    let goes = |state: &LoginState, frames: [Option<Fake>; 3]| card_frozen_frame(state, 1, frames, &target).map(|frame| frame.id);
    let mut moved = checkout().3;
    moved.document_started("pay", false);
    receive(&mut moved, &elsewhere, INSTALLED, reported("https://other.example", "fixture-document-2"), now, true);
    receive(&mut moved, &elsewhere, CARD_FOCUSED, args(&["https://other.example", "fixture-document-2", "1"]), now, true);
    let mut reloaded = checkout().3;
    reloaded.document_started("pay", false);
    receive(&mut reloaded, &pay, INSTALLED, reported("https://pay.example", "fixture-document-2"), now, true);
    receive(&mut reloaded, &pay, CARD_FOCUSED, args(&["https://pay.example", "fixture-document-2", "1"]), now, true);
    let mut left = checkout().3;
    left.navigated("main", true, now);
    let wrong: Vec<String> = [
        ("the same frame at another origin, before its load start", goes(&state, [Some(elsewhere.clone()), Some(elsewhere.clone()), Some(page.clone())])),
        ("the same frame at another origin, after it, with a card field focused there", goes(&moved, [Some(elsewhere.clone()), Some(elsewhere.clone()), Some(page.clone())])),
        ("a reload: the same origin, another document", goes(&reloaded, [Some(pay.clone()), Some(pay.clone()), Some(page.clone())])),
        ("the frame was removed from the page", goes(&state, [None, None, Some(page.clone())])),
        ("the page navigated", goes(&left, [None, Some(page.clone()), Some(page.clone())])),
    ].into_iter().filter(|(_, got)| got.is_some()).map(|(case, got)| format!("{case}: went to {got:?}, expected None")).collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// C10-1: card notes are numbered in their document, from 1, and the
/// browser takes only a well-formed number; the number of the note a menu
/// was opened on goes with the fill, and the renderer fills only while that
/// note is still its document's latest, so a note still on its way when the
/// card is chosen voids the fill there.
#[test]
fn a_card_fill_names_the_card_note_its_menu_was_opened_on() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    for bad in ["0", "", "-1", "+1", "1.0", " 1", "18446744073709551616", "one"] {
        let mut state = ready(&[&page]);
        receive(&mut state, &page, CARD_FOCUSED, args(&["https://shop.example", DOC, bad]), now, true);
        assert_eq!(state.card_focus(), None, "{bad:?}");
    }
    let mut state = ready(&[&page]);
    receive(&mut state, &page, CARD_FOCUSED, note("https://shop.example"), now, true);
    assert_eq!(state.card_focus(), None, "a note without its number");
    receive(&mut state, &page, CARD_FOCUSED, numbered("https://shop.example", 7), now, true);
    assert_eq!(card_menu_target(&state, 1, [Some(page.clone()), Some(page.clone()), Some(page.clone())]).map(|target| (target.document().to_owned(), target.note())), Some((DOC.to_owned(), 7)));
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("let notes = if card { &mut filler.card_notes } else { &mut filler.address_notes };\n                *notes += 1;\n                *notes\n            })) else { return };\n            outgoing(if card { CARD_FOCUSED } else { ADDRESS_FOCUSED }, &[&origin, &document, &number.to_string()])"));
    assert!(renderer.contains("(Fill::Card | Fill::Address, [note, rest @ ..]) => (Some(note), rest),"));
    assert!(renderer.contains("let notes = if kind == Fill::Card { filler.card_notes } else { filler.address_notes };\n        if note.is_some_and(|note| note.expose() != notes.to_string()) { return None; }"), "only while the note is still the document's latest");
}

// -- Round 10: a fill's report, once the engine said what the user can see --------------------

/// An address or card fill that waited for the engine's answer reports its
/// outcome. The browser takes the report once, only from the frame, origin
/// and document of a fill sent moments ago, and only well formed: an
/// outcome login.js sends and two counts of digits, at most 512 each, both
/// zero unless it filled. It uses up the fill, so a later note that nothing
/// was filled counts for nothing; a malformed report uses up nothing. A
/// report carries no value, so it is taken with capture off.
#[test]
fn a_fill_report_is_taken_once_from_the_frame_it_was_sent_to_and_only_well_formed() {
    let t0 = Instant::now();
    let soon = t0 + Duration::from_secs(1);
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let mut state = ready(&[&page, &pay]);
    let report = |origin: &str, rest: [&str; 3]| { let mut all = vec![origin, DOC]; all.extend_from_slice(&rest); args(&all) };
    state.address_fill_sent(&page, t0);
    assert_eq!(state.address_filled(&page, report("https://shop.example", ["done", "4", "2"]), t0 + Duration::from_secs(6)), None, "too late");
    for bad in [["done", "+4", "2"], ["done", "4", " 2"], ["done", "513", "0"], ["done", "", "0"], ["focus-hidden", "1", "0"], ["unconfirmed", "0", "1"], ["filled", "4", "2"]] {
        assert_eq!(state.address_filled(&page, report("https://shop.example", bad), soon), None, "{bad:?}");
    }
    assert_eq!(state.card_filled(&page, report("https://shop.example", ["done", "4", "2"]), soon), None, "a card fill wasn't sent");
    assert_eq!(state.address_filled(&page, args(&["https://shop.example", "fixture-document-2", "done", "4", "2"]), soon), None, "another document");
    assert_eq!(state.address_filled(&page, report("https://shop.example", ["done", "4", "2"]), soon), Some(FillReport::Done { filled: 4, hidden: 2 }));
    assert_eq!(state.address_filled(&page, report("https://shop.example", ["done", "4", "2"]), soon), None, "once");
    assert!(!state.address_unfilled(&page, note("https://shop.example"), soon), "the report used up the fill");
    state.card_fill_sent(&pay, t0);
    assert_eq!(state.address_filled(&pay, report("https://pay.example", ["unconfirmed", "0", "0"]), soon), None, "never an address from another site's frame");
    assert_eq!(state.card_filled(&page, report("https://shop.example", ["done", "1", "0"]), soon), None, "another frame");
    assert_eq!(state.card_filled(&pay, report("https://pay.example", ["focus-hidden", "0", "0"]), soon), Some(FillReport::FocusHidden));
    state.card_fill_sent(&pay, t0);
    assert_eq!(state.card_filled(&pay, report("https://pay.example", ["unconfirmed", "0", "0"]), soon), Some(FillReport::Unconfirmed));
    state.card_fill_sent(&pay, t0);
    assert_eq!(state.card_filled(&pay, report("https://pay.example", ["done", "512", "512"]), soon), Some(FillReport::Done { filled: 512, hidden: 512 }));
    assert!(!carries_login(ADDRESS_FILLED) && !carries_login(CARD_FILLED));
}

/// The renderer's half and the view's, read from the source: login.js's
/// reports reach the browser only well formed, from the frame's own
/// origin (a card's from another site's frame too), whatever the capture
/// setting; the view tells the app only what needs saying.
#[test]
fn a_fill_report_crosses_only_well_formed_and_the_view_says_only_what_needs_saying() {
    let renderer = include_str!("login.rs");
    let native = &renderer[renderer.find("fn native(name: &str").unwrap()..];
    let native = &native[..native.find("\n}\n").unwrap()];
    assert!(native.contains("let Some(report) = report.filter(|report| fill_report(report).is_some()) else { return };"));
    assert!(native.contains("outgoing(if name == \"cardFilled\" { CARD_FILLED } else { ADDRESS_FILLED }, &args)"));
    assert!(renderer.contains("let always = *name == \"focused\" || *name == \"addressFocused\" || *name == \"addressFilled\";"), "an address fill reports with capture off");
    let view = include_str!("view.rs");
    let message = &view[view.find("fn login_message(").unwrap()..];
    let message = &message[..message.find("\n    }\n").unwrap()];
    assert!(message.contains("self.login.borrow_mut().address_filled(frame, args, now)") && message.contains("fill_event(report, WebEvent::AddressNotFilled, WebEvent::AddressFillReport)"));
    assert!(message.contains("self.login.borrow_mut().card_filled(frame, args, now)") && message.contains("fill_event(report, WebEvent::CardNotFilled, WebEvent::CardFillReport)"));
    use crate::view::{WebEvent, fill_event};
    let shown = |report: FillReport| match fill_event(Some(report), WebEvent::AddressNotFilled, WebEvent::AddressFillReport) {
        None => "nothing".to_owned(),
        Some(WebEvent::AddressNotFilled) => "not filled".to_owned(),
        Some(WebEvent::AddressFillReport(report)) => format!("{report:?}"),
        Some(_) => "other".to_owned(),
    };
    assert_eq!(shown(FillReport::Done { filled: 6, hidden: 0 }), "nothing", "every field filled");
    assert_eq!(shown(FillReport::Done { filled: 0, hidden: 0 }), "not filled");
    assert_eq!(shown(FillReport::Done { filled: 4, hidden: 2 }), "Done { filled: 4, hidden: 2 }");
    assert_eq!(shown(FillReport::FocusHidden), "FocusHidden");
    assert_eq!(shown(FillReport::Unconfirmed), "Unconfirmed");
}

// -- Round 13: a frozen fill keeps its form or group (C11-1), and its note count and the frame the user came back to (C11-2) --

/// C11-1 for addresses, recheck 11's case: address notes are numbered as
/// card notes are, and the menu freezes the number, so focus moving from
/// the shipping form to the billing form of the same frame, whose note
/// follows, voids an address fill frozen on the shipping form. The renderer
/// checks the number too, for a note still on its way.
#[test]
fn an_address_fill_frozen_on_one_form_is_void_once_focus_moves_to_another_in_its_frame() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let mut state = ready(&[&page]);
    receive(&mut state, &page, ADDRESS_FOCUSED, numbered("https://shop.example", 1), now, true);
    let frames = [Some(page.clone()), Some(page.clone()), Some(page.clone())];
    let target = address_menu_target(&state, 1, frames.clone()).expect("the menu opens on the shipping form");
    assert_eq!(target.note(), 1, "the menu freezes the shipping form's note");
    assert_eq!(address_frozen_frame(&state, 1, frames.clone(), &target).map(|frame| frame.id), Some("main"), "nothing changed");
    receive(&mut state, &page, ADDRESS_FOCUSED, numbered("https://shop.example", 2), now, true);
    assert_eq!(address_frozen_frame(&state, 1, frames.clone(), &target).map(|frame| frame.id), None, "the billing form's note voids it");
    assert_eq!(address_menu_target(&state, 1, frames).map(|target| target.note()), Some(2), "a menu opened now names the billing form's note");
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("(Fill::Card | Fill::Address, [note, rest @ ..]) => (Some(note), rest),"));
    assert!(renderer.contains("let notes = if kind == Fill::Card { filler.card_notes } else { filler.address_notes };\n        if note.is_some_and(|note| note.expose() != notes.to_string()) { return None; }"));
    let view = include_str!("view.rs");
    let fill = &view[view.find("pub fn fill_address_at(").unwrap()..];
    assert!(fill[..fill.find("\n    }\n").unwrap()].contains("let mut args = vec![target.origin(), target.document(), note.as_str()];"), "the fill names the note its menu was opened on");
}

/// C11-2, recheck 11's cases: Fill Card follows the frame the user came
/// back to. Focus coming back from another frame sends no note, since
/// nothing in the document it left saw it go (see login_js_tests), so the
/// browser takes the frame with focus whenever its current document noted
/// a card field, whichever frame noted last. The menu then names that
/// frame's origin and the card goes there: back in the processor's number
/// after the page's own card field, and, with split fields, back in the
/// number frame after the expiry frame. A frame with focus that never noted
/// a card field still leaves the fill to the page. Addresses do the same in
/// frames of the page's own origin.
#[test]
fn fill_card_follows_the_frame_the_user_came_back_to() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let ad = child("ad", "https://ads.example/", page.clone());
    let goes = |state: &LoginState, tree: &[&Fake], focused: &Fake| {
        let frames = aimed(state, tree, focused, &page);
        card_menu_target(state, 1, frames.clone()).map(|target| (target.origin().to_owned(), card_frozen_frame(state, 1, frames, &target).map(|frame| frame.id)))
    };
    let tree = [&page, &pay, &ad];
    let mut state = ready(&tree);
    receive(&mut state, &pay, CARD_FOCUSED, numbered("https://pay.example", 1), now, true);
    receive(&mut state, &page, CARD_FOCUSED, numbered("https://shop.example", 1), now, true);
    assert_eq!(goes(&state, &tree, &pay), Some(("https://pay.example".to_owned(), Some("pay"))), "back in the processor's number after the page's card field");
    assert_eq!(goes(&state, &tree, &ad), Some(("https://shop.example".to_owned(), Some("main"))), "a frame that never noted a card field: the page");
    let number = child("number", "https://pay.example/number", page.clone());
    let expiry = child("expiry", "https://pay.example/expiry", page.clone());
    let tree = [&page, &number, &expiry];
    let mut split = ready(&tree);
    receive(&mut split, &number, CARD_FOCUSED, numbered("https://pay.example", 1), now, true);
    receive(&mut split, &expiry, CARD_FOCUSED, numbered("https://pay.example", 1), now, true);
    assert_eq!(goes(&split, &tree, &number), Some(("https://pay.example".to_owned(), Some("number"))), "split fields: back in the number frame after the expiry frame");
    let shipping = child("shipping", "https://shop.example/shipping", page.clone());
    let notes = child("notes", "https://shop.example/notes", page.clone());
    let tree = [&page, &shipping, &notes];
    let mut addresses = ready(&tree);
    receive(&mut addresses, &shipping, ADDRESS_FOCUSED, numbered("https://shop.example", 1), now, true);
    receive(&mut addresses, &page, ADDRESS_FOCUSED, numbered("https://shop.example", 1), now, true);
    let noted = addresses.address_focus().and_then(|(id, _)| tree.iter().find(|frame| frame.id == id).map(|frame| (*frame).clone()));
    let address_goes = |focused: &Fake| {
        let frames = [noted.clone(), Some(focused.clone()), Some(page.clone())];
        let target = address_menu_target(&addresses, 1, frames.clone()).expect("an address menu");
        address_frozen_frame(&addresses, 1, frames, &target).map(|frame| frame.id)
    };
    assert_eq!(address_goes(&shipping), Some("shipping"), "back in the shipping frame after the page's address form");
    assert_eq!(address_goes(&notes), Some("main"), "a frame of the page's origin that never noted an address field: the page");
}

/// C11-2's second case, recheck 11's: a renderer answers each setting
/// message by reporting its live document again, now with how many card
/// and address notes it sent there, so the browser keeps the renderer's
/// count and a menu names the note the renderer checks; before, the report
/// reset the browser's count to none and the renderer refused every fill
/// there. A new document's report starts from none.
#[test]
fn a_document_reported_again_keeps_its_note_counts() {
    let now = Instant::now();
    let page = top("https://shop.example/checkout");
    let pay = child("pay", "https://pay.example/fields", page.clone());
    let frames = [Some(pay.clone()), Some(pay.clone()), Some(page.clone())];
    let mut state = ready(&[&page, &pay]);
    receive(&mut state, &pay, CARD_FOCUSED, numbered("https://pay.example", 3), now, true);
    receive(&mut state, &page, ADDRESS_FOCUSED, numbered("https://shop.example", 2), now, true);
    receive(&mut state, &pay, INSTALLED, args(&["https://pay.example", DOC, "3", "0"]), now, true);
    receive(&mut state, &page, INSTALLED, args(&["https://shop.example", DOC, "0", "2"]), now, true);
    assert_eq!(card_menu_target(&state, 1, frames.clone()).map(|target| (target.origin().to_owned(), target.note())), Some(("https://pay.example".to_owned(), 3)));
    assert_eq!(address_menu_target(&state, 1, [Some(page.clone()), Some(page.clone()), Some(page.clone())]).map(|target| target.note()), Some(2));
    receive(&mut state, &pay, INSTALLED, reported("https://pay.example", "fixture-document-2"), now, true);
    assert_eq!(state.card_note("pay"), 0, "a new document");
    let renderer = include_str!("login.rs");
    assert!(renderer.contains("fillers.get(&id).filter(|filler| filler.document == document).map_or((0, 0), |filler| (filler.card_notes, filler.address_notes))"), "the renderer reports its own counts");
}

// -- vault essentials, slice 0: a tab's later documents (N1103-V6) --

/// The tab in these tests, as CEF identifies its browser in every process.
const TAB: i32 = 7;

/// One document of `TAB` at `frame`, its renderer-side half as login.rs's
/// inject() and native() do it: capture functions only if the gate allows
/// when the script context is created, then `between` (CEF creating or
/// destroying the tab's renderer-side browsers), then a submit that sends
/// only if the gate still allows. Its browser-side half: the document's
/// load start, its report, the submit and the navigation that follows.
/// Returns how many logins the browser offered.
fn sign_in(gate: &mut Gate, state: &mut LoginState, frame: &Fake, document: &str, between: impl FnOnce(&mut Gate)) -> usize {
    let origin = page_origin(frame.url).unwrap();
    let now = Instant::now();
    state.document_started(&frame.id(), frame.main);
    receive(state, frame, INSTALLED, reported(&origin, document), now, true);
    let installed = gate.allows(TAB);
    between(gate);
    if installed && gate.allows(TAB) { receive(state, frame, CAPTURE, args(&[&origin, document, USER, PASSWORD]), now, true); }
    let offered = state.navigated(&frame.id(), frame.main, now).into_iter().count();
    assert!(state.navigated(&frame.id(), frame.main, now).is_none(), "once");
    offered
}

/// N1103-V6, the browser's half: with one renderer-side browser for the
/// whole tab, a second, third and reloaded document each offer once, with
/// the main frame's identifier kept or new for each document. So nothing on
/// the browser side (`login_message`, `LoginState::current`, `receive`)
/// drops a later document's submit or follow.
#[test]
fn the_browser_side_offers_once_for_every_document_of_a_tab() {
    let profile = fresh_profile("fixture-later-documents");
    set_login_capture(&profile, true);
    let (generation, on) = capture_setting(&profile);
    for frames in [["main", "main", "main", "main"], ["main", "main-2", "main-3", "main-4"]] {
        let mut gate = Gate::default();
        gate.launched(&launch_switch());
        gate.browser_created(TAB, &profile, generation, on, true);
        let mut state = LoginState::default();
        let offers: Vec<usize> = frames.iter().zip(["doc-1", "doc-2", "doc-3", "doc-3-reloaded"]).map(|(id, document)| {
            let frame = Fake { id: *id, url: "http://127.0.0.1:54234/form", main: true, parent: None };
            sign_in(&mut gate, &mut state, &frame, document, |_| {})
        }).collect();
        assert_eq!(offers, [1, 1, 1, 1], "{frames:?}");
    }
    set_login_capture(&profile, false);
}

/// N1103-V6, the renderer's half. CEF's `OnBrowserCreated`, in
/// include/cef_render_process_handler.h of the CEF in Cargo.lock: "When
/// browsing cross-origin a new browser will be created before the old
/// browser with the same identifier is destroyed." The tab's next page in
/// this renderer then has a second renderer-side browser with the tab's
/// identifier, and destroying the first must not end capture for it, in
/// either order against the new document's script; a reload makes none.
/// Off, no document of the tab ever offers.
#[test]
fn a_tab_keeps_capturing_when_cef_replaces_its_renderer_side_browser() {
    for on in [true, false] {
        let profile = fresh_profile("fixture-replaced-browser");
        set_login_capture(&profile, on);
        let (generation, now_on) = capture_setting(&profile);
        let mut gate = Gate::default();
        gate.launched(&launch_switch());
        let mut state = LoginState::default();
        let page = top("http://127.0.0.1:54234/form");
        let created = |gate: &mut Gate| gate.browser_created(TAB, &profile, generation, now_on, true);
        created(&mut gate);
        let first = sign_in(&mut gate, &mut state, &page, "doc-1", |_| {});
        // The sign-in's next page: a new browser first, the old one destroyed after the new document's script started.
        created(&mut gate);
        let second = sign_in(&mut gate, &mut state, &page, "doc-2", |gate| gate.browser_destroyed(TAB));
        // The page's link back to the form: the old one destroyed before the new document's script.
        created(&mut gate);
        gate.browser_destroyed(TAB);
        let third = sign_in(&mut gate, &mut state, &page, "doc-3", |_| {});
        let reloaded = sign_in(&mut gate, &mut state, &page, "doc-3-reloaded", |_| {});
        assert_eq!([first, second, third, reloaded], if on { [1; 4] } else { [0; 4] }, "on: {on}");
        // The tab closes: its last renderer-side browser goes, and nothing of it captures.
        gate.browser_destroyed(TAB);
        assert!(!gate.allows(TAB), "closed");
        set_login_capture(&profile, false);
    }
}

/// A later renderer-side browser of a tab whose creation values are
/// missing keeps the tab's: a browser's profile and kind never change. A
/// browser never seen with values is still off.
#[test]
fn a_later_renderer_side_browser_without_creation_values_keeps_the_tabs() {
    let profile = fresh_profile("fixture-later-browser");
    set_login_capture(&profile, true);
    let (generation, on) = capture_setting(&profile);
    let mut gate = Gate::default();
    gate.launched(&launch_switch());
    gate.browser_created(TAB, &profile, generation, on, true);
    gate.browser_created(TAB, "", 0, false, false);
    gate.browser_destroyed(TAB);
    assert!(gate.allows(TAB), "the tab's own values");
    gate.browser_created(TAB + 1, "", 0, false, false);
    assert!(!gate.allows(TAB + 1), "never seen with values");
    set_login_capture(&profile, false);
}
