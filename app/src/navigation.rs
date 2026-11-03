//! Ownership of global web navigation, separate from tab-owned commands.

use super::Action;
use crossbow_engine::WebView;
use std::collections::HashMap;

pub(super) trait NavigationView {
    fn go_back(&self);
    fn go_forward(&self);
    fn reload(&self);
    fn reload_ignore_cache(&self);
    fn stop(&self);
    fn load_url(&self, url: &str);
    fn editable_focus(&self) -> bool;
}

impl NavigationView for WebView {
    fn go_back(&self) { self.go_back(); }
    fn go_forward(&self) { self.go_forward(); }
    fn reload(&self) { self.reload(); }
    fn reload_ignore_cache(&self) { self.reload_ignore_cache(); }
    fn stop(&self) { self.stop(); }
    fn load_url(&self, url: &str) { self.load_url(url); }
    fn editable_focus(&self) -> bool { self.nav().editable_focus }
}

pub(super) enum NavigationOwner<V> {
    Peek(V),
    Tab { id: String, view: Option<V> },
}

pub(super) fn select_navigation_owner<V>(
    peek: Option<V>,
    focused_tab: impl FnOnce() -> Option<(String, Option<V>)>,
) -> Option<NavigationOwner<V>> {
    // A closing Peek still owns navigation until its view is removed.
    peek.map(NavigationOwner::Peek)
        .or_else(|| focused_tab().map(|(id, view)| NavigationOwner::Tab { id, view }))
}

pub(super) struct NavigationBatch {
    peek: bool,
    cancelled: bool,
}

impl NavigationBatch {
    pub(super) fn new(peek_closing: Option<bool>) -> Self {
        Self { peek: peek_closing.is_some(), cancelled: peek_closing == Some(true) }
    }

    pub(super) fn observe(&mut self, action: &Action) {
        // Replacement/promotion happens in this batch, not while drawing. Even
        // a failed replacement cancels later navigation rather than rebinding it.
        if matches!(action, Action::OpenInPeek(_) | Action::ClosePeek | Action::PeekToTab) {
            self.cancelled = true;
        }
    }

    pub(super) fn owner<V>(
        &self,
        peek_closing: Option<bool>,
        resolve: impl FnOnce() -> Option<NavigationOwner<V>>,
    ) -> Option<NavigationOwner<V>> {
        if self.cancelled || peek_closing == Some(true) || self.peek != peek_closing.is_some() {
            return None;
        }
        // Tab selection stays lazy so ActivateTab followed by Reload still retries
        // that tab. A removed Peek must not evaluate this fallback at all.
        resolve()
    }
}

impl<V: NavigationView> NavigationOwner<V> {
    pub(super) fn editable_focus(&self) -> bool {
        match self {
            Self::Peek(view) => view.editable_focus(),
            Self::Tab { view, .. } => view.as_ref().is_some_and(NavigationView::editable_focus),
        }
    }

    pub(super) fn apply(self, action: &Action, errors: &mut HashMap<String, (String, i32, String)>) {
        let (view, retry) = match self {
            Self::Peek(view) => (Some(view), None),
            Self::Tab { id, view } => {
                let retry = if matches!(action, Action::Reload) { errors.remove(&id) } else { None };
                (view, retry)
            }
        };
        let Some(view) = view else { return };
        match action {
            Action::Back => view.go_back(),
            Action::Forward => view.go_forward(),
            Action::Reload => match retry {
                Some((url, _, _)) => view.load_url(&url),
                None => view.reload(),
            },
            Action::HardReload => view.reload_ignore_cache(),
            Action::Stop => view.stop(),
            _ => unreachable!("only global navigation actions have a navigation owner"),
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use cef::{Browser, CefString, Frame, ImplBrowser, ImplFrame, sys};
    use cef::rc::{ConvertReturnValue, RcImpl};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Debug, PartialEq, Eq)]
    struct Call {
        receiver: &'static str,
        action: &'static str,
        url: Option<String>,
    }

    type Calls = Rc<RefCell<Vec<Call>>>;

    #[derive(Clone)]
    struct FrameSpy {
        receiver: &'static str,
        calls: Calls,
    }

    struct BrowserSpy {
        receiver: &'static str,
        calls: Calls,
        frame: Frame,
    }

    unsafe extern "C" fn record_load_url(frame: *mut sys::_cef_frame_t, url: *const sys::cef_string_t) {
        let spy = &RcImpl::<sys::_cef_frame_t, FrameSpy>::get(frame).interface;
        let url = unsafe { &*url };
        let text = String::from_utf16(unsafe { std::slice::from_raw_parts(url.str_, url.length) }).unwrap();
        spy.calls.borrow_mut().push(Call { receiver: spy.receiver, action: "load-url", url: Some(text) });
    }

