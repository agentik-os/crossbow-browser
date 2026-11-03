use super::*;

/// One video's strings: number, paused, ended, muted, volume, user started, x, y, width, height.
fn video(id: u32, paused: bool, muted: bool, volume: f64, user: bool, rect: (f64, f64, f64, f64)) -> Vec<String> {
    let flag = |value: bool| if value { "1" } else { "0" }.to_string();
    vec![id.to_string(), flag(paused), "0".into(), flag(muted), volume.to_string(), flag(user), rect.0.to_string(), rect.1.to_string(), rect.2.to_string(), rect.3.to_string()]
}

fn report(videos: &[Vec<String>]) -> Vec<String> {
    let mut args: Vec<String> = ["1", "77", "2", "1280", "800", "0", "1500"].map(String::from).to_vec();
    for video in videos { args.extend(video.iter().cloned()); }
    args
}

fn parsed(videos: &[Vec<String>]) -> MediaReport {
    parse_report(&report(videos)).expect("a valid report")
}

#[test]
fn reports_are_read_strictly_and_anything_out_of_range_drops_them() {
    let small = video(1, false, false, 1.0, false, (10.0, 10.0, 160.0, 90.0));
    let large = video(2, true, true, 0.5, true, (0.0, 100.0, 640.0, 360.0));
    let read = parsed(&[small.clone(), large.clone()]);
    assert_eq!((read.page, read.device_pixel_ratio, read.viewport, read.scroll), (77, 2.0, (1280.0, 800.0), (0.0, 1500.0)));
    assert_eq!(read.videos.iter().map(|video| video.id).collect::<Vec<_>>(), [2, 1], "largest first");
    assert_eq!(read.videos[0], Video { id: 2, paused: true, ended: false, muted: true, volume: 0.5, user_started: true, rect: CssRect { x: 0.0, y: 100.0, width: 640.0, height: 360.0 } });
    assert!(parsed(&[]).videos.is_empty(), "the last video went");

    let mut bad: Vec<Vec<String>> = Vec::new();
    let with = |index: usize, value: &str| { let mut args = report(std::slice::from_ref(&small)); args[index] = value.into(); args };
    for (index, value) in [(0, "2"), (1, "0"), (1, "-3"), (2, "0"), (2, "NaN"), (3, "0"), (4, "inf"), (5, "1e9")] { bad.push(with(index, value)); }
    // The first video's fields start at 7.
    for (field, value) in [(0, "0"), (0, "x"), (1, "true"), (2, ""), (3, "2"), (4, "1.5"), (4, "-0.1"), (5, "yes"), (6, "NaN"), (8, "-1"), (9, "1e7")] { bad.push(with(7 + field, value)); }
    bad.push(report(&[small.clone(), small.clone()]));
    bad.push(report(&[small.clone(), video(2, false, false, 1.0, false, (0.0, 0.0, 1.0, 1.0)), video(3, false, false, 1.0, false, (0.0, 0.0, 1.0, 1.0)), video(4, false, false, 1.0, false, (0.0, 0.0, 1.0, 1.0)), video(5, false, false, 1.0, false, (0.0, 0.0, 1.0, 1.0))]));
    let mut short = report(std::slice::from_ref(&small));
    short.pop();
    bad.push(short);
    bad.push(report(&[])[..6].to_vec());
    for args in bad { assert_eq!(parse_report(&args), None, "{args:?}"); }
}

