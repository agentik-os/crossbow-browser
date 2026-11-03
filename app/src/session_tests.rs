use super::*;
use crossbow_core::store::TabRow;

fn tab(id: &str, space: &str, item: Option<&str>) -> Tab {
    Tab::from_row(TabRow {
        id: id.into(), space_id: space.into(), item_id: item.map(str::to_owned),
        url: format!("http://127.0.0.1/{id}"), title: id.into(), order: "a0".into(),
        created_ms: 1, last_active_ms: 2,
    })
}

fn library() -> Library {
    Library::open(Store::open(std::path::Path::new(":memory:")).unwrap()).unwrap()
}

#[test]
fn tab_context_comes_from_its_owner_and_cross_profile_moves_fail_closed() {
    let mut lib = library();
    let a = lib.create_profile("A").unwrap();
    let b = lib.create_profile("B").unwrap();
    let sa = lib.create_space("A", "A", Theme::preset(0), &a).unwrap();
    let sa2 = lib.create_space("A2", "A", Theme::preset(0), &a).unwrap();
    let sb = lib.create_space("B", "B", Theme::preset(0), &b).unwrap();
    let favorite = lib.add_favorite(&a, "http://127.0.0.1/favorite", "Favorite A").unwrap();
    let pin = lib.add_pinned(&sa, None, "http://127.0.0.1/pin", "Pin A").unwrap();
    for tab in [tab("today", &sa, None), tab("pin", &sa, Some(&pin)), tab("favorite", FAVORITES_SPACE, Some(&favorite))] {
        assert_eq!(tab_profile(&lib, &tab), Some(a.as_str()));
        assert!(can_move_tab(&lib, &tab, &sa2));
        assert!(!can_move_tab(&lib, &tab, &sb));
        assert!(!can_move_tab(&lib, &tab, "deleted-space"));
    }
    assert!(!item_in_profile(&lib, &lib.items[&favorite], &b));
    assert_eq!(tab_profile(&lib, &tab("mismatched-pin", &sb, Some(&pin))), None);
    assert_eq!(tab_profile(&lib, &tab("missing-item", FAVORITES_SPACE, Some("deleted"))), None);
    assert_eq!(tab_profile(&lib, &tab("missing-space", "deleted", None)), None);
    lib.delete_item(&favorite).unwrap();
    assert_eq!(tab_profile(&lib, &tab("favorite", FAVORITES_SPACE, Some(&favorite))), None);
}

#[test]
fn profile_policy_rejects_live_view_ownership_drift() {
    use crossbow_core::model::ProfilePreferences;
    let mut lib = library();
    let a = lib.create_profile("Context A").unwrap();
    let b = lib.create_profile("Context B").unwrap();
    let space = lib.create_space("Context", "C", Theme::preset(0), &a).unwrap();
    let favorite = lib.add_favorite(&a, "https://example.test/", "Context favorite").unwrap();
    let today = tab("context-today", &space, None);
    let favorite_tab = tab("context-favorite", FAVORITES_SPACE, Some(&favorite));
    lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(0) })).unwrap();
    lib.set_profile_preferences(&b, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(1) })).unwrap();
    for tab in [&today, &favorite_tab] {
        assert_eq!(tab_profile_with_view_owner(&lib, tab, Some(&a)), Some(a.as_str()));
        assert_eq!(tab_profile_with_view_owner(&lib, tab, Some(&b)), None);
    }
    lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap();
    lib.update_item(&favorite, |item| item.profile_id = b.clone()).unwrap();
    for tab in [&today, &favorite_tab] {
        assert_eq!(tab_profile_with_view_owner(&lib, tab, Some(&a)), None, "an A context cannot adopt B's policy");
        assert_eq!(tab_profile_with_view_owner(&lib, tab, Some(&b)), Some(b.as_str()));
        assert_eq!(tab_profile_with_view_owner(&lib, tab, None), Some(b.as_str()), "a cold record has no old live context");
    }
}

#[test]
fn rr01_command_dispatch_rejects_changed_owner_focus_space_and_removed_target() {
    for fault in ["owner", "focus", "space", "removed", "unloaded-owner", "view-owner", "deleted-space", "deleted-profile"] {
        let mut lib = library();
        let a = lib.create_profile("Command A").unwrap();
        let b = lib.create_profile("Command B").unwrap();
        let space = lib.create_space("Command A", "A", Theme::preset(0), &a).unwrap();
        let other_space = lib.create_space("Other A space", "A", Theme::preset(0), &a).unwrap();
        let original = tab("original", &space, None);
        let other = tab("other", &space, None);
        let origin = CommandOrigin::capture(&lib, &space, Some(&original), Some(&a)).unwrap();
        assert_eq!(command_url_target(&lib, &origin, &space, Some(&original), Some(&a), false), Some(CommandUrlTarget::Tab(original.id.clone())));
        if matches!(fault, "owner" | "unloaded-owner") { lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap(); }
        if fault == "deleted-space" { lib.spaces.remove(&space); }
        if fault == "deleted-profile" { lib.profiles.remove(&a); }
        let current_space = if fault == "space" { &other_space } else { &space };
        let current = match fault { "focus" => Some(&other), "removed" => None, _ => Some(&original) };
        let view_owner = match fault { "unloaded-owner" => None, "view-owner" => Some(b.as_str()), _ => Some(a.as_str()) };
        for new_tab in [false, true] {
            assert_eq!(command_url_target(&lib, &origin, current_space, current, view_owner, new_tab), None, "queued search retargeted after {fault}, new_tab={new_tab}");
        }
    }
}

#[test]
fn rr01_command_destination_preserves_valid_blank_cold_and_new_tab_behavior() {
    let mut lib = library();
    let a = lib.create_profile("Command A").unwrap();
    let space = lib.create_space("Command", "C", Theme::preset(0), &a).unwrap();
    let mut current = tab("current", &space, None);
    let origin = CommandOrigin::capture(&lib, &space, Some(&current), None).unwrap();
    assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), None, false), Some(CommandUrlTarget::NewTab));
    current.url = NEW_TAB_URL.into();
    assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), None, false), Some(CommandUrlTarget::Tab(current.id.clone())));
    assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), None, true), Some(CommandUrlTarget::NewTab));
    current.url = String::new();
    assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), None, false), Some(CommandUrlTarget::Tab(current.id.clone())));
    let favorite = lib.add_favorite(&a, "https://example.test/favorite", "Favorite").unwrap();
    let pin = lib.add_pinned(&space, None, "https://example.test/pin", "Pin").unwrap();
    for current in [tab("favorite", FAVORITES_SPACE, Some(&favorite)), tab("pin", &space, Some(&pin))] {
        let origin = CommandOrigin::capture(&lib, &space, Some(&current), Some(&a)).unwrap();
        assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), Some(&a), false), Some(CommandUrlTarget::Tab(current.id.clone())));
        assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), Some(&a), true), Some(CommandUrlTarget::NewTab));
    }
    let origin = CommandOrigin::capture(&lib, &space, None, None).unwrap();
    assert_eq!(command_url_target(&lib, &origin, &space, None, None, true), Some(CommandUrlTarget::NewTab));
    assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), Some(&a), false), Some(CommandUrlTarget::NewTab), "new-tab search must not overwrite a later focused tab");
    let b = lib.create_profile("Command B").unwrap();
    lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap();
    assert_eq!(command_url_target(&lib, &origin, &space, Some(&current), Some(&a), true), None);
    let reopened = CommandOrigin::capture(&lib, &space, None, None).unwrap();
    assert_eq!(command_url_target(&lib, &reopened, &space, Some(&current), Some(&a), true), Some(CommandUrlTarget::NewTab), "an explicitly reopened new-tab search may use the new destination profile");
}

