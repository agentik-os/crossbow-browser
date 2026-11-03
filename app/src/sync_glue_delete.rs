//! Delete Account on the Account page: a code to the account's email, the
//! email typed, then the delete. Only a delete the server confirms signs
//! this Mac out, and the library always stays. The Clerk user, if any, is
//! the provider's; nothing here deletes it.

use super::SyncGlue;
use crossbow_core::proto::{DeleteAccountRequest, StepUp, error_codes};
use crossbow_core::library::Library;
use crossbow_core::sync::SyncCommand;

/// On the signed-out page after a confirmed delete.
pub const ACCOUNT_DELETED: &str = "Account deleted. This Mac kept its library and is signed out. A Clerk user you used for browser sign-in still exists.";
/// On the signed-out page when the delete found its session gone.
pub const SESSION_ENDED: &str = "This Mac's session had already ended, so nothing was deleted from here. Sign in to delete the account; if an earlier attempt deleted it, there's nothing left to do.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Closed,
    /// Open, no code sent yet.
    Confirm,
    SendingCode,
    /// A code went to the account's email.
    Code { challenge_id: String, dev_code: Option<String> },
    /// The delete is on its way; it can't be called back.
    Deleting { challenge_id: String, dev_code: Option<String> },
}

pub struct DeleteAccount {
    pub step: Step,
    pub error: Option<String>,
    /// What the signed-out page says after this Mac signed out here, until
    /// the next sign-in. Never stored.
    pub done: Option<&'static str>,
    /// Answers for an earlier attempt are dropped.
    attempt: u64,
}

impl DeleteAccount {
    pub(super) fn new() -> DeleteAccount {
        DeleteAccount { step: Step::Closed, error: None, done: None, attempt: 0 }
    }

    /// Close the sheet; any answer still on its way is dropped.
    pub(super) fn close(&mut self) {
        self.attempt += 1;
        self.step = Step::Closed;
        self.error = None;
    }
}

/// What a refusal means to the person deleting. Nothing on this Mac changed.
fn failure(error: &str) -> String {
    let is = |code: &str| error.starts_with(&format!("{code}:"));
    if is(error_codes::INVALID_CODE) {
        "That code isn't right. Check it, or send a new code.".into()
    } else if is(error_codes::CODE_EXPIRED) {
        "That code is more than 5 minutes old. Send a new code.".into()
    } else if is(error_codes::TOO_MANY_ATTEMPTS) {
        "Too many wrong codes. Send a new code.".into()
    } else if is(error_codes::RATE_LIMITED) {
        "Too many codes went to this email. Try again later.".into()
    } else if is(error_codes::MAIL_UNAVAILABLE) {
        "The sync server can't send email right now. Try again later.".into()
    } else if is(error_codes::CONFIRMATION_MISMATCH) {
        "Type this account's email exactly to delete it.".into()
    } else if is("unsupported") {
        "This sync server can't delete accounts yet. Nothing was deleted.".into()
    } else if is("unconfirmed") {
        "The sync server didn't confirm the delete. This Mac is still signed in; try again.".into()
    } else if is("network") {
        "Couldn't reach the sync server. This Mac is still signed in; try again.".into()
    } else {
        let code = error.split(':').next().unwrap_or(error);
        format!("Couldn't delete the account ({code}). This Mac is still signed in; try again.")
    }
}

impl SyncGlue {
    pub fn open_delete_account(&mut self) {
        if self.session.is_some() && self.delete.step == Step::Closed {
            self.delete.close();
            self.delete.step = Step::Confirm;
        }
    }

    /// Email a code to the account's address. A new code replaces the last.
    pub fn send_delete_code(&mut self) {
        if !matches!(self.delete.step, Step::Confirm | Step::Code { .. }) { return }
        let Some(s) = self.session.as_ref() else { return };
        let (base_url, email) = (s.base_url.clone(), s.email.clone());
        self.delete.attempt += 1;
        self.delete.step = Step::SendingCode;
        self.delete.error = None;
        self.handle.send(SyncCommand::StepUpCode { attempt: self.delete.attempt, base_url, email });
    }

    /// Delete the account with the typed email and the code that came.
    pub fn delete_account(&mut self, typed_email: &str, code: &str) {
        let Step::Code { challenge_id, dev_code } = self.delete.step.clone() else { return };
        let Some(base_url) = self.session.as_ref().map(|s| s.base_url.clone()) else { return };
        let Some(token) = self.session_token() else {
            self.delete.error = Some("Couldn't read this Mac's sign-in. Nothing was deleted.".into());
            return;
        };
        let request = DeleteAccountRequest { confirm: typed_email.trim().into(), step_up: StepUp::EmailCode { challenge_id: challenge_id.clone(), code: code.trim().into() } };
        self.delete.attempt += 1;
        self.delete.step = Step::Deleting { challenge_id, dev_code };
        self.delete.error = None;
        self.handle.send(SyncCommand::DeleteAccount { attempt: self.delete.attempt, base_url, token, request });
    }

    /// Cancel, or Escape: close the sheet unless the delete is on its way.
    /// True when there was a sheet to close.
    pub fn cancel_delete_account(&mut self) -> bool {
        if matches!(self.delete.step, Step::Closed | Step::Deleting { .. }) { return false }
        self.delete.close();
        true
    }

    pub(super) fn step_up_code(&mut self, attempt: u64, challenge_id: String, dev_code: Option<String>) {
        if attempt == self.delete.attempt && self.delete.step == Step::SendingCode {
            self.delete.step = Step::Code { challenge_id, dev_code };
        }
    }

    /// The account is gone: sign out here as the user's own Sign Out does,
    /// which removes this account's credential and key and keeps the library.
    pub(super) fn account_deleted(&mut self, lib: &Library, attempt: u64) {
        if attempt != self.delete.attempt { return }
        self.sign_out(lib);
        self.delete.done = Some(ACCOUNT_DELETED);
    }

    pub(super) fn delete_account_failed(&mut self, lib: &Library, attempt: u64, error: &str) {
        if attempt != self.delete.attempt { return }
        // The session ended before the delete: signed out, as any ended session.
        if error.starts_with("unauthorized") {
            self.handle.send(SyncCommand::SignOut);
            self.forget(lib, false);
            self.delete.done = Some(SESSION_ENDED);
            return;
        }
        self.delete.step = match std::mem::replace(&mut self.delete.step, Step::Closed) {
            Step::Deleting { challenge_id, dev_code } => Step::Code { challenge_id, dev_code },
            _ => Step::Confirm,
        };
        self.delete.error = Some(failure(error));
    }
}
