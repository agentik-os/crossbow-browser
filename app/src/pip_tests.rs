use super::*;
use crossbow_engine::CssRect;

const PAGE: u64 = 41;

fn video(id: u32, paused: bool, muted: bool, user_started: bool, y: f64) -> Video {
    Video { id, paused, ended: false, muted, volume: 1.0, user_started, rect: CssRect { x: 0.0, y, width: 640.0, height: 360.0 } }
}

fn report(videos: Vec<Video>) -> MediaReport {
    MediaReport { page: PAGE, videos, device_pixel_ratio: 2.0, viewport: (1280.0, 800.0), scroll: (0.0, 1500.0) }
}

/// One tab per pass, built from owned parts the test changes between passes.
#[derive(Clone)]
struct Tab {
    id: &'static str,
    visible: bool,
    loaded: bool,
    fullscreen: bool,
    muted: bool,
    automatic: bool,
    navigations: u64,
    report: Option<MediaReport>,
    at: Instant,
}

impl Tab {
    fn new(id: &'static str, report: Option<MediaReport>, at: Instant) -> Tab {
        Tab { id, visible: false, loaded: true, fullscreen: false, muted: false, automatic: true, navigations: 1, report, at }
    }

    fn state(&self) -> TabState<'_> {
        TabState {
            id: self.id, visible: self.visible, loaded: self.loaded, fullscreen: self.fullscreen, muted: self.muted,
            automatic: self.automatic, navigations: self.navigations, media: self.report.as_ref().map(|report| (report, self.at)),
        }
    }
}

fn pass(pip: &mut Pip, tabs: &[Tab], now: Instant) -> Vec<Effect> {
    let states: Vec<TabState> = tabs.iter().map(Tab::state).collect();
    pip.update(&states, now)
}

fn control(pip: &mut Pip, control: Control, tabs: &[Tab], now: Instant) -> Vec<Effect> {
    let states: Vec<TabState> = tabs.iter().map(Tab::state).collect();
    pip.control(control, &states, now)
}

fn commands(effects: &[Effect]) -> Vec<MediaCommand> {
    effects.iter().filter_map(|effect| match effect { Effect::Command { command, .. } => Some(command.clone()), _ => None }).collect()
}

fn rate(tab: &str, fps: i32) -> Effect {
    Effect::FrameRate { tab: tab.into(), fps }
}

/// Tab "a" is in view with `videos`, then the person switches to "b".
fn leave(videos: Vec<Video>, now: Instant, edit: impl FnOnce(&mut Tab)) -> (Pip, Vec<Tab>, Vec<Effect>) {
    let mut pip = Pip::default();
    let mut tabs = vec![Tab::new("a", Some(report(videos)), now), Tab::new("b", None, now)];
    edit(&mut tabs[0]);
    tabs[0].visible = true;
    assert_eq!(pass(&mut pip, &tabs, now), [], "nothing while the tab is in view");
    tabs[0].visible = false;
    tabs[1].visible = true;
    let effects = pass(&mut pip, &tabs, now);
    (pip, tabs, effects)
}