#[test]
fn automatic_picture_in_picture_follows_the_largest_audible_video_never_a_muted_autoplay() {
    // A large muted loop that started itself, and a smaller video playing aloud.
    let background = video(1, false, true, 1.0, false, (0.0, 0.0, 1280.0, 720.0));
    let aloud = video(2, false, false, 0.8, false, (0.0, 0.0, 640.0, 360.0));
    let quiet = video(3, false, false, 0.0, false, (0.0, 0.0, 960.0, 540.0));
    let page = parsed(&[background.clone(), aloud.clone(), quiet.clone()]);
    assert_eq!(page.automatic(false).map(|video| video.id), Some(2), "volume 0 and muted are not heard");
    assert_eq!(parsed(&[background.clone()]).automatic(false), None, "a muted autoplay never counts");
    assert_eq!(parsed(&[quiet]).automatic(false), None);
    // Two audible videos: the larger.
    let larger = video(4, false, false, 1.0, false, (0.0, 0.0, 800.0, 450.0));
    assert_eq!(parsed(&[aloud.clone(), larger]).automatic(false).map(|video| video.id), Some(4));
    // Muted by Crossbow: not heard, but a video the user started still counts.
    assert_eq!(parsed(&[aloud.clone()]).automatic(true), None);
    let clicked = video(5, false, true, 1.0, true, (0.0, 0.0, 320.0, 180.0));
    assert_eq!(parsed(&[background.clone(), clicked.clone()]).automatic(true).map(|video| video.id), Some(5), "user-started");
    // Paused, ended or without size: no.
    assert_eq!(parsed(&[video(6, true, false, 1.0, true, (0.0, 0.0, 320.0, 180.0))]).automatic(false), None);
    let mut ended = video(7, false, false, 1.0, true, (0.0, 0.0, 320.0, 180.0));
    ended[2] = "1".into();
    assert_eq!(parsed(&[ended]).automatic(false), None);
    assert_eq!(parsed(&[video(8, false, false, 1.0, true, (0.0, 0.0, 0.0, 180.0))]).automatic(false), None);

    // Asked for by name: the automatic one, else the largest playing, else the largest.
    assert_eq!(page.manual(false).map(|video| video.id), Some(2));
    assert_eq!(parsed(&[background.clone(), video(9, true, false, 1.0, false, (0.0, 0.0, 1920.0, 1080.0))]).manual(false).map(|video| video.id), Some(1));
    assert_eq!(parsed(&[video(9, true, false, 1.0, false, (0.0, 0.0, 1920.0, 1080.0)), video(10, true, false, 1.0, false, (0.0, 0.0, 20.0, 20.0))]).manual(false).map(|video| video.id), Some(9));
    assert_eq!(parsed(&[]).manual(false), None);
}

#[test]
fn a_reported_rect_only_ever_crops_the_tabs_own_pixels() {
    let page = parsed(&[video(1, false, false, 1.0, false, (100.0, 50.0, 640.0, 360.0))]);
    let shown = &page.videos[0];
    assert_eq!(page.crop(shown, (2560, 1600)), Some(PixelRect { x: 200.0, y: 100.0, width: 1280.0, height: 720.0 }), "at the page's pixel ratio");
    assert!(page.in_view(shown));
    // Partly off screen: only the part in the frame.
    let low = parsed(&[video(1, false, false, 1.0, false, (0.0, 600.0, 640.0, 360.0))]);
    assert_eq!(low.crop(&low.videos[0], (2560, 1600)), Some(PixelRect { x: 0.0, y: 1200.0, width: 1280.0, height: 400.0 }));
    assert!(!low.in_view(&low.videos[0]));
    // Off screen, or a page claiming more than the tab has: never outside its frame.
    let gone = parsed(&[video(1, false, false, 1.0, false, (0.0, 5000.0, 640.0, 360.0))]);
    assert_eq!(gone.crop(&gone.videos[0], (2560, 1600)), None);
    let huge = parsed(&[video(1, false, false, 1.0, false, (-900000.0, -900000.0, 999999.0, 999999.0))]);
    assert_eq!(huge.crop(&huge.videos[0], (2560, 1600)), Some(PixelRect { x: 0.0, y: 0.0, width: 2560.0, height: 1600.0 }));
}

#[test]
fn each_command_is_one_message_whose_parts_the_renderer_checks() {
    for (command, expected) in [
        (MediaCommand::Play(3), ("play", 3.0, 0.0)),
        (MediaCommand::Pause(3), ("pause", 3.0, 0.0)),
        (MediaCommand::Seek { video: 3, seconds: -10.0 }, ("seek", 3.0, -10.0)),
        (MediaCommand::Seek { video: 3, seconds: 1.0e9 }, ("seek", 3.0, 3600.0)),
        (MediaCommand::SetMuted { video: 3, muted: true }, ("mute", 3.0, 1.0)),
        (MediaCommand::SetMuted { video: 3, muted: false }, ("mute", 3.0, 0.0)),
        (MediaCommand::Reveal(3), ("reveal", 3.0, 0.0)),
        (MediaCommand::Restore { x: 12.5, y: 1500.0 }, ("restore", 12.5, 1500.0)),
    ] {
        let encoded = command.encode();
        assert_eq!(MediaCommand::decode(&encoded), Some(expected), "{command:?}");
    }
    for args in [vec!["eval", "1", "0"], vec!["play", "NaN", "0"], vec!["play", "1"], vec!["seek", "1", "inf"], vec!["play", "1", "0", "0"]] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        assert_eq!(MediaCommand::decode(&args), None, "{args:?}");
    }
}