#[test]
fn rr02_peek_promotion_refusal_preserves_original_value_and_allows_same_owner_retry() {
    let ctx = egui::Context::default();
    crate::ui::window::install_fonts(&ctx);
    for fault in ["drift", "choose-owner-space", "deleted-space", "deleted-profile", "closing", "empty-owner"] {
        let mut lib = library();
        let a = lib.create_profile("Peek A").unwrap();
        let b = lib.create_profile("Peek B").unwrap();
        let space = lib.create_space("Peek destination", "P", Theme::preset(0), &a).unwrap();
        let recovery = lib.create_space("Original profile destination", "A", Theme::preset(0), &a).unwrap();
        let original = std::rc::Rc::new((a.clone(), "original live document", 71_u64, 72_u64));
        let mut peek = Some(original.clone());
        match fault {
            "drift" | "choose-owner-space" => lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap(),
            "deleted-space" => { lib.spaces.remove(&space); }
            "deleted-profile" => { lib.profiles.remove(&a); }
            _ => {}
        }
        let rows = lib.store.all_entities().unwrap();
        for _ in 0..2 {
            let result = take_peek_for_tab(&lib, &space, &mut peek, |peek| match fault {
                "closing" => None,
                "empty-owner" => Some(""),
                _ => Some(peek.0.as_str()),
            });
            assert!(result.is_err(), "promotion consumed a Peek with {fault}");
            let error = result.unwrap_err();
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let text = ui.painter().layout_no_wrap(error.into(), egui::FontId::proportional(13.0), egui::Color32::WHITE);
                assert!(text.size().x + 28.0 <= 640.0 - 32.0, "refusal must fit the existing nonwrapping compact toast");
            }).textures_delta.clear();
            assert!(std::rc::Rc::ptr_eq(peek.as_ref().unwrap(), &original), "refusal must retain the exact live view and texture ownership");
            assert_eq!(lib.store.all_entities().unwrap(), rows);
        }
        if matches!(fault, "drift" | "choose-owner-space") {
            let destination = if fault == "drift" {
                lib.update_space(&space, |space| space.profile_id = a.clone()).unwrap();
                &space
            } else { &recovery };
            let promoted = take_peek_for_tab(&lib, destination, &mut peek, |peek| Some(peek.0.as_str())).unwrap().unwrap();
            assert!(std::rc::Rc::ptr_eq(&promoted, &original));
            assert!(peek.is_none());
            assert!(take_peek_for_tab(&lib, destination, &mut peek, |peek| Some(peek.0.as_str())).unwrap().is_none());
        }
    }
}

#[test]
fn active_favorites_restore_per_real_space_without_cross_profile_references() {
    let mut lib = library();
    let a = lib.create_profile("A").unwrap();
    let b = lib.create_profile("B").unwrap();
    let sa = lib.create_space("A", "A", Theme::preset(0), &a).unwrap();
    let sa2 = lib.create_space("A2", "A", Theme::preset(0), &a).unwrap();
    let sb = lib.create_space("B", "B", Theme::preset(0), &b).unwrap();
    let favorite = lib.add_favorite(&a, "http://127.0.0.1/favorite", "Favorite").unwrap();
    let tabs = vec![tab("favorite", FAVORITES_SPACE, Some(&favorite)), tab("today", &sa, None)];
    let state = serde_json::json!({ "active_tabs": { &sa: "favorite", &sa2: "favorite", &sb: "favorite", "deleted": "today" } });
    let mut active = HashMap::from([(sa.clone(), "today".into()), (FAVORITES_SPACE.into(), "favorite".into())]);
    restore_active_tabs(&lib, &tabs, &state, &mut active);
    assert_eq!(active.get(&sa).map(String::as_str), Some("favorite"));
    assert_eq!(active.get(&sa2).map(String::as_str), Some("favorite"));
    assert_eq!(active.len(), 2);
    lib.delete_item(&favorite).unwrap();
    restore_active_tabs(&lib, &tabs, &state, &mut active);
    assert!(active.is_empty());
}

#[test]
fn auto_archive_schedules_the_remaining_idle_deadline() {
    for (now, last, expected_ms) in [(0.0, 0.0, 60_000), (20.0, 0.0, 40_000), (60.0, 0.0, 1), (90.0, 0.0, 1), (90.0, 90.0, 60_000), (10.0, 20.0, 60_000)] {
        assert_eq!(archive_check_delay(now, last).as_millis(), expected_ms);
    }
}

#[test]
fn auto_archive_qa_age_requires_explicit_bounded_isolated_input() {
    assert_eq!(qa_archive_age_ms(Some("5"), true), Some(5000));
    assert_eq!(qa_archive_age_ms(Some("3600"), true), Some(3_600_000));
    for value in [None, Some(""), Some("0"), Some("-1"), Some("3601"), Some("18446744073709551615"), Some("invalid")] {
        assert_eq!(qa_archive_age_ms(value, true), None);
    }
    assert_eq!(qa_archive_age_ms(Some("5"), false), None);
}

#[test]
fn auto_archive_skips_interacted_loading_crashed_and_pinned_tabs() {
    let mut lib = library();
    let profile = lib.create_profile("Archive fixture").unwrap();
    let space = lib.create_space("Archive", "A", Theme::preset(0), &profile).unwrap();
    let now = auto_archive_age_ms(lib.settings.auto_archive_hours) + 10;
    let mut tabs = vec![
        tab("cold-saved", &space, None), tab("interacted", &space, None),
        tab("loading", &space, None), tab("crashed", &space, None),
        tab("pinned", &space, Some("item")), tab("favorite", FAVORITES_SPACE, None),
        tab("at-cutoff", &space, None),
    ];
    tabs[1].nav.auto_archive_protected = true;
    tabs[2].nav.loading = true;
    tabs[3].nav.crashed = true;
    tabs[6].last_active_ms = 10;
    assert_eq!(archive_candidates(&lib, &tabs, &HashMap::new(), &[], now), ["cold-saved"]);
    // A failed persistence flush is not the same as a page edit. A cold URL record
    // has no live document to lose and is still safe to archive transactionally.
    tabs[0].dirty = true;
    assert_eq!(archive_candidates(&lib, &tabs, &HashMap::new(), &[], now), ["cold-saved"]);
}

#[test]
fn auto_archive_excludes_every_visible_split_pane() {
    let mut lib = library();
    let profile = lib.create_profile("Split archive fixture").unwrap();
    let space = lib.create_space("Split", "S", Theme::preset(0), &profile).unwrap();
    let age = auto_archive_age_ms(lib.settings.auto_archive_hours);
    let tabs = vec![tab("active", &space, None), tab("focused", &space, None), tab("visible", &space, None), tab("stale-hidden", &space, None)];
    let active = HashMap::from([(space, "active".into())]);
    let panes = vec![("active".into(), Rect::NOTHING), ("focused".into(), Rect::NOTHING), ("visible".into(), Rect::NOTHING)];
    assert_eq!(archive_candidates(&lib, &tabs, &active, &panes, age + 10), ["stale-hidden"]);
    assert!(archive_candidates(&lib, &tabs, &active, &panes, age + 1).is_empty());
}

#[test]
fn auto_archive_uses_each_tabs_profile_and_never_falls_back_from_missing_owners() {
    use crossbow_core::model::ProfilePreferences;
    let mut lib = library();
    let a = lib.create_profile("Archive A").unwrap();
    let b = lib.create_profile("Archive B").unwrap();
    let inherited = lib.create_profile("Inherited").unwrap();
    let sa = lib.create_space("A", "A", Theme::preset(0), &a).unwrap();
    let sa2 = lib.create_space("A2", "A", Theme::preset(0), &a).unwrap();
    let sb = lib.create_space("B", "B", Theme::preset(0), &b).unwrap();
    let si = lib.create_space("Inherited", "I", Theme::preset(0), &inherited).unwrap();
    lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(1) })).unwrap();
    lib.set_profile_preferences(&b, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(24) })).unwrap();
    let mut shared = lib.settings.clone();
    shared.auto_archive_hours = 0;
    lib.set_settings(shared).unwrap();
    let tabs = vec![tab("a", &sa, None), tab("a2", &sa2, None), tab("b", &sb, None), tab("inherited", &si, None), tab("orphan", "deleted", None)];
    let active = HashMap::from([(sb.clone(), "b".into())]);
    let now = auto_archive_age_ms(1) + 10;
    assert!(auto_archive_enabled(&lib), "shared Never must not disable explicit profile timing");
    assert_eq!(archive_candidates(&lib, &tabs, &active, &[], now), ["a", "a2"]);
    assert_eq!(archive_candidates(&lib, &tabs, &HashMap::new(), &[], auto_archive_age_ms(24) + 10), ["a", "a2", "b"]);
    lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: None, auto_archive_hours: Some(0) })).unwrap();
    assert!(archive_candidates(&lib, &tabs, &active, &[], now).is_empty());
    lib.profiles.remove(&b);
    assert!(!auto_archive_enabled(&lib));
    assert!(archive_candidates(&lib, &tabs, &HashMap::new(), &[], u64::MAX).is_empty());
    assert_eq!(lib.profiles[&inherited].preferences, None, "reads never manufacture overrides");
}

