//! Element hiding without a renderer: the reply's bytes, what the browser
//! sends for a document, and how the renderer path is wired.

use super::*;
use crate::content_rules::{ContentRules, HidingSheet, ResourceRequest};
use std::sync::Mutex;

/// Rules that block nothing and give every web document the same sheet,
/// keeping each document they are asked about.
struct Fixed {
    sheet: Option<HidingSheet>,
    asked: Mutex<Vec<(String, String)>>,
}

impl ContentRules for Fixed {
    fn blocks(&self, _request: &ResourceRequest<'_>) -> bool {
        false
    }

    fn hiding(&self, document: &str, top: &str) -> Option<HidingSheet> {
        self.asked.lock().unwrap().push((document.into(), top.into()));
        self.sheet.clone()
    }
}

fn fixed(generation: u64) -> Fixed {
    let sheet = HidingSheet { specific: ".mine{display:none!important}\n".into(), generic: ".ad{display:none!important}\n".into(), generation };
    Fixed { sheet: Some(sheet), asked: Mutex::default() }
}

#[test]
fn a_reply_names_its_document_and_round_trips() {
    let generic = "é.ad{display:none!important}\n".repeat(3);
    for (generic, cached) in [(Some(generic.as_str()), false), (None, true), (Some(""), false)] {
        let bytes = encode(41, 7, "#x{display:none!important}\n", generic);
        let payload = decode(&bytes).expect("it decodes");
        assert_eq!(payload, Payload { document: 41, generation: 7, specific: "#x{display:none!important}\n", generic }, "cached: {cached}");
    }
    let bytes = encode(1, 2, "a", Some("b"));
    for cut in 0..bytes.len() {
        assert_eq!(decode(&bytes[..cut]), None, "a payload cut at {cut} is refused");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(decode(&longer), None, "trailing bytes are refused");
    let mut bad = encode(1, 2, "a", Some("b"));
    let at = bad.len() - 1;
    bad[at] = 0xff;
    assert_eq!(decode(&bad), None, "text that isn't UTF-8 is refused");
}

#[test]
fn the_browser_sends_a_document_its_sheet_and_the_shared_part_only_when_the_renderer_lacks_it() {
    let rules = fixed(9);
    let top = "https://news.example/";
    let reply = |document, url, cached| super::reply(Some(&rules as &dyn ContentRules), document, url, top, cached);
    let first = reply(3, "https://news.example/story", 0).unwrap();
    assert_eq!(decode(&first).unwrap(), Payload { document: 3, generation: 9, specific: ".mine{display:none!important}\n", generic: Some(".ad{display:none!important}\n") });
    let again = reply(4, "https://ads.example/frame", 9).unwrap();
    assert_eq!(decode(&again).unwrap().generic, None, "the renderer already holds generation 9");
    let stale = reply(5, "https://news.example/next", 8).unwrap();
    assert!(decode(&stale).unwrap().generic.is_some(), "an older generation gets the new part");
    assert_eq!(*rules.asked.lock().unwrap(), [("https://news.example/story".to_owned(), top.to_owned()), ("https://ads.example/frame".to_owned(), top.to_owned()), ("https://news.example/next".to_owned(), top.to_owned())]);

    let unshared = fixed(0);
    let reply = super::reply(Some(&unshared as &dyn ContentRules), 6, "https://shop.example/", "https://shop.example/", 0).unwrap();
    assert!(decode(&reply).unwrap().generic.is_some(), "a part built for one document is always sent");
    assert_eq!(super::reply(None, 7, "https://news.example/", top, 0), None, "no rules, no reply");
    let nothing = Fixed { sheet: None, asked: Mutex::default() };
    assert_eq!(super::reply(Some(&nothing as &dyn ContentRules), 8, "https://news.example/", top, 0), None, "nothing to hide, no reply");
}

#[test]
fn large_replies_go_through_shared_memory() {
    assert_eq!(SHARED_FROM, 64 * 1024);
    assert!(!shared(SHARED_FROM - 1) && shared(SHARED_FROM));
}

/// The renderer asks at every new script context, before page script runs,
/// and adopts a sheet only into the document it asked for; the browser
/// answers from the frame's own address, never one the renderer names.
#[test]
fn the_renderer_path_is_wired_into_every_frame_s_new_context() {
    let login = include_str!("login.rs");
    let created = &login[login.find("fn on_context_created").unwrap()..];
    let created = &created[..created.find("fn on_context_released").unwrap()];
    assert!(created.contains("crate::hiding::inject(frame, context);"), "every frame, cross-site ones included");
    let released = &login[login.find("fn on_context_released").unwrap()..];
    assert!(released[..released.find("fn on_process_message_received").unwrap()].contains("crate::hiding::release(frame, context);"));
    assert!(login.contains("if source_process == ProcessId::BROWSER && crate::hiding::sheet(frame, message) { return 1; }"));
    let view = include_str!("view.rs");
    assert!(view.contains("|| self.h.0.hiding_message(browser.as_deref(), frame, message)"));
    let own = include_str!("hiding.rs");
    let answer = &own[own.find("pub(crate) fn answer(").unwrap()..];
    let answer = &answer[..answer.find("\n}\n").unwrap()];
    assert!(answer.contains("let url = CefString::from(&frame.url()).to_string();"), "the frame's address from the browser's side");
    let apply = &own[own.find("pub(crate) fn sheet(").unwrap()..];
    let apply = &apply[..apply.find("\n}\n").unwrap()];
    assert!(apply.contains("*asked != payload.document"), "a sheet for an older document is dropped");
}

#[cfg(target_os = "macos")]
mod script {
    use super::super::SCRIPT;
    use crate::login::js_tests::Engine;

    /// A stub of the classes the script reads, recording what it adopts.
    const PAGE: &str = r#"
        var adopted = [];
        function CSSStyleSheet() { this.text = null; }
        CSSStyleSheet.prototype.replaceSync = function (css) { this.text = css; };
        function Document() {}
        Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
            configurable: true,
            get: function () { return adopted; },
            set: function (sheets) { adopted = sheets; },
        });
        var document = new Document();
    "#;

    /// What a page might do once its own scripts run.
    const TAMPER: &str = r#"
        CSSStyleSheet = function () { throw new Error('page constructor'); };
        Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
            configurable: true,
            get: function () { throw new Error('page getter'); },
            set: function () { throw new Error('page setter'); },
        });
        Array.prototype.concat = function () { throw new Error('page concat'); };
        Reflect.apply = function () { throw new Error('page apply'); };
        Function.prototype.call = function () { throw new Error('page call'); };
        Function.prototype.apply = function () { throw new Error('page apply'); };
    "#;

    #[test]
    fn the_script_adopts_each_sheet_with_what_it_kept_and_leaves_no_global() {
        let engine = Engine::new();
        engine.run(PAGE, "fixture://page").unwrap();
        let before = engine.run("Object.getOwnPropertyNames(globalThis).sort().join()", "fixture://check").unwrap();
        engine.run(&format!("var adopt = {SCRIPT};"), "crossbow://hiding").unwrap_or_else(|error| panic!("hiding.js doesn't run: {error}"));
        let after = engine.run("Object.getOwnPropertyNames(globalThis).filter(function (n) { return n !== 'adopt'; }).sort().join()", "fixture://check").unwrap();
        assert_eq!(after, before, "the script adds nothing to the page's globals");
        assert_eq!(engine.run("typeof adopt", "fixture://check").unwrap(), "function");
        engine.run(TAMPER, "fixture://page").unwrap();
        engine.run("adopt('.a{display:none!important}\\n'); adopt('.b{display:none!important}\\n')", "fixture://native").unwrap();
        // Read back with the page's originals gone: through the stub's own store.
        let texts = engine.run("var out = []; for (var i = 0; i < adopted.length; i++) out.push(adopted[i].text); out.join('|')", "fixture://check").unwrap();
        assert_eq!(texts, ".a{display:none!important}\n|.b{display:none!important}\n");
    }
}
