use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Records each answer it gives, and calls back into its list as CEF does.
#[derive(Clone, Default)]
struct Fake {
    answers: Rc<RefCell<Vec<bool>>>,
    list: Rc<RefCell<Option<Rc<RefCell<Pending<Fake>>>>>>,
}

impl Respond for Fake {
    fn respond(&self, allow: bool) {
        self.answers.borrow_mut().push(allow);
        if let Some(list) = self.list.borrow().as_ref() { list.borrow_mut().dismissed(u64::MAX); }
    }
}

#[test]
fn every_request_is_answered_exactly_once() {
    let pending = Rc::new(RefCell::new(Pending::default()));
    let fake = || {
        let fake = Fake::default();
        fake.list.replace(Some(pending.clone()));
        fake
    };
    let (a, b, c) = (fake(), fake(), fake());
    let first = pending.borrow_mut().hold(None, a.clone());
    let second = pending.borrow_mut().hold(Some(41), b.clone());
    let third = pending.borrow_mut().hold(Some(42), c.clone());
    assert!(first != second && second != third);
    assert!(answer(&pending, first, true), "answered with the list released, or the call back would panic");
    assert!(!answer(&pending, first, false), "a second answer is ignored");
    assert_eq!(*a.answers.borrow(), [true]);
    // CEF dismissed the prompt itself: no answer is due, and the app's id comes back.
    assert_eq!(pending.borrow_mut().dismissed(41), Some(second));
    assert!(!answer(&pending, second, true));
    assert!(b.answers.borrow().is_empty());
    assert_eq!(pending.borrow_mut().dismissed(41), None);
    assert_eq!(pending.borrow_mut().dismissed(42), Some(third));
    // After an answer, CEF's dismissal finds nothing left to report.
    let fourth = pending.borrow_mut().hold(Some(43), c.clone());
    assert!(answer(&pending, fourth, false));
    assert_eq!(pending.borrow_mut().dismissed(43), None);
    // A navigation or close denies what is left.
    let fifth = pending.borrow_mut().hold(None, c.clone());
    let sixth = pending.borrow_mut().hold(Some(44), c.clone());
    assert_eq!(deny_all(&pending), [fifth, sixth]);
    assert_eq!(*c.answers.borrow(), [false, false, false]);
    assert_eq!(pending.borrow().len(), 0);
    assert!(deny_all(&pending).is_empty());
    assert!(pending.borrow_mut().hold(None, Fake::default()) > sixth, "ids are never reused");
    for fake in [a, b, c] { fake.list.replace(None); }
}

#[test]
fn only_schemes_chromium_cannot_load_are_links_to_other_apps() {
    for url in ["zoommtg://zoom.us/join?confno=1", "msteams:/l/meetup-join/x", "mailto:someone@example.com", "tel:+15550100", "slack://open", "x-apple.systempreferences:com.apple.preference", "vbscript:x", "Web+Coffee:x", "crossbow://"] {
        assert!(is_external(url), "{url}");
    }
    for url in ["https://example.com/", "HTTP://example.com", "file:///tmp/a.html", "data:text/html,x", "blob:https://example.com/1", "about:blank", "javascript:void(0)", "chrome://settings", "devtools://x", "view-source:https://example.com", "", "no-scheme", "1x:y", "a b:c"] {
        assert!(!is_external(url), "{url}");
    }
}