#[test]
fn rename_escape_discards_draft_and_releases_editor_focus() {
    for target in [RenameTarget::Space("space".into()), RenameTarget::Folder("folder".into()), RenameTarget::Item("item".into())] {
        let ctx = egui::Context::default();
        let mut renaming = Some((target, "Uncommitted draft".into()));
        let mut focus = Focus::Chrome;
        let editor = egui::Id::unique("rename-fixture");
        ctx.memory_mut(|memory| memory.request_focus(editor));
        assert!(dismiss_rename(&ctx, &mut renaming, &mut focus));
        assert!(renaming.is_none());
        assert_eq!(focus, Focus::Web);
        assert_eq!(ctx.memory(|memory| memory.focused()), None);
        assert!(!dismiss_rename(&ctx, &mut renaming, &mut focus));
    }
}

fn focus_key(key: Key) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
}

#[test]
fn closing_chrome_restores_keyboard_ownership_across_egui_frames() {
    for size in [egui::vec2(640.0, 420.0), egui::vec2(1360.0, 860.0)] {
        for field in ["find-field", "settings-fixture", "cmd-field"] {
            let ctx = egui::Context::default();
            let mut focus = Focus::Chrome;
            let mut open = true;
            let mut query = String::new();
            let mut forwarded = String::new();
            for frame in 0..10 {
                let events = match frame {
                    3 => vec![egui::Event::Text("KEEP".into())],
                    4 => vec![focus_key(Key::Escape), egui::Event::Text("_SAME_BATCH".into())],
                    5 => vec![egui::Event::Text("_AFTER".into())],
                    7 => vec![egui::Event::Ime(egui::ImeEvent::Commit("_RETRY".into()))],
                    _ => Vec::new(),
                };
                ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                    if take_dismiss_shortcut(ui.ctx()) {
                        open = false;
                        restore_web_focus(ui.ctx(), &mut focus);
                    }
                    if open {
                        if field == "cmd-field" { ui::command_bar::prepare_input(ui.ctx()); }
                        // Settle the field before dismissal, like the native reproduction.
                        if frame <= 2 { ui.memory_mut(|memory| memory.request_focus(egui::Id::unique(field))); }
                        egui::Area::new(egui::Id::unique("focus-fixture")).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                            ui.add(egui::TextEdit::singleline(&mut query).id(egui::Id::unique(field)));
                        });
                    } else {
                        let _ = ui.button("Focusable browser chrome");
                    }
                    // This is the production forwarding gate and translator, not a second policy.
                    let blocked = web_keyboard_blocker(ui.ctx(), focus, open && field != "find-field");
                    if open { assert!(blocked.is_some()); }
                    else { assert_eq!(blocked, None, "{field}, frame {frame}"); }
                    if blocked.is_none() {
                        for input in input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                            if let WebInput::Text(text) = input { forwarded.push_str(&text); }
                        }
                    }
                }).textures_delta.clear();
            }
            assert_eq!(query, "KEEP", "{field}");
            assert_eq!(forwarded, "_SAME_BATCH_AFTER_RETRY", "{field}");
        }
    }
}

#[test]
fn library_dismissal_consumes_escape_and_returns_keyboard_ownership() {
    for size in [egui::vec2(640.0, 420.0), egui::vec2(1360.0, 860.0)] {
        let ctx = egui::Context::default();
        let mut open = true;
        let mut focus = Focus::Web;
        let mut forwarded = String::new();
        for frame in 0..6 {
            let events = match frame {
                2 => vec![egui::Event::Text("BLOCKED".into())],
                3 => vec![focus_key(Key::Escape), egui::Event::Text("AFTER".into())],
                4 => vec![egui::Event::Text("_RETRY".into())],
                _ => Vec::new(),
            };
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                if take_dismiss_shortcut(ui.ctx()) {
                    assert!(dismiss_library(ui.ctx(), &mut open, &mut focus, None, LibraryDismissal::RestoreFocus));
                    assert!(!ui.input(|input| input.key_pressed(Key::Escape)));
                }
                if web_keyboard_blocker(ui.ctx(), focus, open).is_none() {
                    for input in input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                        if let WebInput::Text(text) = input { forwarded.push_str(&text); }
                    }
                }
            }).textures_delta.clear();
        }
        assert!(!open);
        assert_eq!(focus, Focus::Web);
        assert_eq!(forwarded, "AFTER_RETRY");
        assert!(!dismiss_library(&ctx, &mut open, &mut focus, None, LibraryDismissal::RestoreFocus));
    }
}

#[test]
fn library_dismissal_resumes_find_without_dismissing_its_query() {
    let ctx = egui::Context::default();
    let mut open = true;
    let mut focus = Focus::Web;
    let mut find = FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false };
    assert!(dismiss_library(&ctx, &mut open, &mut focus, Some(&mut find), LibraryDismissal::RestoreFocus));
    assert!(!open);
    assert_eq!(focus, Focus::Chrome);
    assert_eq!(find.query, "KEEP_FIND");
    assert!(find.just_opened);
    assert_eq!(ctx.memory(|memory| memory.focused()), Some(egui::Id::unique("find-field")));
    assert_eq!(web_keyboard_blocker(&ctx, focus, false), Some("chrome"));
}

#[test]
fn library_outside_press_preserves_the_clicked_input_owner() {
    for size in [egui::vec2(640.0, 420.0), egui::vec2(1360.0, 860.0)] {
        for clicked_chrome in [false, true] {
            let ctx = egui::Context::default();
            let mut open = true;
            let mut focus = Focus::Chrome;
            let mut find = FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false };
            let clicked_field = egui::Id::unique("outside-field");
            let mut clicked_text = String::new();
            let mut forwarded = String::new();
            for frame in 0..7 {
                let events = if frame == 5 { vec![egui::Event::Text("CLICKED_TARGET".into())] } else { Vec::new() };
                ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                    if frame == 3 {
                        // Content/sidebar pointer handling precedes the Library overlay.
                        if clicked_chrome {
                            ui.memory_mut(|memory| memory.request_focus(clicked_field));
                        } else {
                            focus = Focus::Web;
                            ui.memory_mut(|memory| memory.stop_text_input());
                        }
                        assert!(dismiss_library(ui.ctx(), &mut open, &mut focus, Some(&mut find), LibraryDismissal::OutsidePointer));
                        assert!(!find.just_opened);
                    }
                    ui.add(egui::TextEdit::singleline(&mut find.query).id(egui::Id::unique("find-field")));
                    ui.add(egui::TextEdit::singleline(&mut clicked_text).id(clicked_field));
                    if frame <= 2 { ui.memory_mut(|memory| memory.request_focus(egui::Id::unique("find-field"))); }
                    if web_keyboard_blocker(ui.ctx(), focus, open).is_none() {
                        for input in input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                            if let WebInput::Text(text) = input { forwarded.push_str(&text); }
                        }
                    }
                }).textures_delta.clear();
            }
            assert!(!open);
            assert_eq!(find.query, "KEEP_FIND");
            assert_eq!(clicked_text, if clicked_chrome { "CLICKED_TARGET" } else { "" });
            assert_eq!(forwarded, if clicked_chrome { "" } else { "CLICKED_TARGET" });
        }
    }
}