#[test]
fn leaving_a_tab_whose_video_plays_audibly_opens_the_player_and_coming_back_closes_it() {
    let now = Instant::now();
    let (mut pip, mut tabs, effects) = leave(vec![video(7, false, false, false, 100.0)], now, |_| {});
    let player = pip.player.as_ref().expect("a player");
    assert_eq!((player.tab.as_str(), player.video, player.left), ("a", 7, true));
    assert_eq!(effects, [rate("a", PLAYER_FPS)], "the video is in view: no scrolling, and the tab paints at 30 fps");
    assert_eq!(pip.painting(), Some("a"), "the tab keeps painting while only the player shows it");
    assert_eq!(pass(&mut pip, &tabs, now + Duration::from_secs(1)), []);
    // Back to the tab: the player closes, the video plays on in the tab.
    tabs[0].visible = true;
    tabs[1].visible = false;
    assert_eq!(pass(&mut pip, &tabs, now + Duration::from_secs(2)), [rate("a", FULL_FPS)], "no pause");
    assert!(pip.player.is_none());

    // Never for a muted autoplay, a silent or paused video, a tab muted by
    // Crossbow, a profile that turned it off, an unloaded or full-screen
    // tab, or a report gone stale.
    let background = video(1, false, true, false, 100.0);
    let cases: [(&str, Vec<Video>, fn(&mut Tab)); 7] = [
        ("muted autoplay", vec![background.clone()], |_| {}),
        ("paused", vec![video(2, true, false, true, 100.0)], |_| {}),
        ("muted by Crossbow", vec![video(3, false, false, false, 100.0)], |tab| tab.muted = true),
        ("turned off", vec![video(4, false, false, false, 100.0)], |tab| tab.automatic = false),
        ("unloaded", vec![video(5, false, false, false, 100.0)], |tab| tab.loaded = false),
        ("full screen", vec![video(6, false, false, false, 100.0)], |tab| tab.fullscreen = true),
        ("stale", vec![video(8, false, false, false, 100.0)], |tab| tab.at -= STALE_AFTER + Duration::from_millis(1)),
    ];
    for (case, videos, edit) in cases {
        let (pip, _, effects) = leave(videos, now, edit);
        assert!(pip.player.is_none() && effects.is_empty(), "{case}");
    }
    // A muted video the person started still counts, even in a muted tab.
    let (pip, _, _) = leave(vec![background, video(9, false, true, true, 100.0)], now, |tab| tab.muted = true);
    assert_eq!(pip.player.map(|player| player.video), Some(9));
}

#[test]
fn picture_in_picture_by_name_opens_one_player_and_a_new_one_replaces_it() {
    let now = Instant::now();
    let mut tabs = vec![Tab::new("a", Some(report(vec![video(1, true, false, false, 100.0)])), now), Tab::new("b", Some(report(vec![video(2, false, false, false, 100.0)])), now), Tab::new("c", None, now)];
    tabs[0].visible = true;
    let mut pip = Pip::default();
    pass(&mut pip, &tabs, now);
    assert!(!tabs[2].state().has_video() && tabs[0].state().has_video(), "offered only for a tab with a video");
    assert_eq!(pip.open_manual(&tabs[2].state(), now), []);
    assert!(pip.player.is_none());
    // A paused video, in view in the main window.
    assert_eq!(pip.open_manual(&tabs[0].state(), now), [], "in view: full frame rate, nothing scrolled");
    assert_eq!(pip.player.as_ref().map(|player| (player.tab.as_str(), player.left)), Some(("a", false)));
    assert_eq!(pass(&mut pip, &tabs, now), [], "stays open while its tab is in view");
    // Leaving and coming back closes it, as for an automatic one.
    tabs[0].visible = false;
    tabs[2].visible = true;
    assert_eq!(pass(&mut pip, &tabs, now), [rate("a", PLAYER_FPS)]);
    // One player at a time: asking for another replaces it; the first tab's video plays on.
    let effects = pip.open_manual(&tabs[1].state(), now);
    assert_eq!(effects, [rate("a", FULL_FPS), rate("b", PLAYER_FPS)]);
    assert_eq!(pip.painting(), Some("b"));
    // Leaving a tab whose video plays replaces it too.
    let effects = pass(&mut pip, &tabs, now);
    assert_eq!(effects, []);
    tabs[2].visible = false;
    tabs[1].visible = true;
    pass(&mut pip, &tabs, now);
    assert!(pip.player.is_none(), "b came back into view");
    tabs[1].visible = false;
    tabs[0].visible = true;
    let effects = pass(&mut pip, &tabs, now);
    assert_eq!(effects, [rate("b", PLAYER_FPS)]);
    // Unloaded or full-screen tabs get none.
    for edit in [(|tab: &mut Tab| tab.loaded = false) as fn(&mut Tab), |tab| tab.fullscreen = true] {
        let mut tab = tabs[1].clone();
        edit(&mut tab);
        assert_eq!(Pip::default().open_manual(&tab.state(), now), []);
    }
}

