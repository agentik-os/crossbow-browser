//! Request body validation.

use crossbow_proto::{
    AuthStartRequest, AuthVerifyRequest, MAX_PAYLOAD_BYTES, MAX_PUSH_CHANGES, PushRequest,
    normalize_email,
};
use serde::de::DeserializeOwned;

use super::error::ApiError;
use super::tokens::{code_well_formed, id_well_formed};

pub const PLATFORMS: [&str; 3] = ["macos", "windows", "linux"];
pub const MAX_DEVICE_NAME_CHARS: usize = 64;
/// Largest push body accepted before parsing: every change at the payload limit
/// plus generous room for the envelope.
pub const MAX_PUSH_BODY_BYTES: usize = MAX_PUSH_CHANGES * (MAX_PAYLOAD_BYTES + 512);
/// Auth bodies are tiny.
pub const MAX_AUTH_BODY_BYTES: usize = 4 * 1024;

pub fn parse_json<T: DeserializeOwned>(body: &[u8], max: usize) -> Result<T, ApiError> {
    if body.len() > max {
        return Err(ApiError::payload_too_large("request body too large"));
    }
    serde_json::from_slice(body).map_err(|_| ApiError::bad_request("malformed JSON body"))
}

/// Returns the normalised email.
pub fn auth_start(req: &AuthStartRequest) -> Result<String, ApiError> {
    normalize_email(&req.email).ok_or_else(|| ApiError::bad_request("invalid email address"))
}

/// Validated verify request with a trimmed device name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyInput {
    pub challenge_id: String,
    pub code: String,
    pub device_name: String,
    pub platform: String,
}

pub fn auth_verify(req: AuthVerifyRequest) -> Result<VerifyInput, ApiError> {
    if !id_well_formed(&req.challenge_id) {
        return Err(ApiError::bad_request("invalid challenge_id"));
    }
    let device_name = device_name(&req.device_name)?;
    if !PLATFORMS.contains(&req.platform.as_str()) {
        return Err(ApiError::bad_request("platform must be macos, windows or linux"));
    }
    // A malformed code can never match: count it as a wrong code, not a 400,
    // so it still uses up an attempt.
    let code = if code_well_formed(req.code.trim()) { req.code.trim().to_string() } else { String::new() };
    Ok(VerifyInput { challenge_id: req.challenge_id, code, device_name, platform: req.platform })
}

pub fn device_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_DEVICE_NAME_CHARS || n.chars().any(char::is_control) {
        return Err(ApiError::bad_request("device_name must be 1-64 printable characters"));
    }
    Ok(n.to_string())
}

/// Whole-request validation: any invalid change rejects the push.
pub fn push(req: &PushRequest) -> Result<(), ApiError> {
    if req.changes.len() > MAX_PUSH_CHANGES {
        return Err(ApiError::payload_too_large("too many changes in one push"));
    }
    for change in &req.changes {
        change.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_proto::{Change, Encoding, Hlc};

    fn verify_req() -> AuthVerifyRequest {
        AuthVerifyRequest {
            challenge_id: "chl_00ff".into(),
            code: "012345".into(),
            device_name: "  Sam's MacBook  ".into(),
            platform: "macos".into(),
        }
    }

    #[test]
    fn start_normalises_or_rejects() {
        assert_eq!(auth_start(&AuthStartRequest { email: " A@Example.com".into() }).unwrap(), "a@example.com");
        assert_eq!(auth_start(&AuthStartRequest { email: "bad".into() }).unwrap_err().status, 400);
    }

    #[test]
    fn verify_validation() {
        let v = auth_verify(verify_req()).unwrap();
        assert_eq!(v.device_name, "Sam's MacBook");
        assert_eq!(v.code, "012345");
        let mut r = verify_req();
        r.platform = "beos".into();
        assert_eq!(auth_verify(r).unwrap_err().status, 400);
        let mut r = verify_req();
        r.device_name = "x".repeat(65);
        assert!(auth_verify(r).is_err());
        let mut r = verify_req();
        r.device_name = "a\u{7}b".into();
        assert!(auth_verify(r).is_err());
        let mut r = verify_req();
        r.challenge_id = "../../etc".into();
        assert!(auth_verify(r).is_err());
        let mut r = verify_req();
        r.code = "12ab56".into();
        assert_eq!(auth_verify(r).unwrap().code, "", "malformed codes count as wrong codes");
    }

    #[test]
    fn push_validation() {
        let ok = Change {
            kind: "item".into(),
            id: "a".into(),
            hlc: Hlc::new(1, 0, "n"),
            deleted: false,
            enc: Encoding::Json,
            payload: "{}".into(),
        };
        assert!(push(&PushRequest { changes: vec![ok.clone()], sync_epoch: None }).is_ok());
        assert!(push(&PushRequest { changes: vec![], sync_epoch: None }).is_ok());
        let too_many = PushRequest { changes: vec![ok.clone(); MAX_PUSH_CHANGES + 1], sync_epoch: None };
        assert_eq!(push(&too_many).unwrap_err().status, 413);
        let mut big = ok.clone();
        big.payload = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        assert_eq!(push(&PushRequest { changes: vec![ok.clone(), big], sync_epoch: None }).unwrap_err().status, 413);
        let mut bad = ok;
        bad.kind = "Bad".into();
        assert_eq!(push(&PushRequest { changes: vec![bad], sync_epoch: None }).unwrap_err().status, 400);
    }

    #[test]
    fn json_parsing_limits() {
        assert_eq!(parse_json::<AuthStartRequest>(b"{", 10).unwrap_err().status, 400);
        assert_eq!(parse_json::<AuthStartRequest>(&[b' '; 11], 10).unwrap_err().status, 413);
        // A bad HLC is a deserialisation error, so it is a 400 for the whole push.
        let body = br#"{"changes":[{"kind":"item","id":"a","hlc":"nope","payload":""}]}"#;
        assert_eq!(parse_json::<PushRequest>(body, MAX_PUSH_BODY_BYTES).unwrap_err().status, 400);
    }
}