#[test]
fn chromiums_own_picture_in_picture_is_turned_off_within_any_existing_list() {
    let ours = "PictureInPictureAPI,DocumentPictureInPictureAPI";
    assert_eq!(merge_features("", DISABLED_BLINK_FEATURES), ours);
    assert_eq!(merge_features("Foo, Bar,", DISABLED_BLINK_FEATURES), format!("Foo,Bar,{ours}"));
    assert_eq!(merge_features("PictureInPictureAPI,Foo", DISABLED_BLINK_FEATURES), "PictureInPictureAPI,Foo,DocumentPictureInPictureAPI", "no duplicate");
    // The switch is read, merged and written back on the real command line.
    let lib = include_str!("lib.rs");
    let at = lib.find("fn on_before_command_line_processing").unwrap();
    let body = &lib[at..at + lib[at..].find("fn browser_process_handler").unwrap()];
    let read = body.find("cl.switch_value(Some(&name))").expect("reads any existing list");
    let merge = body.find("media::merge_features(&existing, media::DISABLED_BLINK_FEATURES)").expect("merges ours in");
    let write = body.find("cl.append_switch_with_value(Some(&name), Some(&CefString::from(merged.as_str())))").expect("writes the merged list");
    assert!(read < merge && merge < write);
    assert_eq!(DISABLE_BLINK_FEATURES, "disable-blink-features");
}

/// The script's capture rule and its wiring, checked in source. The
/// JavaScriptCore tests below also compile and run it.
#[test]
fn the_reporter_captures_its_built_ins_first_and_is_wired_to_top_frames_only() {
    let script = SCRIPT;
    let body = &script[script.find("// -- Everything below calls only the references captured above. --").expect("a capture boundary")..];
    for unsafe_use in ["...", "for (const", "for (let [", " of ", ".push(", ".forEach(", "JSON.", "console.", "document.", "window.", "navigator.", "Math.", "setTimeout(", ".getBoundingClientRect(", ".play(", ".pause(", ".scrollIntoView(", ".scrollTo("] {
        assert!(!body.contains(unsafe_use), "the script body uses {unsafe_use:?}, which a page could have patched");
    }
    assert!(body.contains("apply(intoView, video, [{ block: 'center', inline: 'center', behavior: 'instant' }])"));
    assert!(body.contains("apply(scrollTo, win, [{ left: first, top: second, behavior: 'instant' }])"));
    assert!(body.contains("apply(weakSet, started, [video, activated()])"), "user-started is decided when playback starts");

    let renderer = include_str!("login.rs");
    for wired in ["crate::media::inject(frame, context);", "crate::media::release(frame, context);", "if source_process == ProcessId::BROWSER && crate::media::command(frame, message) { return 1; }"] {
        assert!(renderer.contains(wired), "{wired}");
    }
    let media = include_str!("media.rs");
    assert!(media.contains("if frame.is_main() != 1 || crate::login::page_origin("), "http(s) top frames only");
    assert!(media.contains("context.frame().filter(|frame| frame.is_main() == 1)"), "reports from top frames only");
    let view = include_str!("view.rs");
    assert!(view.contains("self.h.0.login_message(frame, message) || self.h.0.media_message(frame, message)"));
    assert!(view.contains("// A new document: its script reports its own videos.\n                self.h.0.media.borrow_mut().take();"));
}

/// The page script compiled and run in the system JavaScriptCore against
/// `media_tests.js`, a stand-in page: CEF's V8 can't run in unit tests, and a
/// script checked only as text can still fail to compile.
#[cfg(target_os = "macos")]
pub(crate) mod javascript_core {
    use std::ffi::{CString, c_char, c_void};

