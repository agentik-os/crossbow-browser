//! Rate limits, expiries and the sign-in challenge state machine.

use super::error::ApiError;
use super::tokens::{ct_eq, sha256_hex};
use super::{DAY_MS, HOUR_MS, MINUTE_MS};

/// `POST /v1/auth/start`: at most this many starts per normalised email per hour.
pub const STARTS_PER_EMAIL_PER_HOUR: u64 = 5;
/// `POST /v1/auth/start`: at most this many starts per client IP per hour.
pub const STARTS_PER_IP_PER_HOUR: u64 = 30;
/// Sign-in codes are valid for ten minutes.
pub const CHALLENGE_TTL_MS: u64 = 10 * MINUTE_MS;
/// Wrong codes allowed before the challenge locks.
pub const MAX_VERIFY_ATTEMPTS: u64 = 5;
/// Sessions slide forward on use; idle sessions die after 180 days.
pub const SESSION_TTL_MS: u64 = 180 * DAY_MS;
/// `devices.last_seen_at` (and the sliding session expiry) is written at most
/// this often per device.
pub const TOUCH_INTERVAL_MS: u64 = MINUTE_MS;
/// Old challenges are purged after this long (they only feed rate limits).
pub const CHALLENGE_RETENTION_MS: u64 = DAY_MS;
/// Live (non-tombstone) entities per account.
pub const MAX_LIVE_ROWS: u64 = 100_000;

/// Start of the sliding rate-limit window that ends at `now`.
pub fn window_start(now_ms: u64) -> u64 {
    now_ms.saturating_sub(HOUR_MS)
}

/// `email_starts` / `ip_starts` are the starts already recorded inside the window.
pub fn check_start_limits(email_starts: u64, ip_starts: Option<u64>) -> Result<(), ApiError> {
    if email_starts >= STARTS_PER_EMAIL_PER_HOUR {
        return Err(ApiError::rate_limited("too many sign-in requests for this email; try later"));
    }
    if ip_starts.is_some_and(|n| n >= STARTS_PER_IP_PER_HOUR) {
        return Err(ApiError::rate_limited("too many sign-in requests from this network; try later"));
    }
    Ok(())
}

/// What a verify call may do with the stored challenge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeState {
    /// Compare the code.
    Open,
    /// Already used (single-use) or unknown.
    Consumed,
    Expired,
    Locked,
}

pub fn challenge_state(
    attempts: u64,
    expires_at: u64,
    consumed_at: Option<u64>,
    now_ms: u64,
) -> ChallengeState {
    if consumed_at.is_some() {
        ChallengeState::Consumed
    } else if now_ms >= expires_at {
        ChallengeState::Expired
    } else if attempts >= MAX_VERIFY_ATTEMPTS {
        ChallengeState::Locked
    } else {
        ChallengeState::Open
    }
}

impl ChallengeState {
    /// The error a non-open challenge returns.
    pub fn error(self) -> Option<ApiError> {
        match self {
            ChallengeState::Open => None,
            ChallengeState::Consumed => Some(ApiError::invalid_code()),
            ChallengeState::Expired => Some(ApiError::code_expired()),
            ChallengeState::Locked => Some(ApiError::too_many_attempts()),
        }
    }
}

/// How `POST /v1/auth/start` hands out the code it made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeDelivery {
    /// Email it.
    Mail,
    /// Return it as `dev_code`: dev only, to a caller holding the dev key.
    Reveal,
    /// Answer as a mailed code would and send it nowhere.
    Withhold,
    /// No mail outside dev: `503 mail_unavailable`.
    Unavailable,
}

/// `mail`: a mail key is set. `dev_key`: the `DEV_CODE_KEY` secret.
/// `presented`: the request's `x-crossbow-dev-key` header. Without the key,
/// dev answers exactly as a mailed code would; prod never reveals.
pub fn code_delivery(dev: bool, mail: bool, dev_key: Option<&str>, presented: Option<&str>) -> CodeDelivery {
    // Hashes first, so the comparison takes the same time whatever the lengths.
    let holds_key = match (dev_key, presented) {
        (Some(key), Some(p)) if !key.is_empty() => ct_eq(&sha256_hex(key.as_bytes()), &sha256_hex(p.as_bytes())),
        _ => false,
    };
    if dev && holds_key {
        CodeDelivery::Reveal
    } else if mail {
        CodeDelivery::Mail
    } else if dev {
        CodeDelivery::Withhold
    } else {
        CodeDelivery::Unavailable
    }
}

pub fn session_valid(expires_at: u64, revoked_at: Option<u64>, now_ms: u64) -> bool {
    revoked_at.is_none() && now_ms < expires_at
}

/// Whether this authenticated call should write `last_seen_at` and slide the
/// session expiry.
pub fn should_touch(last_seen_at: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(last_seen_at) >= TOUCH_INTERVAL_MS
}

/// Change in live (non-tombstone) rows when an entity goes from `prev` to
/// `next_deleted`. `prev` is `None` for a new row, else its `deleted` flag.
pub fn live_delta(prev: Option<bool>, next_deleted: bool) -> i64 {
    let before = matches!(prev, Some(false)) as i64;
    let after = (!next_deleted) as i64;
    after - before
}