#[test]
fn command_chat_settings_transition_replaces_overlays_and_preserves_find() {
    let (mut lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
    let entry = crate::chat::Entry::capture(&lib, &space, &None).unwrap();
    let mut state = UiState {
        sidebar_visible: true, sidebar_width: 252.0,
        command: Some(CommandBar { query: "Ask AI".into(), edit_current: false, origin: None, chat_entry: Some(entry.clone()), needs_focus: false, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new() }),
        peek: None, find: Some(FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false }),
        settings: Some(ui::settings::SettingsState::new(ui::settings::Page::General, 0.0)), onboarding: None,
        menu: Some(MenuState { pos: Pos2::ZERO, kind: MenuKind::Space(space.clone()) }), toasts: Vec::new(),
        renaming: None, drag: None, space_anim: None, show_archive: true, show_downloads: false,
        focus: Focus::Web, panes: Vec::new(), address: None, focused_pane: None, pointer_capture: None, pointer_over: Default::default(), last_press: None,
        sidebar_peek: false, settings_page_hint: Some(ui::settings::Page::Account), new_space_profile: None,
    };
    let settings = ui::settings::SettingsState::from_chat_entry(&entry, &mut lib, &space, &None, 1.0).unwrap();
    state.open_settings(settings);
    assert_eq!(state.settings.as_ref().unwrap().page, ui::settings::Page::AiChat);
    assert!(state.command.is_none() && state.menu.is_none() && !state.show_archive && state.settings_page_hint.is_none());
    assert_eq!(state.focus, Focus::Chrome);
    assert_eq!(state.find.as_ref().unwrap().query, "KEEP_FIND");
    let ctx = egui::Context::default();
    assert!(dismiss_settings(&ctx, &mut state.settings, &mut state.focus, state.find.as_mut()));
    assert!(state.settings.is_none());
    assert!(state.find.as_ref().unwrap().just_opened);
    assert_eq!(ctx.memory(|memory| memory.focused()), Some(egui::Id::unique("find-field")));
    state.open_settings(ui::settings::SettingsState::new(ui::settings::Page::General, 2.0));
    assert_eq!(state.settings.as_ref().unwrap().page, ui::settings::Page::General, "no stale chat/account page hint");
}

#[test]
fn review_command_chat_rejection_preserves_find_focus() {
    let (mut lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
    let ctx = egui::Context::default();
    let mut command = Some(CommandBar {
        query: "Ask AI".into(), edit_current: false, origin: None,
        chat_entry: crate::chat::Entry::capture(&lib, &space, &None), needs_focus: false,
        selected: 1, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
    });
    let Action::OpenChat(entry) = command.as_ref().unwrap().ask_ai_action() else { panic!("missing entry") };
    let mut focus = Focus::Chrome;
    let mut find = FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false };
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("cmd-field")));
    finish_command(&ctx, &mut command, &mut focus, Some(&mut find));
    let account = Some(("https://sync.example.test".into(), "changed-account".into()));
    assert!(ui::settings::SettingsState::from_chat_entry(&entry, &mut lib, &space, &account, 1.0).is_err());
    complete_find_handoff(&ctx, focus, Some(&mut find));
    assert!(command.is_none());
    assert_eq!(find.query, "KEEP_FIND");
    assert_eq!(focus, Focus::Chrome, "rejected chat entry must return input to the preserved Find bar");
    assert!(find.just_opened);
    assert_eq!(ctx.memory(|memory| memory.focused()), Some(egui::Id::unique("find-field")));
    assert!(web_keyboard_blocker(&ctx, focus, false).is_some());
}

#[test]
fn residual_command_escape_consumes_its_entire_input_batch() {
    for size in [egui::vec2(640.0, 420.0), egui::vec2(1360.0, 860.0)] {
        for has_find in [false, true] {
            let ctx = egui::Context::default();
            let mut command = Some(CommandBar {
                query: "SYNTHETIC_COMMAND".into(), edit_current: false, origin: None, chat_entry: None,
                needs_focus: false, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
            });
            let mut find = has_find.then(|| FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false });
            let mut focus = Focus::Chrome;
            let mut forwarded = Vec::new();
            for frame in 0..6 {
                let events = match frame {
                    2 => vec![
                        egui::Event::Paste("SYNTHETIC_PRIVATE_COMMAND".into()), egui::Event::Text("COMMAND_TEXT".into()),
                        egui::Event::Copy, egui::Event::Cut, egui::Event::Ime(egui::ImeEvent::Commit("COMMAND_IME".into())),
                        focus_key(Key::Escape),
                    ],
                    4 => vec![egui::Event::Text("NEXT_FRAME".into())],
                    _ => Vec::new(),
                };
                ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                    if take_dismiss_shortcut(ui.ctx()) {
                        assert!(cancel_command(ui.ctx(), &mut command, &mut focus, find.as_mut()));
                    }
                    if command.is_some() { ui.memory_mut(|memory| memory.request_focus(egui::Id::unique("cmd-field"))); }
                    if let Some(find) = &mut find {
                        let response = ui.add(egui::TextEdit::singleline(&mut find.query).id(egui::Id::unique("find-field")));
                        if find.just_opened { response.request_focus(); find.just_opened = false; }
                    }
                    if web_keyboard_blocker(ui.ctx(), focus, command.is_some()).is_none() {
                        forwarded.extend(input::web_key_inputs(&ui.input(|input| input.events.clone())));
                    }
                    if frame == 2 {
                        assert!(forwarded.is_empty(), "Escape leaked command input to the page: {forwarded:?}");
                        assert!(ui.input(|input| input.events.iter().all(|event| !matches!(event, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Ime(_) | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)))));
                        if let Some(find) = &find { assert_eq!(find.query, "KEEP_FIND", "canceled input must not enter Find either"); }
                    }
                }).textures_delta.clear();
            }
            assert!(command.is_none());
            if let Some(find) = find {
                assert!(find.query.contains("NEXT_FRAME"));
                assert!(!find.query.contains("COMMAND"));
                assert!(forwarded.is_empty());
            } else {
                assert!(matches!(forwarded.as_slice(), [WebInput::Text(text)] if text == "NEXT_FRAME"));
            }
        }
    }
}

#[test]
fn residual_command_successful_destination_supersedes_find_restoration() {
    for size in [egui::vec2(640.0, 420.0), egui::vec2(1360.0, 860.0)] {
        let ctx = egui::Context::default();
        let mut command = Some(CommandBar {
            query: "Destination tab".into(), edit_current: false, origin: None, chat_entry: None,
            needs_focus: false, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
        });
        let mut find = FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false };
        let mut focus = Focus::Chrome;
        let mut forwarded = String::new();
        for frame in 0..7 {
            let events = if frame == 5 { vec![egui::Event::Text("DESTINATION_INPUT".into())] } else { Vec::new() };
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                let response = ui.add(egui::TextEdit::singleline(&mut find.query).id(egui::Id::unique("find-field")));
                if find.just_opened { response.request_focus(); find.just_opened = false; }
                if command.is_some() { ui.memory_mut(|memory| memory.request_focus(egui::Id::unique("cmd-field"))); }
                if frame == 3 { finish_command(ui.ctx(), &mut command, &mut focus, Some(&mut find)); }
                if web_keyboard_blocker(ui.ctx(), focus, command.is_some()).is_none() {
                    for input in input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                        if let WebInput::Text(text) = input { forwarded.push_str(&text); }
                    }
                }
                // Successful activate() runs after forward_keys and chooses the web destination.
                if frame == 3 { focus = Focus::Web; }
                complete_find_handoff(ui.ctx(), focus, Some(&mut find));
            }).textures_delta.clear();
        }
        assert_eq!(find.query, "KEEP_FIND", "a successful destination must not resume typing in Find");
        assert_eq!(forwarded, "DESTINATION_INPUT");
        assert_eq!(web_keyboard_blocker(&ctx, focus, false), None);
    }
}

#[test]
fn residual_command_handoff_preserves_existing_find_focus() {
    let ctx = egui::Context::default();
    let mut find = FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false };
    let field = egui::Id::unique("find-field");
    ctx.memory_mut(|memory| memory.request_focus(field));
    complete_find_handoff(&ctx, Focus::Web, Some(&mut find));
    assert_eq!(ctx.memory(|memory| memory.focused()), Some(field), "unrelated actions must not take an already-active Find field");
    assert_eq!(find.query, "KEEP_FIND");
}