    type Context = *mut c_void;
    type Text = *mut c_void;
    type Value = *const c_void;
    type Object = *mut c_void;

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        fn JSGlobalContextCreate(global_class: *mut c_void) -> Context;
        fn JSGlobalContextRelease(context: Context);
        fn JSContextGetGlobalObject(context: Context) -> Object;
        fn JSStringCreateWithUTF8CString(text: *const c_char) -> Text;
        fn JSStringRelease(text: Text);
        fn JSStringGetMaximumUTF8CStringSize(text: Text) -> usize;
        fn JSStringGetUTF8CString(text: Text, buffer: *mut c_char, size: usize) -> usize;
        fn JSCheckScriptSyntax(context: Context, script: Text, source_url: Text, line: i32, exception: *mut Value) -> bool;
        fn JSEvaluateScript(context: Context, script: Text, this: Object, source_url: Text, line: i32, exception: *mut Value) -> Value;
        fn JSValueToStringCopy(context: Context, value: Value, exception: *mut Value) -> Text;
        fn JSValueToObject(context: Context, value: Value, exception: *mut Value) -> Object;
        fn JSValueToNumber(context: Context, value: Value, exception: *mut Value) -> f64;
        fn JSObjectGetProperty(context: Context, object: Object, name: Text, exception: *mut Value) -> Value;
        fn JSObjectSetProperty(context: Context, object: Object, name: Text, value: Value, attributes: u32, exception: *mut Value);
    }

    /// A JavaScriptCore string, released when dropped.
    struct JsText(Text);

    impl JsText {
        fn new(text: &str) -> JsText {
            let text = CString::new(text).expect("no NUL in a script");
            JsText(unsafe { JSStringCreateWithUTF8CString(text.as_ptr()) })
        }

        fn read(&self) -> String {
            let size = unsafe { JSStringGetMaximumUTF8CStringSize(self.0) };
            let mut buffer = vec![0u8; size];
            let written = unsafe { JSStringGetUTF8CString(self.0, buffer.as_mut_ptr().cast(), size) };
            buffer.truncate(written.saturating_sub(1));
            String::from_utf8(buffer).expect("UTF-8 from JavaScriptCore")
        }
    }

    impl Drop for JsText {
        fn drop(&mut self) {
            unsafe { JSStringRelease(self.0) }
        }
    }

    /// One global context, like one page's.
    pub(crate) struct Page(Context);

    impl Page {
        pub(crate) fn new() -> Page {
            Page(unsafe { JSGlobalContextCreate(std::ptr::null_mut()) })
        }

        /// The engine's syntax error if `script` doesn't compile.
        pub(crate) fn compile(&self, script: &str, url: &str) -> Result<(), String> {
            let (script, url) = (JsText::new(script), JsText::new(url));
            let mut exception: Value = std::ptr::null();
            let compiled = unsafe { JSCheckScriptSyntax(self.0, script.0, url.0, 1, &mut exception) };
            if compiled { Ok(()) } else { Err(self.error(exception)) }
        }

        /// Run `script` and give its value as a string, or what it threw.
        pub(crate) fn run(&self, script: &str) -> Result<String, String> {
            let value = self.evaluate(script, "crossbow://media-test")?;
            let mut exception: Value = std::ptr::null();
            let text = unsafe { JSValueToStringCopy(self.0, value, &mut exception) };
            if text.is_null() { return Err(self.error(exception)); }
            Ok(JsText(text).read())
        }

        /// Run `script` as CEF's renderer does and keep its value as the global `name`.
        pub(crate) fn keep(&self, name: &str, script: &str, url: &str) -> Result<(), String> {
            let value = self.evaluate(script, url)?;
            let name = JsText::new(name);
            let mut exception: Value = std::ptr::null();
            unsafe { JSObjectSetProperty(self.0, JSContextGetGlobalObject(self.0), name.0, value, 0, &mut exception) };
            if exception.is_null() { Ok(()) } else { Err(self.error(exception)) }
        }

        fn evaluate(&self, script: &str, url: &str) -> Result<Value, String> {
            let (script, url) = (JsText::new(script), JsText::new(url));
            let mut exception: Value = std::ptr::null();
            let value = unsafe { JSEvaluateScript(self.0, script.0, std::ptr::null_mut(), url.0, 1, &mut exception) };
            if value.is_null() { Err(self.error(exception)) } else { Ok(value) }
        }

        /// A thrown value as its message and line.
        fn error(&self, exception: Value) -> String {
            if exception.is_null() { return "no exception was given".into(); }
            let mut ignored: Value = std::ptr::null();
            let text = unsafe { JSValueToStringCopy(self.0, exception, &mut ignored) };
            let message = if text.is_null() { "an exception".to_string() } else { JsText(text).read() };
            let object = unsafe { JSValueToObject(self.0, exception, &mut ignored) };
            if object.is_null() { return message; }
            let name = JsText::new("line");
            let line = unsafe { JSValueToNumber(self.0, JSObjectGetProperty(self.0, object, name.0, &mut ignored), &mut ignored) };
            format!("{message} (line {line})")
        }
    }

    impl Drop for Page {
        fn drop(&mut self) {
            unsafe { JSGlobalContextRelease(self.0) }
        }
    }
}

