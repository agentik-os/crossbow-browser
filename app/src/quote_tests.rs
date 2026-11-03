use super::*;
use std::cell::RefCell;
use std::rc::Rc;

const PAGE: &str = "https://fixture-user:fixture-secret@www.example.test/article?id=1&part=2#notes";
const ADDRESS: &str = "https://www.example.test/article?id=1&part=2#notes";

/// Records every write; fails each one while `fail` is set.
#[derive(Clone, Default)]
struct FakeBoard {
    written: Rc<RefCell<Vec<Quote>>>,
    fail: bool,
}

impl QuoteBoard for FakeBoard {
    fn copy(&mut self, quote: &Quote) -> Result<(), String> {
        if self.fail { return Err(COPY_FAILED.into()); }
        self.written.borrow_mut().push(quote.clone());
        Ok(())
    }
}

fn menu(selection: &str, editable: bool) -> ContextMenuInfo {
    ContextMenuInfo { selection_text: selection.into(), page_url: PAGE.into(), is_editable: editable, ..Default::default() }
}

#[test]
fn the_quote_is_one_line_with_the_title_and_the_address_without_its_login() {
    let quote = quote("  Rust is\n\ta language\u{7}.  ", "Fixture Article", PAGE).unwrap();
    assert_eq!(quote.plain, format!("“Rust is a language.” — Fixture Article ({ADDRESS})"));
    assert_eq!(quote.html, "<meta charset=\"utf-8\"><blockquote cite=\"https://www.example.test/article?id=1&amp;part=2#notes\"><p>Rust is a language.</p><p>— <a href=\"https://www.example.test/article?id=1&amp;part=2#notes\">Fixture Article</a></p></blockquote>");
    for text in [&quote.plain, &quote.html] {
        assert!(!text.contains("fixture-user") && !text.contains("fixture-secret"), "no user name or password: {text}");
    }
}

#[test]
fn the_quote_and_the_title_are_escaped_in_html_and_kept_as_selected_in_plain_text() {
    let selection = "<script>alert(\"x\")</script> & 'y'";
    let title = "Tom & Jerry <b>\"Live\"</b>";
    let quote = quote(selection, title, "https://example.test/<a>?q=\"x\"").unwrap();
    assert!(quote.html.contains("<p>&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;y&#39;</p>"), "{}", quote.html);
    assert!(quote.html.contains(">Tom &amp; Jerry &lt;b&gt;&quot;Live&quot;&lt;/b&gt;</a>"), "{}", quote.html);
    assert!(!quote.html.contains("<script") && !quote.html.contains("<b>"), "{}", quote.html);
    // The address arrives encoded, so it never closes its attribute.
    assert!(quote.html.contains("cite=\"https://example.test/%3Ca%3E?q=%22x%22\""), "{}", quote.html);
    assert_eq!(quote.plain, format!("“{selection}” — {title} (https://example.test/%3Ca%3E?q=%22x%22)"));
}

#[test]
fn a_long_quote_is_cut_to_1000_characters_with_an_ellipsis() {
    let text = |quote: &Quote| quote.plain.split('”').next().unwrap().trim_start_matches('“').to_owned();
    let exact = "é".repeat(QUOTE_MAX);
    assert_eq!(text(&quote(&exact, "T", PAGE).unwrap()), exact, "1,000 characters are kept whole");
    let over = text(&quote(&"é".repeat(QUOTE_MAX + 1), "T", PAGE).unwrap());
    assert_eq!(over.chars().count(), QUOTE_MAX);
    assert_eq!(over, format!("{}…", "é".repeat(QUOTE_MAX - 1)));
    // The 999th character is a space: it goes, rather than sit before the ellipsis.
    let words = text(&quote(&"abcdefgh ".repeat(200), "T", PAGE).unwrap());
    assert_eq!(words.chars().count(), QUOTE_MAX - 1);
    assert!(words.ends_with("abcdefgh…"), "{words}");
    let long = quote(&"x".repeat(5000), &"T".repeat(500), PAGE).unwrap();
    assert!(long.html.contains(&format!("<p>{}…</p>", "x".repeat(QUOTE_MAX - 1))));
    assert!(long.plain.contains(&format!(" — {}… (", "T".repeat(TITLE_MAX - 1))), "the title is cut too");
}