#[test]
fn each_control_sends_exactly_one_command_and_close_pauses() {
    let now = Instant::now();
    let playing = |muted| leave(vec![video(7, false, muted, true, 100.0)], now, |_| {});
    for (control, expected) in [
        (Control::PlayPause, MediaCommand::Pause(7)),
        (Control::Back, MediaCommand::Seek { video: 7, seconds: -10.0 }),
        (Control::Forward, MediaCommand::Seek { video: 7, seconds: 10.0 }),
        (Control::Mute, MediaCommand::SetMuted { video: 7, muted: true }),
        (Control::Close, MediaCommand::Pause(7)),
    ] {
        let (mut pip, tabs, _) = playing(false);
        let effects = control_once(&mut pip, control, &tabs, now);
        assert_eq!(commands(&effects), [expected], "{control:?}");
        assert_eq!(pip.player.is_none(), control == Control::Close, "{control:?}");
    }
    // A paused video plays; a muted one unmutes.
    let (mut pip, mut tabs, _) = playing(false);
    let shown = &mut tabs[0].report.as_mut().unwrap().videos[0];
    (shown.paused, shown.muted) = (true, true);
    assert_eq!(commands(&control_once(&mut pip, Control::PlayPause, &tabs, now)), [MediaCommand::Play(7)]);
    assert_eq!(commands(&control_once(&mut pip, Control::Mute, &tabs, now)), [MediaCommand::SetMuted { video: 7, muted: false }]);
    // Back to Tab shows the tab and closes the player; the video plays on.
    let effects = control_once(&mut pip, Control::BackToTab, &tabs, now);
    assert_eq!(effects, [Effect::ShowTab("a".into()), rate("a", FULL_FPS)]);
    assert!(pip.player.is_none());
    // Once the video is gone, the controls send nothing.
    let (mut pip, mut tabs, _) = playing(false);
    tabs[0].report = None;
    for control in [Control::PlayPause, Control::Back, Control::Forward, Control::Mute] {
        assert_eq!(commands(&control_once(&mut pip, control, &tabs, now)), [], "{control:?}");
    }
    assert_eq!(commands(&control_once(&mut pip, Control::Close, &tabs, now)), []);
    assert!(pip.player.is_none());
    assert_eq!(control_once(&mut Pip::default(), Control::Close, &tabs, now), [], "no player");
}

fn control_once(pip: &mut Pip, which: Control, tabs: &[Tab], now: Instant) -> Vec<Effect> {
    control(pip, which, tabs, now)
}

#[test]
fn the_player_closes_with_its_tab_or_full_screen_and_says_video_ended_for_three_seconds() {
    let now = Instant::now();
    let playing = || leave(vec![video(7, false, false, false, 100.0)], now, |_| {});
    // Closed, unloaded or full screen: at once.
    let (mut pip, mut tabs, _) = playing();
    tabs.remove(0);
    assert_eq!(pass(&mut pip, &tabs, now), [rate("a", FULL_FPS)]);
    assert!(pip.player.is_none(), "tab closed");
    for edit in [(|tab: &mut Tab| tab.loaded = false) as fn(&mut Tab), |tab| tab.fullscreen = true] {
        let (mut pip, mut tabs, _) = playing();
        edit(&mut tabs[0]);
        pass(&mut pip, &tabs, now);
        assert!(pip.player.is_none());
    }
    // Ended, removed, navigated away or silent: "Video ended", then closed after 3 s.
    let ends: [fn(&mut Tab); 5] = [
        |tab| tab.report.as_mut().unwrap().videos[0].ended = true,
        |tab| tab.report.as_mut().unwrap().videos.clear(),
        |tab| tab.report = None,
        |tab| tab.report.as_mut().unwrap().page = PAGE + 1,
        |tab| tab.at -= STALE_AFTER + Duration::from_millis(1),
    ];
    for (case, end) in ends.into_iter().enumerate() {
        let (mut pip, mut tabs, _) = playing();
        let player = pip.player.clone().unwrap();
        // Later than STALE_AFTER after opening, with a report just in.
        let at = now + Duration::from_secs(4);
        tabs[0].at = at;
        assert!(matches!(player.playing(&tabs[0].state(), at), Playing::Video(_, video) if video.id == 7), "case {case}");
        end(&mut tabs[0]);
        assert_eq!(player.playing(&tabs[0].state(), at), Playing::Ended, "case {case}");
        assert_eq!(pass(&mut pip, &tabs, at), [], "case {case}");
        assert!(pip.player.is_some(), "case {case}: says Video ended first");
        pass(&mut pip, &tabs, at + ENDED_FOR - Duration::from_millis(1));
        assert!(pip.player.is_some(), "case {case}");
        assert_eq!(pass(&mut pip, &tabs, at + ENDED_FOR), [rate("a", FULL_FPS)], "case {case}");
        assert!(pip.player.is_none(), "case {case}");
    }
    // A video that plays again before then keeps its player.
    let (mut pip, mut tabs, _) = playing();
    tabs[0].report.as_mut().unwrap().videos[0].ended = true;
    pass(&mut pip, &tabs, now);
    tabs[0].report.as_mut().unwrap().videos[0].ended = false;
    for seconds in [2, 4, 6] {
        tabs[0].at = now + Duration::from_secs(seconds);
        pass(&mut pip, &tabs, tabs[0].at);
        assert!(pip.player.as_ref().is_some_and(|player| player.ended.is_none()), "{seconds} s");
    }
}

