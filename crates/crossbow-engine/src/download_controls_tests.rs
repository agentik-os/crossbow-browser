use super::*;
use cef::rc::{ConvertReturnValue, RcImpl};
use cef::sys;
use std::time::Instant;

#[derive(Default)]
struct Calls { actions: Vec<DownloadCommand>, drops: usize }
struct Spy { calls: Rc<RefCell<Calls>>, reenter: Option<Rc<dyn Fn()>> }
impl Drop for Spy { fn drop(&mut self) { self.calls.borrow_mut().drops += 1; } }

fn record(raw: *mut sys::_cef_download_item_callback_t, command: DownloadCommand) {
    let spy = &RcImpl::<sys::_cef_download_item_callback_t, Spy>::get(raw).interface;
    spy.calls.borrow_mut().actions.push(command);
    if let Some(reenter) = &spy.reenter { reenter(); }
}
unsafe extern "C" fn pause(raw: *mut sys::_cef_download_item_callback_t) { record(raw, DownloadCommand::Pause); }
unsafe extern "C" fn resume(raw: *mut sys::_cef_download_item_callback_t) { record(raw, DownloadCommand::Resume); }
unsafe extern "C" fn cancel(raw: *mut sys::_cef_download_item_callback_t) { record(raw, DownloadCommand::Cancel); }

fn callback(reenter: Option<Rc<dyn Fn()>>) -> (DownloadItemCallback, Rc<RefCell<Calls>>) {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut raw: sys::_cef_download_item_callback_t = unsafe { std::mem::zeroed() };
    raw.pause = Some(pause);
    raw.resume = Some(resume);
    raw.cancel = Some(cancel);
    let callback = RcImpl::new(raw, Spy { calls: calls.clone(), reenter }).cast::<sys::_cef_download_item_callback_t>().wrap_result();
    (callback, calls)
}

fn info() -> DownloadInfo {
    DownloadInfo { profile_id: "A".into(), id: 7, in_progress: true, path: "/synthetic/fixture.txt".into(), ..Default::default() }
}

#[test]
fn download_controls_wait_for_pause_resume_cancel_acknowledgments() {
    let mut info = info();
    let control = DownloadControl::new(&info);
    let stale = control.clone();
    let (callback, calls) = callback(None);
    control.update(&info, Some(callback.clone()));
    control.request("A", 7, DownloadCommand::Pause).unwrap();
    assert!(!info.paused, "submission is not a paused snapshot");
    assert_eq!(stale.status().pending.unwrap().command, DownloadCommand::Pause);
    assert!(!stale.status().pause && !stale.status().resume && stale.status().cancel);
    assert!(stale.request("A", 7, DownloadCommand::Pause).is_err());
    control.update(&info, Some(callback.clone()));
    assert!(control.status().pending.is_some(), "ordinary progress is not pause acknowledgment");
    info.paused = true;
    control.update(&info, Some(callback.clone()));
    assert!(stale.status().pending.is_none() && stale.status().resume);
    control.request("A", 7, DownloadCommand::Resume).unwrap();
    assert!(info.paused, "submission does not synthesize a resumed state");
    info.paused = false;
    control.update(&info, Some(callback.clone()));
    assert!(control.status().pause && control.status().pending.is_none());
    control.request("A", 7, DownloadCommand::Cancel).unwrap();
    assert!(!info.canceled);
    assert!(stale.request("A", 7, DownloadCommand::Cancel).is_err());
    drop(callback);
    assert_eq!(calls.borrow().drops, 0);
    info.canceled = true;
    control.update(&info, None);
    assert!(!stale.status().available && stale.status().pending.is_none());
    assert!(stale.request("A", 7, DownloadCommand::Resume).is_err());
    assert_eq!(calls.borrow().actions, [DownloadCommand::Pause, DownloadCommand::Resume, DownloadCommand::Cancel]);
    assert_eq!(calls.borrow().drops, 1);
}

#[test]
fn download_controls_reject_wrong_profile_id_and_keep_equal_ids_independent() {
    let a = info();
    let b = DownloadInfo { profile_id: "B".into(), paused: true, ..a.clone() };
    let a_control = DownloadControl::new(&a);
    let b_control = DownloadControl::new(&b);
    let (a_callback, a_calls) = callback(None);
    let (b_callback, b_calls) = callback(None);
    a_control.update(&a, Some(a_callback));
    b_control.update(&b, Some(b_callback));
    assert!(a_control.request("B", 7, DownloadCommand::Cancel).is_err());
    assert!(a_control.request("A", 8, DownloadCommand::Cancel).is_err());
    assert!(a_calls.borrow().actions.is_empty());
    a_control.request("A", 7, DownloadCommand::Pause).unwrap();
    b_control.request("B", 7, DownloadCommand::Resume).unwrap();
    assert_eq!(a_calls.borrow().actions, [DownloadCommand::Pause]);
    assert_eq!(b_calls.borrow().actions, [DownloadCommand::Resume]);
}

#[test]
fn download_controls_timeout_is_unconfirmed_and_only_explicit_retry_dispatches() {
    let info = info();
    let control = DownloadControl::new(&info);
    let (callback, calls) = callback(None);
    control.update(&info, Some(callback));
    let now = Instant::now();
    control.request_at("A", 7, DownloadCommand::Pause, now).unwrap();
    let before = now + CONTROL_TIMEOUT - std::time::Duration::from_millis(1);
    assert!(!control.status_at(before).pending.unwrap().overdue);
    assert!(control.request_at("A", 7, DownloadCommand::Pause, before).is_err());
    let after = now + CONTROL_TIMEOUT;
    assert!(control.status_at(after).pending.unwrap().overdue);
    assert!(control.status_at(after).pause);
    assert!(!control.status_at(after).resume);
    assert_eq!(calls.borrow().actions, [DownloadCommand::Pause]);
    control.request_at("A", 7, DownloadCommand::Pause, after).unwrap();
    assert!(!control.status_at(after).pending.unwrap().overdue);
    assert_eq!(calls.borrow().actions, [DownloadCommand::Pause, DownloadCommand::Pause]);
}