#[test]
fn settings_dismissal_restores_find_or_web_without_losing_the_query() {
    for has_find in [false, true] {
        let ctx = egui::Context::default();
        let mut settings = Some(ui::settings::SettingsState::new(ui::settings::Page::General, 0.0));
        let mut focus = Focus::Chrome;
        let mut find = has_find.then(|| FindBar { query: "KEEP_FIND".into(), last_sent: "KEEP_FIND".into(), just_opened: false });
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("settings-field")));
        assert!(dismiss_settings(&ctx, &mut settings, &mut focus, find.as_mut()));
        assert!(settings.is_none());
        if let Some(find) = &find {
            assert_eq!(find.query, "KEEP_FIND");
            assert!(find.just_opened);
            assert_eq!(focus, Focus::Chrome);
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(egui::Id::unique("find-field")));
        } else {
            assert_eq!(focus, Focus::Web);
            assert_eq!(ctx.memory(|memory| memory.focused()), None);
        }
        assert!(!dismiss_settings(&ctx, &mut settings, &mut focus, find.as_mut()));
    }
}

#[test]
fn settled_find_search_double_escape_releases_the_production_keyboard_gate() {
    for (size, resume_find) in [egui::vec2(640.0, 420.0), egui::vec2(1360.0, 860.0)].into_iter().flat_map(|size| [(size, false), (size, true)]) {
        let ctx = egui::Context::default();
        let dismiss_frame = if resume_find { 10 } else { 9 };
        let mut focus = Focus::Chrome;
        let mut find = Some(FindBar { query: String::new(), last_sent: String::new(), just_opened: true });
        let mut command = None;
        let mut find_query = String::new();
        let mut search_query = String::new();
        let mut forwarded = String::new();
        let mut handoffs = 0;
        for frame in 0..14 {
            let events = match frame {
                3 => vec![egui::Event::Text("KEEP_FIND".into())],
                4 => vec![egui::Event::Key { key: Key::T, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }, egui::Event::Text("FAVORITE 23".into())],
                8 if resume_find => vec![focus_key(Key::Escape), egui::Event::Text("_RETURN".into())],
                8 => vec![focus_key(Key::Escape)],
                frame if frame == dismiss_frame => vec![focus_key(Key::Escape), egui::Event::Text("_AFTER_FIND".into())],
                9 if resume_find => vec![egui::Event::Text("_CONTINUE".into())],
                12 => vec![egui::Event::Text("_RETRY".into())],
                _ => Vec::new(),
            };
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                if take_shortcut(ui.ctx(), egui::Modifiers::COMMAND, Key::T) {
                    command = Some(CommandBar {
                        query: String::new(), edit_current: false, origin: None, chat_entry: None,
                        needs_focus: false, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
                    });
                    focus = Focus::Chrome;
                }
                if take_dismiss_shortcut(ui.ctx()) && !cancel_command(ui.ctx(), &mut command, &mut focus, find.as_mut()) {
                    dismiss_find(ui.ctx(), &mut find, &mut focus, || handoffs += 1);
                }
                if command.is_some() { ui::command_bar::prepare_input(ui.ctx()); }
                if let Some(find) = &mut find {
                    if frame <= 2 { ui.memory_mut(|memory| memory.request_focus(egui::Id::unique("find-field"))); }
                    egui::Area::new(egui::Id::unique("find-bar")).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                        let response = ui.add(egui::TextEdit::singleline(&mut find.query).id(egui::Id::unique("find-field")));
                        if find.just_opened { response.request_focus(); find.just_opened = false; }
                    });
                    find_query.clone_from(&find.query);
                }
                if command.is_some() {
                    egui::Area::new(egui::Id::unique("cmd-bar")).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                        ui.memory_mut(|memory| {
                            if !memory.has_focus(egui::Id::unique("cmd-field")) { memory.request_focus(egui::Id::unique("cmd-field")); }
                        });
                        ui.add(egui::TextEdit::singleline(&mut search_query).id(egui::Id::unique("cmd-field")).lock_focus(true));
                    });
                }
                if (8..dismiss_frame).contains(&frame) {
                    assert_eq!(focus, Focus::Chrome, "Find still owns input after dismissing search");
                    assert_eq!(handoffs, 0);
                    assert!(web_keyboard_blocker(ui.ctx(), focus, command.is_some()).is_some());
                }
                if frame >= dismiss_frame { assert_eq!(web_keyboard_blocker(ui.ctx(), focus, command.is_some()), None, "frame {frame}"); }
                if web_keyboard_blocker(ui.ctx(), focus, command.is_some()).is_none() {
                    for input in input::web_key_inputs(&ui.input(|input| input.events.clone())) {
                        if let WebInput::Text(text) = input { forwarded.push_str(&text); }
                    }
                }
            }).textures_delta.clear();
        }
        assert_eq!(find_query, if resume_find { "KEEP_FIND_CONTINUE" } else { "KEEP_FIND" });
        assert_eq!(search_query, "FAVORITE 23");
        assert_eq!(forwarded, "_AFTER_FIND_RETRY");
        assert_eq!(handoffs, 1);
    }
}

#[test]
fn find_dismissal_releases_widget_and_calls_the_engine_only_once() {
    let ctx = egui::Context::default();
    let mut focus = Focus::Chrome;
    let mut find = Some(FindBar { query: "NO_MATCH".into(), last_sent: "NO_MATCH".into(), just_opened: false });
    let mut handoffs = 0;
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("find-field")));
    assert!(dismiss_find(&ctx, &mut find, &mut focus, || handoffs += 1));
    assert_eq!(focus, Focus::Web);
    assert!(find.is_none());
    assert_eq!(ctx.memory(|memory| memory.focused()), None);
    assert_eq!(web_keyboard_blocker(&ctx, focus, false), None);
    assert!(!dismiss_find(&ctx, &mut find, &mut focus, || handoffs += 1));
    assert_eq!(handoffs, 1);
}

#[test]
fn search_dismissal_without_find_releases_widget_focus() {
    let ctx = egui::Context::default();
    let mut focus = Focus::Chrome;
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("cmd-field")));
    restore_after_search(&ctx, &mut focus, None);
    assert_eq!(focus, Focus::Web);
    assert_eq!(ctx.memory(|memory| memory.focused()), None);
}

#[test]
fn focus_diagnostic_counts_do_not_depend_on_input_content() {
    let counts = |key, text: &str| keyboard_event_counts(&[
        focus_key(key), egui::Event::Text(text.into()),
        egui::Event::Ime(egui::ImeEvent::Commit(text.into())),
        egui::Event::Paste(text.into()), egui::Event::Copy, egui::Event::Cut,
        egui::Event::WindowFocused(false),
    ]);
    assert_eq!(counts(Key::A, "synthetic short"), [1, 1, 1, 3]);
    assert_eq!(counts(Key::Escape, "a different synthetic input with a different length"), [1, 1, 1, 3]);
}

