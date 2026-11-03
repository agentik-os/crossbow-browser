use super::*;
use crossbow_core::captures::{DELETE_FAILED, SAVE_FAILED};
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

/// 2026-10-29 15:04:05 UTC.
const AT: u64 = 1_793_286_245_000;

/// A disposable data folder, removed afterwards.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let dir = std::env::temp_dir().join(format!("crossbow-captures-{}", crossbow_core::new_id()));
        std::fs::create_dir(&dir).unwrap();
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A clipboard that keeps what was copied, or refuses.
#[derive(Clone, Default)]
struct Clip {
    copied: Rc<RefCell<Vec<(Vec<u8>, Option<PathBuf>)>>>,
    refuse: bool,
}

impl Board for Clip {
    fn copy(&mut self, png: &[u8], file: Option<&Path>) -> Result<(), String> {
        if self.refuse { return Err(capture::COPY_FAILED.into()); }
        self.copied.borrow_mut().push((png.to_vec(), file.map(Path::to_path_buf)));
        Ok(())
    }
}

/// A save panel that answers at once, and keeps the names it offered.
#[derive(Clone, Default)]
struct Panel {
    offered: Rc<RefCell<Vec<String>>>,
    answer: Rc<RefCell<Option<Result<Option<PathBuf>, String>>>>,
}

impl SavePanel for Panel {
    fn choose(&mut self, name: &str) -> Receiver<Result<Option<PathBuf>, String>> {
        self.offered.borrow_mut().push(name.to_owned());
        let (send, receive) = mpsc::channel();
        if let Some(answer) = self.answer.borrow_mut().take() { let _ = send.send(answer); }
        receive
    }
}

/// A host whose clipboard and save panel are fakes: never the person's.
fn fake_host(clip: &Clip, panel: &Panel) -> CaptureHost {
    CaptureHost {
        prefs: CapturePrefs::default(),
        selecting: None,
        peek_page: None,
        encoding: None,
        finished: None,
        list: ListState::default(),
        store: None,
        board: Box::new(clip.clone()),
        panel: Box::new(panel.clone()),
        saving: None,
        notices: Vec::new(),
        after_show: None,
    }
}

/// A real 3×2 PNG.
fn png() -> Vec<u8> {
    let image = capture::Image { width: 3, height: 2, rgba: [255, 0, 0, 255].repeat(6) };
    capture::png(&image, 2.0).unwrap()
}

fn encoded(profile: &str, url: &str, created_ms: u64) -> Encoded {
    Encoded { png: png(), width: 3, height: 2, profile: profile.into(), url: url.into(), title: "A story".into(), created_ms, anchor: Rect::from_min_size(egui::pos2(300.0, 100.0), egui::vec2(200.0, 120.0)), owner: None }
}

/// Every file under `dir`, hidden ones included.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            if entry.path().is_dir() { folders.push(entry.path()) } else { found.push(entry.path()) }
        }
    }
    found.sort();
    found
}

#[test]
fn a_capture_is_saved_then_copied_with_its_file_or_only_copied_when_saving_is_off() {
    let dir = Dir::new();
    let captures = Captures::open(&dir.0).unwrap();
    let mut clip = Clip::default();
    let finished = finish(encoded("personal", "https://news.example/story", AT), true, Some(Ok(&captures)), &mut clip, 10.0);
    let saved = finished.saved.clone().expect("saved to Captures");
    let path = captures.path(&saved).unwrap();
    assert_eq!((finished.notice.clone(), std::fs::read(&path).unwrap()), (None, png()));
    assert_eq!(*clip.copied.borrow(), [(png(), Some(path.clone()))], "the image and the saved file, in one copy");
    assert_eq!(captures.list("personal").unwrap(), [saved.clone()]);
    assert_eq!((saved.url.as_str(), saved.title.as_str(), saved.width, saved.height), ("https://news.example/story", "A story", 3, 2));
    // The card hides 6 seconds after it appeared, or after the pointer left it.
    assert!(!finished.expired(15.9) && finished.expired(16.0));

    // Off: the image only, and nothing written.
    let off = Dir::new();
    let mut clip = Clip::default();
    let finished = finish(encoded("personal", "https://news.example/story", AT), false, None, &mut clip, 0.0);
    assert!(finished.saved.is_none() && finished.notice.is_none());
    assert_eq!(*clip.copied.borrow(), [(png(), None)]);
    assert!(files(&off.0).is_empty(), "nothing on disk when saving is off");
}