#[test]
fn download_controls_cancel_supersedes_pending_pause_without_claiming_completion() {
    let info = info();
    let control = DownloadControl::new(&info);
    let (callback, calls) = callback(None);
    control.update(&info, Some(callback.clone()));
    control.request("A", 7, DownloadCommand::Pause).unwrap();
    control.request("A", 7, DownloadCommand::Cancel).unwrap();
    control.update(&DownloadInfo { paused: true, ..info }, Some(callback));
    assert_eq!(control.status().pending.unwrap().command, DownloadCommand::Cancel);
    assert!(!control.status().resume && !control.status().cancel);
    assert_eq!(calls.borrow().actions, [DownloadCommand::Pause, DownloadCommand::Cancel]);
}

#[test]
fn interruption_retires_pending_pause_and_allows_explicit_resume() {
    for paused in [false, true] {
        let info = info();
        let control = DownloadControl::new(&info);
        let (callback, calls) = callback(None);
        control.update(&info, Some(callback.clone()));
        let now = Instant::now();
        control.request_at("A", 7, DownloadCommand::Pause, now).unwrap();
        control.update(&DownloadInfo { interrupted: true, in_progress: false, paused, ..info }, Some(callback));
        for at in [now, now + CONTROL_TIMEOUT] {
            let status = control.status_at(at);
            assert!(status.pending.is_none(), "interruption supersedes Pause, not a pause acknowledgment");
            assert!(status.resume && status.cancel && !status.pause);
        }
        assert_eq!(calls.borrow().actions, [DownloadCommand::Pause], "no implicit recovery request");
        control.request("A", 7, DownloadCommand::Resume).unwrap();
        assert_eq!(calls.borrow().actions, [DownloadCommand::Pause, DownloadCommand::Resume]);
    }
}

#[test]
fn revoked_download_snapshots_stop_activity_without_fabricating_outcomes() {
    for paused in [false, true] {
        let mut info = DownloadInfo { paused, ..info() };
        let control = DownloadControl::new(&info);
        let (callback, calls) = callback(None);
        control.update(&info, Some(callback));
        info.control = Some(control.clone());
        assert!(info.active());
        control.update(&info, None);
        assert!(info.active(), "a missing callback alone does not revoke ongoing observation");
        control.invalidate();
        assert!(!info.active(), "closed ownership cannot keep the progress card active");
        assert!(control.status().pending.is_none());
        assert!(info.in_progress && !info.complete && !info.canceled, "do not fabricate the unknown native outcome");
        assert!(calls.borrow().actions.is_empty());
    }
}

#[test]
fn interrupted_download_resume_is_an_attempt_until_in_progress_update() {
    let mut info = DownloadInfo { in_progress: false, interrupted: true, ..info() };
    let control = DownloadControl::new(&info);
    let (callback, calls) = callback(None);
    control.update(&info, Some(callback.clone()));
    assert!(!control.status().pause && control.status().resume && control.status().cancel);
    control.request("A", 7, DownloadCommand::Resume).unwrap();
    control.update(&info, Some(callback.clone()));
    assert!(control.status().pending.is_some());
    info.interrupted = false;
    info.in_progress = true;
    control.update(&info, Some(callback));
    assert!(control.status().pending.is_none() && control.status().pause);
    assert_eq!(calls.borrow().actions, [DownloadCommand::Resume]);
}

#[test]
fn download_controls_missing_callback_and_terminal_updates_revoke_old_clones() {
    for terminal in [DownloadInfo { complete: true, ..info() }, DownloadInfo { canceled: true, ..info() }] {
        let control = DownloadControl::new(&info());
        let stale = control.clone();
        let (callback, calls) = callback(None);
        control.update(&info(), Some(callback));
        control.update(&info(), None);
        assert_eq!(calls.borrow().drops, 1);
        assert!(stale.request("A", 7, DownloadCommand::Pause).is_err());
        let (replacement, replacement_calls) = self::callback(None);
        control.update(&info(), Some(replacement.clone()));
        assert!(stale.status().available);
        drop(replacement);
        control.update(&terminal, None);
        assert_eq!(replacement_calls.borrow().drops, 1);
        assert!(!stale.status().available);
        assert!(stale.request("A", 7, DownloadCommand::Cancel).is_err());
    }
}

#[test]
fn download_control_callback_reenters_without_a_live_state_borrow() {
    let info = info();
    let control = DownloadControl::new(&info);
    let weak = Rc::downgrade(&control.0);
    let (callback, calls) = callback(Some(Rc::new(move || {
        let control = DownloadControl(weak.upgrade().unwrap());
        assert!(control.status().pending.is_some());
        control.update(&DownloadInfo { canceled: true, ..self::info() }, None);
    })));
    control.update(&info, Some(callback));
    control.request("A", 7, DownloadCommand::Cancel).unwrap();
    assert!(control.status().pending.is_none());
    assert!(!control.status().available);
    assert_eq!(calls.borrow().drops, 1);
}

#[test]
fn download_controls_close_invalidation_releases_callback_without_cancelling() {
    let info = info();
    let control = DownloadControl::new(&info);
    let (callback, calls) = callback(None);
    control.update(&info, Some(callback));
    control.invalidate();
    assert!(!control.status().available);
    assert_eq!(calls.borrow().drops, 1);
    assert!(calls.borrow().actions.is_empty(), "close invalidates controls, not a fabricated transfer cancellation");
}
