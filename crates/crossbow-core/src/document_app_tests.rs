use super::*;

fn store() -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let store = Store::open(&path).unwrap();
    (dir, path, store)
}

#[test]
fn each_known_app_opens_its_own_new_document_address_and_none_opens_nothing() {
    let addresses: Vec<_> = DocumentApp::KNOWN.iter().map(|app| (app.label(), app.address().unwrap())).collect();
    assert_eq!(addresses, [
        ("Google Docs", "https://docs.new"),
        ("Google Sheets", "https://sheets.new"),
        ("Google Slides", "https://slides.new"),
        ("Microsoft Word", "https://word.new"),
        ("Notion", "https://notion.new"),
    ]);
    assert_eq!(DocumentApp::default(), DocumentApp::None);
    assert_eq!(DocumentApp::None.address(), None);
    assert_eq!(DocumentApp::Custom("https://pad.example/new".into()).address(), Some("https://pad.example/new"));
}

#[test]
fn a_custom_address_must_be_https_without_credentials_and_short_enough() {
    let ok = |input: &str| custom_address(input).unwrap_or_else(|reason| panic!("{input:?}: {reason}"));
    assert_eq!(ok("https://pad.example/new"), "https://pad.example/new");
    assert_eq!(ok("  https://Pad.Example/new?kind=doc  "), "https://pad.example/new?kind=doc", "trimmed and normalized");
    assert_eq!(ok("HTTPS://pad.example"), "https://pad.example/");
    for (input, reason) in [
        ("", EMPTY),
        ("   ", EMPTY),
        ("javascript:alert(1)", UNSAFE),
        ("  JavaScript:alert(1)", UNSAFE),
        ("data:text/html,<p>hi</p>", UNSAFE),
        ("DATA:text/plain,hi", UNSAFE),
        ("file:///etc/hosts", UNSAFE),
        ("File:///Users", UNSAFE),
        ("java\tscript:alert(1)", SPACES),
        ("https://pad.example/a b", SPACES),
        ("https://pad.example/\nnew", SPACES),
        ("https://pad.example/\u{7}", SPACES),
        ("http://pad.example/new", NOT_HTTPS),
        ("ftp://pad.example/new", NOT_HTTPS),
        ("about:blank", NOT_HTTPS),
        ("crossbow://settings", NOT_HTTPS),
        ("https://user@pad.example/new", CREDENTIALS),
        ("https://user:secret@pad.example/new", CREDENTIALS),
        ("https://:secret@pad.example/new", CREDENTIALS),
        ("pad.example/new", NOT_ADDRESS),
        ("https://", NOT_ADDRESS),
    ] {
        assert_eq!(custom_address(input), Err(reason), "{input:?}");
    }
    let base = "https://pad.example/";
    let longest = format!("{base}{}", "a".repeat(ADDRESS_MAX - base.len()));
    assert_eq!(custom_address(&longest).map(|address| address.len()), Ok(ADDRESS_MAX));
    assert_eq!(custom_address(&format!("{longest}a")), Err(TOO_LONG));
    // Short as typed, but longer than the limit once each character is escaped.
    assert_eq!(custom_address(&format!("{base}{}", "é".repeat(1_000))), Err(TOO_LONG));
}

#[test]
fn each_profiles_choice_is_kept_on_its_own_and_read_back_after_reopening() {
    let (_dir, path, store) = store();
    assert_eq!(load(&store, "personal"), (DocumentApp::None, None), "None until chosen");
    save(&store, "personal", &DocumentApp::GoogleSheets).unwrap();
    save(&store, "work", &DocumentApp::Custom("https://pad.example/new".into())).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(load(&store, "personal"), (DocumentApp::GoogleSheets, None));
    assert_eq!(load(&store, "work"), (DocumentApp::Custom("https://pad.example/new".into()), None));
    assert_eq!(load(&store, "other"), (DocumentApp::None, None), "another profile never changes");
    for app in std::iter::once(DocumentApp::None).chain(DocumentApp::KNOWN) {
        save(&store, "personal", &app).unwrap();
        assert_eq!(load(&store, "personal").0, app);
    }
}

#[test]
fn a_saved_value_this_build_cant_trust_reads_as_none_and_says_so() {
    let (_dir, _path, store) = store();
    for raw in ["sepia", "custom:javascript:alert(1)", "custom:http://pad.example/", "custom:https://user@pad.example/", "custom:https://Pad.Example/new", "custom:"] {
        store.kv_set("document_app.personal", raw).unwrap();
        assert_eq!(load(&store, "personal"), (DocumentApp::None, Some(UNREADABLE)), "{raw:?}");
    }
}