#[test]
fn a_failed_save_leaves_no_file_and_no_record_and_says_so_on_the_card() {
    let dir = Dir::new();
    std::fs::write(dir.0.join(crossbow_core::captures::FOLDER), b"a file where the folder goes").unwrap();
    let captures = Captures::open(&dir.0).unwrap();
    let mut clip = Clip::default();
    let finished = finish(encoded("personal", "https://news.example/story", AT), true, Some(Ok(&captures)), &mut clip, 0.0);
    assert_eq!((finished.saved.clone(), finished.notice.as_deref()), (None, Some(SAVE_FAILED)));
    assert!(captures.list("personal").unwrap().is_empty(), "no record");
    assert_eq!(files(&dir.0).iter().filter(|path| path.extension().is_some_and(|ext| ext == "png" || ext == "partial")).count(), 0, "no file");
    assert_eq!(*clip.copied.borrow(), [(png(), None)], "still copied, without a file");

    // The library itself couldn't open: the same, with its own reason.
    let mut clip = Clip::default();
    let finished = finish(encoded("personal", "", AT), true, Some(Err(CapturesError::Unavailable)), &mut clip, 0.0);
    assert_eq!(finished.notice.as_deref(), Some(crossbow_core::captures::UNAVAILABLE));
    // Or it's damaged: the card says so in a line it fits, and still copies.
    let mut clip = Clip::default();
    let finished = finish(encoded("personal", "", AT), true, Some(Err(CapturesError::Damaged)), &mut clip, 0.0);
    assert_eq!((finished.saved.is_none(), finished.notice.as_deref()), (true, Some(capture_ui::NOT_SAVED_DAMAGED)));
    assert_eq!(*clip.copied.borrow(), [(png(), None)]);

    // A failed copy keeps the saved capture and says so.
    let ok = Dir::new();
    let captures = Captures::open(&ok.0).unwrap();
    let mut refusing = Clip { refuse: true, ..Clip::default() };
    let finished = finish(encoded("personal", "https://news.example/story", AT), true, Some(Ok(&captures)), &mut refusing, 0.0);
    assert!(finished.saved.is_some());
    assert_eq!(finished.notice.as_deref(), Some(capture::COPY_FAILED));
}

#[test]
fn the_card_copies_again_saves_a_copy_elsewhere_or_opens_captures() {
    let dir = Dir::new();
    let (clip, panel) = (Clip::default(), Panel::default());
    let mut host = fake_host(&clip, &panel);
    host.open(&dir.0).unwrap();
    let finished = finish(encoded("personal", "https://news.example/story", AT), true, opened(&host.store).map(Ok), &mut clip.clone(), 0.0);
    let saved = finished.saved.clone().unwrap();
    host.finished = Some(finished);

    host.card_action(CardAction::Copy);
    assert_eq!(clip.copied.borrow().len(), 2, "copied again");
    assert_eq!(clip.copied.borrow()[1].1.as_deref(), opened(&host.store).unwrap().path(&saved).as_deref());

    // Save As…: offered under the capture's name, written where chosen.
    let chosen = dir.0.join("Elsewhere.png");
    *panel.answer.borrow_mut() = Some(Ok(Some(chosen.clone())));
    host.card_action(CardAction::SaveAs);
    assert!(!host.poll_save_as(), "answered");
    assert_eq!(*panel.offered.borrow(), [saved.file.clone()]);
    assert_eq!(std::fs::read(&chosen).unwrap(), png());
    // Cancel writes nothing and says nothing; a folder that can't be written says so.
    *panel.answer.borrow_mut() = Some(Ok(None));
    host.card_action(CardAction::SaveAs);
    host.poll_save_as();
    *panel.answer.borrow_mut() = Some(Ok(Some(dir.0.join("missing folder").join("x.png"))));
    host.card_action(CardAction::SaveAs);
    host.poll_save_as();
    assert_eq!(host.notices, [capture::SAVE_AS_FAILED]);
    assert!(!dir.0.join("missing folder").exists());

    host.card_action(CardAction::OpenInCaptures);
    assert!(host.finished.is_none() && host.list.show);
    assert_eq!(host.list.viewer.as_ref().map(|(id, _)| *id), Some(saved.id), "the capture opens in the viewer");

    host.finished = Some(finish(encoded("personal", "", AT), false, None, &mut clip.clone(), 0.0));
    host.card_action(CardAction::Close);
    assert!(host.finished.is_none());
}