    unsafe extern "C" fn main_frame(browser: *mut sys::_cef_browser_t) -> *mut sys::_cef_frame_t {
        RcImpl::<sys::_cef_browser_t, BrowserSpy>::get(browser).interface.frame.clone().into()
    }

    macro_rules! record_navigation {
        ($( $callback:ident => $action:literal ),+ $(,)?) => {
            $(unsafe extern "C" fn $callback(browser: *mut sys::_cef_browser_t) {
                let spy = &RcImpl::<sys::_cef_browser_t, BrowserSpy>::get(browser).interface;
                spy.calls.borrow_mut().push(Call { receiver: spy.receiver, action: $action, url: None });
            })+
        };
    }

    record_navigation!(
        record_back => "back", record_forward => "forward", record_reload => "reload",
        record_hard_reload => "hard-reload", record_stop => "stop",
    );

    #[derive(Clone)]
    struct ViewSpy {
        browser: Browser,
        editable: bool,
    }

    impl NavigationView for ViewSpy {
        fn go_back(&self) { self.browser.go_back(); }
        fn go_forward(&self) { self.browser.go_forward(); }
        fn reload(&self) { self.browser.reload(); }
        fn reload_ignore_cache(&self) { self.browser.reload_ignore_cache(); }
        fn stop(&self) { self.browser.stop_load(); }
        fn load_url(&self, url: &str) { self.browser.main_frame().unwrap().load_url(Some(&CefString::from(url))); }
        fn editable_focus(&self) -> bool { self.editable }
    }

    fn view(receiver: &'static str, calls: &Calls, editable: bool) -> ViewSpy {
        static LOAD_CEF: std::sync::Once = std::sync::Once::new();
        LOAD_CEF.call_once(|| {
            use std::os::unix::ffi::OsStrExt;
            let path = sys::get_cef_dir().expect("CEF_PATH must identify the pinned CEF distribution")
                .join(sys::FRAMEWORK_PATH).canonicalize().expect("CEF framework is required");
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // String utilities only: no CEF initialization, browser or window.
            assert_eq!(unsafe { sys::cef_load_library(path.as_ptr()) }, 1);
        });
        let mut raw_frame: sys::_cef_frame_t = unsafe { std::mem::zeroed() };
        raw_frame.load_url = Some(record_load_url);
        let frame = RcImpl::new(raw_frame, FrameSpy { receiver, calls: calls.clone() })
            .cast::<sys::_cef_frame_t>().wrap_result();
        let mut raw_browser: sys::_cef_browser_t = unsafe { std::mem::zeroed() };
        raw_browser.get_main_frame = Some(main_frame);
        raw_browser.go_back = Some(record_back);
        raw_browser.go_forward = Some(record_forward);
        raw_browser.reload = Some(record_reload);
        raw_browser.reload_ignore_cache = Some(record_hard_reload);
        raw_browser.stop_load = Some(record_stop);
        let browser = RcImpl::new(raw_browser, BrowserSpy { receiver, calls: calls.clone(), frame })
            .cast::<sys::_cef_browser_t>().wrap_result();
        ViewSpy { browser, editable }
    }

    fn errors() -> HashMap<String, (String, i32, String)> {
        HashMap::from([
            ("parent".into(), ("http://127.0.0.1:18791/parent-failed".into(), -102, "net::ERR_CONNECTION_REFUSED".into())),
            ("other".into(), ("http://127.0.0.1:18791/other-failed".into(), -105, "net::ERR_NAME_NOT_RESOLVED".into())),
        ])
    }