#[test]
fn control_and_invisible_characters_are_stripped() {
    let quote = quote("a\u{202E}b\u{200B}c\u{0}d\r\ne\u{85}f\u{2028}g", "\u{202E}Fixture\u{0} Title\n", PAGE).unwrap();
    assert!(quote.plain.starts_with("“abcd e f g” — Fixture Title ("), "{}", quote.plain);
    assert!(!quote.html.chars().any(|c| c.is_control() || crate::ai_features::invisible(c)));
    for nothing in ["", "   ", "\n\t\r", "\u{200B}\u{202E}", "\u{0}\u{7}"] {
        assert_eq!(super::quote(nothing, "T", PAGE), None, "{nothing:?} has nothing to quote");
    }
}

#[test]
fn the_site_names_a_page_without_a_title_and_only_web_addresses_are_included() {
    for title in ["", "  ", PAGE, ADDRESS] {
        let quote = quote("q", title, PAGE).unwrap();
        assert_eq!(quote.plain, format!("“q” — example.test ({ADDRESS})"), "title {title:?}");
    }
    let local = quote("q", "Fixture Notes", "file:///fixture/notes.html").unwrap();
    assert_eq!(local.plain, "“q” — Fixture Notes");
    assert_eq!(local.html, "<meta charset=\"utf-8\"><blockquote><p>q</p><p>— Fixture Notes</p></blockquote>");
    for page in ["file:///fixture/notes.html", "data:text/html,<p>q</p>", "javascript:alert(1)", "about:blank", ""] {
        let quote = quote("q", page, page).unwrap();
        assert_eq!(quote.plain, "“q”", "{page:?} names nothing");
        assert_eq!(quote.html, "<meta charset=\"utf-8\"><blockquote><p>q</p></blockquote>");
    }
    assert_eq!(quote("q", "", "http://example.test:8080/").unwrap().plain, "“q” — example.test (http://example.test:8080/)");
}

#[test]
fn copy_as_quote_is_offered_only_with_the_switch_on_and_text_selected_outside_a_field() {
    assert!(offered(true, &menu("Selected words", false)));
    assert!(!offered(false, &menu("Selected words", false)), "off by default and while off");
    assert!(!offered(true, &menu("Typed words", true)), "not in a text field");
    for nothing in ["", " \n ", "\u{200B}"] {
        assert!(!offered(true, &menu(nothing, false)), "{nothing:?}");
    }
}

#[test]
fn the_title_is_the_tabs_only_while_it_shows_the_menus_page() {
    // On the top page, CEF leaves the frame's address empty.
    let info = menu("q", false);
    assert_eq!(source(&info, PAGE, "Fixture Article"), ("Fixture Article".to_owned(), PAGE.to_owned()));
    assert_eq!(source(&info, "https://example.test/next", "Next Page"), (String::new(), PAGE.to_owned()), "the tab moved on: name the site instead");
}

/// From the review (ai-review1): a selection in a frame from another site
/// was quoted with the top page's title and address.
#[test]
fn a_selection_in_a_frame_is_quoted_with_the_frames_address_and_never_another_sites_title() {
    let framed = |frame: &str, selection: &str| ContextMenuInfo { frame_url: frame.into(), ..menu(selection, false) };
    let same = "https://www.example.test/embed/notes";
    assert_eq!(source(&framed(same, "q"), PAGE, "Fixture Article"), ("Fixture Article".to_owned(), same.to_owned()), "a frame on the page's origin");
    assert_eq!(source(&framed(PAGE, "q"), PAGE, "Fixture Article"), ("Fixture Article".to_owned(), PAGE.to_owned()), "the top page named as its own frame");
    for other in ["https://video.example.test/embed/42", "http://www.example.test/article", "https://www.example.test:8443/x"] {
        assert_eq!(source(&framed(other, "q"), PAGE, "Fixture Article"), (String::new(), other.to_owned()), "another origin: {other}");
    }
    let info = framed("https://fixture-user:fixture-secret@video.example.test/embed/42?t=3#c", "Quoted words");
    let (title, address) = source(&info, PAGE, "Fixture Article");
    let quoted = quote(&info.selection_text, &title, &address).unwrap();
    assert_eq!(quoted.plain, "“Quoted words” — video.example.test (https://video.example.test/embed/42?t=3#c)", "named by its own site, without its login");
    assert!(!quoted.html.contains("Fixture Article") && !quoted.html.contains("www.example.test"));
    for frame in ["about:srcdoc", "about:blank", "data:text/html,<p>q</p>", "blob:https://video.example.test/1", "file:///fixture/notes.html"] {
        let (title, address) = source(&framed(frame, "q"), PAGE, "Fixture Article");
        assert_eq!(title, "", "{frame}");
        assert_eq!(quote("q", &title, &address).unwrap().plain, "“q”", "{frame}: no borrowed title and no address");
    }
}