#[test]
fn the_captures_list_shows_and_acts_on_the_current_profiles_captures_only_and_survives_reopening() {
    let dir = Dir::new();
    let (personal, work) = {
        let captures = Captures::open(&dir.0).unwrap();
        let mut clip = Clip::default();
        let first = finish(encoded("personal", "https://news.example/story", AT), true, Some(Ok(&captures)), &mut clip, 0.0).saved.unwrap();
        let second = finish(encoded("personal", "https://docs.example/guide", AT + 60_000), true, Some(Ok(&captures)), &mut clip, 0.0).saved.unwrap();
        let work = finish(encoded("work", "https://work.example/", AT), true, Some(Ok(&captures)), &mut clip, 0.0).saved.unwrap();
        ((first, second), work)
    };
    let ctx = egui::Context::default();
    let (clip, panel) = (Clip::default(), Panel::default());
    let mut host = fake_host(&clip, &panel);
    let rows = host.rows(&ctx, &dir.0, "personal").unwrap();
    assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [personal.1.id, personal.0.id], "newest first, this profile only");
    assert!(rows[0].detail.starts_with("docs.example · ") && rows[0].title == "A story" && !rows[0].missing);
    assert!(rows.iter().all(|row| row.thumbnail.is_some()), "thumbnails decoded");

    // Another profile's capture can't be reached from this one.
    for action in [ListAction::Delete(work.id), ListAction::Copy(work.id), ListAction::Reveal(work.id), ListAction::Open(work.id)] {
        assert_eq!(host.list_action(&dir.0, "personal", action), None);
    }
    assert!(clip.copied.borrow().is_empty() && host.list.viewer.is_none());
    assert_eq!(host.rows(&ctx, &dir.0, "work").unwrap().len(), 1, "still there");

    let path = opened(&host.store).unwrap().path(&personal.0).unwrap();
    assert_eq!(host.list_action(&dir.0, "personal", ListAction::Reveal(personal.0.id)), Some(path.clone()));
    host.list_action(&dir.0, "personal", ListAction::Copy(personal.0.id));
    assert_eq!(*clip.copied.borrow(), [(png(), Some(path.clone()))]);
    host.list_action(&dir.0, "personal", ListAction::Open(personal.0.id));
    assert_eq!(host.list.viewer.as_ref().map(|(id, _)| *id), Some(personal.0.id));
    host.list_action(&dir.0, "personal", ListAction::Delete(personal.0.id));
    assert!(!path.exists() && host.list.viewer.is_none(), "the file goes, and the viewer showing it closes");
    assert!(host.notices.is_empty());

    // Quit and reopen: a new host on the same data.
    let mut reopened = fake_host(&clip, &panel);
    assert_eq!(reopened.rows(&ctx, &dir.0, "personal").unwrap().iter().map(|row| row.id).collect::<Vec<_>>(), [personal.1.id]);
    assert_eq!(reopened.rows(&ctx, &dir.0, "work").unwrap().iter().map(|row| row.id).collect::<Vec<_>>(), [work.id]);
}

#[test]
fn a_failed_delete_keeps_the_file_and_the_record_and_says_so() {
    let dir = Dir::new();
    let captures = Captures::open(&dir.0).unwrap();
    let saved = finish(encoded("personal", "https://news.example/story", AT), true, Some(Ok(&captures)), &mut Clip::default(), 0.0).saved.unwrap();
    let folder = captures.folder("personal").unwrap();
    drop(captures);
    let (clip, panel) = (Clip::default(), Panel::default());
    let mut host = fake_host(&clip, &panel);
    // A real failure: the profile's folder can't be changed.
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    host.list_action(&dir.0, "personal", ListAction::Delete(saved.id));
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(host.notices, [DELETE_FAILED]);
    let rows = host.rows(&egui::Context::default(), &dir.0, "personal").unwrap();
    assert_eq!((rows.len(), rows[0].missing), (1, false), "the record and the file stay");
}

