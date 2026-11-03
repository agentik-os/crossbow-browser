//! Profile-scoped completion tracking for asynchronous browsing-data removal.

use cef::*;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClearDataStep {
    Pending,
    Completed,
    Failed(&'static str),
    Unconfirmed(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearDataStatus {
    pub cookies: ClearDataStep,
    pub cache: ClearDataStep,
}

impl ClearDataStatus {
    pub fn pending(&self) -> bool {
        [&self.cookies, &self.cache].into_iter().any(|step| matches!(step, ClearDataStep::Pending | ClearDataStep::Unconfirmed(_)))
    }

    pub fn completed(&self) -> bool {
        self.cookies == ClearDataStep::Completed && self.cache == ClearDataStep::Completed
    }
}

/// Dropping this handle does not cancel an operation already sent to Chromium.
#[derive(Clone)]
pub struct ClearDataTask {
    status: Arc<Mutex<ClearDataStatus>>,
}

impl ClearDataTask {
    fn new() -> Self {
        Self { status: Arc::new(Mutex::new(ClearDataStatus { cookies: ClearDataStep::Pending, cache: ClearDataStep::Pending })) }
    }

    pub fn status(&self) -> ClearDataStatus {
        self.status.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    fn finish(&self, cookies: bool, result: ClearDataStep) {
        let mut status = self.status.lock().unwrap_or_else(|error| error.into_inner());
        let step = if cookies { &mut status.cookies } else { &mut status.cache };
        if matches!(step, ClearDataStep::Pending | ClearDataStep::Unconfirmed(_)) { *step = result; }
        drop(status);
        crate::wake(Duration::ZERO);
    }

    fn unavailable(message: &'static str) -> Self {
        let task = Self::new();
        task.finish(true, ClearDataStep::Failed(message));
        task.finish(false, ClearDataStep::Failed(message));
        task
    }
}

/// Begin cookie and HTTP-cache removal for exactly one profile on the UI thread.
/// Completion means Chromium delivered the respective callback, not a verified
/// disk flush or removal of other site storage. Missing callbacks stay pending.
pub fn clear_browsing_data(data_dir: &Path, profile_id: &str) -> ClearDataTask {
    if crate::ROOT_CACHE_PATH.get().is_none() {
        return ClearDataTask::unavailable("Chromium is not running; nothing was cleared.");
    }
    if cef::currently_on(ThreadId::UI) != 1 {
        return ClearDataTask::unavailable("Clearing must start on the browser UI thread; nothing was cleared.");
    }
    let Some(context) = crate::profile_context(data_dir, profile_id) else {
        return ClearDataTask::unavailable("Profile storage is unavailable; nothing was cleared.");
    };
    start(context)
}

fn start(context: RequestContext) -> ClearDataTask {
    let task = ClearDataTask::new();
    let mut ready = CookiesReady::new(context.clone(), task.clone());
    if context.cookie_manager(Some(&mut ready)).is_none() {
        task.finish(true, ClearDataStep::Failed("Cookie storage is unavailable; cookie clearing was not started."));
    }
    let mut complete = CacheCleared::new(task.clone());
    context.clear_http_cache(Some(&mut complete));
    task
}

wrap_completion_callback! {
    struct CookiesReady {
        context: RequestContext,
        task: ClearDataTask,
    }

    impl CompletionCallback {
        fn on_complete(&self) {
            if self.task.status().cookies != ClearDataStep::Pending { return; }
            let Some(manager) = self.context.cookie_manager(None) else {
                self.task.finish(true, ClearDataStep::Failed("Cookie storage is unavailable; cookie clearing was not started."));
                return;
            };
            let mut complete = CookiesCleared::new(self.task.clone());
            if manager.delete_cookies(None, None, Some(&mut complete)) == 0 {
                self.task.finish(true, ClearDataStep::Failed("Chromium did not accept cookie clearing. Some cookies may remain."));
            }
        }
    }
}

wrap_delete_cookies_callback! {
    struct CookiesCleared {
        task: ClearDataTask,
    }

    impl DeleteCookiesCallback {
        fn on_complete(&self, num_deleted: i32) {
            let result = if num_deleted >= 0 {
                ClearDataStep::Completed
            } else {
                ClearDataStep::Unconfirmed("Chromium returned an unexpected cookie count; completion is unconfirmed.")
            };
            self.task.finish(true, result);
        }
    }
}

wrap_completion_callback! {
    struct CacheCleared {
        task: ClearDataTask,
    }

    impl CompletionCallback {
        fn on_complete(&self) {
            self.task.finish(false, ClearDataStep::Completed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Calls {
        ready: Option<CompletionCallback>,
        cookies: Option<DeleteCookiesCallback>,
        cache: Option<CompletionCallback>,
        deletion_calls: usize,
        reject_delete: bool,
        missing_manager: bool,
    }

    use cef::rc::{ConvertReturnValue, RcImpl};

    unsafe extern "C" fn delete_cookies(manager: *mut sys::_cef_cookie_manager_t, url: *const sys::cef_string_t, name: *const sys::cef_string_t, callback: *mut sys::_cef_delete_cookies_callback_t) -> i32 {
        assert!(url.is_null() && name.is_null());
        let calls = &RcImpl::<sys::_cef_cookie_manager_t, Arc<Mutex<Calls>>>::get(manager).interface;
        let mut calls = calls.lock().unwrap();
        calls.deletion_calls += 1;
        calls.cookies = (!callback.is_null()).then(|| callback.wrap_result());
        i32::from(!calls.reject_delete)
    }

    unsafe extern "C" fn cookie_manager(context: *mut sys::_cef_request_context_t, callback: *mut sys::_cef_completion_callback_t) -> *mut sys::_cef_cookie_manager_t {
        let calls = &RcImpl::<sys::_cef_request_context_t, Arc<Mutex<Calls>>>::get(context).interface;
        let mut state = calls.lock().unwrap();
        if !callback.is_null() { state.ready = Some(callback.wrap_result()); }
        if state.missing_manager { return std::ptr::null_mut(); }
        let mut manager: sys::_cef_cookie_manager_t = unsafe { std::mem::zeroed() };
        manager.delete_cookies = Some(delete_cookies);
        RcImpl::new(manager, calls.clone()).cast()
    }

    unsafe extern "C" fn clear_cache(context: *mut sys::_cef_request_context_t, callback: *mut sys::_cef_completion_callback_t) {
        let calls = &RcImpl::<sys::_cef_request_context_t, Arc<Mutex<Calls>>>::get(context).interface;
        calls.lock().unwrap().cache = (!callback.is_null()).then(|| callback.wrap_result());
    }

    fn fixture_context(calls: Arc<Mutex<Calls>>) -> RequestContext {
        let mut context: sys::_cef_request_context_t = unsafe { std::mem::zeroed() };
        context.get_cookie_manager = Some(cookie_manager);
        context.clear_http_cache = Some(clear_cache);
        RcImpl::new(context, calls).cast::<sys::_cef_request_context_t>().wrap_result()
    }

    fn ready(calls: &Arc<Mutex<Calls>>) {
        let callback = calls.lock().unwrap().ready.take().unwrap();
        callback.on_complete();
    }

    fn cookies(calls: &Arc<Mutex<Calls>>, count: i32) {
        let callback = calls.lock().unwrap().cookies.take().unwrap();
        callback.on_complete(count);
    }

    fn cache(calls: &Arc<Mutex<Calls>>) {
        let callback = calls.lock().unwrap().cache.take().unwrap();
        callback.on_complete();
    }

    #[test]
    fn dispatch_waits_for_storage_then_independent_callbacks_in_either_order() {
        for cookies_first in [false, true] {
            let calls = Arc::new(Mutex::new(Calls::default()));
            let task = start(fixture_context(calls.clone()));
            assert_eq!(calls.lock().unwrap().deletion_calls, 0, "initialization is not deletion completion");
            assert!(task.status().pending());
            if !cookies_first { cache(&calls); }
            assert!(!task.status().completed());
            ready(&calls);
            assert_eq!(calls.lock().unwrap().deletion_calls, 1);
            assert_eq!(task.status().cookies, ClearDataStep::Pending, "accepted is not completed");
            cookies(&calls, 0);
            if cookies_first {
                assert!(task.status().pending());
                cache(&calls);
            }
            assert!(task.status().completed(), "zero deleted cookies is a valid completed request");
        }
    }

    #[test]
    fn dispatch_handles_absent_storage_and_rejection_without_hiding_cache_completion() {
        for missing_manager in [false, true] {
            let calls = Arc::new(Mutex::new(Calls { missing_manager, reject_delete: true, ..Default::default() }));
            let task = start(fixture_context(calls.clone()));
            ready(&calls);
            assert_eq!(calls.lock().unwrap().deletion_calls, usize::from(!missing_manager));
            assert!(matches!(task.status().cookies, ClearDataStep::Failed(_)));
            assert!(task.status().pending());
            cache(&calls);
            assert_eq!(task.status().cache, ClearDataStep::Completed);
            assert!(!task.status().completed());
            assert!(!task.status().pending());
            if !missing_manager {
                cookies(&calls, 3);
                assert!(matches!(task.status().cookies, ClearDataStep::Failed(_)), "a late contradictory callback cannot turn a rejected start into success");
            }
        }
    }

    #[test]
    fn missing_or_unexpected_callbacks_remain_unconfirmed_until_late_completion() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let task = start(fixture_context(calls.clone()));
        ready(&calls);
        let callback = calls.lock().unwrap().cookies.take().unwrap();
        callback.on_complete(-1);
        assert!(matches!(task.status().cookies, ClearDataStep::Unconfirmed(_)));
        assert_eq!(task.status().cache, ClearDataStep::Pending);
        assert!(task.status().pending());
        assert!(!task.status().completed());
        callback.on_complete(0);
        assert!(task.status().pending(), "missing cache callback remains unconfirmed");
        cache(&calls);
        assert!(task.status().completed());
    }

    #[test]
    fn context_and_retained_handles_never_cross_between_operations() {
        let first = Arc::new(Mutex::new(Calls::default()));
        let second = Arc::new(Mutex::new(Calls::default()));
        let task = start(fixture_context(first.clone()));
        let retained = task.clone();
        drop(task);
        let other = start(fixture_context(second.clone()));
        ready(&first);
        cookies(&first, 2);
        cache(&first);
        assert!(retained.status().completed());
        assert_eq!(second.lock().unwrap().deletion_calls, 0);
        assert_eq!(other.status().cookies, ClearDataStep::Pending);
        assert_eq!(other.status().cache, ClearDataStep::Pending);
        ready(&second);
        cookies(&second, 1);
        cache(&second);
        assert!(other.status().completed());
    }

    #[test]
    fn public_entry_without_engine_fails_before_calling_cef_or_creating_storage() {
        assert!(crate::ROOT_CACHE_PATH.get().is_none());
        let path = std::env::temp_dir().join("crossbow-uninitialized-clearing-no-storage");
        assert!(!path.exists());
        let task = clear_browsing_data(&path, "fixture-profile");
        assert!(!task.status().pending());
        assert!(!task.status().completed());
        assert!(matches!(task.status().cookies, ClearDataStep::Failed(_)));
        assert!(!path.exists());
    }

    #[test]
    fn both_independent_completions_are_required_in_either_order() {
        for cookies_first in [true, false] {
            let task = ClearDataTask::new();
            assert!(task.status().pending());
            assert!(!task.status().completed());
            task.finish(cookies_first, ClearDataStep::Completed);
            assert!(task.status().pending());
            assert!(!task.status().completed());
            task.finish(!cookies_first, ClearDataStep::Completed);
            assert!(!task.status().pending());
            assert!(task.status().completed());
        }
    }

    #[test]
    fn partial_failure_never_becomes_combined_success() {
        let task = ClearDataTask::new();
        task.finish(true, ClearDataStep::Failed("Cookie deletion was rejected."));
        assert!(task.status().pending());
        task.finish(false, ClearDataStep::Completed);
        assert!(!task.status().pending());
        assert!(!task.status().completed());
        task.finish(true, ClearDataStep::Completed);
        assert_eq!(task.status().cookies, ClearDataStep::Failed("Cookie deletion was rejected."));
    }

    #[test]
    fn cloned_handles_keep_operation_state_separate() {
        let first = ClearDataTask::new();
        let retained = first.clone();
        let second = ClearDataTask::new();
        first.finish(true, ClearDataStep::Completed);
        drop(first);
        assert_eq!(retained.status().cookies, ClearDataStep::Completed);
        assert_eq!(second.status().cookies, ClearDataStep::Pending);
        retained.finish(false, ClearDataStep::Completed);
        assert!(retained.status().completed());
        assert!(second.status().pending());
    }

    #[test]
    fn unavailable_context_reports_both_operations_not_started() {
        let task = ClearDataTask::unavailable("Profile storage is unavailable; nothing was cleared.");
        assert!(!task.status().pending());
        assert!(!task.status().completed());
        assert!(matches!(task.status().cookies, ClearDataStep::Failed(_)));
        assert!(matches!(task.status().cache, ClearDataStep::Failed(_)));
    }
}
