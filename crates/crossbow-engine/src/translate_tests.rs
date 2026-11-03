use super::*;

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

#[test]
fn reports_are_read_strictly_and_anything_out_of_range_drops_the_report() {
    let new = Earlier::default();
    assert_eq!(parse_report(&strings(&["probe", "7", "0", "0", "0", "0", "fr-CA", "Bonjour"])), Some(TranslateReport::Probe { page: 7, earlier: new, lang: "fr-CA".into(), sample: "Bonjour".into() }));
    assert_eq!(parse_report(&strings(&["probe", "7", "0", "0", "0", "0", "", ""])), Some(TranslateReport::Probe { page: 7, earlier: new, lang: String::new(), sample: String::new() }));
    // A document that did something before: what it says, within its limits.
    let before = Earlier { sampled: true, nodes: 12, units: 340, shown: 3 };
    assert_eq!(parse_report(&strings(&["probe", "7", "1", "12", "340", "3", "fr", ""])), Some(TranslateReport::Probe { page: 7, earlier: before, lang: "fr".into(), sample: String::new() }));
    let most = Earlier { sampled: false, nodes: MAX_SEGMENTS, units: MAX_CHARS, shown: u64::MAX };
    assert_eq!(parse_report(&strings(&["hello", "7", "0", "2000", "100000", "18446744073709551615"])), Some(TranslateReport::Hello { page: 7, earlier: most }));
    let segments = parse_report(&strings(&["segments", "7", "2", "1", "1", "Bonjour", "2", "le monde"])).unwrap();
    assert_eq!(segments, TranslateReport::Segments {
        page: 7, pass: 2, truncated: true,
        segments: vec![Segment { id: 1, text: "Bonjour".into() }, Segment { id: 2, text: "le monde".into() }],
    });
    assert_eq!(parse_report(&strings(&["segments", "7", "1", "0"])), Some(TranslateReport::Segments { page: 7, pass: 1, truncated: false, segments: vec![] }));
    assert_eq!(parse_report(&strings(&["applied", "7", "2", "40", "3"])), Some(TranslateReport::Applied { page: 7, pass: 2, written: 40, skipped: 3 }));
    assert_eq!(parse_report(&strings(&["restored", "7", "2", "40"])), Some(TranslateReport::Restored { page: 7, pass: 2, restored: 40 }));
    assert_eq!(parse_report(&strings(&["declined", "7"])), Some(TranslateReport::Declined { page: 7 }));
    assert_eq!(parse_report(&strings(&["hello", "7", "0", "0", "0", "0"])), Some(TranslateReport::Hello { page: 7, earlier: new }));

    let long = "x".repeat(MAX_SEGMENT_CHARS + 1);
    let lang = "x".repeat(65);
    let sample = "é".repeat(MAX_SAMPLE + 1);
    for bad in [
        vec![], vec!["unknown", "7"], vec!["probe", "0", "0", "0", "0", "0", "fr", "x"], vec!["probe", "7", "0", "0", "0", "0", "fr"], vec!["probe", "7", "0", "0", "0", "0", &lang, "x"],
        vec!["probe", "7", "0", "0", "0", "0", "fr", &sample], vec!["probe", "7", "fr", "x"], vec!["probe", "7", "2", "0", "0", "0", "fr", "x"], vec!["probe", "7", "0", "2001", "0", "0", "fr", "x"],
        vec!["probe", "7", "0", "0", "100001", "0", "fr", "x"], vec!["probe", "7", "0", "0", "0", "-1", "fr", "x"], vec!["probe", "7", "0", "x", "0", "0", "fr", "x"],
        vec!["segments", "7", "1", "2"], vec!["segments", "7", "0", "0"], vec!["segments", "x", "1", "0"],
        vec!["segments", "7", "1", "0", "1"], vec!["segments", "7", "1", "0", "2", "out of order"], vec!["segments", "7", "1", "0", "1", "a", "1", "again"],
        vec!["segments", "7", "1", "0", "1", "  "], vec!["segments", "7", "1", "0", "1", &long], vec!["segments", "7", "1", "0", "0", "zero"],
        vec!["applied", "7", "2", "40"], vec!["applied", "7", "2", "-1", "0"], vec!["applied", "7", "2", "2001", "0"], vec!["restored", "7", "0", "1"],
        vec!["declined"], vec!["declined", "0"], vec!["declined", "7", "fr"], vec!["hello"], vec!["hello", "7"], vec!["hello", "0", "0", "0", "0", "0"], vec!["hello", "7", "0", "0", "0"],
        vec!["hello", "7", "0", "0", "0", "0", "x"], vec!["hello", "7", "", "0", "0", "0"], vec!["hello", "7", "0", "0", "0", "18446744073709551616"],
    ] {
        assert_eq!(parse_report(&strings(&bad)), None, "{bad:?} must be dropped");
    }
    // Too many nodes, and too much text in all.
    let mut many = strings(&["segments", "7", "1", "1"]);
    for id in 1..=MAX_SEGMENTS + 1 { many.extend([id.to_string(), "word".into()]); }
    assert_eq!(parse_report(&many), None);
    let mut heavy = strings(&["segments", "7", "1", "1"]);
    for id in 1..=MAX_CHARS / MAX_SEGMENT_CHARS + 1 { heavy.extend([id.to_string(), "y".repeat(MAX_SEGMENT_CHARS)]); }
    assert_eq!(parse_report(&heavy), None);
    // UTF-16 units, as the script counts them: 2000 astral characters are 4000.
    assert_eq!(parse_report(&strings(&["segments", "7", "1", "0", "1", &"𝄞".repeat(1001)])), None);
    assert!(parse_report(&strings(&["segments", "7", "1", "0", "1", &"𝄞".repeat(1000)])).is_some());
}

#[test]
fn commands_encode_every_part_as_a_string() {
    assert_eq!(TranslateCommand::Probe { sample: 300 }.encode(), strings(&["probe", "300"]));
    assert_eq!(TranslateCommand::Probe { sample: 1_000_000 }.encode(), strings(&["probe", "2000"]), "never more than the script reads");
    assert_eq!(TranslateCommand::Collect.encode(), strings(&["collect"]));
    assert_eq!(TranslateCommand::Hello.encode(), strings(&["hello"]));
    let apply = TranslateCommand::Apply { pass: 3, texts: vec![(2, "Hello".into()), (1, "<b>world</b>".into())] };
    assert_eq!(apply.encode(), strings(&["apply", "3", "2", "Hello", "1", "<b>world</b>"]));
    assert_eq!(TranslateCommand::Restore { pass: 3 }.encode(), strings(&["restore", "3"]));
}

#[test]
fn the_content_language_kept_is_the_latest_top_document_responses_own() {
    let mut languages = Languages::default();
    assert_eq!(languages.of("https://example.test/"), None);
    languages.response("https://example.test/", &strings(&[" de-DE ", "en"]));
    assert_eq!(languages.of("https://example.test/").as_deref(), Some("de-DE, en"));
    assert_eq!(languages.of("https://example.test/other"), None, "another address isn't that response");
    languages.response("https://example.test/other", &[]);
    assert_eq!(languages.of("https://example.test/"), None, "a newer response without the header clears it");
    assert_eq!(languages.of("https://example.test/other"), None);
    languages.response("https://example.test/", &[format!("x{}", "-y".repeat(200))]);
    assert_eq!(languages.of("https://example.test/"), None, "an oversized header is ignored");
}

/// The script's capture rule, its text-only writes and its wiring, checked
/// in source. The JavaScriptCore tests below also compile and run it.
#[test]
fn the_translator_captures_its_built_ins_writes_only_text_and_is_wired_to_top_frames_only() {
    let body = &SCRIPT[SCRIPT.find("// -- Everything below calls only the references captured above. --").expect("a capture boundary")..];
    for unsafe_use in [
        "...", "for (const", "for (let [", " of ", ".push(", ".forEach(", ".map(", ".join(", ".slice(", ".indexOf(", ".charCodeAt(", ".toLowerCase(", ".test(", ".exec(", ".replace(", ".split(",
        "JSON.", "console.", "document.", "window.", "Math.", "RegExp", "new RegExp", ".getAttribute(", ".querySelector(", ".checkVisibility(",
        ".querySelectorAll(", ".contains(", ".item(", ".has(", ".add(", "getComputedStyle(", ".getPropertyValue(", ".getBoundingClientRect(", ".createRange(",
        ".selectNodeContents(", ".getClientRects(", ".assignedSlot", ".host", ".scrollX", ".scrollY", ".clientWidth", ".lastIndexOf(",
    ] {
        assert!(!body.contains(unsafe_use), "the script body uses {unsafe_use:?}, which a page could have patched");
    }
    for markup in ["innerHTML", "outerHTML", "insertAdjacentHTML", "createContextualFragment", "DOMParser", "document.write", "textContent", "createElement", "appendChild", "insertBefore", "replaceChild", "eval(", "Function("] {
        assert!(!SCRIPT.contains(markup), "the script uses {markup:?}: translated text is written only as a text node's value");
    }
    assert_eq!(SCRIPT.matches("apply(setNodeValue, ").count(), 2, "one write for a translation, one for an original");

    let renderer = include_str!("login.rs");
    for wired in ["crate::translate::inject(frame, context);", "crate::translate::release(frame, context);", "if source_process == ProcessId::BROWSER && crate::translate::command(frame, message) { return 1; }"] {
        assert!(renderer.contains(wired), "{wired}");
    }
    let translate = include_str!("translate.rs");
    assert!(translate.contains("if frame.is_main() != 1 || crate::login::page_origin("), "http(s) top frames only");
    assert!(translate.contains("context.frame().filter(|frame| frame.is_main() == 1)"), "reports from top frames only");
    let view = include_str!("view.rs");
    assert!(view.contains("|| self.h.0.translate_message(frame, message))"));
    assert!(view.contains("self.h.0.media.borrow_mut().take();\n                self.h.0.translate.borrow_mut().clear();"), "a new document's reports start empty");
    assert!(view.contains("if request.resource_type() == ResourceType::MAIN_FRAME {\n                self.languages.lock()"), "only the top frame's Content-Language");
}