#[test]
fn a_missing_file_is_listed_as_missing_and_remove_forgets_it() {
    let dir = Dir::new();
    let captures = Captures::open(&dir.0).unwrap();
    let untitled = Encoded { title: String::new(), ..encoded("personal", "", AT) };
    let saved = finish(untitled, true, Some(Ok(&captures)), &mut Clip::default(), 0.0).saved.unwrap();
    std::fs::remove_file(captures.path(&saved).unwrap()).unwrap();
    let (clip, panel) = (Clip::default(), Panel::default());
    let mut host = fake_host(&clip, &panel);
    let ctx = egui::Context::default();
    let rows = host.rows(&ctx, &dir.0, "personal").unwrap();
    assert_eq!((rows.len(), rows[0].missing, rows[0].thumbnail.is_some()), (1, true, false));
    assert_eq!(rows[0].title, CAPTURE, "no title and no address: called a capture");
    host.list_action(&dir.0, "personal", ListAction::Copy(saved.id));
    assert_eq!(host.notices, [crate::ui::capture::FILE_MISSING]);
    host.list_action(&dir.0, "personal", ListAction::Remove(saved.id));
    assert!(host.rows(&ctx, &dir.0, "personal").unwrap().is_empty());
}

#[test]
fn both_switches_start_on_and_saving_is_chosen_per_profile() {
    let fixture = crate::shortcuts::tests::Fixture::new();
    let store = fixture.store();
    let mut prefs = CapturePrefs::default();
    assert!(prefs.drag(&store) && prefs.save(&store, "personal") && prefs.save(&store, "work"));
    prefs.save_switch(&store, "personal").choose(&store, false);
    let mut reopened = CapturePrefs::default();
    assert!(!reopened.save(&store, "personal") && reopened.save(&store, "work"));
    prefs.drag_switch(&store).choose(&store, false);
    assert!(!CapturePrefs::default().drag(&store));
}

/// The copy goes to a private, uniquely named pasteboard: never the clipboard.
#[cfg(target_os = "macos")]
#[test]
fn the_pasteboard_gets_the_image_and_the_saved_file_as_one_item() {
    let dir = Dir::new();
    let file = dir.0.join("Capture 2026-10-29 at 15.04.05.png");
    std::fs::write(&file, png()).unwrap();
    let mut board = capture::mac::Pasteboard::unique().expect("a private pasteboard");
    board.copy(&png(), Some(&file)).unwrap();
    let items = board.items();
    assert_eq!(items.len(), 1);
    let url = String::from_utf8(items[0].iter().find(|(kind, _)| kind == capture::mac::FILE_URL).expect("the file").1.clone()).unwrap();
    assert!(url.starts_with("file:///") && url.ends_with("/Capture%202026-10-29%20at%2015.04.05.png"), "{url}");
    assert_eq!(items[0].iter().find(|(kind, _)| kind == capture::mac::PNG).map(|(_, bytes)| bytes.clone()), Some(png()));

    board.copy(&png(), None).unwrap();
    let items = board.items();
    assert_eq!(items.iter().map(|item| item.iter().map(|(kind, _)| kind.as_str()).collect::<Vec<_>>()).collect::<Vec<_>>(), [[capture::mac::PNG]], "the image only, replacing the last copy");
    board.release();
}

#[test]
fn capture_is_offered_by_its_shortcut_the_command_bar_and_the_tab_menu() {
    use crate::shortcuts::{Chord, Command, Shortcuts};
    let shortcuts = Shortcuts::defaults();
    assert_eq!(shortcuts.binding(Command::Capture, crate::shortcuts::Slot::Primary), Some(Chord { key: Key::Num2, mods: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT }));
    // Running them needs the whole app: checked in source.
    let app = include_str!("app.rs");
    assert!(app.contains("Command::Capture => push(Action::Capture(None)),"));
    assert!(app.contains("Action::Capture(tab) => self.start_capture(tab),"));
    assert!(include_str!("ui/command_bar.rs").contains("\"capture\" => Some(Action::Capture(None)),\n                \"captures\" => Some(Action::ShowCaptures),"));
    assert!(include_str!("ui/overlays.rs").contains("if m.item(ui, crate::app::capture_host::CAPTURE, &hint(Command::Capture), true) {\n                        app.actions.push(Action::Capture(Some(tab.clone())));"));
}

