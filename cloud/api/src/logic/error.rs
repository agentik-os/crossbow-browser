//! API errors and their wire body `{"error":{"code":"…","message":"…"}}`.

use crossbow_proto::{ErrorBody, ErrorDetail, ProtoError, error_codes};

/// An error that maps to one HTTP status and one `error_codes` value. Messages
/// are fixed strings chosen by the server: they never echo secrets or codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, error_codes::BAD_REQUEST, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(401, error_codes::UNAUTHORIZED, "missing or invalid session")
    }

    pub fn invalid_code() -> Self {
        Self::new(401, error_codes::INVALID_CODE, "the code is not valid for this sign-in")
    }

    pub fn code_expired() -> Self {
        Self::new(401, error_codes::CODE_EXPIRED, "the code has expired; start again")
    }

    pub fn too_many_attempts() -> Self {
        Self::new(429, error_codes::TOO_MANY_ATTEMPTS, "too many attempts; start again")
    }

    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::new(429, error_codes::RATE_LIMITED, message)
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(413, error_codes::PAYLOAD_TOO_LARGE, message)
    }

    pub fn not_found() -> Self {
        Self::new(404, error_codes::NOT_FOUND, "not found")
    }

    pub fn mail_unavailable() -> Self {
        Self::new(503, error_codes::MAIL_UNAVAILABLE, "sign-in mail is unavailable right now")
    }

    pub fn sync_epoch_changed() -> Self {
        Self::new(409, error_codes::SYNC_EPOCH_CHANGED, "this account's sync data was deleted since this device last synced")
    }

    pub fn clerk_unavailable() -> Self {
        Self::new(503, error_codes::CLERK_UNAVAILABLE, "sign-in with Clerk isn't available on this server")
    }

    pub fn identity_in_use() -> Self {
        Self::new(409, error_codes::IDENTITY_IN_USE, "this browser sign-in is linked to another account")
    }

    pub fn internal() -> Self {
        Self::new(500, error_codes::INTERNAL, "internal error")
    }

    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: ErrorDetail { code: self.code.to_string(), message: self.message.clone() },
        }
    }

    pub fn body_json(&self) -> String {
        serde_json::to_string(&self.body()).unwrap_or_else(|_| {
            r#"{"error":{"code":"internal","message":"internal error"}}"#.to_string()
        })
    }
}

impl From<ProtoError> for ApiError {
    fn from(e: ProtoError) -> Self {
        match e {
            ProtoError::PayloadTooLarge => ApiError::payload_too_large(e.to_string()),
            _ => ApiError::bad_request(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shape_matches_spec() {
        let v: serde_json::Value =
            serde_json::from_str(&ApiError::invalid_code().body_json()).unwrap();
        assert_eq!(v["error"]["code"], "invalid_code");
        assert!(v["error"]["message"].is_string());
        assert_eq!(v.as_object().unwrap().len(), 1);
    }

    #[test]
    fn proto_errors_map_to_400_or_413() {
        assert_eq!(ApiError::from(ProtoError::PayloadTooLarge).status, 413);
        assert_eq!(ApiError::from(ProtoError::PayloadTooLarge).code, "payload_too_large");
        assert_eq!(ApiError::from(ProtoError::InvalidKind).status, 400);
        assert_eq!(ApiError::from(ProtoError::InvalidHlc).code, "bad_request");
    }
}