#[test]
fn failed_session_writes_retry_and_survive_reopen() {
    let dir = std::env::temp_dir().join(format!("crossbow-session-{}", new_id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("fixture.sqlite");
    {
        let store = Store::open(&path).unwrap();
        store.kv_set("ui_state", "old").unwrap();
    }
    let escaped = path.to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
    let read_only = Store::open(std::path::Path::new(&format!("file:{escaped}?mode=ro"))).unwrap();
    let mut tabs = vec![tab("pending", "space", None)];
    tabs[0].dirty = true;
    let state = r#"{"space":"space","active_tabs":{"space":"pending"}}"#;
    let mut cached = "old".to_string();
    flush_tabs(&read_only, &mut tabs);
    save_ui_state(&read_only, state, &mut cached);
    assert!(tabs[0].dirty);
    assert_eq!(cached, "old");
    assert!(read_only.tabs().unwrap().is_empty());
    drop(read_only);
    let store = Store::open(&path).unwrap();
    flush_tabs(&store, &mut tabs);
    save_ui_state(&store, state, &mut cached);
    assert!(!tabs[0].dirty);
    assert_eq!(cached, state);
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.tabs().unwrap()[0].id, "pending");
    assert_eq!(reopened.kv_get("ui_state").unwrap().as_deref(), Some(state));
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

fn read_only_store(path: &std::path::Path) -> Store {
    let escaped = path.to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
    Store::open(std::path::Path::new(&format!("file:{escaped}?mode=ro"))).unwrap()
}

#[test]
fn archive_write_failed_close_preserves_live_metadata_until_a_successful_retry() {
    for kind in ["today", "pin", "favorite", "blank"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        let space = lib.spaces_ordered()[0].0.clone();
        let item = match kind {
            "pin" => Some(lib.add_pinned(&space, None, "https://fixture.example/pin", "Pin").unwrap()),
            "favorite" => Some(lib.add_favorite(&profile, "https://fixture.example/favorite", "Favorite").unwrap()),
            _ => None,
        };
        let mut live = tab("draft", if kind == "favorite" { FAVORITES_SPACE } else { &space }, item.as_deref());
        live.url = if kind == "blank" { NEW_TAB_URL.into() } else { format!("https://fixture.example/{kind}") };
        lib.store.put_tab(&live.row()).unwrap();
        let saved = lib.store.tabs().unwrap();
        live.title = "Unsaved live draft".into();
        live.dirty = true;
        live.muted = true;
        live.nav.loading = true;
        let latest = live.row();
        let expected_archive = should_archive_tab(&live);
        let mut tabs = vec![live];
        let read_only = Library::open(read_only_store(&path)).unwrap();
        assert!(take_closed_tab(&read_only, &mut tabs, "draft").is_err());
        assert_eq!(tabs.len(), 1, "failed close must not remove or unload the live tab");
        assert_eq!(tabs[0].row(), latest);
        assert!(tabs[0].dirty && tabs[0].muted && tabs[0].nav.loading);
        assert_eq!(read_only.store.tabs().unwrap(), saved);
        assert!(read_only.store.archive(&profile, 2000).unwrap().is_empty());
        drop(read_only);
        let closed = take_closed_tab(&lib, &mut tabs, "draft").unwrap().unwrap();
        assert_eq!(closed.row(), latest);
        assert!(tabs.is_empty());
        assert!(take_closed_tab(&lib, &mut tabs, "draft").unwrap().is_none());
        drop(lib);
        let reopened = Store::open(&path).unwrap();
        assert!(reopened.tabs().unwrap().is_empty());
        let archive = reopened.archive(&profile, 2000).unwrap();
        assert_eq!(archive.len(), usize::from(expected_archive));
        if expected_archive { assert_eq!(archive[0].title, latest.title); }
    }
}

#[test]
fn archive_write_failed_restore_keeps_archive_and_does_not_create_a_live_tab() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    let profile = lib.spaces[&space].profile_id.clone();
    lib.store.archive_tab(&profile, "https://fixture.example/restore", "Archived title", &space, 1).unwrap();
    let archive = lib.store.archive(&profile, 2000).unwrap();
    let rowid = archive[0].rowid;
    let mut tabs = vec![tab("already-open", &space, None)];
    tabs[0].order = key_between(None, None);
    lib.store.put_tab(&tabs[0].row()).unwrap();
    let original = tabs[0].row();
    drop(lib);
    let lib = Library::open(read_only_store(&path)).unwrap();
    assert!(restore_archived_tab(&lib, &mut tabs, &space, rowid).is_err());
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].row(), original);
    assert_eq!(lib.store.tabs().unwrap(), vec![original.clone()]);
    assert_eq!(lib.store.archive(&profile, 2000).unwrap(), archive);
    drop(lib);
    let lib = Library::open(Store::open(&path).unwrap()).unwrap();
    let restored_id = restore_archived_tab(&lib, &mut tabs, &space, rowid).unwrap().unwrap();
    assert_eq!(tabs.len(), 2);
    let restored = &tabs[1];
    assert_eq!(restored.id, restored_id);
    assert_eq!(restored.title, archive[0].title);
    assert_eq!(restored.url, archive[0].url);
    assert_eq!(restored.space_id, space);
    assert!(restored.order < original.order);
    assert!(!restored.dirty, "the helper must persist before exposing the tab");
    let row = restored.row();
    assert!(restore_archived_tab(&lib, &mut tabs, &space, rowid).unwrap().is_none());
    assert_eq!(tabs.len(), 2);
    drop(lib);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.tabs().unwrap(), vec![row, original]);
    assert!(store.archive(&profile, 2000).unwrap().is_empty());
}

#[test]
fn archive_write_failed_auto_archive_remains_eligible_for_the_next_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    let profile = lib.default_profile_id();
    let space = lib.spaces_ordered()[0].0.clone();
    let mut tabs = vec![tab("active", &space, None), tab("stale", &space, None)];
    for tab in &tabs { lib.store.put_tab(&tab.row()).unwrap(); }
    let active = HashMap::from([(space, "active".into())]);
    let panes = vec![("active".into(), Rect::NOTHING)];
    let now = auto_archive_age_ms(lib.settings.auto_archive_hours) + 10;
    assert_eq!(archive_candidates(&lib, &tabs, &active, &panes, now), ["stale"]);
    let read_only = Library::open(read_only_store(&path)).unwrap();
    assert!(take_closed_tab(&read_only, &mut tabs, "stale").is_err());
    assert_eq!(archive_candidates(&lib, &tabs, &active, &panes, now), ["stale"]);
    take_closed_tab(&lib, &mut tabs, "stale").unwrap().unwrap();
    assert!(archive_candidates(&lib, &tabs, &active, &panes, now).is_empty());
    assert_eq!(tabs[0].id, "active");
    assert_eq!(lib.store.archive(&profile, 2000).unwrap().len(), 1);
}

#[test]
fn archive_write_summary_counts_only_successful_archives_and_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    let profile = lib.default_profile_id();
    let space = lib.spaces_ordered()[0].0.clone();
    let mut tabs = vec![tab("archive", &space, None), tab("blank", &space, None), tab("retry", &space, None)];
    tabs[1].url = NEW_TAB_URL.into();
    for tab in &tabs { lib.store.put_tab(&tab.row()).unwrap(); }
    let read_only = Library::open(read_only_store(&path)).unwrap();
    let mut summary = CloseSummary::default();
    summary.record(take_closed_tab(&read_only, &mut tabs, "retry").map(|tab| tab.map(|tab| should_archive_tab(&tab))));
    for id in ["archive", "blank", "missing"] {
        summary.record(take_closed_tab(&lib, &mut tabs, id).map(|tab| tab.map(|tab| should_archive_tab(&tab))));
    }
    assert_eq!(summary, CloseSummary { archived: 1, closed: 1, failed: 1 });
    assert_eq!(summary.message().as_deref(), Some("Archived 1 tab. Closed 1 tab. 1 tab could not be closed and is still open. Try again."));
    assert_eq!(tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(), ["retry"]);
    assert_eq!(lib.store.tabs().unwrap().len(), 1);
    assert_eq!(lib.store.archive(&profile, 2000).unwrap().len(), 1);
    let mut retry = CloseSummary::default();
    retry.record(take_closed_tab(&lib, &mut tabs, "retry").map(|tab| tab.map(|tab| should_archive_tab(&tab))));
    assert_eq!(retry.message().as_deref(), Some("Archived 1 tab."));
    assert!(tabs.is_empty());
    assert_eq!(lib.store.archive(&profile, 2000).unwrap().len(), 2);
    assert!(CloseSummary::default().message().is_none());
}

#[test]
fn parent_removal_selection_skips_all_removed_tabs_and_prefers_the_next_survivor() {
    let order = ["above", "active", "removed-next", "below"].map(str::to_owned);
    let tabs = vec![tab("above", "space", None), tab("below", "space", None)];
    assert_eq!(surviving_neighbor(&order, "active", &tabs).as_deref(), Some("below"));
    assert_eq!(surviving_neighbor(&order, "active", &tabs[..1]).as_deref(), Some("above"));
    assert_eq!(surviving_neighbor(&order, "active", &[]), None);
    assert_eq!(surviving_neighbor(&order, "missing", &tabs), None);
}

#[test]
fn parent_removal_keeps_surviving_split_panes_selected_and_focused() {
    let mut lib = library();
    lib.ensure_defaults().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    let tabs = vec![tab("unrelated", &space, None), tab("pane-b", &space, None), tab("pane-c", &space, None)];
    let mut split = Split::new("removed-pin".into(), "pane-b".into());
    split.add("pane-c".into());
    for active in ["removed-pin", "pane-b"] {
        let mut selection = RemovalSelection {
            space: space.clone(), profile: lib.default_profile_id(),
            active: HashMap::from([(space.clone(), active.into())]), focused: Some("removed-pin".into()),
            panes: vec!["removed-pin".into(), "pane-b".into(), "pane-c".into()],
            splits: HashMap::from([(space.clone(), split.clone())]),
            order: vec!["removed-pin".into(), "unrelated".into(), "pane-b".into(), "pane-c".into()],
        };
        selection.reconcile(&lib, &tabs);
        assert_eq!(selection.active[&space], "pane-b");
        assert_eq!(selection.focused.as_deref(), Some("pane-b"));
        assert_eq!(selection.space, space);
    }
}