/// One definition of the styles that hide what is drawn: the text rule
/// reads exactly the properties and initial values the vault's field rule
/// (`login.js` `visible`) reads, in its order.
#[test]
fn text_and_fields_count_the_same_styles_as_hiding() {
    let vault: Vec<&str> = include_str!("login.js").split("initial(style, '").skip(1).flat_map(|call| call.split(')').next().unwrap().split(", ").map(|quoted| quoted.trim_matches('\''))).collect();
    let start = SCRIPT.find("const HIDING = [").expect("a HIDING list") + "const HIDING = [".len();
    let text: Vec<&str> = SCRIPT[start..start + SCRIPT[start..].find("];").unwrap()].split(", ").map(|quoted| quoted.trim_matches('\'')).collect();
    assert_eq!(vault.len(), 20, "{vault:?}");
    assert_eq!(text, vault);
}

/// One rule for card fields: a field the vault would offer Fill Card on by
/// its words (`login.js` `CARD_WORDS`, read by `wordRole` from its labels,
/// then its aria-label, placeholder, name and id) keeps the text around it
/// unread. The words, the matching and the order are the vault's, character
/// for character; so are the number of labels read and the input types
/// read. One difference is allowed, one way: text reads the words on every
/// select, the vault on single-choice ones only, which fails closed.
#[test]
fn text_and_the_vault_find_card_fields_by_the_same_words() {
    let vault = include_str!("login.js");
    let block = |script: &'static str, start: &str, end: &str| -> &'static str {
        let at = script.find(start).unwrap_or_else(|| panic!("{start:?}"));
        &script[at..at + script[at..].find(end).unwrap_or_else(|| panic!("{start:?} ends with {end:?}")) + end.len()]
    };
    for (start, end) in [("  const CARD_WORDS = [\n", "\n  ];\n"), ("  const wordRole = (text, table) => {\n", "\n  };\n"), ("    const names = ['aria-label', 'placeholder', 'name', 'id'];\n", "\n")] {
        assert_eq!(block(SCRIPT, start, end), block(vault, start, end), "{start:?}");
    }
    assert!(block(vault, "  const CARD_NAME = 0,", ";\n").contains("CARD_NAME = 0, CARD_GIVEN = 1, CARD_FAMILY = 2, CARD_NUMBER = 3, CARD_EXP = 4, CARD_MONTH = 5, CARD_YEAR = 6, CSC = 7;"));
    assert_eq!(block(SCRIPT, "  const CARD_NAME = 0,", ";\n"), block(vault, "  const CARD_NAME = 0,", ";\n"));
    assert!(vault.contains("const NONE = -1,") && SCRIPT.contains("const NONE = -1;"));
    assert_eq!(block(SCRIPT, "  const MAX_LABEL = ", ";\n"), "  const MAX_LABEL = 200;\n");
    assert!(vault.contains("MAX_LABEL = 200;"));

    // labelRole reads the same number of labels.
    let labels_read = |script: &'static str| -> usize {
        let role = block(script, "  const labelRole = (", "\n  };\n");
        let at = role.find("i < count && i < ").expect("the label loop") + "i < count && i < ".len();
        role[at..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
    };
    assert_eq!(labels_read(SCRIPT), labels_read(vault), "labels read by labelRole");

    // The input types: the vault's text-like, number and month inputs
    // (cardKind), less email, which it never fills as a card part
    // (cardRoleOf); text's (cardNamed).
    let quoted = |text: &str, before: &str| -> std::collections::BTreeSet<String> {
        text.match_indices(before).map(|(at, _)| { let rest = &text[at + before.len()..]; rest[..rest.find('\'').unwrap()].to_owned() }).collect()
    };
    let kind = block(vault, "  const cardKind = (field) => {\n", "\n  };\n");
    let mut vault_inputs = quoted(kind, "type === '");
    if kind.contains("textLike(type)") { vault_inputs.extend(quoted(block(vault, "  const textLike = (type) =>", ";\n"), "type === '")); }
    if block(vault, "  const cardRoleOf = (field, kind) => {\n", "\n  };\n").contains("if (kind === 'email') return NONE;") { vault_inputs.remove("email"); }
    let named = block(SCRIPT, "  const cardNamed = (element, kind) => {\n", "\n  };\n");
    let text_inputs = quoted(named, "type !== '");
    assert!(text_inputs.contains("text"), "{text_inputs:?}");
    assert_eq!(text_inputs, vault_inputs, "input types read for card words");

    // Selects: the vault's single-choice ones only; text's every one, when
    // cardNamed lets a select through without a type.
    let vault_selects = quoted(kind, "apply(selectType, field, []) === '");
    let every: std::collections::BTreeSet<String> = ["select-multiple".to_string(), "select-one".to_string()].into();
    let text_selects = if named.contains("} else if (kind !== 1 && kind !== -1) return false;") && !named.contains("select") { every } else { quoted(named, "apply(selectType, element, []) === '") };
    assert!(!vault_selects.is_empty() && vault_selects.is_subset(&text_selects), "text reads the words on every select the vault fills: {vault_selects:?} {text_selects:?}");
    assert_eq!(text_selects.difference(&vault_selects).map(String::as_str).collect::<Vec<_>>(), ["select-multiple"], "the one allowed difference");
}

#[cfg(target_os = "macos")]
mod javascript_core {
    use super::*;
    use crate::media::tests::javascript_core::Page;

    /// A JavaScript string literal of `text`, every UTF-16 unit escaped.
    fn literal(text: &str) -> String {
        let mut out = String::from("'");
        for unit in text.encode_utf16() { out.push_str(&format!("\\u{unit:04x}")); }
        out.push('\'');
        out
    }

    /// `translate.js` started in the stand-in page as the renderer starts
    /// it, over a document built by `build`, a script for `crossbow.page`.
    fn started(build: &str) -> Page {
        let page = Page::new();
        page.compile(SCRIPT, SCRIPT_URL).unwrap_or_else(|error| panic!("translate.js doesn't compile: {error}"));
        page.run(include_str!("translate_tests.js")).expect("the stand-in page");
        page.run(build).unwrap_or_else(|error| panic!("the stand-in document: {error}"));
        page.keep("factory", SCRIPT, SCRIPT_URL).unwrap_or_else(|error| panic!("translate.js doesn't evaluate: {error}"));
        let command = page.run("var command = factory(function () { crossbow.reports.push(Array.prototype.slice.call(arguments)); }); typeof command");
        assert_eq!(command.unwrap_or_else(|error| panic!("the factory threw: {error}")), "function");
        page
    }

    /// Send `command` as the renderer does: every part a string.
    fn send(page: &Page, command: &TranslateCommand) {
        let args: Vec<String> = command.encode().iter().map(|arg| literal(arg)).collect();
        page.run(&format!("command({}); 'sent'", args.join(", "))).unwrap_or_else(|error| panic!("{command:?} threw: {error}"));
    }

    fn sent(page: &Page) -> Vec<TranslateReport> {
        let sent = page.run("crossbow.sent()").unwrap();
        assert_ne!(sent, "not strings", "report() is only ever given strings");
        if sent.is_empty() { return Vec::new(); }
        sent.split('\u{1e}').map(|report| {
            let parts: Vec<String> = report.split('\u{1f}').map(String::from).collect();
            parse_report(&parts).unwrap_or_else(|| panic!("the browser can't read {parts:?}"))
        }).collect()
    }

    fn texts(page: &Page) -> Vec<String> {
        page.run("crossbow.texts()").unwrap().split('\u{1f}').map(String::from).collect()
    }

    fn one(page: &Page) -> TranslateReport {
        let mut reports = sent(page);
        assert_eq!(reports.len(), 1, "one report: {reports:?}");
        reports.remove(0)
    }

    fn collect(page: &Page) -> (u64, u64, bool, Vec<Segment>) {
        send(page, &TranslateCommand::Collect);
        match sent(page).as_slice() {
            [TranslateReport::Segments { page, pass, truncated, segments }] => (*page, *pass, *truncated, segments.clone()),
            other => panic!("expected one segments report, got {other:?}"),
        }
    }