/// `media.js` started in the stand-in page as the renderer starts it, with
/// a film, a muted autoplay background and a video that takes no space.
#[cfg(target_os = "macos")]
fn started_page() -> javascript_core::Page {
    let page = javascript_core::Page::new();
    page.compile(SCRIPT, SCRIPT_URL).unwrap_or_else(|error| panic!("media.js doesn't compile: {error}"));
    page.run(include_str!("media_tests.js")).expect("the stand-in page");
    page.keep("factory", SCRIPT, SCRIPT_URL).unwrap_or_else(|error| panic!("media.js doesn't evaluate: {error}"));
    assert_eq!(page.run("typeof factory").unwrap(), "function", "the script evaluates to its factory");
    page.run("var film = crossbow.video('film', [10, 20, 640, 360], false); var background = crossbow.video('background', [700, 20, 320, 180], true); crossbow.video('hidden', [0, 0, 0, 0], false);").unwrap();
    let command = page.run("var command = factory(function () { crossbow.reports.push(Array.prototype.slice.call(arguments)); }); typeof command");
    assert_eq!(command.unwrap_or_else(|error| panic!("the factory threw: {error}")), "function", "the factory gives the command function");
    page
}

#[cfg(target_os = "macos")]
fn sent(page: &javascript_core::Page) -> Vec<MediaReport> {
    let sent = page.run("crossbow.sent()").unwrap();
    assert_ne!(sent, "not strings", "report() is only ever given strings");
    sent.lines().map(|line| parse_report(&line.split(' ').map(String::from).collect::<Vec<_>>()).unwrap_or_else(|| panic!("the browser can't read {line:?}"))).collect()
}

#[cfg(target_os = "macos")]
#[test]
fn the_reporter_compiles_and_its_reports_of_a_stand_in_page_read_as_the_browser_expects() {
    let page = started_page();
    let listening = [
        "document play true", "document playing true", "document pause true", "document ended true", "document volumechange true",
        "document emptied true", "document loadedmetadata true", "document resize true",
        r#"window scroll {"capture":true,"passive":true}"#, r#"window resize {"passive":true}"#,
    ];
    assert_eq!(page.run("crossbow.listening()").unwrap(), listening.join("\n"));
    assert!(sent(&page).is_empty(), "nothing before a media event");

    // The muted background starts without a user activation, the film with one.
    page.run("crossbow.fire('play', background); crossbow.active = true; crossbow.fire('play', film); crossbow.active = false;").unwrap();
    assert_eq!(page.run("crossbow.run()").unwrap(), "250", "one throttled report for both events");
    let [report] = sent(&page).try_into().expect("one report");
    assert_eq!((report.device_pixel_ratio, report.viewport, report.scroll), (2.0, (1280.0, 720.0), (0.0, 0.0)));
    assert_eq!(report.videos.len(), 2, "a video that takes no space isn't reported");
    let (film, background) = (&report.videos[0], &report.videos[1]);
    assert_eq!((film.rect.x, film.rect.y, film.rect.width, film.rect.height), (10.0, 20.0, 640.0, 360.0));
    assert!(film.user_started && !film.muted && film.volume == 1.0 && !film.paused);
    assert!(background.muted && !background.user_started);
    assert_eq!(report.automatic(false).map(|video| video.id), Some(film.id));
    assert_eq!(page.run("crossbow.timers.map(function (timer) { return timer.delay; }).join(' ')").unwrap(), "1000", "then every second while there's a video");

    // Scrolling is reported, from the same page.
    page.run("crossbow.scroll = [0, 480]; crossbow.layout('scroll');").unwrap();
    assert_eq!(page.run("crossbow.run()").unwrap(), "1000", "already due");
    let [scrolled] = sent(&page).try_into().expect("one report");
    assert_eq!((scrolled.page, scrolled.scroll), (report.page, (0.0, 480.0)));

    // The last video going is said once, then the page is quiet.
    page.run("crossbow.videos = []; crossbow.fire('emptied', film);").unwrap();
    page.run("crossbow.run()").unwrap();
    let [gone] = sent(&page).try_into().expect("one report");
    assert!(gone.videos.is_empty() && gone.page == report.page);
    page.run("crossbow.layout('resize'); crossbow.run();").unwrap();
    assert!(sent(&page).is_empty() && page.run("crossbow.timers.length").unwrap() == "0", "quiet once said");
}