#[test]
fn parent_removal_repairs_inactive_shared_favorite_selection_without_switching_spaces() {
    let (lib, profile, _, space, sibling, other) = archive_profiles();
    let tabs = vec![tab("current", &space, None), tab("sibling-today", &sibling, None), tab("other-profile", &other, None)];
    let mut selection = RemovalSelection {
        space: space.clone(), profile,
        active: HashMap::from([(space.clone(), "current".into()), (sibling.clone(), "removed-favorite".into()), (other.clone(), "other-profile".into())]),
        focused: Some("current".into()), panes: vec!["current".into()],
        order: vec!["removed-favorite".into(), "current".into()], ..Default::default()
    };
    selection.reconcile(&lib, &tabs);
    assert_eq!(selection.space, space);
    assert_eq!(selection.active[&space], "current");
    assert_eq!(selection.active[&sibling], "sibling-today");
    assert_eq!(selection.active[&other], "other-profile");
    assert_eq!(selection.focused.as_deref(), Some("current"));
}

#[test]
fn parent_removal_space_fallback_preserves_profile_and_prunes_deleted_keys() {
    let (mut lib, profile, _, space, sibling, other) = archive_profiles();
    let tabs = vec![tab("sibling-today", &sibling, None), tab("other-profile", &other, None)];
    let mut selection = RemovalSelection {
        space: space.clone(), profile,
        active: HashMap::from([(space.clone(), "removed".into()), (other.clone(), "other-profile".into())]),
        focused: Some("removed".into()), panes: vec!["removed".into()], ..Default::default()
    };
    lib.delete_space(&space).unwrap();
    selection.reconcile(&lib, &tabs);
    assert_eq!(selection.space, sibling);
    assert_eq!(selection.active[&sibling], "sibling-today");
    assert_eq!(selection.focused.as_deref(), Some("sibling-today"));
    assert!(!selection.active.contains_key(&space));
    assert_eq!(selection.active[&other], "other-profile");
}

#[test]
fn parent_removal_rejects_mismatched_live_ownership_without_detaching_tabs() {
    let mut lib = library();
    lib.ensure_defaults().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    let profile = lib.create_profile("Other").unwrap();
    let other = lib.create_space("Other", "O", Theme::preset(0), &profile).unwrap();
    let pin = lib.add_pinned(&space, None, "https://fixture.example/pin", "Pin").unwrap();
    let mut tabs = vec![tab("mismatched-pin", &other, Some(&pin))];
    tabs[0].dirty = true;
    let row = tabs[0].row();
    for parent in [ParentRemoval::Item(&pin), ParentRemoval::Space(&space)] {
        assert!(take_removed_parent_tabs(&mut lib, &mut tabs, parent, 100).is_err());
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].row(), row);
        assert!(tabs[0].dirty);
        assert!(lib.spaces.contains_key(&space) && lib.items.contains_key(&pin));
        assert!(lib.store.tabs().unwrap().is_empty());
    }
}

#[test]
fn parent_removal_late_failure_retains_every_live_tab_then_retry_commits_once() {
    for kind in ["item", "space"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        let profile = lib.spaces[&space].profile_id.clone();
        let sibling = lib.create_space("Keep", "K", Theme::preset(0), &profile).unwrap();
        let other_profile = lib.create_profile("Other").unwrap();
        let other = lib.create_space("Other", "O", Theme::preset(0), &other_profile).unwrap();
        let folder = lib.create_folder(&space, None, "Keep on failure").unwrap();
        let pin = lib.add_pinned(&space, Some(&folder), "https://fixture.example/pin", "Pin").unwrap();
        let favorite = lib.add_favorite(&profile, "https://fixture.example/favorite", "Favorite").unwrap();
        let mut tabs = vec![
            tab("today-one", &space, None), tab("today-two", &space, None),
            tab("pin-one", &space, Some(&pin)), tab("pin-two", &space, Some(&pin)),
            tab("blank", &space, None), tab("sibling", &sibling, None),
            tab("other", &other, None), tab("favorite", FAVORITES_SPACE, Some(&favorite)),
        ];
        tabs[4].url = NEW_TAB_URL.into();
        for tab in &tabs { lib.store.put_tab(&tab.row()).unwrap(); }
        let persisted = lib.store.tabs().unwrap();
        for tab in &mut tabs {
            tab.title = format!("Live {}", tab.id);
            tab.dirty = true;
            tab.muted = true;
            tab.nav.loading = true;
        }
        let live: Vec<_> = tabs.iter().map(Tab::row).collect();
        let spaces = lib.spaces.clone();
        let folders = lib.folders.clone();
        let items = lib.items.clone();
        let entities = lib.store.all_entities().unwrap();
        let revision = lib.revision;
        let parent = if kind == "item" { ParentRemoval::Item(&pin) } else { ParentRemoval::Space(&space) };
        let id = if kind == "item" { &pin } else { &space };
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&format!("CREATE TRIGGER reject_parent_removal BEFORE UPDATE ON entities WHEN NEW.id = '{id}' AND NEW.deleted = 1 BEGIN SELECT RAISE(ABORT, 'fixture parent failure'); END;")).unwrap();
        assert!(take_removed_parent_tabs(&mut lib, &mut tabs, parent, 100).is_err());
        assert_eq!(tabs.iter().map(Tab::row).collect::<Vec<_>>(), live, "{kind}: late parent failure must retain every live tab");
        assert!(tabs.iter().all(|tab| tab.dirty && tab.muted && tab.nav.loading));
        assert_eq!(lib.store.tabs().unwrap(), persisted);
        assert_eq!(lib.spaces, spaces);
        assert_eq!(lib.folders, folders);
        assert_eq!(lib.items, items);
        assert_eq!(lib.revision, revision);
        assert_eq!(lib.store.all_entities().unwrap(), entities);
        assert!(lib.store.archive(&profile, 2000).unwrap().is_empty());
        connection.execute_batch("DROP TRIGGER reject_parent_removal;").unwrap();
        let removed = take_removed_parent_tabs(&mut lib, &mut tabs, parent, 100).unwrap();
        assert_eq!(removed.len(), if kind == "item" { 2 } else { 5 });
        assert!(removed.iter().all(|tab| tab.dirty && tab.muted && tab.nav.loading), "views are still owned by the caller until post-commit cleanup");
        assert!(tabs.iter().all(|tab| tab.dirty && tab.muted && tab.nav.loading));
        assert!(tabs.iter().any(|tab| tab.id == "sibling"));
        assert!(tabs.iter().any(|tab| tab.id == "other"));
        assert!(tabs.iter().any(|tab| tab.id == "favorite"));
        assert!(lib.items.contains_key(&favorite));
        assert!(lib.spaces.contains_key(&sibling) && lib.spaces.contains_key(&other));
        let archive = lib.store.archive(&profile, 2000).unwrap();
        assert_eq!(archive.len(), if kind == "item" { 0 } else { 2 });
        assert!(archive.iter().all(|row| row.title.starts_with("Live today-") && row.archived_ms == 100));
        assert!(lib.store.archive(&other_profile, 2000).unwrap().is_empty());
        let saved = lib.store.tabs().unwrap();
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(lib.store.tabs().unwrap(), saved);
        assert_eq!(lib.store.archive(&profile, 2000).unwrap(), archive);
        let survivors: Vec<_> = tabs.iter().map(Tab::row).collect();
        let _ = take_removed_parent_tabs(&mut lib, &mut tabs, parent, 200);
        assert_eq!(tabs.iter().map(Tab::row).collect::<Vec<_>>(), survivors);
        assert_eq!(lib.store.archive(&profile, 2000).unwrap(), archive, "a repeat must not archive twice");
    }
}