#[test]
fn a_video_out_of_view_is_scrolled_into_view_and_the_page_comes_back_where_it_was() {
    let now = Instant::now();
    // Below the viewport: 900 + 360 > 800.
    let low = || leave(vec![video(7, false, false, false, 900.0)], now, |_| {});
    let back = |tabs: &mut Vec<Tab>| { tabs[0].visible = true; tabs[1].visible = false; };
    let restore = MediaCommand::Restore { x: 0.0, y: 1500.0 };

    let (mut pip, mut tabs, effects) = low();
    assert_eq!(effects, [Effect::Command { tab: "a".into(), command: MediaCommand::Reveal(7) }, rate("a", PLAYER_FPS)]);
    // Coming back: the scroll position the page had before, once.
    back(&mut tabs);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), [restore.clone()]);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), []);

    // Back to Tab: as soon as the tab is in view.
    let (mut pip, mut tabs, _) = low();
    control(&mut pip, Control::BackToTab, &tabs, now);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), [], "not yet in view");
    back(&mut tabs);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), [restore.clone()]);

    // Closed while away: when the person returns.
    let (mut pip, mut tabs, _) = low();
    control(&mut pip, Control::Close, &tabs, now);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), []);
    back(&mut tabs);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), [restore.clone()]);

    // The page changed meanwhile: nothing is said or done.
    let changes: [fn(&mut Tab); 4] = [
        |tab| tab.navigations += 1,
        |tab| tab.report.as_mut().unwrap().page = PAGE + 1,
        |tab| tab.report.as_mut().unwrap().videos.clear(),
        |tab| tab.loaded = false,
    ];
    for (case, change) in changes.into_iter().enumerate() {
        let (mut pip, mut tabs, _) = low();
        control(&mut pip, Control::Close, &tabs, now);
        change(&mut tabs[0]);
        back(&mut tabs);
        assert_eq!(pass(&mut pip, &tabs, now), [], "case {case}");
        assert_eq!(pass(&mut pip, &tabs, now), [], "case {case}: dropped");
    }
    // A closed tab drops it too.
    let (mut pip, mut tabs, _) = low();
    control(&mut pip, Control::Close, &tabs, now);
    tabs.remove(0);
    pass(&mut pip, &tabs, now);
    assert!(pip.restores.is_empty());

    // Asked for in view and closed without leaving: the page stays where the person now has it.
    let mut tabs = vec![Tab::new("a", Some(report(vec![video(7, false, false, false, 900.0)])), now)];
    tabs[0].visible = true;
    let mut pip = Pip::default();
    pass(&mut pip, &tabs, now);
    assert_eq!(commands(&pip.open_manual(&tabs[0].state(), now)), [MediaCommand::Reveal(7)]);
    control(&mut pip, Control::Close, &tabs, now);
    assert_eq!(commands(&pass(&mut pip, &tabs, now)), []);
}