#[test]
fn the_bits_are_cefs() {
    for (ours, cefs) in [
        (media::MICROPHONE, MediaAccessPermissionTypes::DEVICE_AUDIO_CAPTURE.get_raw()), (media::CAMERA, MediaAccessPermissionTypes::DEVICE_VIDEO_CAPTURE.get_raw()),
        (media::SCREEN_AUDIO, MediaAccessPermissionTypes::DESKTOP_AUDIO_CAPTURE.get_raw()), (media::SCREEN_VIDEO, MediaAccessPermissionTypes::DESKTOP_VIDEO_CAPTURE.get_raw()),
    ] {
        assert_eq!(ours, cefs);
    }
    for (ours, cefs) in [
        (prompt::AR_SESSION, PermissionRequestTypes::AR_SESSION), (prompt::CAMERA_PAN_TILT_ZOOM, PermissionRequestTypes::CAMERA_PAN_TILT_ZOOM),
        (prompt::CAMERA_STREAM, PermissionRequestTypes::CAMERA_STREAM), (prompt::CAPTURED_SURFACE_CONTROL, PermissionRequestTypes::CAPTURED_SURFACE_CONTROL),
        (prompt::CLIPBOARD, PermissionRequestTypes::CLIPBOARD), (prompt::TOP_LEVEL_STORAGE_ACCESS, PermissionRequestTypes::TOP_LEVEL_STORAGE_ACCESS),
        (prompt::DISK_QUOTA, PermissionRequestTypes::DISK_QUOTA), (prompt::LOCAL_FONTS, PermissionRequestTypes::LOCAL_FONTS),
        (prompt::GEOLOCATION, PermissionRequestTypes::GEOLOCATION), (prompt::HAND_TRACKING, PermissionRequestTypes::HAND_TRACKING),
        (prompt::IDENTITY_PROVIDER, PermissionRequestTypes::IDENTITY_PROVIDER), (prompt::IDLE_DETECTION, PermissionRequestTypes::IDLE_DETECTION),
        (prompt::MIC_STREAM, PermissionRequestTypes::MIC_STREAM), (prompt::MIDI_SYSEX, PermissionRequestTypes::MIDI_SYSEX),
        (prompt::MULTIPLE_DOWNLOADS, PermissionRequestTypes::MULTIPLE_DOWNLOADS), (prompt::NOTIFICATIONS, PermissionRequestTypes::NOTIFICATIONS),
        (prompt::KEYBOARD_LOCK, PermissionRequestTypes::KEYBOARD_LOCK), (prompt::POINTER_LOCK, PermissionRequestTypes::POINTER_LOCK),
        (prompt::PROTECTED_MEDIA_IDENTIFIER, PermissionRequestTypes::PROTECTED_MEDIA_IDENTIFIER), (prompt::REGISTER_PROTOCOL_HANDLER, PermissionRequestTypes::REGISTER_PROTOCOL_HANDLER),
        (prompt::STORAGE_ACCESS, PermissionRequestTypes::STORAGE_ACCESS), (prompt::VR_SESSION, PermissionRequestTypes::VR_SESSION),
        (prompt::WEB_APP_INSTALLATION, PermissionRequestTypes::WEB_APP_INSTALLATION), (prompt::WINDOW_MANAGEMENT, PermissionRequestTypes::WINDOW_MANAGEMENT),
        (prompt::FILE_SYSTEM_ACCESS, PermissionRequestTypes::FILE_SYSTEM_ACCESS), (prompt::LOCAL_NETWORK_ACCESS, PermissionRequestTypes::LOCAL_NETWORK_ACCESS_DEPRECATED),
        (prompt::LOCAL_NETWORK, PermissionRequestTypes::LOCAL_NETWORK), (prompt::LOOPBACK_NETWORK, PermissionRequestTypes::LOOPBACK_NETWORK),
        (prompt::SENSORS, PermissionRequestTypes::SENSORS),
    ] {
        assert_eq!(ours, cefs.get_raw());
    }
}

#[test]
fn a_link_prints_its_scheme_only() {
    let link = ExternalLink { url: "zoommtg://zoom.us/join?confno=1&pwd=fixture-token".into(), gesture: true, page: "https://meet.example/room?key=fixture-key".into(), frame: "https://meet.example/room?key=fixture-key".into(), load: 3 };
    let shown = format!("{link:?}");
    assert!(shown.contains("zoommtg") && !shown.contains("fixture-token") && !shown.contains("fixture-key") && !shown.contains("meet.example"), "{shown}");
}

/// CEF can't run in unit tests, so the handlers are checked in the source.
#[test]
fn the_view_holds_requests_and_never_lets_cef_open_another_app() {
    let view = include_str!("view.rs");
    let section = |start: &str| {
        let from = view.find(start).unwrap_or_else(|| panic!("{start}"));
        &view[from..from + view[from..].find("\n        }\n").unwrap()]
    };
    assert!(view.contains("fn permission_handler(&self) -> Option<PermissionHandler> { Some(self.permission.clone()) }"));
    let media = section("fn on_request_media_access_permission(");
    assert!(media.contains("Callback::Media { callback: callback.clone(), required: requested_permissions }") && media.contains("self.h.0.ask_permission(None, true,") && media.trim_end().ends_with('1'));
    let prompt = section("fn on_show_permission_prompt(");
    assert!(prompt.contains("Callback::Prompt { callback: callback.clone() }") && prompt.contains("self.h.0.ask_permission(Some(prompt_id), false,") && prompt.trim_end().ends_with('1'));
    assert!(section("fn on_dismiss_permission_prompt(").contains(".dismissed(prompt_id)"));
    // Every link to another app is cancelled and handed to the app, with or without a new window.
    let browse = section("fn on_before_browse(");
    assert!(browse.contains("permissions::is_external(&url)") && browse.contains("self.h.0.external_link(") && browse.contains("return 1;"));
    let popup = section("fn on_before_popup(");
    let external = popup.find("permissions::is_external(&url)").expect("checked first");
    assert!(external < popup.find("WebEvent::OpenUrl").unwrap() && popup.contains("self.h.0.external_link("));
    assert!(!view.contains("on_protocol_execution") && !view.contains("allow_os_execution"), "CEF's own hand-off stays off");
    // A new page, or closing, denies what the old one asked.
    let load = section("fn on_load_start(");
    assert!(load.contains("self.h.0.loads.set(") && load.contains("deny_permissions()"));
    assert!(section("fn on_before_close(").contains("deny_permissions()"));
    let lib = include_str!("lib.rs");
    assert!(!lib.contains("enable-media-stream") && !lib.contains("use-fake-ui-for-media-stream"), "no switch grants media to every page");
}