/// What a page does after the script started: every built-in it captured is
/// replaced with one that throws.
#[cfg(target_os = "macos")]
const PATCHED_PAGE: &str = r#"(function () {
  const broken = function () { throw new Error('a built-in the page replaced was called'); };
  const getters = (owner, names) => names.forEach(function (name) { Object.defineProperty(owner, name, { get: broken, set: broken, configurable: true }); });
  getters(HTMLMediaElement.prototype, ['paused', 'ended', 'muted', 'volume', 'currentTime', 'duration']);
  getters(DOMRectReadOnly.prototype, ['x', 'y', 'width', 'height']);
  getters(HTMLCollection.prototype, ['length']);
  getters(window, ['devicePixelRatio', 'innerWidth', 'innerHeight', 'scrollX', 'scrollY']);
  getters(Event.prototype, ['type', 'target']);
  getters(Navigator.prototype, ['userActivation']);
  getters(UserActivation.prototype, ['isActive']);
  Reflect.apply = broken;
  Object.getOwnPropertyDescriptor = broken;
  EventTarget.prototype.addEventListener = broken;
  Document.prototype.getElementsByTagName = broken;
  HTMLCollection.prototype.item = broken;
  Element.prototype.getBoundingClientRect = broken;
  Element.prototype.scrollIntoView = broken;
  HTMLMediaElement.prototype.play = broken;
  HTMLMediaElement.prototype.pause = broken;
  window.scrollTo = broken;
  window.setTimeout = broken;
  WeakMap.prototype.get = broken;
  WeakMap.prototype.set = broken;
  Array.prototype.sort = broken;
  Promise.prototype.catch = broken;
  Math.max = broken; Math.min = broken; Math.floor = broken; Math.random = broken;
  Number.isFinite = broken;
  String = broken;
})()"#;

#[cfg(target_os = "macos")]
#[test]
fn each_player_command_reaches_its_video_through_built_ins_captured_before_the_page_ran() {
    let page = started_page();
    page.run("crossbow.fire('play', film); crossbow.run();").unwrap();
    let [report] = sent(&page).try_into().expect("one report");
    let (film, background) = (report.videos[0].id, report.videos[1].id);
    page.run(PATCHED_PAGE).unwrap();

    // The film is at 30 s of 120 s.
    for (command, effect) in [
        (MediaCommand::Pause(film), "pause film"),
        (MediaCommand::Seek { video: film, seconds: 10.0 }, "time film 40"),
        (MediaCommand::Seek { video: film, seconds: -100.0 }, "time film 0"),
        (MediaCommand::Seek { video: film, seconds: 1000.0 }, "time film 120"),
        (MediaCommand::SetMuted { video: film, muted: true }, "muted film true"),
        (MediaCommand::SetMuted { video: film, muted: false }, "muted film false"),
        (MediaCommand::Play(film), "play film"),
        (MediaCommand::Reveal(background), "reveal background center center instant"),
        (MediaCommand::Restore { x: 15.0, y: 300.0 }, "scrollTo 15 300 instant"),
        (MediaCommand::Pause(film + background + 1), ""),
    ] {
        let (name, first, second) = MediaCommand::decode(&command.encode()).expect("the renderer reads it");
        page.run(&format!("command('{name}', {first}, {second})")).unwrap_or_else(|error| panic!("{command:?}: {error}"));
        assert_eq!(page.run("crossbow.done()").unwrap(), effect, "{command:?} has exactly this one effect");
    }
    // The next report reads the paused film and the restored scroll.
    page.run("crossbow.run()").unwrap();
    let [after] = sent(&page).try_into().expect("one report");
    assert!(after.video(film).is_some_and(|video| video.paused) && after.scroll == (15.0, 300.0) && after.page == report.page);
}