    fn actions() -> [(Action, &'static str); 5] {
        [(Action::Back, "back"), (Action::Forward, "forward"), (Action::Reload, "reload"),
            (Action::HardReload, "hard-reload"), (Action::Stop, "stop")]
    }

    fn assert_peek_receiver(action: Action, expected: &'static str) {
        let calls = Calls::default();
        let parent = view("parent", &calls, false);
        let peek = view("peek", &calls, false);
        let mut errors = HashMap::new();
        select_navigation_owner(Some(peek), || Some(("parent".into(), Some(parent))))
            .unwrap().apply(&action, &mut errors);
        assert_eq!(*calls.borrow(), [Call { receiver: "peek", action: expected, url: None }]);
    }

    #[test]
    fn peek_back_reaches_only_peek_cef_browser() { assert_peek_receiver(Action::Back, "back"); }

    #[test]
    fn peek_forward_reaches_only_peek_cef_browser() { assert_peek_receiver(Action::Forward, "forward"); }

    #[test]
    fn peek_reload_reaches_only_peek_cef_browser() { assert_peek_receiver(Action::Reload, "reload"); }

    #[test]
    fn peek_hard_reload_reaches_only_peek_cef_browser() { assert_peek_receiver(Action::HardReload, "hard-reload"); }

    #[test]
    fn peek_stop_reaches_only_peek_cef_browser() { assert_peek_receiver(Action::Stop, "stop"); }

    #[test]
    fn peek_reload_does_not_consume_parent_failed_url() {
        let calls = Calls::default();
        let parent = view("parent", &calls, false);
        let peek = view("peek", &calls, false);
        let mut errors = errors();
        let before = errors.clone();
        select_navigation_owner(Some(peek), || Some(("parent".into(), Some(parent))))
            .unwrap().apply(&Action::Reload, &mut errors);
        assert_eq!(errors, before);
        assert_eq!(*calls.borrow(), [Call { receiver: "peek", action: "reload", url: None }]);
    }

    #[test]
    fn peek_navigation_does_not_resolve_parent_even_when_absent() {
        for (action, expected) in actions() {
            let calls = Calls::default();
            let peek = view("peek", &calls, false);
            let parent_lookups = Cell::new(0);
            let mut errors = errors();
            let before = errors.clone();
            let owner = select_navigation_owner(Some(peek), || {
                parent_lookups.set(parent_lookups.get() + 1);
                None
            });
            assert_eq!(parent_lookups.get(), 0);
            owner.expect("Peek remains actionable without a tab").apply(&action, &mut errors);
            assert_eq!(errors, before);
            assert_eq!(*calls.borrow(), [Call { receiver: "peek", action: expected, url: None }]);
        }
    }

    #[test]
    fn closing_peek_retains_navigation_until_removed_then_tab_retry_resumes() {
        for (action, expected) in actions() {
            let calls = Calls::default();
            let parent = view("parent", &calls, false);
            let mut peek = Some(view("peek", &calls, true));
            let mut errors = errors();
            let before = errors.clone();
            // ClosePeek keeps Some(Peek) during animation; only final cleanup takes it.
            for _stage in ["open", "closing"] {
                calls.borrow_mut().clear();
                select_navigation_owner(peek.clone(), || Some(("parent".into(), Some(parent.clone()))))
                    .unwrap().apply(&action, &mut errors);
                assert_eq!(errors, before);
                assert_eq!(*calls.borrow(), [Call { receiver: "peek", action: expected, url: None }]);
            }
            peek.take();
            calls.borrow_mut().clear();
            select_navigation_owner(peek, || Some(("parent".into(), Some(parent))))
                .unwrap().apply(&Action::Reload, &mut errors);
            assert_eq!(*calls.borrow(), [Call { receiver: "parent", action: "load-url", url: Some(before["parent"].0.clone()) }]);
            assert!(!errors.contains_key("parent"));
            assert_eq!(errors.get("other"), before.get("other"));
        }
    }

    #[test]
    fn normal_tab_navigation_reaches_selected_cef_browser() {
        for (action, expected) in actions() {
            let calls = Calls::default();
            let parent = view("parent", &calls, false);
            let mut errors = HashMap::new();
            select_navigation_owner(None, || Some(("parent".into(), Some(parent))))
                .unwrap().apply(&action, &mut errors);
            assert_eq!(*calls.borrow(), [Call { receiver: "parent", action: expected, url: None }]);
        }
    }

    #[test]
    fn normal_tab_reload_retries_exact_failed_url_and_preserves_other_errors() {
        let calls = Calls::default();
        let parent = view("parent", &calls, false);
        let mut errors = errors();
        let before = errors.clone();
        select_navigation_owner(None, || Some(("parent".into(), Some(parent))))
            .unwrap().apply(&Action::Reload, &mut errors);
        assert_eq!(*calls.borrow(), [Call { receiver: "parent", action: "load-url", url: Some(before["parent"].0.clone()) }]);
        assert!(!errors.contains_key("parent"));
        assert_eq!(errors.get("other"), before.get("other"));
    }

    #[test]
    fn non_retry_tab_actions_do_not_consume_errors() {
        for (action, expected) in actions().into_iter().filter(|(action, _)| !matches!(action, Action::Reload)) {
            let calls = Calls::default();
            let parent = view("parent", &calls, false);
            let mut errors = errors();
            let before = errors.clone();
            select_navigation_owner(None, || Some(("parent".into(), Some(parent))))
                .unwrap().apply(&action, &mut errors);
            assert_eq!(errors, before);
            assert_eq!(*calls.borrow(), [Call { receiver: "parent", action: expected, url: None }]);
        }
    }

    #[test]
    fn absent_peek_and_parent_have_no_navigation_owner() {
        let owner = select_navigation_owner::<ViewSpy>(None, || None);
        assert!(owner.is_none());
    }

    #[test]
    fn unloaded_tab_preserves_existing_retry_semantics_without_cef_call() {
        let mut errors = errors();
        let before = errors.clone();
        let owner = select_navigation_owner::<ViewSpy>(None, || Some(("parent".into(), None))).unwrap();
        assert!(!owner.editable_focus());
        owner.apply(&Action::Reload, &mut errors);
        assert!(!errors.contains_key("parent"));
        assert_eq!(errors.get("other"), before.get("other"));
    }

    #[test]
    fn editable_focus_belongs_to_peek_not_parent() {
        for editable in [false, true] {
            let calls = Calls::default();
            let peek = view("peek", &calls, editable);
            let parent = view("parent", &calls, !editable);
            let owner = select_navigation_owner(Some(peek), || Some(("parent".into(), Some(parent)))).unwrap();
            assert_eq!(owner.editable_focus(), editable);
            assert!(calls.borrow().is_empty());
        }
    }

    #[test]
    fn batch_live_peek_navigation_preserves_parent_errors_for_all_five_actions() {
        for (action, expected) in actions() {
            let calls = Calls::default();
            let peek = view("peek", &calls, false);
            let mut errors = errors();
            let before = errors.clone();
            let mut batch = NavigationBatch::new(Some(false));
            batch.observe(&action);
            let owner = batch.owner(Some(false), || select_navigation_owner(Some(peek), || panic!("parent lookup"))).unwrap();
            owner.apply(&action, &mut errors);
            assert_eq!(*calls.borrow(), [Call { receiver: "peek", action: expected, url: None }]);
            assert_eq!(errors, before);
        }
    }

    #[test]
    fn batch_enqueue_cleanup_dispatch_never_reaches_parent() {
        for (action, _) in actions() {
            let calls = Calls::default();
            let parent = view("parent", &calls, false);
            let mut peek = Some(view("peek", &calls, false));
            let mut errors = errors();
            let before = errors.clone();
            // App::ui binds ownership, handles shortcuts, then draws overlays.
            let mut batch = NavigationBatch::new(peek.as_ref().map(|_| false));
            let queued = vec![action];
            peek.take(); // Overlay cleanup removes Peek before apply_actions.
            let resolutions = Cell::new(0);
            for action in queued {
                batch.observe(&action);
                if let Some(owner) = batch.owner(peek.as_ref().map(|_| false), || {
                    resolutions.set(resolutions.get() + 1);
                    select_navigation_owner(peek.clone(), || Some(("parent".into(), Some(parent.clone()))))
                }) {
                    owner.apply(&action, &mut errors);
                }
            }
            assert!(calls.borrow().is_empty(), "queued Peek action escaped after cleanup: {:?}", calls.borrow());
            assert_eq!(errors, before);
            assert_eq!(resolutions.get(), 0);
        }
    }

    #[test]
    fn batch_closing_peek_cancels_navigation_with_or_without_parent() {
        for closing_at_enqueue in [false, true] {
            for parent_present in [false, true] {
                for (action, _) in actions() {
                    let calls = Calls::default();
                    let peek = view("peek", &calls, true);
                    let parent = parent_present.then(|| view("parent", &calls, false));
                    let mut errors = errors();
                    let before = errors.clone();
                    let mut batch = NavigationBatch::new(Some(closing_at_enqueue));
                    batch.observe(&action);
                    let resolutions = Cell::new(0);
                    if let Some(owner) = batch.owner(Some(true), || {
                        resolutions.set(resolutions.get() + 1);
                        select_navigation_owner(Some(peek), || parent.map(|view| ("parent".into(), Some(view))))
                    }) {
                        owner.apply(&action, &mut errors);
                    }
                    assert!(calls.borrow().is_empty());
                    assert_eq!(errors, before);
                    assert_eq!(resolutions.get(), 0);
                }
            }
        }
    }

    #[test]
    fn batch_peek_close_replace_or_promotion_cancels_later_navigation() {
        for transition in [Action::ClosePeek, Action::OpenInPeek("http://127.0.0.1:18791/replacement".into()), Action::PeekToTab] {
            for (action, _) in actions() {
                let calls = Calls::default();
                let parent = view("parent", &calls, false);
                let original = view("original-peek", &calls, false);
                let mut peek = Some(original.clone());
                let mut errors = errors();
                let before = errors.clone();
                let mut batch = NavigationBatch::new(Some(false));
                batch.observe(&transition);
                let closing = match transition {
                    Action::ClosePeek => true,
                    Action::OpenInPeek(_) => { peek = Some(view("replacement-peek", &calls, false)); false },
                    Action::PeekToTab => { peek.take(); false },
                    _ => unreachable!(),
                };
                batch.observe(&action);
                let resolutions = Cell::new(0);
                if let Some(owner) = batch.owner(peek.as_ref().map(|_| closing), || {
                    resolutions.set(resolutions.get() + 1);
                    select_navigation_owner(peek, || Some(("parent".into(), Some(parent))))
                }) {
                    owner.apply(&action, &mut errors);
                }
                assert!(calls.borrow().is_empty(), "navigation crossed {transition:?}: {:?}", calls.borrow());
                assert_eq!(errors, before);
                assert_eq!(resolutions.get(), 0);
            }
        }
    }

    #[test]
    fn batch_failed_peek_replacement_also_cancels_later_navigation() {
        let calls = Calls::default();
        let peek = view("original-peek", &calls, false);
        let mut batch = NavigationBatch::new(Some(false));
        batch.observe(&Action::OpenInPeek("http://127.0.0.1:18791/replacement".into()));
        let owner = batch.owner(Some(false), || select_navigation_owner(Some(peek), || None));
        assert!(owner.is_none(), "failed replacement must not rebind an older queued command");
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn batch_does_not_adopt_a_peek_opened_after_enqueue() {
        let calls = Calls::default();
        let peek = view("new-peek", &calls, false);
        let batch = NavigationBatch::new(None);
        assert!(batch.owner(Some(false), || select_navigation_owner(Some(peek), || None)).is_none());
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn batch_normal_tab_activation_then_reload_resolves_new_tab_lazily() {
        let calls = Calls::default();
        let parent = view("parent", &calls, false);
        let other = view("other", &calls, false);
        let mut active = "parent";
        let mut errors = errors();
        let before = errors.clone();
        let mut batch = NavigationBatch::new(None);
        let queued = [Action::ActivateTab("other".into()), Action::Reload];
        for action in queued {
            batch.observe(&action);
            match action {
                Action::ActivateTab(_) => active = "other",
                Action::Reload => {
                    let owner = batch.owner(None, || select_navigation_owner(None, || {
                        Some((active.into(), Some(if active == "parent" { parent.clone() } else { other.clone() })))
                    })).unwrap();
                    owner.apply(&action, &mut errors);
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(*calls.borrow(), [Call { receiver: "other", action: "load-url", url: Some(before["other"].0.clone()) }]);
        assert!(!errors.contains_key("other"));
        assert_eq!(errors.get("parent"), before.get("parent"));
    }

    #[test]
    fn batch_next_frame_after_peek_removal_allows_normal_tab_retry() {
        let calls = Calls::default();
        let parent = view("parent", &calls, false);
        let mut errors = errors();
        let before = errors.clone();
        let closing_frame = NavigationBatch::new(Some(true));
        assert!(closing_frame.owner(None, || select_navigation_owner(None, || Some(("parent".into(), Some(parent.clone()))))).is_none());
        let next_frame = NavigationBatch::new(None);
        next_frame.owner(None, || select_navigation_owner(None, || Some(("parent".into(), Some(parent)))))
            .unwrap().apply(&Action::Reload, &mut errors);
        assert_eq!(*calls.borrow(), [Call { receiver: "parent", action: "load-url", url: Some(before["parent"].0.clone()) }]);
        assert!(!errors.contains_key("parent"));
        assert_eq!(errors.get("other"), before.get("other"));
    }

    #[test]
    fn editable_focus_follows_peek_close_and_removal() {
        let calls = Calls::default();
        let parent = view("parent", &calls, false);
        let mut peek = Some(view("peek", &calls, true));
        for _stage in ["open", "closing"] {
            assert!(select_navigation_owner(peek.clone(), || Some(("parent".into(), Some(parent.clone()))))
                .unwrap().editable_focus());
        }
        peek.take();
        assert!(!select_navigation_owner(peek, || Some(("parent".into(), Some(parent))))
            .unwrap().editable_focus());
        assert!(calls.borrow().is_empty());
    }
}