#[test]
fn the_player_starts_bottom_right_keeps_16_9_and_stays_on_screen_after_a_display_change() {
    let laptop = Rect::from_min_max(pos2(0.0, 25.0), pos2(1512.0, 920.0));
    let external = Rect::from_min_max(pos2(1512.0, -200.0), pos2(4072.0, 1240.0));
    let first = default_frame(laptop);
    assert_eq!(first, Rect::from_min_size(pos2(1512.0 - 320.0 - 16.0, 920.0 - 180.0 - 16.0), vec2(320.0, 180.0)));
    assert_eq!(place(None, laptop, &[laptop, external]), first);

    // Sizes keep 16:9, no smaller than the minimum and no larger than the room.
    assert_eq!(fit(100.0, vec2(2000.0, 2000.0)), MIN_SIZE);
    assert_eq!(fit(800.0, vec2(2000.0, 2000.0)), vec2(800.0, 450.0));
    assert_eq!(fit(800.0, vec2(600.0, 2000.0)), vec2(600.0, 337.5));
    assert_eq!(fit(800.0, vec2(2000.0, 270.0)), vec2(480.0, 270.0));
    assert_eq!(fit(f32::NAN, vec2(2000.0, 2000.0)), DEFAULT_SIZE);

    // A saved place on the external display, which is then unplugged.
    let saved = Rect::from_min_size(pos2(3500.0, 900.0), vec2(480.0, 270.0));
    assert_eq!(place(Some(saved), laptop, &[laptop, external]), saved, "kept while it fits");
    assert!(on_screen(saved, &[laptop, external]));
    assert!(!on_screen(saved, &[laptop]));
    assert_eq!(place(Some(saved), laptop, &[laptop]), first, "back to the main window's screen");
    // Partly off an edge, or larger than a smaller display: moved and shrunk onto it.
    let across = Rect::from_min_size(pos2(1400.0, 800.0), vec2(480.0, 270.0));
    let placed = place(Some(across), laptop, &[laptop]);
    assert!(on_screen(placed, &[laptop]), "{placed:?}");
    assert_eq!(placed, Rect::from_min_size(pos2(1512.0 - 480.0, 920.0 - 270.0), vec2(480.0, 270.0)));
    let small = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 400.0));
    let huge = Rect::from_min_size(pos2(-50.0, -50.0), vec2(1600.0, 900.0));
    let placed = place(Some(huge), small, &[small]);
    assert_eq!(placed.min, pos2(0.0, 0.0));
    assert!((placed.width() - 400.0 * 16.0 / 9.0).abs() < 1e-3, "{placed:?}");
    assert!((placed.width() / placed.height() - 16.0 / 9.0).abs() < 1e-4 && on_screen(placed, &[small]));
    let broken = Rect::from_min_size(pos2(f32::NAN, 0.0), vec2(320.0, 180.0));
    assert_eq!(place(Some(broken), laptop, &[laptop]), first);
}

#[test]
fn choices_and_the_players_place_survive_reopening_and_a_failed_save_keeps_the_old_one() {
    let fixture = crate::shortcuts::tests::Fixture::new();
    let store = fixture.store();
    let mut prefs = PipPrefs::default();
    assert!(prefs.automatic(&store, "work") && prefs.automatic(&store, "home"), "on by default, as in Arc");
    assert!(prefs.player_on_top(&store).enabled(), "the player floats by default");
    assert!(!prefs.little_on_top(&store).enabled());
    prefs.automatic_switch(&store, "work").choose(&store, false);
    prefs.player_on_top(&store).choose(&store, false);
    prefs.little_on_top(&store).choose(&store, true);
    let frame = Rect::from_min_size(pos2(1100.5, 600.0), vec2(400.0, 225.0));
    save_frame(&store, frame).unwrap();
    drop(store);

    let store = fixture.store();
    let mut prefs = PipPrefs::default();
    assert!(!prefs.automatic(&store, "work"), "per profile");
    assert!(prefs.automatic(&store, "home"));
    assert!(!prefs.player_on_top(&store).enabled());
    assert!(prefs.little_on_top(&store).enabled());
    assert_eq!(load_frame(&store), Some(frame));
    for raw in ["", "1 2 3", "1 2 3 4 5", "a b c d", "0 0 0 0", "NaN 0 320 180"] {
        store.kv_set(FRAME_KEY, raw).unwrap();
        assert_eq!(load_frame(&store), None, "{raw:?}");
    }
    drop(store);

    // A failed save keeps the previous choice and says so.
    let store = fixture.read_only();
    let mut prefs = PipPrefs::default();
    let automatic = prefs.automatic_switch(&store, "home");
    automatic.choose(&store, false);
    assert!(automatic.enabled() && automatic.failure.as_ref().is_some_and(|failure| !failure.wanted));
    let on_top = prefs.player_on_top(&store);
    on_top.choose(&store, true);
    assert!(!on_top.enabled() && on_top.failure.is_some());
    assert!(save_frame(&store, frame).is_err());
}
