//! The hook against fake CEF browsers, frames and requests, with rules that
//! block everything: what is never cancelled, what the rules are told, and
//! each profile's rules.

use super::*;
use crate::view::ResourceBuilder;
use cef::rc::{ConvertReturnValue, RcImpl};
use std::sync::Mutex;

fn userfree(text: &str) -> sys::cef_string_userfree_t {
    let text: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let raw = sys::cef_string_userfree_utf16_alloc();
        if !raw.is_null() && sys::cef_string_utf16_set(text.as_ptr(), text.len(), raw, 1) == 0 {
            sys::cef_string_userfree_utf16_free(raw);
            return std::ptr::null_mut();
        }
        raw
    }
}

struct FrameSpy {
    url: String,
    parent: Option<Frame>,
    main: bool,
}

fn frame_spy(raw: *mut sys::_cef_frame_t) -> &'static FrameSpy {
    &RcImpl::<sys::_cef_frame_t, FrameSpy>::get(raw).interface
}

unsafe extern "C" fn frame_url(raw: *mut sys::_cef_frame_t) -> sys::cef_string_userfree_t { userfree(&frame_spy(raw).url) }
unsafe extern "C" fn frame_parent(raw: *mut sys::_cef_frame_t) -> *mut sys::_cef_frame_t { frame_spy(raw).parent.clone().map_or(std::ptr::null_mut(), Into::into) }
unsafe extern "C" fn frame_is_main(raw: *mut sys::_cef_frame_t) -> i32 { i32::from(frame_spy(raw).main) }

pub(crate) fn frame(url: &str, parent: Option<&Frame>) -> Frame {
    let mut raw: sys::_cef_frame_t = unsafe { std::mem::zeroed() };
    raw.get_url = Some(frame_url);
    raw.get_parent = Some(frame_parent);
    raw.is_main = Some(frame_is_main);
    RcImpl::new(raw, FrameSpy { url: url.into(), parent: parent.cloned(), main: parent.is_none() }).cast::<sys::_cef_frame_t>().wrap_result()
}

struct BrowserSpy(Frame);

unsafe extern "C" fn main_frame(raw: *mut sys::_cef_browser_t) -> *mut sys::_cef_frame_t {
    RcImpl::<sys::_cef_browser_t, BrowserSpy>::get(raw).interface.0.clone().into()
}

pub(crate) fn browser(top: &Frame) -> Browser {
    let mut raw: sys::_cef_browser_t = unsafe { std::mem::zeroed() };
    raw.get_main_frame = Some(main_frame);
    RcImpl::new(raw, BrowserSpy(top.clone())).cast::<sys::_cef_browser_t>().wrap_result()
}

struct RequestSpy {
    url: String,
    kind: ResourceType,
}

fn request_spy(raw: *mut sys::_cef_request_t) -> &'static RequestSpy {
    &RcImpl::<sys::_cef_request_t, RequestSpy>::get(raw).interface
}

unsafe extern "C" fn request_url(raw: *mut sys::_cef_request_t) -> sys::cef_string_userfree_t { userfree(&request_spy(raw).url) }
unsafe extern "C" fn request_type(raw: *mut sys::_cef_request_t) -> sys::cef_resource_type_t { request_spy(raw).kind.into() }

pub(crate) fn request(url: &str, kind: ResourceType) -> Request {
    let mut raw: sys::_cef_request_t = unsafe { std::mem::zeroed() };
    raw.get_url = Some(request_url);
    raw.get_resource_type = Some(request_type);
    RcImpl::new(raw, RequestSpy { url: url.into(), kind }).cast::<sys::_cef_request_t>().wrap_result()
}

/// Rules that block every request they are asked about, and keep each one.
#[derive(Default)]
pub(crate) struct Everything {
    asked: Mutex<Vec<(String, ResourceKind, String, String)>>,
}

impl ContentRules for Everything {
    fn blocks(&self, request: &ResourceRequest<'_>) -> bool {
        self.asked.lock().unwrap().push((request.url.into(), request.kind, request.context.into(), request.top.into()));
        true
    }
}