#[test]
fn copy_writes_both_versions_once_and_nothing_while_off() {
    let store = Store::open_in_memory().unwrap();
    let board = FakeBoard::default();
    let mut host = QuoteHost::with_board(Box::new(board.clone()));
    assert!(!host.enabled(&store), "off by default");
    assert_eq!(host.copy(&store, "q", "T", PAGE), None);
    assert!(board.written.borrow().is_empty(), "nothing is written while off");

    host.switch(&store).choose(&store, true);
    assert_eq!(host.copy(&store, "Quoted words", "Fixture Article", PAGE), Some(Ok(())));
    assert_eq!(*board.written.borrow(), [quote("Quoted words", "Fixture Article", PAGE).unwrap()]);
    assert_eq!(host.copy(&store, "\u{200B}", "T", PAGE), Some(Err(NOTHING.into())));
    assert_eq!(board.written.borrow().len(), 1, "nothing to quote writes nothing");

    let failing = FakeBoard { fail: true, ..FakeBoard::default() };
    let mut host = QuoteHost::with_board(Box::new(failing.clone()));
    assert!(host.enabled(&store), "read back from the store");
    assert_eq!(host.copy(&store, "q", "T", PAGE), Some(Err(COPY_FAILED.into())));
    assert!(failing.written.borrow().is_empty());
}

#[test]
fn copying_a_quote_is_wired_only_to_its_menu_entry() {
    // Running the menu and its action needs the whole app: checked in source.
    let app = include_str!("app.rs");
    let overlays = include_str!("ui/overlays.rs");
    assert_eq!(app.matches("self.quote.copy(").count(), 1);
    assert!(app.contains("Action::CopyAsQuote { text, title, page_url } => match self.quote.copy(&self.lib.store, &text, &title, &page_url) {"));
    assert_eq!(overlays.matches("Action::CopyAsQuote {").count(), 1, "pushed from one place");
    let pick = overlays.find("Some(SelectionPick::Quote) => {").expect("the entry's pick");
    let push = overlays.find("app.actions.push(Action::CopyAsQuote {").unwrap();
    let next = overlays.find("Some(SelectionPick::Search) => {").unwrap();
    assert!(pick < push && push < next, "only choosing the entry copies; dismissing the menu writes nothing");
    assert!(overlays[pick..next].contains("let (title, page_url) = crate::quote::source(info, &nav.url, &nav.title);"), "the selection's own frame decides");
    assert!(overlays[pick..next].contains("Action::CopyAsQuote { text: info.selection_text.clone(), title, page_url }"));
    assert!(!overlays[pick..next].contains("info.page_url"), "never the top page's address");
    assert!(overlays.contains("let quote = app.quote.enabled(&app.lib.store);\n                        match selection_entries(&m, ui, info, quote, engine.as_ref()) {"));
}

#[cfg(target_os = "macos")]
#[test]
fn the_mac_pasteboard_gets_one_item_with_the_text_and_the_html() {
    let mut board = crate::capture::mac::Pasteboard::unique().expect("a private pasteboard");
    let first = quote("Première “citation”", "Fixture Article", PAGE).unwrap();
    QuoteBoard::copy(&mut board, &first).unwrap();
    let items = board.items();
    assert_eq!(items.len(), 1);
    let read = |kind: &str| items[0].iter().find(|(shown, _)| shown == kind).map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap());
    assert_eq!(read(PLAIN_TEXT), Some(first.plain.clone()));
    assert_eq!(read(HTML), Some(first.html.clone()));

    let second = quote("Second", "", PAGE).unwrap();
    QuoteBoard::copy(&mut board, &second).unwrap();
    let items = board.items();
    assert_eq!(items.len(), 1, "the last copy is replaced");
    assert_eq!(items[0].iter().find(|(kind, _)| kind == PLAIN_TEXT).map(|(_, bytes)| bytes.clone()), Some(second.plain.into_bytes()));
    board.release();
}