/// The main window's pages and Peek need the whole app: checked in source.
#[test]
fn command_shift_drag_is_taken_before_the_page_and_a_selection_keeps_every_key() {
    let content = include_str!("ui/content.rs");
    let press = content.find("if app.capture_press(&ctx, tab_id, rect, resp.hovered()) {").expect("panes");
    assert!(press < content.find("    forward_pointer(app, ui, &view, tab_id, rect").unwrap(), "before the page gets the press");
    assert!(content.contains("app.quit.borrow().asking() || app.capture.selecting.is_some()"), "no page input while selecting");
    let overlays = include_str!("ui/overlays.rs");
    assert!(overlays.contains("if capturing || (input_enabled && closing_at.is_none() && open_t >= 1.0 && app.capture_press(&ctx, \"peek\", shown, resp.hovered())) {"));
    let app = include_str!("app.rs");
    assert!(app.contains("        // A capture being selected takes every key, Escape included.\n        if self.capture.selecting.is_some() { return; }"));
    assert!(app.contains("|| self.tidy_tabs.sheet.is_some() || self.capture.selecting.is_some()"), "pages get no keys");
}

#[test]
fn a_damaged_captures_file_never_stops_capturing_and_move_aside_lists_its_files_again() {
    let dir = Dir::new();
    let ctx = egui::Context::default();
    let (clip, panel) = (Clip::default(), Panel::default());
    let mut host = fake_host(&clip, &panel);
    host.open(&dir.0).unwrap();
    let store = opened(&host.store).unwrap();
    let first = finish(encoded("personal", "https://news.example/one", AT), true, Some(Ok(store)), &mut clip.clone(), 0.0).saved.unwrap();
    finish(encoded("personal", "https://news.example/two", AT + 1000), true, Some(Ok(store)), &mut clip.clone(), 0.0);
    finish(encoded("work", "https://news.example/work", AT), true, Some(Ok(store)), &mut clip.clone(), 0.0);
    host.store = None;
    let file = dir.0.join(crossbow_core::captures::FILE_NAME);
    std::fs::write(&file, b"damaged beyond reading".repeat(300)).unwrap();
    for suffix in ["-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", file.display())); }

    // The list says why, every time, and the file is left as it is.
    for _ in 0..2 { assert_eq!(host.rows(&ctx, &dir.0, "personal").err(), Some(CapturesError::Damaged)); }
    assert_eq!(host.open(&dir.0), Err(CapturesError::Damaged));
    assert_eq!(host.list_action(&dir.0, "personal", ListAction::Copy(first.id)), None);
    assert_eq!(host.notices.drain(..).collect::<Vec<_>>(), [crossbow_core::captures::DAMAGED]);
    assert_eq!(std::fs::read(&file).unwrap(), b"damaged beyond reading".repeat(300));

    host.move_aside(&dir.0);
    assert_eq!(host.notices.drain(..).collect::<Vec<_>>(), ["Moved the damaged file aside. 3 captures are listed again, without their pages."]);
    let rows = host.rows(&ctx, &dir.0, "personal").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.title == CAPTURE && !row.missing), "listed again without a page: {rows:?}");
    assert_eq!(host.rows(&ctx, &dir.0, "work").unwrap().len(), 1);
    let kept: Vec<_> = std::fs::read_dir(&dir.0).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("captures-damaged-")).collect();
    assert_eq!(kept.len(), 1, "the damaged file is kept beside the new one: {kept:?}");
    // And capturing saves again.
    let saved = finish(encoded("personal", "https://news.example/three", AT + 2000), true, Some(Ok(opened(&host.store).unwrap())), &mut clip.clone(), 0.0);
    assert!(saved.saved.is_some() && saved.notice.is_none());
}

#[test]
fn a_folder_that_refuses_is_tried_again_and_not_called_damaged() {
    let dir = Dir::new();
    let (clip, panel) = (Clip::default(), Panel::default());
    let mut host = fake_host(&clip, &panel);
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(host.open(&dir.0), Err(CapturesError::Unavailable));
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(host.open(&dir.0), Ok(()), "opened once the folder allows it");
}
