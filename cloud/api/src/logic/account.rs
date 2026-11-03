//! Deleting an account: the typed confirmation, and what a step-up must show
//! before anything is deleted. Every refusal here is `403` or `429`, never
//! `401`, which on this route means only that the session itself is gone.

use crossbow_proto::{error_codes, normalize_email};

use super::MINUTE_MS;
use super::error::ApiError;
use super::limits::ChallengeState;

/// An email code or a browser sign-in proves the user is present only this
/// long after it was made.
pub const STEP_UP_MAX_AGE_MS: u64 = 5 * MINUTE_MS;

/// The user typed the account's email.
pub fn confirmation(typed: &str, account_email: &str) -> Result<(), ApiError> {
    match (normalize_email(typed), normalize_email(account_email)) {
        (Some(typed), Some(account)) if typed == account => Ok(()),
        _ => Err(ApiError::new(400, error_codes::CONFIRMATION_MISMATCH, "type this account's email to delete it")),
    }
}

/// A stored sign-in code as a step-up, before its code is compared: sent to
/// this account's email, still open, and started at most
/// [`STEP_UP_MAX_AGE_MS`] ago. A code for another address reads as unknown.
pub fn email_step_up(state: ChallengeState, challenge_email: &str, created_at: u64, account_email: &str, now_ms: u64) -> Result<(), ApiError> {
    if normalize_email(challenge_email).is_none() || normalize_email(challenge_email) != normalize_email(account_email) {
        return Err(wrong_code());
    }
    if let Some(error) = state.error() {
        return Err(step_up_error(error));
    }
    if now_ms.saturating_sub(created_at) > STEP_UP_MAX_AGE_MS {
        return Err(ApiError::new(403, error_codes::CODE_EXPIRED, "the code is more than 5 minutes old; send a new one"));
    }
    Ok(())
}

pub fn wrong_code() -> ApiError {
    ApiError::new(403, error_codes::INVALID_CODE, "the code is not valid for this account")
}

/// A browser sign-in as a step-up: an identity linked to this account,
/// issued at most [`STEP_UP_MAX_AGE_MS`] ago.
pub fn browser_step_up(known: Option<&str>, caller: &str, issued_at_ms: u64, now_ms: u64) -> Result<(), ApiError> {
    if known != Some(caller) {
        return Err(ApiError::new(403, error_codes::STEP_UP_REJECTED, "this browser sign-in isn't linked to this account"));
    }
    if now_ms.saturating_sub(issued_at_ms) > STEP_UP_MAX_AGE_MS {
        return Err(ApiError::new(403, error_codes::STEP_UP_REJECTED, "the browser sign-in is more than 5 minutes old; start again"));
    }
    Ok(())
}

/// A step-up's refusal keeps its code; a `401` becomes `403`, so the app
/// never reads it as a rejected session.
pub fn step_up_error(error: ApiError) -> ApiError {
    if error.status == 401 { ApiError { status: 403, ..error } } else { error }
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
