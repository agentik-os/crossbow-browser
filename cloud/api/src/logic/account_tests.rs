use super::*;
use crate::logic::limits::challenge_state;

/// 2026-11-01T00:00:00Z, in milliseconds.
const NOW: u64 = 1_793_491_200_000;
const EMAIL: &str = "ada@example.com";

fn refusal(result: Result<(), ApiError>) -> (u16, &'static str) {
    let error = result.unwrap_err();
    (error.status, error.code)
}

#[test]
fn the_typed_confirmation_must_be_the_accounts_email() {
    assert!(confirmation(EMAIL, EMAIL).is_ok());
    // Typed as people type it: case and surrounding spaces don't matter.
    assert!(confirmation("  Ada@Example.COM ", EMAIL).is_ok());
    for typed in ["", "ada", "ada@example.org", "bob@example.com", "ada@example.com.", "delete"] {
        assert_eq!(refusal(confirmation(typed, EMAIL)), (400, error_codes::CONFIRMATION_MISMATCH), "{typed:?}");
    }
}

#[test]
fn an_email_code_steps_up_only_for_this_accounts_email_while_open_and_fresh() {
    let open = challenge_state(0, NOW + 5 * MINUTE_MS, None, NOW);
    assert!(email_step_up(open, EMAIL, NOW - MINUTE_MS, EMAIL, NOW).is_ok());
    assert!(email_step_up(open, "Ada@Example.com", NOW - STEP_UP_MAX_AGE_MS, EMAIL, NOW).is_ok(), "exactly 5 minutes old");
    // A code for another address is as good as none, whatever its state.
    assert_eq!(refusal(email_step_up(open, "bob@example.com", NOW, EMAIL, NOW)), (403, error_codes::INVALID_CODE));
    let used = challenge_state(0, NOW + MINUTE_MS, Some(NOW - 1), NOW);
    assert_eq!(refusal(email_step_up(used, "bob@example.com", NOW, EMAIL, NOW)), (403, error_codes::INVALID_CODE));
    // A sign-in code may live 10 minutes; a step-up only 5.
    assert_eq!(refusal(email_step_up(open, EMAIL, NOW - STEP_UP_MAX_AGE_MS - 1, EMAIL, NOW)), (403, error_codes::CODE_EXPIRED));
    assert_eq!(refusal(email_step_up(used, EMAIL, NOW, EMAIL, NOW)), (403, error_codes::INVALID_CODE), "single use");
    let expired = challenge_state(0, NOW, None, NOW);
    assert_eq!(refusal(email_step_up(expired, EMAIL, NOW - MINUTE_MS, EMAIL, NOW)), (403, error_codes::CODE_EXPIRED));
    let locked = challenge_state(5, NOW + MINUTE_MS, None, NOW);
    assert_eq!(refusal(email_step_up(locked, EMAIL, NOW, EMAIL, NOW)), (429, error_codes::TOO_MANY_ATTEMPTS));
    assert_eq!((wrong_code().status, wrong_code().code), (403, error_codes::INVALID_CODE));
}

#[test]
fn a_browser_sign_in_steps_up_only_for_an_identity_linked_to_this_account() {
    assert!(browser_step_up(Some("acc_1"), "acc_1", NOW - MINUTE_MS, NOW).is_ok());
    for known in [None, Some("acc_2")] {
        assert_eq!(refusal(browser_step_up(known, "acc_1", NOW, NOW)), (403, error_codes::STEP_UP_REJECTED), "{known:?}");
    }
    assert_eq!(refusal(browser_step_up(Some("acc_1"), "acc_1", NOW - STEP_UP_MAX_AGE_MS - 1, NOW)), (403, error_codes::STEP_UP_REJECTED));
}

#[test]
fn no_step_up_refusal_reads_as_a_rejected_session() {
    let rejected = crate::logic::clerk::Rejection::Nonce.error();
    assert_eq!(rejected.status, 401, "a browser sign-in refusal is 401 on the sign-in route");
    let as_step_up = step_up_error(rejected);
    assert_eq!((as_step_up.status, as_step_up.code), (403, error_codes::SIGN_IN_REJECTED));
    for error in [ApiError::invalid_code(), ApiError::code_expired()] {
        let code = error.code;
        let as_step_up = step_up_error(error);
        assert_eq!((as_step_up.status, as_step_up.code), (403, code));
    }
    // Anything that isn't a 401 keeps its status.
    let busy = step_up_error(ApiError::too_many_attempts());
    assert_eq!((busy.status, busy.code), (429, error_codes::TOO_MANY_ATTEMPTS));
    let down = step_up_error(crate::logic::clerk::provider_failed());
    assert_eq!((down.status, down.code), (502, error_codes::PROVIDER_UNAVAILABLE));
}