impl Everything {
    fn asked(&self) -> Vec<(String, ResourceKind, String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

#[test]
fn a_top_level_page_and_a_request_without_a_browser_are_never_cancelled() {
    crate::load_cef_strings();
    let rules = Arc::new(Everything::default());
    let top = frame("https://news.example/", None);
    let browser = browser(&top);
    for kind in [ResourceType::MAIN_FRAME, ResourceType::NAVIGATION_PRELOAD_MAIN_FRAME] {
        assert!(!cancels(rules.as_ref(), Some(&browser), Some(&top), &request("https://news.example/next", kind)), "{kind:?}");
    }
    assert!(!cancels(rules.as_ref(), None, None, &request("https://ads.example/a.js", ResourceType::SCRIPT)), "a service worker's or a preview's fetch");
    assert!(!cancels(rules.as_ref(), None, Some(&top), &request("https://ads.example/a.js", ResourceType::SCRIPT)), "no browser, even with a frame");
    assert!(rules.asked().is_empty(), "the rules are never asked about them");
    assert!(cancels(rules.as_ref(), Some(&browser), Some(&top), &request("https://ads.example/a.js", ResourceType::SCRIPT)), "a page's script is");

    // Through the handler CEF calls, with its C entry points. Only what it
    // cancels counts for the page.
    let blocked = Arc::new(BlockedCount::default());
    blocked.start(3);
    let handler = ResourceBuilder::new(Default::default(), Default::default(), Some(rules.clone()), blocked.clone());
    let decide = |browser: Option<&Browser>, kind: ResourceType| {
        handler.on_before_resource_load(browser.cloned().as_mut(), Some(&mut top.clone()), Some(&mut request("https://ads.example/b.js", kind)), None)
    };
    assert_eq!(decide(Some(&browser), ResourceType::MAIN_FRAME), ReturnValue::CONTINUE);
    assert_eq!(decide(None, ResourceType::SCRIPT), ReturnValue::CONTINUE);
    assert_eq!(decide(Some(&browser), ResourceType::SCRIPT), ReturnValue::CANCEL);
    assert_eq!(blocked.get(3), 1, "one cancelled, two let through");
    let without = ResourceBuilder::new(Default::default(), Default::default(), None, blocked.clone());
    assert_eq!(without.on_before_resource_load(Some(&mut browser.clone()), Some(&mut top.clone()), Some(&mut request("https://ads.example/c.js", ResourceType::SCRIPT)), None), ReturnValue::CONTINUE, "a navigation's handler without rules");
}

#[test]
fn rules_see_the_requesting_document_and_the_tab_s_top_page() {
    crate::load_cef_strings();
    let top = frame("https://news.example/story", None);
    let ad = frame("https://ads.example/frame", Some(&top));
    let blank = frame("about:blank", Some(&ad));
    let browser = browser(&top);
    let rules = Everything::default();
    for (frame, url, kind) in [
        (Some(&top), "https://cdn.example/a.js", ResourceType::SCRIPT),
        (Some(&ad), "https://ads.example/b.png", ResourceType::IMAGE),
        (Some(&blank), "https://ads.example/c.js", ResourceType::SCRIPT),
        (Some(&ad), "https://ads.example/frame2", ResourceType::SUB_FRAME),
        (None, "https://cdn.example/d.js", ResourceType::XHR),
    ] {
        assert!(cancels(&rules, Some(&browser), frame, &request(url, kind)));
    }
    let top_url = "https://news.example/story".to_owned();
    let asked = |url: &str, kind, context: &str| (url.to_owned(), kind, context.to_owned(), top_url.clone());
    assert_eq!(rules.asked(), [
        asked("https://cdn.example/a.js", ResourceKind::Script, "https://news.example/story"),
        asked("https://ads.example/b.png", ResourceKind::Image, "https://ads.example/frame"),
        asked("https://ads.example/c.js", ResourceKind::Script, "https://ads.example/frame"),
        // A frame's own navigation was made by its parent.
        asked("https://ads.example/frame2", ResourceKind::Subdocument, "https://news.example/story"),
        asked("https://cdn.example/d.js", ResourceKind::Xhr, "https://news.example/story"),
    ]);
}

#[test]
fn each_cef_resource_type_has_a_kind_and_only_top_level_pages_have_none() {
    use ResourceKind::*;
    let cases = [
        (ResourceType::MAIN_FRAME, None),
        (ResourceType::NAVIGATION_PRELOAD_MAIN_FRAME, None),
        (ResourceType::SUB_FRAME, Some(Subdocument)),
        (ResourceType::NAVIGATION_PRELOAD_SUB_FRAME, Some(Subdocument)),
        (ResourceType::STYLESHEET, Some(Stylesheet)),
        (ResourceType::SCRIPT, Some(Script)),
        (ResourceType::WORKER, Some(Script)),
        (ResourceType::SHARED_WORKER, Some(Script)),
        (ResourceType::SERVICE_WORKER, Some(Script)),
        (ResourceType::IMAGE, Some(Image)),
        (ResourceType::FAVICON, Some(Image)),
        (ResourceType::FONT_RESOURCE, Some(Font)),
        (ResourceType::OBJECT, Some(Object)),
        (ResourceType::PLUGIN_RESOURCE, Some(Object)),
        (ResourceType::MEDIA, Some(Media)),
        (ResourceType::XHR, Some(Xhr)),
        (ResourceType::PING, Some(Ping)),
        (ResourceType::CSP_REPORT, Some(Ping)),
        (ResourceType::PREFETCH, Some(Other)),
        (ResourceType::SUB_RESOURCE, Some(Other)),
    ];
    for (resource_type, expected) in cases {
        assert_eq!(kind(resource_type), expected, "{resource_type:?}");
    }
}

#[test]
fn rules_are_kept_per_profile_and_replaced_or_cleared() {
    let first: Arc<dyn ContentRules> = Arc::new(Everything::default());
    let second: Arc<dyn ContentRules> = Arc::new(Everything::default());
    set_content_rules("fixture-rules-a", Some(first.clone()));
    assert!(rules_for("fixture-rules-a").is_some_and(|rules| Arc::ptr_eq(&rules, &first)));
    assert!(!has_content_rules("fixture-rules-b"), "another profile has none");
    set_content_rules("fixture-rules-a", Some(second.clone()));
    assert!(rules_for("fixture-rules-a").is_some_and(|rules| Arc::ptr_eq(&rules, &second)), "replaced");
    set_content_rules("fixture-rules-a", None);
    assert!(!has_content_rules("fixture-rules-a"), "cleared");
}

#[test]
fn the_blocked_count_belongs_to_one_page_and_counts_each_address_once() {
    let (a, b) = ("https://ads.example/a.js", "https://ads.example/b.png");
    let count = BlockedCount::default();
    assert_eq!(count.get(0), 0);
    count.start(1);
    for url in [a, a, b] { count.add(url); }
    assert_eq!(count.get(1), 2, "the same address cancelled twice is one; two addresses are two");
    assert_eq!(count.get(2), 0, "not the next page's yet");
    count.start(2);
    assert_eq!((count.get(2), count.get(1)), (0, 0), "a new page starts at 0 and the old count is gone");
    count.add(a);
    count.add(a);
    assert_eq!(count.get(2), 1, "an address the page before counted counts once again");

    // Past the addresses one page holds, every cancel counts, repeats too.
    count.start(3);
    for n in 0..SEEN { count.add(&format!("https://ads.example/{n}")); }
    count.add(a);
    count.add(a);
    assert_eq!(count.get(3), SEEN as u32 + 2);
    count.start(4);
    count.add(a);
    count.add(a);
    assert_eq!(count.get(4), 1, "the next page has every slot again");

    // Cancels on many IO threads at once lose none and count none twice.
    let count = Arc::new(BlockedCount::default());
    count.start(7);
    let threads: Vec<_> = (0..8).map(|thread| {
        let count = count.clone();
        std::thread::spawn(move || for _ in 0..2 {
            for n in 0..64 { count.add(&format!("https://ads.example/shared/{n}")); }
            for n in 0..40 { count.add(&format!("https://ads.example/{thread}/{n}")); }
        })
    }).collect();
    for thread in threads { thread.join().unwrap(); }
    assert_eq!(count.get(7), 64 + 8 * 40);
}

#[test]
fn a_request_decided_as_the_next_page_starts_is_missed_or_counted_once_on_it() {
    let (a, b) = ("https://ads.example/a.js", "https://ads.example/b.png");
    let count = BlockedCount::default();
    count.start(1);
    // The IO thread read page 1, then the UI thread started page 2 before it counted.
    count.start(2);
    count.add_on(1, a);
    assert_eq!(count.get(2), 0, "missed");
    count.add(a);
    assert_eq!(count.get(2), 1, "the new page's own request for it counts");
    // The page before's request decided once page 2 is current counts on it, once.
    count.add(b);
    count.add(b);
    assert_eq!(count.get(2), 2);

    // A late decision for page 1 never frees a slot page 2 holds.
    let count = BlockedCount::default();
    count.start(2);
    let urls: Vec<String> = (0..SEEN).map(|n| format!("https://ads.example/{n}")).collect();
    for url in &urls { count.add(url); }
    count.add_on(1, a);
    for url in &urls { count.add(url); }
    assert_eq!(count.get(2), SEEN as u32, "each of page 2's addresses still once");
}