fn archive_profiles() -> (Library, String, String, String, String, String) {
    let mut lib = library();
    let a = lib.create_profile("A").unwrap();
    let b = lib.create_profile("B").unwrap();
    let sa = lib.create_space("A", "A", Theme::preset(0), &a).unwrap();
    let sa2 = lib.create_space("A2", "A", Theme::preset(0), &a).unwrap();
    let sb = lib.create_space("B", "B", Theme::preset(0), &b).unwrap();
    (lib, a, b, sa, sa2, sb)
}

#[test]
fn archive_profile_list_and_reopen_selection_filter_before_the_limit() {
    let (lib, _, _, sa, sa2, sb) = archive_profiles();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/older", "A older", &sa, 1).unwrap();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/latest", "A latest", &sa2, 2).unwrap();
    lib.store.archive_tab(&lib.spaces[&sb].profile_id, "https://b.example/latest", "B latest", &sb, 3).unwrap();
    let rows = archive_for_space(&lib, &sa, 300).unwrap();
    assert_eq!(rows.iter().map(|row| row.title.as_str()).collect::<Vec<_>>(), ["A latest", "A older"]);
    let rows = archive_for_space(&lib, &sa, 1).unwrap();
    assert_eq!(rows[0].title, "A latest", "ReopenClosed must select this profile's latest entry");
    let mut tabs = Vec::new();
    restore_archived_tab(&lib, &mut tabs, &sa, rows[0].rowid).unwrap().unwrap();
    assert_eq!(tabs[0].space_id, sa2, "keep the original same-profile space");
    assert_eq!(archive_for_space(&lib, &sb, 300).unwrap()[0].title, "B latest");
}

#[test]
fn archive_profile_direct_restore_cannot_switch_to_another_profile() {
    let (lib, _, _, sa, _, sb) = archive_profiles();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/private", "A private", &sa, 1).unwrap();
    let rowid = archive_for_space(&lib, &sa, 1).unwrap()[0].rowid;
    let mut tabs = Vec::new();
    assert!(restore_archived_tab(&lib, &mut tabs, &sb, rowid).unwrap().is_none());
    assert!(tabs.is_empty());
    assert!(lib.store.tabs().unwrap().is_empty());
    assert_eq!(archive_for_space(&lib, &sa, 1).unwrap()[0].rowid, rowid);
}

#[test]
fn archive_profile_deleted_space_fallback_stays_in_its_durable_profile() {
    let (mut lib, _, _, sa, sa2, sb) = archive_profiles();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/private", "A private", &sa, 1).unwrap();
    let rowid = archive_for_space(&lib, &sa, 1).unwrap()[0].rowid;
    lib.delete_space(&sa).unwrap();
    let mut tabs = Vec::new();
    assert!(restore_archived_tab(&lib, &mut tabs, &sb, rowid).unwrap().is_none());
    assert!(tabs.is_empty());
    assert!(archive_for_space(&lib, &sb, 300).unwrap().is_empty());
    restore_archived_tab(&lib, &mut tabs, &sa2, rowid).unwrap().unwrap();
    assert_eq!(tabs[0].space_id, sa2);
    assert_eq!(tabs[0].title, "A private");
}

#[test]
fn archive_profile_reassigned_original_space_does_not_reassign_its_archive() {
    let (mut lib, _, b, sa, sa2, sb) = archive_profiles();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/private", "A private", &sa, 1).unwrap();
    let rowid = archive_for_space(&lib, &sa, 1).unwrap()[0].rowid;
    lib.update_space(&sa, |space| space.profile_id = b).unwrap();
    let mut tabs = Vec::new();
    for space in [&sa, &sa2, &sb] {
        assert!(archive_for_space(&lib, space, 300).unwrap().is_empty());
        assert!(restore_archived_tab(&lib, &mut tabs, space, rowid).unwrap().is_none());
    }
    assert!(tabs.is_empty());
    assert!(lib.store.tabs().unwrap().is_empty());
}

#[test]
fn archive_profile_missing_active_space_or_profile_never_uses_a_default() {
    let (mut lib, a, _, sa, _, _) = archive_profiles();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/private", "A private", &sa, 1).unwrap();
    let rowid = archive_for_space(&lib, &sa, 1).unwrap()[0].rowid;
    let mut tabs = Vec::new();
    assert!(archive_for_space(&lib, "deleted-active-space", 300).unwrap().is_empty());
    assert!(restore_archived_tab(&lib, &mut tabs, "deleted-active-space", rowid).unwrap().is_none());
    lib.profiles.remove(&a);
    assert!(archive_for_space(&lib, &sa, 300).unwrap().is_empty());
    assert!(restore_archived_tab(&lib, &mut tabs, &sa, rowid).unwrap().is_none());
    assert!(tabs.is_empty());
    assert!(lib.store.tabs().unwrap().is_empty());
}

#[test]
fn archive_profile_store_revalidates_a_stale_in_memory_space_before_restore() {
    let (lib, _, b, sa, _, _) = archive_profiles();
    lib.store.archive_tab(&lib.spaces[&sa].profile_id, "https://a.example/private", "A private", &sa, 1).unwrap();
    let rowid = archive_for_space(&lib, &sa, 1).unwrap()[0].rowid;
    let mut row = lib.store.all_entities().unwrap().into_iter().find(|row| row.id == sa).unwrap();
    let mut changed = lib.spaces[&sa].clone();
    changed.profile_id = b;
    row.payload = serde_json::to_string(&changed).unwrap();
    lib.store.put_entity(&row, false).unwrap();
    let mut tabs = Vec::new();
    assert!(restore_archived_tab(&lib, &mut tabs, &sa, rowid).unwrap().is_none());
    assert!(tabs.is_empty());
    assert!(lib.store.tabs().unwrap().is_empty());
}

#[test]
fn archive_profile_stale_active_space_cannot_list_or_restore_a_valid_original() {
    for deleted in [false, true] {
        let (lib, a, b, sa, sa2, _) = archive_profiles();
        lib.store.archive_tab(&a, "https://a.example/private", "A private", &sa, 1).unwrap();
        let rowid = archive_for_space(&lib, &sa, 1).unwrap()[0].rowid;
        let mut row = lib.store.all_entities().unwrap().into_iter().find(|row| row.id == sa2).unwrap();
        let mut changed = lib.spaces[&sa2].clone();
        changed.profile_id = b;
        row.payload = serde_json::to_string(&changed).unwrap();
        row.deleted = deleted;
        lib.store.put_entity(&row, false).unwrap();
        let mut tabs = Vec::new();
        assert!(archive_for_space(&lib, &sa2, 300).unwrap().is_empty());
        assert!(restore_archived_tab(&lib, &mut tabs, &sa2, rowid).unwrap().is_none());
        assert!(tabs.is_empty());
        assert!(lib.store.tabs().unwrap().is_empty());
        assert_eq!(archive_for_space(&lib, &sa, 1).unwrap()[0].rowid, rowid);
    }
}

#[test]
fn archive_profile_unresolved_or_stale_live_owner_keeps_the_tab_open() {
    for failure in ["missing-memory-profile", "changed-persisted-space", "deleted-persisted-space"] {
        let (mut lib, a, b, sa, _, _) = archive_profiles();
        let mut tabs = vec![tab("draft", &sa, None)];
        tabs[0].dirty = true;
        tabs[0].muted = true;
        let before = tabs[0].row();
        lib.store.put_tab(&before).unwrap();
        if failure == "missing-memory-profile" {
            lib.profiles.remove(&a);
        } else {
            let mut row = lib.store.all_entities().unwrap().into_iter().find(|row| row.id == sa).unwrap();
            let mut changed = lib.spaces[&sa].clone();
            changed.profile_id = b.clone();
            row.payload = serde_json::to_string(&changed).unwrap();
            row.deleted = failure == "deleted-persisted-space";
            lib.store.put_entity(&row, false).unwrap();
        }
        assert!(take_closed_tab(&lib, &mut tabs, "draft").is_err(), "{failure}");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].row(), before);
        assert!(tabs[0].dirty && tabs[0].muted);
        assert_eq!(lib.store.tabs().unwrap(), vec![before]);
        assert!(lib.store.archive(&a, 300).unwrap().is_empty());
        assert!(lib.store.archive(&b, 300).unwrap().is_empty());
    }
}