/// Quota check for a push: only pushes that grow the live set can fail.
pub fn within_quota(live_before: u64, delta: i64) -> bool {
    delta <= 0 || live_before.saturating_add(delta as u64) <= MAX_LIVE_ROWS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_limits() {
        assert!(check_start_limits(0, Some(0)).is_ok());
        assert!(check_start_limits(4, Some(29)).is_ok());
        assert!(check_start_limits(4, None).is_ok());
        let e = check_start_limits(5, Some(0)).unwrap_err();
        assert_eq!((e.status, e.code), (429, "rate_limited"));
        assert_eq!(check_start_limits(0, Some(30)).unwrap_err().status, 429);
        assert_eq!(window_start(HOUR_MS + 5), 5);
        assert_eq!(window_start(5), 0);
    }

    #[test]
    fn challenge_states() {
        let now = 1_000_000;
        let exp = now + CHALLENGE_TTL_MS;
        assert_eq!(challenge_state(0, exp, None, now), ChallengeState::Open);
        assert_eq!(challenge_state(4, exp, None, now), ChallengeState::Open);
        assert_eq!(challenge_state(5, exp, None, now), ChallengeState::Locked);
        assert_eq!(challenge_state(0, exp, None, exp), ChallengeState::Expired);
        assert_eq!(challenge_state(0, exp, Some(now), now), ChallengeState::Consumed);
        assert_eq!(ChallengeState::Locked.error().unwrap().code, "too_many_attempts");
        assert_eq!(ChallengeState::Expired.error().unwrap().code, "code_expired");
        assert_eq!(ChallengeState::Consumed.error().unwrap().code, "invalid_code");
        assert!(ChallengeState::Open.error().is_none());
    }

    const KEY: &str = "fixture-dev-key-0123456789abcdef";

    #[test]
    fn dev_without_a_dev_key_configured_reveals_nothing() {
        assert_eq!(code_delivery(true, false, None, None), CodeDelivery::Withhold);
        assert_eq!(code_delivery(true, false, None, Some(KEY)), CodeDelivery::Withhold);
        assert_eq!(code_delivery(true, false, None, Some("")), CodeDelivery::Withhold);
        // An empty secret is no secret.
        assert_eq!(code_delivery(true, false, Some(""), Some("")), CodeDelivery::Withhold);
    }

    #[test]
    fn dev_without_the_header_reveals_nothing() {
        assert_eq!(code_delivery(true, false, Some(KEY), None), CodeDelivery::Withhold);
        // A mail key still mails, as it would with no dev key.
        assert_eq!(code_delivery(true, true, Some(KEY), None), CodeDelivery::Mail);
    }

    #[test]
    fn dev_with_a_wrong_key_reveals_nothing() {
        for wrong in ["", "fixture-dev-key-0123456789abcdee", "fixture-dev-key", "FIXTURE-DEV-KEY-0123456789ABCDEF", "fixture-dev-key-0123456789abcdef "] {
            assert_eq!(code_delivery(true, false, Some(KEY), Some(wrong)), CodeDelivery::Withhold, "{wrong:?}");
            assert_eq!(code_delivery(true, true, Some(KEY), Some(wrong)), CodeDelivery::Mail, "{wrong:?}");
        }
    }

    #[test]
    fn dev_with_the_right_key_reveals_the_code() {
        assert_eq!(code_delivery(true, false, Some(KEY), Some(KEY)), CodeDelivery::Reveal);
        // Then nothing is mailed, even where mail is set up.
        assert_eq!(code_delivery(true, true, Some(KEY), Some(KEY)), CodeDelivery::Reveal);
    }

    #[test]
    fn prod_never_reveals_even_with_the_right_key() {
        assert_eq!(code_delivery(false, true, Some(KEY), Some(KEY)), CodeDelivery::Mail);
        assert_eq!(code_delivery(false, false, Some(KEY), Some(KEY)), CodeDelivery::Unavailable);
        assert_eq!(code_delivery(false, false, None, None), CodeDelivery::Unavailable);
    }

    #[test]
    fn sessions_and_touch() {
        assert!(session_valid(10, None, 9));
        assert!(!session_valid(10, None, 10));
        assert!(!session_valid(10, Some(1), 5));
        assert!(!should_touch(1000, 1000 + TOUCH_INTERVAL_MS - 1));
        assert!(should_touch(1000, 1000 + TOUCH_INTERVAL_MS));
        assert!(!should_touch(5000, 1000)); // clock skew never underflows
    }

    #[test]
    fn live_row_accounting() {
        assert_eq!(live_delta(None, false), 1);
        assert_eq!(live_delta(None, true), 0);
        assert_eq!(live_delta(Some(false), false), 0);
        assert_eq!(live_delta(Some(false), true), -1);
        assert_eq!(live_delta(Some(true), false), 1);
        assert_eq!(live_delta(Some(true), true), 0);
        assert!(within_quota(MAX_LIVE_ROWS, 0));
        assert!(within_quota(MAX_LIVE_ROWS, -3));
        assert!(within_quota(MAX_LIVE_ROWS - 2, 2));
        assert!(!within_quota(MAX_LIVE_ROWS - 2, 3));
    }
}