    fn ids_of<'a>(segments: &'a [Segment], texts: &[&str]) -> Vec<u32> {
        texts.iter().map(|text| segments.iter().find(|segment| segment.text == *text).unwrap_or_else(|| panic!("{text:?} wasn't read")).id).collect()
    }

    const ARTICLE: &str = r#"crossbow.page('fr', [
        h('H1', {}, [t('  Bonjour le monde  ')]),
        h('P', {}, [t('Premier paragraphe, '), h('A', { href: '#' }, [t('un lien')]), t(' et la fin.\n')]),
        h('SCRIPT', {}, [t('var secret = "never read";')]),
        h('STYLE', {}, [t('p { color: red }')]),
        h('NOSCRIPT', {}, [t('Activez JavaScript')]),
        h('CODE', {}, [t('let x = traduire();')]),
        h('PRE', {}, [t('  bloc préformaté')]),
        h('KBD', {}, [t('Entrée')]),
        h('TEXTAREA', {}, [t('Brouillon privé')]),
        h('INPUT', { type: 'text', value: 'valeur' }, []),
        h('SELECT', {}, [h('OPTION', {}, [t('Choix privé')])]),
        h('DIV', { contenteditable: 'true' }, [t('Texte modifiable'), h('SPAN', {}, [t('aussi modifiable')])]),
        h('DIV', { contenteditable: 'true' }, [h('SPAN', { contenteditable: 'false' }, [t('pas modifiable, mais dans un éditeur')])]),
        h('SPAN', { translate: 'NO' }, [t('Nom de marque')]),
        h('SPAN', { class: 'logo notranslate' }, [t('Autre marque')]),
        h('SPAN', { class: 'notranslatex' }, [t('Classe voisine')]),
        h('DIV', { style: 'display: none' }, [t('Texte caché')]),
        svg([t('Texte SVG')]),
        h('FORM', {}, [h('LABEL', {}, [t('Identifiant')]), h('INPUT', { type: 'Password' }, [])]),
        h('FORM', {}, [h('LABEL', {}, [t('Numéro de carte')]), h('INPUT', { autocomplete: 'cc-number' }, [])]),
        h('FORM', {}, [h('LABEL', {}, [t('Rechercher sur le site')]), h('INPUT', { type: 'search' }, [])]),
        h('P', {}, [t('  42 - 12:30 !  ')]),
        h('P', {}, [t(' Dernière ligne ')]),
    ]);"#;

    #[test]
    fn collect_reads_visible_text_in_order_and_never_code_fields_editors_or_sensitive_forms() {
        let page = started(ARTICLE);
        assert!(sent(&page).is_empty(), "nothing is reported until the browser asks");
        let (_, pass, truncated, segments) = collect(&page);
        assert_eq!((pass, truncated), (1, false));
        let read: Vec<&str> = segments.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(read, [
            "Bonjour le monde", "Premier paragraphe,", "un lien", "et la fin.", "Classe voisine", "Rechercher sur le site", "Dernière ligne",
        ], "an editor is skipped whole, and digits and punctuation alone aren't read");
        assert_eq!(segments.iter().map(|segment| segment.id).collect::<Vec<_>>(), (1..=7).collect::<Vec<_>>());
        assert_eq!(page.run("crossbow.selectors.join(' | ')").unwrap(), "*", "one query finds the document's fields, frames, editors and shadow hosts");
        assert_eq!(page.run("crossbow.writes").unwrap(), "0", "reading writes nothing");
    }

    #[test]
    fn probe_sends_the_lang_attribute_and_at_most_the_asked_sample() {
        let page = started(ARTICLE);
        send(&page, &TranslateCommand::Probe { sample: 30 });
        let TranslateReport::Probe { lang, sample, .. } = one(&page) else { panic!("a probe report") };
        assert_eq!(lang, "fr");
        assert_eq!(sample, "Bonjour le monde\nPremier parag");
        assert_eq!(sample.encode_utf16().count(), 30);
        send(&page, &TranslateCommand::Probe { sample: 10_000 });
        let TranslateReport::Probe { sample, .. } = one(&page) else { panic!("a probe report") };
        assert!(sample.ends_with("Dernière ligne") && !sample.contains("secret") && !sample.contains("Identifiant"), "{sample:?}");
        // Out of range: the script ignores it.
        page.run("command('probe', '0'); command('probe', '2001'); command('probe', 'x');").unwrap();
        assert!(sent(&page).is_empty());

        let page = started("crossbow.page(null, [h('P', {}, [t('Sans langue')])]);");
        send(&page, &TranslateCommand::Probe { sample: 300 });
        let TranslateReport::Probe { lang, sample, .. } = one(&page) else { panic!("a probe report") };
        assert_eq!((lang.as_str(), sample.as_str()), ("", "Sans langue"));
    }

    #[test]
    fn apply_writes_only_matching_ids_as_text_and_show_original_restores_every_byte() {
        let page = started(ARTICLE);
        let before = texts(&page);
        let elements = page.run("crossbow.elements()").unwrap();
        let (_, pass, _, segments) = collect(&page);
        let [hello, first, link, end, last] = ids_of(&segments, &["Bonjour le monde", "Premier paragraphe,", "un lien", "et la fin.", "Dernière ligne"]).try_into().unwrap();
        // Reordered, invented, repeated and stale ids; markup in a translation.
        page.run(&format!("command('apply', '{pass}', '{end}', 'and the end.', '{hello}', '<b>Hello</b> <img src=x onerror=alert(1)>', '999', 'invented', '0', 'zero', 'x', 'nan', '{hello}', 'again', '{last}', 'Last line')")).unwrap();
        let TranslateReport::Applied { written, skipped, .. } = one(&page) else { panic!("an applied report") };
        assert_eq!((written, skipped), (3, 4));
        let after = texts(&page);
        assert!(after.contains(&"  <b>Hello</b> <img src=x onerror=alert(1)>  ".to_string()), "markup stays text, white space stays put: {after:?}");
        assert!(after.contains(&" and the end.\n".to_string()) && after.contains(&"\u{a0}Last line\u{a0}".to_string()));
        assert!(after.contains(&"Premier paragraphe, ".to_string()) && after.contains(&"un lien".to_string()), "nodes without an answer keep their text");
        assert_eq!(page.run("crossbow.elements()").unwrap(), elements, "no element was created");
        // A later batch for the same pass.
        page.run(&format!("command('apply', '{pass}', '{first}', 'First paragraph,', '{link}', 'a link')")).unwrap();
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 2, skipped: 0, .. }]));
        // Another pass's apply is ignored.
        page.run(&format!("command('apply', '{}', '{first}', 'stale')", pass + 1)).unwrap();
        assert!(sent(&page).is_empty());

        send(&page, &TranslateCommand::Restore { pass });
        let TranslateReport::Restored { restored, .. } = one(&page) else { panic!("a restored report") };
        assert_eq!(restored, 5);
        assert_eq!(texts(&page), before, "Show Original puts back every text node byte for byte");
        // The pass is over: a late answer writes nothing.
        page.run(&format!("command('apply', '{pass}', '{hello}', 'late')")).unwrap();
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 0, skipped: 1, .. }]));
        assert_eq!(texts(&page), before);
    }

    #[test]
    fn text_the_page_changes_is_left_alone() {
        let page = started("crossbow.page('de', [h('P', {}, [t('Erster')]), h('P', {}, [t('Zweiter')]), h('P', {}, [t('Dritter')])]);");
        let (_, pass, _, _) = collect(&page);
        // The page rewrites the first node before the answer comes.
        page.run("crossbow.body.kids[0].kids[0].value = 'Neu vom Skript';").unwrap();
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "First".into()), (2, "Second".into()), (3, "Third".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 2, skipped: 1, .. }]));
        // And the second after the answer.
        page.run("crossbow.body.kids[1].kids[0].value = 'Live: 3 neue';").unwrap();
        send(&page, &TranslateCommand::Restore { pass });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Restored { restored: 1, .. }]));
        assert_eq!(texts(&page)[1..], ["Neu vom Skript", "Live: 3 neue", "Dritter"]);
    }

    #[test]
    fn css_hidden_text_is_not_read_but_display_contents_children_are() {
        let page = started("crossbow.page('fr', [h('P', { style: 'visibility: hidden' }, [t('Caché')]), h('P', { style: 'opacity: 0' }, [t('Invisible')]), h('DIV', { style: 'display: contents' }, [h('P', {}, [t('Visible')])])]);");
        let (_, _, _, segments) = collect(&page);
        assert_eq!(segments, vec![Segment { id: 1, text: "Visible".into() }]);
    }

    #[test]
    fn a_node_that_moves_into_an_editor_or_sensitive_form_is_not_written() {
        let page = started("crossbow.page('fr', [h('P', {}, [t('Bonjour')]), h('P', {}, [t('Monde')]), h('P', {}, [t('Menu')]), h('P', {}, [t('Retiré')])]);");
        let (_, pass, _, _) = collect(&page);
        page.run("crossbow.body.kids[0].attrs.contenteditable = 'true'; crossbow.body.kids[1].tag = 'FORM'; crossbow.add(crossbow.body.kids[1], [h('INPUT', { type: 'password' }, [])]); crossbow.body.kids[2].attrs.style = 'display: none'; crossbow.body.kids[3].kids[0].up = null; crossbow.body.kids[3].kids = [];").unwrap();
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into()), (2, "World".into()), (3, "Menu EN".into()), (4, "Removed".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 1, skipped: 3, .. }]), "only the node merely hidden since is written");
        assert_eq!(texts(&page)[1..], ["Bonjour", "Monde", "Menu EN"]);
    }

    #[test]
    fn a_new_translation_reads_the_originals_and_collect_respects_its_limits() {
        let page = started("crossbow.page('es', [h('P', {}, [t('Hola')])]);");
        let (_, pass, _, _) = collect(&page);
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into())] });
        sent(&page);
        let (_, again, _, segments) = collect(&page);
        assert_eq!((again, segments), (pass + 1, vec![]), "a collect within the translation leaves it in place");
        assert_eq!(texts(&page)[1..], ["Hello"]);
        send(&page, &TranslateCommand::Restore { pass: again });
        sent(&page);
        let (_, third, _, segments) = collect(&page);
        assert_eq!((third, segments), (pass + 2, vec![Segment { id: 1, text: "Hola".into() }]), "after Show Original, a new translation reads the original");

        let many = format!("crossbow.page('es', [{}]);", (0..=MAX_SEGMENTS).map(|i| format!("h('P', {{}}, [t('Línea {i}')])")).collect::<Vec<_>>().join(","));
        let page = started(&many);
        let (_, _, truncated, segments) = collect(&page);
        assert!(truncated && segments.len() == MAX_SEGMENTS);
        let long = format!("crossbow.page('es', [h('P', {{}}, [t('{}')]), h('P', {{}}, [t('Corta')])]);", "a".repeat(MAX_SEGMENT_CHARS + 1));
        let page = started(&long);
        let (_, _, truncated, segments) = collect(&page);
        assert!(truncated, "a node too long to send is skipped and said");
        assert_eq!(segments, vec![Segment { id: 1, text: "Corta".into() }]);
    }

    fn read(segments: &[Segment]) -> Vec<&str> {
        segments.iter().map(|segment| segment.text.as_str()).collect()
    }

    /// `count` paragraphs from `from` on, as `crossbow.page` or `crossbow.add` takes them.
    fn paragraphs(from: usize, count: usize, text: impl Fn(usize) -> String) -> String {
        format!("[{}]", (from..from + count).map(|i| format!("h('P', {{}}, [t('{}')])", text(i))).collect::<Vec<_>>().join(","))
    }

    #[test]
    fn a_later_collect_reads_only_text_the_document_has_not_sent_and_restore_ends_every_pass() {
        let page = started("crossbow.page('fr', [h('P', {}, [t('Premier')]), h('P', {}, [t('Deuxième')])]);");
        let (_, first, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Premier", "Deuxième"]);
        // The second is left as it was, as when the model gives no usable answer.
        send(&page, &TranslateCommand::Apply { pass: first, texts: vec![(1, "First".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 1, skipped: 0, .. }]));
        // history.pushState, and the page adds its next part.
        page.run("crossbow.add(crossbow.body, [h('P', {}, [t('Troisième')])])").unwrap();
        let (_, second, truncated, segments) = collect(&page);
        assert_eq!((second, truncated), (first + 1, false));
        assert_eq!(read(&segments), ["Troisième"], "nothing the document already sent goes again, translated or not");
        assert_eq!(texts(&page)[1..], ["First", "Deuxième", "Troisième"], "a later collect leaves the translation in place");
        send(&page, &TranslateCommand::Apply { pass: second, texts: vec![(1, "Third".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 1, skipped: 0, .. }]));
        // Show Original for the earlier pass, as the browser sends it when
        // the click crossed the later collect: every pass of it ends.
        send(&page, &TranslateCommand::Restore { pass: first });
        assert!(matches!(one(&page), TranslateReport::Restored { pass, restored: 2, .. } if pass == first));
        assert_eq!(texts(&page)[1..], ["Premier", "Deuxième", "Troisième"], "every original is back");
        // Asked again, as the browser asks a document that came back from
        // the back/forward cache before its answer did: nothing is shown,
        // and the page says so. A pass it never had stays unanswered.
        send(&page, &TranslateCommand::Restore { pass: second });
        assert!(matches!(one(&page), TranslateReport::Restored { pass, restored: 0, .. } if pass == second));
        assert_eq!(texts(&page)[1..], ["Premier", "Deuxième", "Troisième"]);
        page.run("command('restore', '0'); command('restore', '99');").unwrap();
        assert!(sent(&page).is_empty());
        send(&page, &TranslateCommand::Apply { pass: second, texts: vec![(1, "late".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 0, skipped: 1, .. }]), "the session is over");
        // Translate again, after Show Original: the page is read whole.
        let (_, third, _, segments) = collect(&page);
        assert_eq!((third, read(&segments)), (second + 1, vec!["Premier", "Deuxième", "Troisième"]));
    }

    #[test]
    fn show_original_for_the_latest_pass_restores_text_from_before_and_after_a_same_document_change() {
        // As the browser drives it for an Always language: a pass, the page's
        // own change, a pass for the new text, then Show Original for the latest.
        let page = started("crossbow.page('fr', [h('P', {}, [t('Avant')])]);");
        let (_, first, _, _) = collect(&page);
        send(&page, &TranslateCommand::Apply { pass: first, texts: vec![(1, "Before".into())] });
        sent(&page);
        page.run("crossbow.add(crossbow.body, [h('P', {}, [t('Après')])])").unwrap();
        let (_, second, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Après"]);
        send(&page, &TranslateCommand::Apply { pass: second, texts: vec![(1, "After".into())] });
        sent(&page);
        assert_eq!(texts(&page)[1..], ["Before", "After"]);
        send(&page, &TranslateCommand::Restore { pass: second });
        assert!(matches!(one(&page), TranslateReport::Restored { pass, restored: 2, .. } if pass == second));
        assert_eq!(texts(&page)[1..], ["Avant", "Après"], "both passes are restored");
    }

    #[test]
    fn a_document_reads_at_most_one_budget_of_nodes_and_units_over_all_its_collects() {
        let page = started(&format!("crossbow.page('es', {});", paragraphs(0, 1500, |i| format!("Línea {i}"))));
        let (_, pass, truncated, segments) = collect(&page);
        assert_eq!((truncated, segments.len()), (false, 1500));
        page.run(&format!("crossbow.add(crossbow.body, {})", paragraphs(1500, 1000, |i| format!("Línea {i}")))).unwrap();
        let (_, _, truncated, segments) = collect(&page);
        assert_eq!((truncated, segments.len(), segments[0].text.as_str()), (true, MAX_SEGMENTS - 1500, "Línea 1500"), "only what is left of the document's nodes");
        page.run(&format!("crossbow.add(crossbow.body, {})", paragraphs(2500, 10, |i| format!("Línea {i}")))).unwrap();
        let (_, last, truncated, segments) = collect(&page);
        assert!(truncated && segments.is_empty(), "nothing is left");
        // Show Original, then Translate: what would go again counts again
        // (round 2 of the AI reviews: the browser counts it too).
        send(&page, &TranslateCommand::Restore { pass: last });
        assert!(matches!(one(&page), TranslateReport::Restored { .. }));
        let (_, again, truncated, segments) = collect(&page);
        assert_eq!((again > pass, truncated, segments.len()), (true, true, 0));

        // UTF-16 units: 49 nodes of 2,000 and one of 1,000 leave 1,000.
        let page = started(&format!("crossbow.page('es', {});", paragraphs(0, 50, |i| if i == 49 { "b".repeat(1000) } else { "a".repeat(MAX_SEGMENT_CHARS) })));
        let (_, _, truncated, segments) = collect(&page);
        assert_eq!((truncated, segments.len()), (false, 50));
        page.run(&format!("crossbow.add(crossbow.body, {})", paragraphs(0, 1, |_| "c".repeat(1001)))).unwrap();
        let (_, _, truncated, segments) = collect(&page);
        assert!(truncated && segments.is_empty(), "1,001 units don't fit in what is left");
        page.run(&format!("crossbow.body.kids.pop(); crossbow.add(crossbow.body, {})", paragraphs(0, 1, |_| "d".repeat(1000)))).unwrap();
        let (_, _, truncated, segments) = collect(&page);
        assert_eq!((truncated, read(&segments)), (false, vec!["d".repeat(1000).as_str()]), "1,000 do");
    }

    #[test]
    fn a_document_that_opts_out_on_its_root_or_body_is_never_read_and_says_so() {
        for (element, opt_out) in [("root", "attrs.translate = 'no'"), ("root", "attrs['class'] = 'notranslate'"), ("body", "attrs.translate = 'NO'"), ("body", "attrs['class'] = 'page notranslate'")] {
            let page = started("crossbow.page('fr', [h('P', {}, [t('Texte de la page entière')]), h('P', {}, [t('Deuxième paragraphe')])]);");
            page.run(&format!("crossbow.{element}.{opt_out}; 'ok'")).unwrap();
            // The pre-consent probe, and a collect.
            page.run("crossbow.calls.texts = 0; 'reset'").unwrap();
            send(&page, &TranslateCommand::Probe { sample: 300 });
            send(&page, &TranslateCommand::Collect);
            assert_eq!(page.run("crossbow.calls.texts").unwrap(), "0", "{element} {opt_out}: no text node is read, not even for the pre-consent sample");
            let raw = page.run("crossbow.sent()").unwrap();
            assert!(!raw.contains("Texte") && !raw.contains("Deuxième"), "{element} {opt_out}: nothing is read: {raw:?}");
            let kinds: Vec<&str> = raw.split('\u{1e}').map(|report| report.split('\u{1f}').next().unwrap_or_default()).collect();
            assert_eq!(kinds, ["declined", "declined"], "{element} {opt_out}: the browser is told why");
            send(&page, &TranslateCommand::Apply { pass: 1, texts: vec![(1, "Whole page text".into())] });
            assert_eq!(page.run("crossbow.writes").unwrap(), "0", "{element} {opt_out}: nothing is written");
        }
        // Opting out later: a translation under way is still restored.
        let page = started("crossbow.page('fr', [h('P', {}, [t('Bonjour')])]);");
        let (_, pass, _, _) = collect(&page);
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into())] });
        sent(&page);
        page.run("crossbow.root.attrs.translate = 'no'; 'ok'").unwrap();
        send(&page, &TranslateCommand::Collect);
        assert!(page.run("crossbow.sent()").unwrap().starts_with("declined\u{1f}"));
        send(&page, &TranslateCommand::Restore { pass });
        assert!(page.run("crossbow.sent()").unwrap().starts_with("restored\u{1f}"));
        assert_eq!(texts(&page)[1..], ["Bonjour"]);
    }

    #[test]
    fn card_password_and_code_fields_keep_their_form_owner_group_and_labels_unread_whatever_their_tokens() {
        let page = started(r#"crossbow.page('fr', [
            h('P', {}, [t('Article')]),
            h('FORM', {}, [h('LABEL', {}, [t('Numéro de carte')]), h('SPAN', {}, [t('Visa se terminant par 4242, Jeanne Dupont')]), h('INPUT', { autocomplete: 'billing cc-number' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Cryptogramme')]), h('INPUT', { autocomplete: 'section-pay cc-csc' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Expiration')]), h('SELECT', { autocomplete: ' Section-A  Shipping CC-EXP-MONTH ' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Clé de sécurité')]), h('INPUT', { autocomplete: 'current-password webauthn' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Code reçu')]), h('INPUT', { autocomplete: 'section-x one-time-code' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Carte mal décrite')]), h('INPUT', { autocomplete: 'cc-number foo' }, [])]),
            h('FORM', { id: 'pay' }, [h('LABEL', {}, [t('Titulaire de la carte')])]),
            h('DIV', {}, [h('SPAN', {}, [t('Champ relié au formulaire')]), h('INPUT', { form: 'pay', autocomplete: 'cc-name' }, [])]),
            h('DIV', {}, [h('DIV', {}, [h('LABEL', {}, [t('Carte sans formulaire')]), h('INPUT', { autocomplete: 'cc-number' }, [])]), h('DIV', {}, [h('SPAN', {}, [t('Visa 1881')]), h('INPUT', { autocomplete: 'cc-exp' }, [])])]),
            h('DIV', {}, [h('LABEL', { 'for': 'pin' }, [t('Code secret')])]),
            h('DIV', {}, [h('SPAN', {}, [t('Saisie du code')]), h('INPUT', { id: 'pin', type: 'PASSWORD' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Adresse de livraison')]), h('INPUT', { autocomplete: 'section-a shipping street-address' }, []), h('INPUT', { autocomplete: 'off' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Réinitialiser le mot de passe')]), h('INPUT', { autocomplete: 'section-password-reset email' }, [])]),
            h('FORM', { id: 'later' }, [h('LABEL', {}, [t('Nom sur la carte')])]),
            h('P', {}, [t('Fin')]),
        ]);"#);
        let readable = ["Article", "Adresse de livraison", "Réinitialiser le mot de passe", "Nom sur la carte", "Fin"];
        send(&page, &TranslateCommand::Probe { sample: 2000 });
        let TranslateReport::Probe { sample, .. } = one(&page) else { panic!("a probe report") };
        assert_eq!(sample, readable.join("\n"), "the pre-consent sample skips them too");
        let (_, pass, _, segments) = collect(&page);
        assert_eq!(read(&segments), readable, "only text outside every card, password and code field's form, group and labels");
        // A field tied to a form by form= after the read: nothing is written into that form.
        page.run("crossbow.add(crossbow.body, [h('DIV', {}, [h('INPUT', { form: 'later', autocomplete: 'billing cc-number' }, [])])])").unwrap();
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Article EN".into()), (4, "Name on card".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 1, skipped: 1, .. }]));

        // Fields placed directly in the page, with no form or container: the page isn't read.
        let page = started(r#"crossbow.page('fr', [h('LABEL', {}, [t('Numéro de carte')]), h('INPUT', { autocomplete: 'cc-number' }, []), h('SPAN', {}, [t('Visa 4242')]), h('P', {}, [t('Article')])]);"#);
        let (_, _, _, segments) = collect(&page);
        assert!(segments.is_empty(), "{segments:?}");
    }

    /// What the sample and a collect read of a page holding `layout`
    /// between an "Article" and a "Fin" paragraph.
    fn read_around(layout: &str) -> (String, Vec<String>) {
        let page = started(&format!("crossbow.page('fr', [h('P', {{}}, [t('Article')]), {layout}, h('P', {{}}, [t('Fin')])]);"));
        send(&page, &TranslateCommand::Probe { sample: 2000 });
        let TranslateReport::Probe { sample, .. } = one(&page) else { panic!("a probe report") };
        let (_, _, _, segments) = collect(&page);
        (sample, read(&segments).into_iter().map(str::to_owned).collect())
    }

    /// AI-12 (second AI review): a card or password field the document's
    /// field list doesn't show still keeps the text around it unread: a
    /// payment frame (any frame in a form, or one named for a card part by
    /// the vault's words, its title included), a field in an open shadow
    /// root, however deep, standing for its outermost host, and the elements
    /// a sensitive field or its form names with aria-labelledby or
    /// aria-describedby.
    #[test]
    fn fields_in_payment_frames_and_open_shadow_roots_and_the_text_they_reference_keep_their_context_unread() {
        let layouts = [
            ("a payment frame in a form", r#"h('FORM', {}, [h('LABEL', {}, [t('Carte bancaire')]), h('DIV', {}, [h('IFRAME', { src: 'https://pay.example/card' }, [])]), h('SPAN', {}, [t('Titulaire : Jeanne Dupont, Visa 4242')]), h('BUTTON', {}, [t('Payer 20 €')])])"#),
            ("a loose frame titled for a card number", r#"h('DIV', {}, [h('SPAN', {}, [t('Numéro de carte, Visa 4242')]), h('DIV', {}, [h('IFRAME', { title: 'Secure card number input frame' }, [])])])"#),
            ("a card field in an open shadow root", r#"h('FORM', {}, [h('LABEL', {}, [t('Numéro de carte')]), crossbow.shadow(h('CARD-INPUT', {}, [h('SPAN', { slot: 'aide' }, [t('Aide dans la fente')])]), 'open', [h('SLOT', { name: 'aide' }, []), h('INPUT', { autocomplete: 'cc-number' }, [])]), h('SPAN', {}, [t('Titulaire : Jeanne Dupont')])])"#),
            ("a password field two open shadow roots down", r#"h('DIV', {}, [h('SPAN', {}, [t('Mot de passe du compte jeanne@example.com')]), crossbow.shadow(h('LOGIN-BOX', {}, []), 'open', [h('DIV', {}, [crossbow.shadow(h('PW-FIELD', {}, []), 'open', [h('INPUT', { type: 'password' }, [])])])])])"#),
            ("a form and a field named by aria-labelledby elsewhere", r#"h('H2', { id: 'paytitle' }, [t('Payer avec la Visa 4242 de Jeanne Dupont')]), h('FORM', { 'aria-labelledby': 'paytitle' }, [h('INPUT', { autocomplete: 'cc-csc', 'aria-labelledby': 'paytitle' }, [])])"#),
            ("a code field described elsewhere", r#"h('P', { id: 'sent' }, [t('Code envoyé au 06 12 34 56 78')]), h('FORM', {}, [h('INPUT', { autocomplete: 'one-time-code', 'aria-describedby': 'nothing sent' }, [])])"#),
        ];
        for (name, layout) in layouts {
            let (sample, read) = read_around(layout);
            assert_eq!(read, ["Article", "Fin"], "{name}");
            assert_eq!(sample, "Article\nFin", "{name}: the sample too");
        }
        // A frame outside any form, named for nothing the vault knows, and
        // an ordinary form's labels by id: their text is read.
        let (_, read) = read_around(r#"h('DIV', {}, [h('SPAN', {}, [t('Vidéo intégrée')]), h('IFRAME', { title: 'Lecteur vidéo' }, [])]), h('H2', { id: 'news' }, [t('Lettre d’information')]), h('FORM', { 'aria-labelledby': 'news' }, [h('INPUT', { type: 'email', 'aria-labelledby': 'news' }, [])])"#);
        assert_eq!(read, ["Article", "Vidéo intégrée", "Lettre d’information", "Fin"]);
    }

    /// AI-13 (second AI review): a card field is found as the vault finds
    /// one for Fill Card, by its words, whatever its autocomplete says; an
    /// editor counts when it is named so or holds a card number (13 to 19
    /// digits passing the Luhn check, read only to decide). A field named
    /// for nothing on a card, an email field, and an editor holding other
    /// digits leave their form readable.
    #[test]
    fn card_fields_named_by_the_vaults_words_and_editors_holding_a_card_number_keep_their_form_unread() {
        let (sample, read) = read_around(r#"
            h('FORM', {}, [h('LABEL', {}, [t('Numéro de carte')]), h('INPUT', { name: 'cardnumber', autocomplete: 'off', inputmode: 'numeric' }, []), h('SPAN', {}, [t('Visa se terminant par 4242')])]),
            h('FORM', {}, [h('LABEL', {}, [t('Carte')]), h('INPUT', { placeholder: 'Card number' }, []), h('SPAN', {}, [t('Indication en anglais')])]),
            h('FORM', {}, [h('LABEL', {}, [t('Expiry month'), h('SELECT', {}, [])]), h('SPAN', {}, [t('Mois d’expiration')])]),
            h('FORM', {}, [h('LABEL', {}, [t('Cryptogramme')]), h('INPUT', { id: 'cvc', autocomplete: 'off' }, [])]),
            h('FORM', {}, [h('LABEL', {}, [t('Numéro de carte')]), h('DIV', { contenteditable: 'true', role: 'textbox' }, [t('4242 4242 4242 4242')]), h('SPAN', {}, [t('Expire 12/29')])]),
            h('FORM', {}, [h('DIV', { contenteditable: 'true', 'aria-label': 'Card number' }, []), h('SPAN', {}, [t('Éditeur nommé')])]),
            h('FORM', {}, [h('LABEL', {}, [t('Recherche')]), h('INPUT', { name: 'search-query' }, []), h('SPAN', {}, [t('Formulaire ordinaire')])]),
            h('FORM', {}, [h('DIV', { contenteditable: 'true' }, [t('4242 4242 4242 4241')]), h('SPAN', {}, [t('Chiffres sans carte')])]),
            h('FORM', {}, [h('LABEL', {}, [t('Courriel')]), h('INPUT', { type: 'email', name: 'card-email' }, []), h('SPAN', {}, [t('Adresse électronique')])])"#);
        let readable = ["Article", "Recherche", "Formulaire ordinaire", "Chiffres sans carte", "Courriel", "Adresse électronique", "Fin"];
        assert_eq!(read, readable);
        assert_eq!(sample, readable.join("\n"), "the sample too");
    }

    /// AI-13 (second AI review): a lone sensitive field's group, or a row of
    /// code boxes, often holds no text of its own; what is left unread then
    /// reaches up to the nearest container holding some, at most CLIMB (4)
    /// levels up, so the label in a sibling box and the message above the
    /// boxes aren't read. Text further up still is.
    #[test]
    fn a_lone_sensitive_fields_group_reaches_up_to_the_nearest_container_holding_text() {
        let layouts = [
            ("a lone card field, its label in a sibling box", r#"h('DIV', { class: 'row' }, [h('DIV', {}, [h('SPAN', {}, [t('Numéro de carte')]), h('SPAN', {}, [t('Visa se terminant par 4242, Jeanne Dupont')])]), h('DIV', {}, [h('INPUT', { autocomplete: 'cc-number' }, [])])])"#),
            ("code boxes under their message", r#"h('DIV', {}, [h('P', {}, [t('Saisissez le code envoyé au 06 12 34 56 78')]), h('DIV', { class: 'otp' }, [h('INPUT', { autocomplete: 'one-time-code' }, []), h('INPUT', {}, []), h('INPUT', {}, [])])])"#),
            ("a password field two wrappers down", r#"h('DIV', {}, [h('LABEL', {}, [t('Mot de passe')]), h('DIV', {}, [h('DIV', {}, [h('INPUT', { type: 'password' }, [])])])])"#),
            ("a card field in a form holding no text", r#"h('DIV', {}, [h('SPAN', {}, [t('Numéro de carte')]), h('FORM', {}, [h('INPUT', { autocomplete: 'cc-number' }, [])])])"#),
        ];
        for (name, layout) in layouts {
            let (_, read) = read_around(layout);
            assert_eq!(read, ["Article", "Fin"], "{name}");
        }
        // Five empty wrappers up: past CLIMB, the text beside them is read.
        let (_, read) = read_around(r#"h('DIV', {}, [h('P', {}, [t('Texte lointain')]), h('DIV', {}, [h('DIV', {}, [h('DIV', {}, [h('DIV', {}, [h('DIV', {}, [h('INPUT', { autocomplete: 'cc-number' }, [])])])])])])])"#);
        assert_eq!(read, ["Article", "Texte lointain", "Fin"]);
    }

    /// A limit, not a goal: a field in a closed shadow root is out of the
    /// page script's sight, as it is for the vault, so the text beside its
    /// host is read.
    #[test]
    fn a_field_in_a_closed_shadow_root_is_out_of_sight_and_the_text_beside_its_host_is_read() {
        let (_, read) = read_around(r#"h('DIV', {}, [h('SPAN', {}, [t('Mot de passe du compte')]), crossbow.shadow(h('PW-FIELD', {}, []), 'closed', [h('INPUT', { type: 'password' }, [])])])"#);
        assert_eq!(read, ["Article", "Mot de passe du compte", "Fin"]);
    }

    #[test]
    fn text_the_user_cannot_see_is_never_read_and_text_merely_out_of_view_still_is() {
        let page = started(r#"crossbow.page('fr', [
            h('P', {}, [t('Visible')]),
            h('SPAN', { style: 'color: rgb(200, 30, 0)' }, [t('Rouge')]),
            h('SPAN', { style: 'position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0px, 0px, 0px, 0px)', box: '0 0 1 1' }, [t('Pour lecteur d’écran')]),
            h('DIV', { style: 'clip-path: inset(50%)' }, [t('Découpé')]),
            h('DIV', { style: 'filter: blur(4px)' }, [h('SPAN', {}, [t('Clé API floutée')])]),
            h('DIV', { style: 'mask-image: linear-gradient(transparent, transparent)' }, [t('Masqué')]),
            h('DIV', { style: '-webkit-mask-image: linear-gradient(transparent, transparent)' }, [t('Masqué aussi')]),
            h('DIV', { style: 'mask-border-source: url(masque.png)' }, [t('Bordure masquante')]),
            h('DIV', { style: 'mix-blend-mode: multiply' }, [t('Fondu')]),
            h('DIV', { style: 'opacity: 0.6' }, [h('P', { style: 'opacity: 0.6' }, [t('Trop pâle')])]),
            h('DIV', { style: 'opacity: 0.6' }, [t('Atténué')]),
            h('SPAN', { style: '-webkit-text-security: disc' }, [t('Numéro de compte')]),
            h('DIV', { style: 'font-size: 0px' }, [t('Taille nulle'), h('SPAN', { style: 'font-size: 16px' }, [t('Taille rétablie')])]),
            h('SPAN', { style: 'color: rgba(0, 0, 0, 0)' }, [t('Transparent')]),
            h('SPAN', { style: '-webkit-text-fill-color: transparent' }, [t('Remplissage transparent')]),
            h('DIV', { style: 'overflow: hidden', box: '0 0 0 0' }, [h('P', {}, [t('Boîte nulle')])]),
            h('DIV', { box: '0 0 800 4' }, [t('Petite boîte qui déborde')]),
            h('DIV', { style: 'position: absolute; left: -10000px', box: '-10000 0 100 20' }, [t('Hors écran')]),
            h('DIV', { box: '0 -900 800 20' }, [t('Au-dessus du document')]),
            h('DIV', { box: '0 -500 800 20' }, [t('Déjà lu plus haut')]),
            h('DIV', { box: '0 4000 800 20' }, [t('Plus bas')]),
            crossbow.shadow(h('DIV', {}, [t('Hors des fentes'), h('SPAN', { slot: 'titre' }, [t('Dans la fente')])]), 'open', [h('SLOT', { name: 'titre', style: 'display: contents' }, [])]),
            crossbow.shadow(h('DIV', {}, [h('SPAN', { slot: 'x' }, [t('Fente floutée')])]), 'open', [h('DIV', { style: 'filter: blur(8px)' }, [h('SLOT', { name: 'x', style: 'display: contents' }, [])])]),
            crossbow.shadow(h('DIV', {}, [t('Hôte fermé')]), 'closed', []),
            h('P', {}, [t('Fin')]),
        ]); crossbow.scrollY = 600;"#);
        let readable = ["Visible", "Rouge", "Atténué", "Taille rétablie", "Petite boîte qui déborde", "Déjà lu plus haut", "Plus bas", "Dans la fente", "Fin"];
        send(&page, &TranslateCommand::Probe { sample: 2000 });
        let TranslateReport::Probe { sample, .. } = one(&page) else { panic!("a probe report") };
        assert_eq!(sample, readable.join("\n"), "the pre-consent sample is held to the same rule");
        let (_, _, _, segments) = collect(&page);
        assert_eq!(read(&segments), readable);
    }

    #[test]
    fn the_root_and_body_clip_nothing_and_a_page_that_scrolls_left_keeps_its_text_there() {
        // The root's and the body's overflow is the viewport's: a body with no height still shows its text.
        let page = started(r#"crossbow.page('fr', [h('P', {}, [t('Corps sans hauteur')])]); crossbow.body.attrs.style = 'overflow: hidden'; crossbow.body.attrs.box = '0 0 800 0';"#);
        let (_, _, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Corps sans hauteur"]);
        // Right to left, or written in vertical lines from the right, a page scrolls left to text there.
        for root in ["direction: rtl", "writing-mode: vertical-rl"] {
            let page = started(&format!(r#"crossbow.page('ar', [h('DIV', {{ box: '-300 0 100 20' }}, [t('Plus loin')])]); crossbow.root.attrs.style = '{root}';"#));
            let (_, _, _, segments) = collect(&page);
            assert_eq!(read(&segments), ["Plus loin"], "{root}");
        }
    }

    /// AI-6's remainder (second AI review): text pushed off the page by
    /// text-indent, scaled to nothing, in a colour too faint to read by the
    /// opacity rule's threshold (with the opacity it is drawn with), or past
    /// a right edge the user can't scroll to isn't read, nor sampled. Text
    /// faint but readable, indented on the page, partly past the edge, or
    /// past the right edge of a page that scrolls there still is.
    #[test]
    fn text_pushed_off_scaled_away_too_faint_or_past_an_unscrollable_right_edge_is_not_read() {
        let kids = r#"[
            h('H1', { style: 'text-indent: -9999px; overflow: hidden', box: '0 0 300 40' }, [t('Logo remplacé par une image')]),
            h('DIV', { style: 'transform: scale(0)' }, [t('Réduit à rien')]),
            h('SPAN', { style: 'color: rgba(0, 0, 0, 0.01)' }, [t('Presque transparent')]),
            h('SPAN', { style: 'color: rgba(0, 0, 0, 0.4)' }, [t('Trop pâle')]),
            h('SPAN', { style: 'color: color(srgb 0 0 0 / 0.3)' }, [t('Pâle aussi')]),
            h('SPAN', { style: '-webkit-text-fill-color: rgba(0, 0, 0, 0.2)' }, [t('Remplissage pâle')]),
            h('DIV', { style: 'opacity: 0.7' }, [h('SPAN', { style: 'color: rgba(0, 0, 0, 0.6)' }, [t('Pâle deux fois')])]),
            h('SPAN', { style: 'color: rgba(0, 0, 0, 0.6)' }, [t('Gris lisible')]),
            h('P', { style: 'text-indent: 32px' }, [t('Retrait ordinaire')]),
            h('DIV', { box: '10000 0 100 20' }, [t('Au-delà du bord droit')]),
            h('DIV', { box: '700 0 200 20' }, [t('À cheval sur le bord')]),
            h('P', {}, [t('Fin')]),
        ]"#;
        let readable = ["Gris lisible", "Retrait ordinaire", "À cheval sur le bord", "Fin"];
        for viewport in ["crossbow.root.attrs.style = 'overflow-x: hidden'", "crossbow.root.attrs.style = 'overflow-x: clip'", "crossbow.body.attrs.style = 'overflow-x: hidden'"] {
            let page = started(&format!("crossbow.page('fr', {kids}); {viewport};"));
            send(&page, &TranslateCommand::Probe { sample: 2000 });
            let TranslateReport::Probe { sample, .. } = one(&page) else { panic!("a probe report") };
            assert_eq!(sample, readable.join("\n"), "the sample is held to the same rule: {viewport}");
            let (_, _, _, segments) = collect(&page);
            assert_eq!(read(&segments), readable, "{viewport}");
        }
        // Right to left, a page scrolls left but never right of where it starts.
        let page = started(r#"crossbow.page('ar', [h('DIV', { box: '10800 0 100 20' }, [t('Hors écran à droite')]), h('DIV', { box: '-300 0 100 20' }, [t('Plus loin à gauche')]), h('P', {}, [t('Fin')])]); crossbow.root.attrs.style = 'direction: rtl';"#);
        let (_, _, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Plus loin à gauche", "Fin"]);
        // Left to right with nothing clipping: the user scrolls right to it.
        let page = started(r#"crossbow.page('fr', [h('DIV', { box: '10000 0 100 20' }, [t('Plus loin à droite')])]);"#);
        let (_, _, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Plus loin à droite"]);
    }

    /// AI-11 (second AI review): a page that put index setters on
    /// Array.prototype before it could see anything gets no record, no
    /// list and no report's strings from the script, so it can't reset what
    /// the document has read: over 60 rounds of new text in one node, the
    /// document reads at most one budget (the review's probe read 119,870
    /// UTF-16 units).
    #[test]
    fn a_page_with_array_index_setters_gets_nothing_from_the_script_and_one_budget_holds() {
        let page = started("crossbow.page('es', [h('P', {}, [t('Hola')])]);");
        page.run(r#"
            var caught = 0, held = Object.create(null), holding = 0;
            for (var i = 0; i < 12; i += 1) (function (index) {
                Object.defineProperty(Array.prototype, String(index), { configurable: true, set: function (value) {
                    if (value !== null && typeof value === 'object' && Object.getPrototypeOf(value) === null && 'original' in value) { held[holding] = value; holding += 1; caught += 1; }
                    else if (this !== crossbow.reports && this[0] === 'segments') caught += 1;
                    Object.defineProperty(this, String(index), { value: value, writable: true, enumerable: true, configurable: true });
                } });
            })(i);
            'armed'"#).unwrap();
        let (_, pass, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Hola"]);
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into())] });
        sent(&page);
        let mut units = 0;
        for k in 0..60 {
            page.run(&format!("var n = crossbow.body.kids[0].kids[0]; n.value = 'Texto {k} ' + Array(1990).join('a'); for (var j = 0; j < holding; j += 1) {{ held[j].original = n.value; held[j].first = -1; }} 'reset'")).unwrap();
            let (_, _, _, segments) = collect(&page);
            units += segments.iter().map(|segment| segment.text.encode_utf16().count()).sum::<usize>();
        }
        assert_eq!(page.run("String(caught)").unwrap(), "0", "the page saw none of the script's records or reports");
        assert!(units <= MAX_CHARS, "one document read {units} UTF-16 units");
    }

    /// Not a pass/fail regression check but a measurement, kept bounded:
    /// the rule's cost on 2,000 text nodes.
    #[test]
    fn the_visibility_rule_reads_each_element_once_on_two_thousand_text_nodes() {
        let sections: Vec<String> = (0..40).map(|s| format!("h('SECTION', {{}}, [{}])", (0..50).map(|p| format!("h('P', {{}}, [h('SPAN', {{}}, [t('Ligne {s}-{p}')])])")).collect::<Vec<_>>().join(","))).collect();
        let page = started(&format!("crossbow.page('fr', [h('MAIN', {{}}, [h('ARTICLE', {{}}, [{}])])]);", sections.join(",")));
        // The root, the body, MAIN, ARTICLE, 40 sections, and a paragraph and a span for each text node.
        let elements = 4 + 40 + 2 * 2000;
        page.run("crossbow.calls = { styles: 0, boxes: 0, ranges: 0, texts: 0 }; 'reset'").unwrap();
        let start = std::time::Instant::now();
        let (_, _, truncated, segments) = collect(&page);
        let elapsed = start.elapsed();
        assert_eq!((truncated, segments.len()), (false, 2000));
        let calls: Vec<usize> = page.run("[crossbow.calls.styles, crossbow.calls.boxes, crossbow.calls.ranges].join(' ')").unwrap().split(' ').map(|n| n.parse().unwrap()).collect();
        println!("translate visibility cost: 2,000 text nodes under {elements} elements: {} computed styles, {} boxes, {} text ranges, collect {elapsed:?} in JavaScriptCore", calls[0], calls[1], calls[2]);
        assert!(calls[0] <= elements + 1 && calls[1] <= elements && calls[2] <= 2000, "at most one style and one box for each element, one range for each text node: {calls:?}");
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    /// A limit, not a goal (AI-14, second AI review): the rule shares the
    /// vault's hiding styles, so text drawn under any clip-path, filter,
    /// mask or blend mode, or with a transparent fill painted by its
    /// background (gradient text), counts as hidden even when it shows:
    /// all four of the review's visible examples stay untranslated. Telling
    /// them apart would need the clip's shape and the gradient's colours.
    #[test]
    fn visible_text_under_a_clip_path_mask_blend_mode_or_gradient_fill_stays_untranslated() {
        let (_, read) = read_around(r#"
            h('H1', { style: '-webkit-text-fill-color: transparent; background-clip: text; background-image: linear-gradient(red, blue)' }, [t('Titre en dégradé visible')]),
            h('SECTION', { style: 'clip-path: polygon(0 0, 100% 0, 100% 90%, 0 100%)' }, [h('P', {}, [t('Section en biais visible')])]),
            h('DIV', { style: 'mask-image: linear-gradient(black 70%, transparent)' }, [t('Aperçu qui s’estompe visible')]),
            h('DIV', { style: 'mix-blend-mode: multiply' }, [t('Texte en multiplication visible')])"#);
        assert_eq!(read, ["Article", "Fin"]);
    }

    /// AI-15 (second AI review): slotted children don't multiply the reads.
    /// 200 children slotted through 100 wrappers (the review counted 20,404
    /// computed styles and 20,203 boxes) read each element's style and box
    /// at most once in a walk; a wrapper that hides still hides every child
    /// it passes on, and one too faint with the text's own colour too.
    #[test]
    fn slotted_children_read_each_wrapper_once() {
        let (children, depth) = (200usize, 100usize);
        let kids = (0..children).map(|i| format!("h('SPAN', {{ slot: 'x' }}, [t('Élément {i}')])")).collect::<Vec<_>>().join(",");
        let wrappers = |style: &str| format!("{}h('SLOT', {{ name: 'x', style: 'display: contents' }}, []){}", format!("h('DIV', {{ style: '{style}' }}, [").repeat(depth), "])".repeat(depth));
        let page = started(&format!("crossbow.page('fr', [crossbow.shadow(h('DIV', {{}}, [{kids}]), 'open', [{}])]);", wrappers("")));
        page.run("crossbow.calls = { styles: 0, boxes: 0, ranges: 0, texts: 0 }; 'reset'").unwrap();
        // The root, the body, the host, its children, the wrappers and the slot.
        let elements = 2 + 1 + children + depth + 1;
        let (_, _, _, segments) = collect(&page);
        assert_eq!(segments.len(), children);
        let calls: Vec<usize> = page.run("[crossbow.calls.styles, crossbow.calls.boxes].join(' ')").unwrap().split(' ').map(|n| n.parse().unwrap()).collect();
        println!("translate slot cost: {children} slotted children through {depth} wrappers, {elements} elements: {} computed styles, {} boxes", calls[0], calls[1]);
        assert!(calls[0] <= elements + 1 && calls[1] <= elements, "at most one style and one box for each element: {calls:?}");

        // One wrapper hides: nothing it passes on is read, slotted text included.
        let page = started(&format!("crossbow.page('fr', [crossbow.shadow(h('DIV', {{}}, [{kids}, t('Texte glissé')]), 'open', [h('DIV', {{ style: 'filter: blur(4px)' }}, [h('SLOT', {{ name: 'x', style: 'display: contents' }}, []), h('SLOT', {{ style: 'display: contents' }}, [])])]), h('P', {{}}, [t('Fin')])]);"));
        let (_, _, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Fin"]);
        // A wrapper at 0.8 above a slot passing on text in a colour at 0.6:
        // drawn at 0.48, under the opacity rule's threshold.
        let page = started(r#"crossbow.page('fr', [crossbow.shadow(h('DIV', {}, [t('Texte pâle'), h('SPAN', { slot: 'x' }, [t('Élément lisible')])]), 'open', [h('DIV', { style: 'opacity: 0.8' }, [h('SLOT', { style: 'display: contents; color: rgba(0, 0, 0, 0.6)' }, []), h('SLOT', { name: 'x', style: 'display: contents' }, [])])]), h('P', {}, [t('Fin')])]);"#);
        let (_, _, _, segments) = collect(&page);
        assert_eq!(read(&segments), ["Élément lisible", "Fin"]);
    }

    #[test]
    fn a_page_that_patches_built_ins_after_the_script_started_cannot_break_it() {
        let page = started("crossbow.page('it', [h('P', {}, [t('Ciao')]), h('P', {}, [t('Mondo')])]);");
        page.run(r#"
            var boom = function () { throw new Error('patched'); };
            Object.defineProperty(Node.prototype, 'nodeValue', { get: boom, set: boom, configurable: true });
            Object.defineProperty(Node.prototype, 'firstChild', { get: boom, configurable: true });
            Element.prototype.getAttribute = boom; Element.prototype.querySelector = boom; Element.prototype.checkVisibility = boom;
            Element.prototype.getBoundingClientRect = boom; Range.prototype.getClientRects = boom; Range.prototype.selectNodeContents = boom; Document.prototype.createRange = boom;
            Range.prototype.getBoundingClientRect = boom; Object.defineProperty(Element.prototype, 'clientWidth', { get: boom, configurable: true }); String.prototype.lastIndexOf = boom;
            CSSStyleDeclaration.prototype.getPropertyValue = boom; globalThis.getComputedStyle = boom;
            Object.defineProperty(DOMRectReadOnly.prototype, 'width', { get: boom, configurable: true });
            Object.defineProperty(globalThis, 'scrollY', { get: boom, configurable: true });
            String.prototype.slice = boom; String.prototype.charCodeAt = boom; String.prototype.toLowerCase = boom; String.prototype.indexOf = boom;
            Math.random = boom; Number.isInteger = boom; RegExp.prototype.exec = boom;
            'patched'"#).unwrap();
        let (_, pass, _, segments) = collect(&page);
        assert_eq!(segments.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>(), ["Ciao", "Mondo"]);
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into())] });
        assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 1, skipped: 0, .. }]));
    }

    // AI-18 (third AI review): text naming a sensitive field in five more
    // layouts, each the review's failure scenario. Only the text around
    // the field stays unread; the rest of the page still is.

    /// A role=group of loose card fields named by aria-labelledby: what the
    /// group's own aria-labelledby names, between the field and its group.
    #[test]
    fn a_card_group_named_by_aria_labelledby_keeps_its_name_unread() {
        let (sample, read) = read_around(r#"h('H2', { id: 'grp' }, [t('Payer avec la Visa 4242 de Jeanne Dupont')]), h('P', {}, [t('Texte entre deux')]), h('DIV', { role: 'group', 'aria-labelledby': 'grp' }, [h('INPUT', { autocomplete: 'cc-number' }, []), h('INPUT', { autocomplete: 'cc-exp' }, [])])"#);
        assert_eq!(read, ["Article", "Texte entre deux", "Fin"]);
        assert_eq!(sample, "Article\nTexte entre deux\nFin", "the sample too");
    }

    /// A fieldset in a card form, named by aria-labelledby outside the form.
    #[test]
    fn a_fieldset_named_by_aria_labelledby_outside_its_form_keeps_its_name_unread() {
        let (sample, read) = read_around(r#"h('H2', { id: 'fs' }, [t('Carte de Jeanne Dupont, Visa 4242')]), h('FORM', {}, [h('FIELDSET', { 'aria-labelledby': 'fs' }, [h('INPUT', { autocomplete: 'cc-number' }, [])])])"#);
        assert_eq!(read, ["Article", "Fin"]);
        assert_eq!(sample, "Article\nFin", "the sample too");
    }

    /// A light-DOM label for= a shadow host whose open shadow root holds a
    /// card field, the label in a sibling box.
    #[test]
    fn a_label_for_a_shadow_host_holding_a_card_field_is_not_read() {
        let (sample, read) = read_around(r#"h('DIV', {}, [h('LABEL', { 'for': 'cardhost' }, [t('Numéro de carte, Visa 4242 de Jeanne Dupont')])]), h('DIV', {}, [h('SPAN', {}, [t('Saisie sécurisée')]), crossbow.shadow(h('CARD-FIELD', { id: 'cardhost' }, []), 'open', [h('INPUT', { autocomplete: 'cc-number' }, [])])])"#);
        assert_eq!(read, ["Article", "Fin"]);
        assert_eq!(sample, "Article\nFin", "the sample too");
    }

    /// A light-DOM label for= an id inside a host's open shadow root, which
    /// the DOM doesn't tie to the field, though its words name it.
    #[test]
    fn a_label_for_an_id_inside_an_open_shadow_root_is_not_read() {
        let (sample, read) = read_around(r#"h('DIV', {}, [h('LABEL', { 'for': 'cc' }, [t('Numéro de carte, Visa 4242 de Jeanne Dupont')])]), h('DIV', {}, [h('SPAN', {}, [t('Saisie sécurisée')]), crossbow.shadow(h('CARD-FIELD', {}, []), 'open', [h('INPUT', { id: 'cc', autocomplete: 'cc-number' }, [])])])"#);
        assert_eq!(read, ["Article", "Fin"]);
        assert_eq!(sample, "Article\nFin", "the sample too");
        // A label for= an id nothing sensitive holds is read.
        let (_, read) = read_around(r#"h('DIV', {}, [h('LABEL', { 'for': 'q' }, [t('Rechercher')])]), h('DIV', {}, [crossbow.shadow(h('SEARCH-BOX', {}, []), 'open', [h('INPUT', { id: 'q', type: 'search' }, [])])])"#);
        assert_eq!(read, ["Article", "Rechercher", "Fin"]);
    }

    /// aria-labelledby naming 17 ids: every one is unread, past the 16
    /// autocomplete tokens a field may have.
    #[test]
    fn every_id_aria_labelledby_names_is_unread_however_many() {
        let spans: Vec<String> = (1..=40).map(|i| format!("h('SPAN', {{ id: 'x{i}' }}, [t('Partie {i} : Visa 4242')])")).collect();
        for count in [17usize, 40] {
            let ids: Vec<String> = (1..=count).map(|i| format!("x{i}")).collect();
            let (sample, read) = read_around(&format!("h('DIV', {{}}, [{}]), h('P', {{}}, [t('Entre')]), h('FORM', {{}}, [h('INPUT', {{ type: 'password', 'aria-labelledby': '{}' }}, [])])", spans[..count].join(", "), ids.join(" ")));
            assert_eq!(read, ["Article", "Entre", "Fin"], "{count} ids");
            assert_eq!(sample, "Article\nEntre\nFin", "{count} ids: the sample too");
        }
    }

    /// A loose payment frame a processor names without the vault's card
    /// words, as Stripe names its own; a frame named for nothing of a
    /// payment's is still read beside.
    #[test]
    fn a_loose_frame_a_payment_processor_names_keeps_its_group_unread() {
        for frame in [
            "{ title: 'Secure card payment input frame', name: '__privateStripeFrame5' }",
            "{ name: '__privateStripeFrame5' }",
            "{ title: 'Secure payment input frame' }",
            "{ src: 'https://js.stripe.com/v3/elements-inner-card.html' }",
            "{ title: 'Iframe for secured card number', src: 'https://checkoutshopper-live.adyen.com/x/securedFields.html' }",
            "{ id: 'braintree-hosted-field-number' }",
        ] {
            let (sample, read) = read_around(&format!("h('DIV', {{}}, [h('SPAN', {{}}, [t('Titulaire : Jeanne Dupont, Visa 4242')]), h('DIV', {{}}, [h('IFRAME', {frame}, [])])])"));
            assert_eq!(read, ["Article", "Fin"], "{frame}");
            assert_eq!(sample, "Article\nFin", "{frame}: the sample too");
        }
        let (_, read) = read_around(r#"h('DIV', {}, [h('SPAN', {}, [t('Vidéo intégrée')]), h('IFRAME', { title: 'Lecteur vidéo', src: 'https://www.youtube-nocookie.com/embed/x' }, [])])"#);
        assert_eq!(read, ["Article", "Vidéo intégrée", "Fin"]);
    }

    // AI-20 (third AI review): the script trims what Rust's `trim` trims.

    /// A node of only a Unicode space no longer makes the browser drop the
    /// whole report: the rest of the page is read and can be translated.
    #[test]
    fn a_node_of_only_a_unicode_space_leaves_the_rest_of_the_page_readable() {
        for ch in ['\u{2028}', '\u{2029}', '\u{85}', '\u{1680}', '\u{b}', '\u{a0}', '\u{3000}'] {
            let page = started(&format!("crossbow.page('fr', [h('P', {{}}, [t('Bonjour')]), h('P', {{}}, [t({})]), h('P', {{}}, [t({})]), h('P', {{}}, [t('Fin')])]);", literal(&ch.to_string()), literal(&format!("{ch}Milieu{ch}"))));
            let (_, pass, _, segments) = collect(&page);
            assert_eq!(read(&segments), ["Bonjour", "Milieu", "Fin"], "U+{:04X}", ch as u32);
            send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into()), (2, "Middle".into()), (3, "End".into())] });
            assert!(matches!(sent(&page).as_slice(), [TranslateReport::Applied { written: 3, skipped: 0, .. }]), "U+{:04X}", ch as u32);
            assert_eq!(texts(&page)[3], format!("{ch}Middle{ch}"), "U+{:04X}: the spaces stay in place", ch as u32);
        }
    }

    /// The script's white space is Rust's, over all of Unicode: what the
    /// browser's `trim` would remove, the script trims first. Characters
    /// past U+FFFF reach the script as two surrogates, neither of them
    /// white space, and Rust has no white space there.
    #[test]
    fn the_scripts_white_space_is_rusts_over_all_of_unicode() {
        let start = SCRIPT.find("  const space = (code) => ").expect("space()");
        let line = &SCRIPT[start..start + SCRIPT[start..].find(";\n").unwrap() + 1];
        let page = Page::new();
        let found = page.run(&format!("{line}\nvar found = []; for (var code = 0; code <= 0xffff; code += 1) if (space(code)) found.push(code); found.join(' ')")).unwrap();
        let script: Vec<u32> = found.split(' ').map(|code| code.parse().unwrap()).collect();
        let rust: Vec<u32> = (0..=0xffff).filter(|code| char::from_u32(*code).is_some_and(char::is_whitespace)).collect();
        assert_eq!(script, rust);
        assert!((0x1_0000..=0x10_ffff).filter_map(char::from_u32).all(|c| !c.is_whitespace()));
    }

    /// What the document did before, as its hello says it.
    fn said_before(page: &Page) -> Earlier {
        send(page, &TranslateCommand::Hello);
        let TranslateReport::Hello { earlier, .. } = one(page) else { panic!("a hello") };
        earlier
    }

    /// AI-17 (third AI review): the script keeps what its document did, and
    /// says it with each probe and hello, so a document the back/forward
    /// cache brings back tells the browser whatever the browser kept:
    /// whether a probe reported a sample, the nodes and units its collects
    /// read, and the first pass of the translation it shows. A new
    /// document's is all zero, and a probe's own sample counts only after.
    #[test]
    fn a_documents_probe_and_hello_say_what_it_did_before() {
        let page = started("crossbow.page('fr', [h('P', {}, [t('Bonjour')]), h('P', {}, [t('le monde')])]);");
        assert_eq!(said_before(&page), Earlier::default());
        send(&page, &TranslateCommand::Probe { sample: 300 });
        let TranslateReport::Probe { earlier: before, sample, .. } = one(&page) else { panic!("a probe") };
        assert_eq!((before, sample.as_str()), (Earlier::default(), "Bonjour\nle monde"));
        assert_eq!(said_before(&page), Earlier { sampled: true, ..Earlier::default() });
        let (_, pass, _, _) = collect(&page);
        send(&page, &TranslateCommand::Apply { pass, texts: vec![(1, "Hello".into())] });
        sent(&page);
        send(&page, &TranslateCommand::Probe { sample: 300 });
        let TranslateReport::Probe { earlier: before, .. } = one(&page) else { panic!("a probe") };
        assert_eq!(before, Earlier { sampled: true, nodes: 2, units: 15, shown: pass });
        // Show Original: nothing shown, the totals stay.
        send(&page, &TranslateCommand::Restore { pass });
        sent(&page);
        assert_eq!(said_before(&page), Earlier { sampled: true, nodes: 2, units: 15, shown: 0 });
        // A new translation reads again and counts again.
        let (_, again, _, segments) = collect(&page);
        assert_eq!(segments.len(), 2);
        assert_eq!(said_before(&page), Earlier { sampled: true, nodes: 4, units: 30, shown: again });

        // A page with no text to sample has sent none.
        let page = started("crossbow.page('fr', [h('P', {}, [t('42')])]);");
        send(&page, &TranslateCommand::Probe { sample: 300 });
        one(&page);
        assert_eq!(said_before(&page), Earlier::default());
    }
}
