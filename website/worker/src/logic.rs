//! Pure validation and provider contracts, independently testable without Workers.

use serde::{Deserialize, Serialize};
use url::Url;

pub const MAX_BODY_BYTES: usize = 4_096;
pub const MAX_EMAIL_BYTES: usize = 254;
pub const MAX_TOKEN_BYTES: usize = 2_048;
pub const MAX_PROVIDER_BYTES: usize = 16_384;
pub const SUCCESS_MESSAGE: &str = "You're on the waitlist. Thanks for your interest in Crossbow.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteError {
    pub status: u16,
    pub message: &'static str,
    pub retry_after: Option<u16>,
}

impl SiteError {
    pub const UNAVAILABLE: Self = Self {
        status: 503,
        message: "The waitlist is not available right now. Please try again later.",
        retry_after: Some(60),
    };
    pub const INVALID: Self = Self {
        status: 400,
        message: "We couldn't accept that request. Please refresh and try again.",
        retry_after: None,
    };
    pub const EMAIL: Self = Self {
        status: 400,
        message: "Enter a valid email address, such as you@example.com.",
        retry_after: None,
    };
    pub const VERIFICATION: Self = Self {
        status: 400,
        message: "Verification expired or failed. Please try again.",
        retry_after: None,
    };
    pub const FORBIDDEN: Self = Self {
        status: 403,
        message: "Submit the form from this website. Please refresh and try again.",
        retry_after: None,
    };
    pub const TOO_LARGE: Self = Self {
        status: 413,
        message: "That request is too large.",
        retry_after: None,
    };
    pub const CONTENT_TYPE: Self = Self {
        status: 415,
        message: "Send this request as application/json.",
        retry_after: None,
    };
    pub const RATE_LIMITED: Self = Self {
        status: 429,
        message: "Too many attempts. Wait a minute, then try again.",
        retry_after: Some(60),
    };
}

// Intentionally not Debug: callers must never log these credentials.
pub struct Settings<'a> {
    pub enabled: &'a str,
    pub environment: &'a str,
    pub site_origin: &'a str,
    pub site_key: &'a str,
    pub clerk_secret: &'a str,
    pub turnstile_secret: &'a str,
    pub privacy_contact: &'a str,
    pub has_rate_limiter: bool,
}

impl Settings<'_> {
    pub fn ready(&self) -> bool {
        let key_prefix = match self.environment {
            "preview" => "sk_test_",
            "production" => "sk_live_",
            _ => return false,
        };
        let Some(origin) = parse_site_origin(self.site_origin) else {
            return false;
        };
        self.enabled == "true"
            && (self.environment != "production" || origin.scheme() == "https")
            && self.has_rate_limiter
            && self.clerk_secret.starts_with(key_prefix)
            && self.clerk_secret.len() > key_prefix.len()
            && safe_config_value(self.clerk_secret)
            && safe_config_value(self.site_key)
            && safe_config_value(self.turnstile_secret)
            && normalize_email(self.privacy_contact).is_some()
            && !dummy_site_key(self.site_key)
            && !dummy_secret(self.turnstile_secret)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfig {
    pub enabled: bool,
    pub turnstile_site_key: Option<String>,
    pub privacy_contact: Option<String>,
}

pub fn public_config(active_site_key: Option<&str>, privacy_contact: &str) -> PublicConfig {
    let privacy_contact = normalize_email(privacy_contact);
    let site_key = active_site_key.filter(|_| privacy_contact.is_some());
    PublicConfig {
        enabled: site_key.is_some(),
        turnstile_site_key: site_key.map(str::to_owned),
        // Keep removal contact available after enrollment is switched off.
        privacy_contact,
    }
}

fn safe_config_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn dummy_site_key(value: &str) -> bool {
    matches!(
        value,
        "1x00000000000000000000AA"
            | "2x00000000000000000000AB"
            | "1x00000000000000000000BB"
            | "2x00000000000000000000BB"
            | "3x00000000000000000000FF"
    )
}

fn dummy_secret(value: &str) -> bool {
    matches!(
        value,
        "1x0000000000000000000000000000000AA"
            | "2x0000000000000000000000000000000AA"
            | "3x0000000000000000000000000000000AA"
    )
}

pub fn parse_site_origin(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || value != url.origin().ascii_serialization()
        || matches!(host, "legacy.example" | "www.legacy.example")
    {
        return None;
    }
    Some(url)
}

pub fn same_site_url(request: &Url, site: &Url) -> bool {
    request.username().is_empty()
        && request.password().is_none()
        && request.origin() == site.origin()
}

pub fn valid_submit_context(
    request: &Url,
    origin: Option<&str>,
    fetch_site: Option<&str>,
    site: &Url,
) -> bool {
    same_site_url(request, site)
        && origin == Some(site.origin().ascii_serialization().as_str())
        && matches!(fetch_site, None | Some("same-origin"))
}

pub fn legacy_redirect(request: &Url) -> Option<Url> {
    if !matches!(
        request.host_str(),
        Some("legacy.example" | "www.legacy.example")
    ) {
        return None;
    }
    // Change the authority, never resolve an untrusted path against the target.
    let mut target = request.clone();
    target.set_scheme("https").ok()?;
    target.set_host(Some("crossbow.zone")).ok()?;
    target.set_port(None).ok()?;
    target.set_username("").ok()?;
    target.set_password(None).ok()?;
    target.set_fragment(None);
    Some(target)
}

pub fn validate_content_type(content_type: Option<&str>) -> Result<(), SiteError> {
    let mut parts = content_type.unwrap_or_default().split(';');
    if !parts
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(SiteError::CONTENT_TYPE);
    }
    let params: Vec<&str> = parts.collect();
    if params.len() > 1
        || params
            .first()
            .is_some_and(|p| !p.trim().eq_ignore_ascii_case("charset=utf-8"))
    {
        return Err(SiteError::CONTENT_TYPE);
    }
    Ok(())
}

pub fn validate_content_length(length: Option<&str>) -> Result<(), SiteError> {
    if let Some(length) = length {
        if length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SiteError::INVALID);
        }
        let value = length.parse::<usize>().map_err(|_| SiteError::TOO_LARGE)?;
        if value > MAX_BODY_BYTES {
            return Err(SiteError::TOO_LARGE);
        }
    }
    Ok(())
}

pub fn append_bounded(bytes: &mut Vec<u8>, chunk: &[u8], limit: usize) -> bool {
    if chunk.len() > limit.saturating_sub(bytes.len()) {
        return false;
    }
    bytes.extend_from_slice(chunk);
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Submission {
    email: String,
    turnstile_token: String,
    website: String,
}

// Intentionally not Debug: these values contain user input.
pub struct ValidSubmission {
    pub email: String,
    pub turnstile_token: String,
}

pub fn parse_submission(body: &[u8]) -> Result<ValidSubmission, SiteError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(SiteError::TOO_LARGE);
    }
    let input: Submission = serde_json::from_slice(body).map_err(|_| SiteError::INVALID)?;
    if !input.website.is_empty() {
        // A honeypot does not simulate a successful signup or write to Clerk.
        return Err(SiteError::INVALID);
    }
    let email = normalize_email(&input.email).ok_or(SiteError::EMAIL)?;
    if input.turnstile_token.is_empty()
        || input.turnstile_token.len() > MAX_TOKEN_BYTES
        || !input.turnstile_token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(SiteError::VERIFICATION);
    }
    Ok(ValidSubmission {
        email,
        turnstile_token: input.turnstile_token,
    })
}

pub fn normalize_email(input: &str) -> Option<String> {
    if input.len() > MAX_EMAIL_BYTES || !input.is_ascii() {
        return None;
    }
    let input = input.trim_matches(' ');
    let (local, domain) = input.split_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~.".contains(&b))
        || domain.len() > 253
        || !domain.contains('.')
    {
        return None;
    }
    if domain.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) || !domain
        .rsplit('.')
        .next()?
        .bytes()
        .any(|b| b.is_ascii_alphabetic())
    {
        return None;
    }
    Some(format!("{local}@{}", domain.to_ascii_lowercase()))
}

#[derive(Serialize)]
pub struct ClerkCreate<'a> {
    pub email_address: &'a str,
    pub notify: bool,
}

#[derive(Deserialize)]
struct ClerkEntry {
    id: String,
    email_address: String,
}

pub fn confirm_clerk_entry(status: u16, body: &[u8], email: &str) -> Result<(), SiteError> {
    if !matches!(status, 200 | 201) || body.len() > MAX_PROVIDER_BYTES {
        return Err(SiteError::UNAVAILABLE);
    }
    let entry: ClerkEntry = serde_json::from_slice(body).map_err(|_| SiteError::UNAVAILABLE)?;
    if entry.id.is_empty()
        || entry.id.len() > 256
        || !entry.email_address.eq_ignore_ascii_case(email)
    {
        return Err(SiteError::UNAVAILABLE);
    }
    Ok(())
}

#[derive(Deserialize)]
struct TurnstileResult {
    success: bool,
    hostname: Option<String>,
    action: Option<String>,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

pub fn confirm_turnstile(status: u16, body: &[u8], hostname: &str) -> Result<(), SiteError> {
    if status != 200 || body.len() > MAX_PROVIDER_BYTES {
        return Err(SiteError::UNAVAILABLE);
    }
    let result: TurnstileResult =
        serde_json::from_slice(body).map_err(|_| SiteError::UNAVAILABLE)?;
    if result.error_codes.iter().any(|e| {
        matches!(
            e.as_str(),
            "internal-error" | "missing-input-secret" | "invalid-input-secret"
        )
    }) {
        return Err(SiteError::UNAVAILABLE);
    }
    if !result.success
        || !result.error_codes.is_empty()
        || result.hostname.as_deref() != Some(hostname)
        || result.action.as_deref() != Some("waitlist")
    {
        return Err(SiteError::VERIFICATION);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings() -> Settings<'static> {
        Settings {
            enabled: "true",
            environment: "preview",
            site_origin: "https://crossbow.zone",
            site_key: "0xFixtureSiteKey",
            clerk_secret: "sk_test_fixture_not_a_real_credential",
            turnstile_secret: "0xFixtureSecret",
            privacy_contact: "privacy@example.com",
            has_rate_limiter: true,
        }
    }

    fn payload(email: &str, token: &str, website: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"email":email,"turnstileToken":token,"website":website}))
            .unwrap()
    }

    #[test]
    fn config_requires_explicit_enable_and_all_credentials() {
        assert!(settings().ready());
        for enabled in ["", "false", "TRUE", "1", " true"] {
            assert!(
                !Settings {
                    enabled,
                    ..settings()
                }
                .ready()
            );
        }
        assert!(
            !Settings {
                site_key: "",
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                clerk_secret: "",
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                turnstile_secret: "",
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                has_rate_limiter: false,
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                clerk_secret: "sk_test_",
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                clerk_secret: "sk_test_bad\nvalue",
                ..settings()
            }
            .ready()
        );
    }

    #[test]
    fn privacy_contact_is_required_and_must_be_a_valid_email() {
        for privacy_contact in [
            "",
            "not an email",
            "mailto:privacy@example.com",
            "privacy@example.com\r\nBcc:other@example.com",
            "privacy@example.com?subject=remove",
        ] {
            assert!(
                !Settings {
                    privacy_contact,
                    ..settings()
                }
                .ready()
            );
        }
        assert!(
            Settings {
                privacy_contact: "Privacy@EXAMPLE.COM",
                ..settings()
            }
            .ready()
        );
    }

    #[test]
    fn public_config_is_minimal_and_keeps_removal_contact_while_closed() {
        let closed = serde_json::to_value(public_config(None, "")).unwrap();
        assert_eq!(
            closed,
            json!({"enabled":false,"turnstileSiteKey":null,"privacyContact":null})
        );
        let closed_with_contact =
            serde_json::to_value(public_config(None, "Privacy@EXAMPLE.COM")).unwrap();
        assert_eq!(
            closed_with_contact,
            json!({"enabled":false,"turnstileSiteKey":null,"privacyContact":"Privacy@example.com"})
        );
        let missing_contact =
            serde_json::to_value(public_config(Some("0xFixtureSiteKey"), "")).unwrap();
        assert_eq!(missing_contact, closed);
        let open = serde_json::to_value(public_config(
            Some("0xFixtureSiteKey"),
            "privacy@example.com",
        ))
        .unwrap();
        assert_eq!(
            open,
            json!({"enabled":true,"turnstileSiteKey":"0xFixtureSiteKey","privacyContact":"privacy@example.com"})
        );
    }

    #[test]
    fn config_separates_preview_and_production() {
        assert!(
            !Settings {
                environment: "production",
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                clerk_secret: "sk_live_fixture",
                ..settings()
            }
            .ready()
        );
        assert!(
            !Settings {
                environment: "unknown",
                ..settings()
            }
            .ready()
        );
        let prod = Settings {
            environment: "production",
            clerk_secret: "sk_live_fixture",
            ..settings()
        };
        assert!(prod.ready());
        assert!(
            !Settings {
                site_origin: "http://localhost:8798",
                ..prod
            }
            .ready()
        );
    }

    #[test]
    fn dummy_captcha_keys_cannot_enable_collection() {
        for site_key in [
            "1x00000000000000000000AA",
            "2x00000000000000000000AB",
            "1x00000000000000000000BB",
            "2x00000000000000000000BB",
            "3x00000000000000000000FF",
        ] {
            assert!(
                !Settings {
                    site_key,
                    ..settings()
                }
                .ready()
            );
        }
        for turnstile_secret in [
            "1x0000000000000000000000000000000AA",
            "2x0000000000000000000000000000000AA",
            "3x0000000000000000000000000000000AA",
        ] {
            assert!(
                !Settings {
                    turnstile_secret,
                    ..settings()
                }
                .ready()
            );
        }
    }

    #[test]
    fn origin_config_is_an_exact_secure_origin() {
        for good in [
            "https://crossbow.zone",
            "https://preview.example.com",
            "http://127.0.0.1:8798",
            "http://localhost:8798",
            "http://[::1]:8798",
        ] {
            assert!(parse_site_origin(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "null",
            "http://crossbow.zone",
            "https://crossbow.zone/",
            "https://crossbow.zone/path",
            "https://crossbow.zone?x=1",
            "https://crossbow.zone#x",
            "https://user@crossbow.zone",
            "https://legacy.example",
            "https://www.legacy.example",
            "https://crossbow.zone:443",
            "https://CROSSBOW.zone",
            "http://192.168.1.1",
        ] {
            assert!(parse_site_origin(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn submit_requires_exact_same_origin_not_merely_same_site() {
        let site = parse_site_origin("https://crossbow.zone").unwrap();
        let request = site.join("/api/waitlist").unwrap();
        assert!(valid_submit_context(
            &request,
            Some("https://crossbow.zone"),
            Some("same-origin"),
            &site
        ));
        assert!(valid_submit_context(
            &request,
            Some("https://crossbow.zone"),
            None,
            &site
        ));
        for origin in [
            None,
            Some("null"),
            Some("https://www.crossbow.zone"),
            Some("https://crossbow.zone.evil.test"),
            Some("https://crossbow.zone/"),
            Some("https://crossbow.zone@evil.test"),
        ] {
            assert!(!valid_submit_context(
                &request,
                origin,
                Some("same-origin"),
                &site
            ));
        }
        for fetch_site in ["cross-site", "same-site", "none", ""] {
            assert!(!valid_submit_context(
                &request,
                Some("https://crossbow.zone"),
                Some(fetch_site),
                &site
            ));
        }
        let forged = Url::parse("https://evil.test/api/waitlist").unwrap();
        assert!(!valid_submit_context(
            &forged,
            Some("https://crossbow.zone"),
            None,
            &site
        ));
    }

    #[test]
    fn redirects_keep_path_query_but_never_an_attacker_authority() {
        for host in ["legacy.example", "www.legacy.example"] {
            let original = Url::parse(&format!(
                "http://{host}:8080//evil.test/%2f?q=https%3A%2F%2Fevil.test#fragment"
            ))
            .unwrap();
            let redirect = legacy_redirect(&original).unwrap();
            assert_eq!(
                redirect.as_str(),
                "https://crossbow.zone//evil.test/%2f?q=https%3A%2F%2Fevil.test"
            );
        }
        assert!(legacy_redirect(&Url::parse("https://legacy.example.evil.test/").unwrap()).is_none());
        assert!(legacy_redirect(&Url::parse("https://crossbow.zone/").unwrap()).is_none());
    }

    #[test]
    fn content_type_is_json_only() {
        for good in [
            "application/json",
            "Application/JSON",
            "application/json; charset=UTF-8",
        ] {
            assert!(validate_content_type(Some(good)).is_ok());
        }
        for bad in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
            Some("application/json; charset=latin1"),
            Some("application/json; charset=utf-8; charset=utf-8"),
        ] {
            assert_eq!(validate_content_type(bad), Err(SiteError::CONTENT_TYPE));
        }
    }

    #[test]
    fn length_header_is_not_the_only_body_limit() {
        assert!(validate_content_length(None).is_ok());
        assert!(validate_content_length(Some("4096")).is_ok());
        assert_eq!(
            validate_content_length(Some("4097")),
            Err(SiteError::TOO_LARGE)
        );
        assert_eq!(
            validate_content_length(Some("999999999999999999999999999999999")),
            Err(SiteError::TOO_LARGE)
        );
        for bad in ["-1", "1,2", "1.0", "", "a"] {
            assert_eq!(validate_content_length(Some(bad)), Err(SiteError::INVALID));
        }
        let mut data = vec![0; MAX_BODY_BYTES - 1];
        assert!(append_bounded(&mut data, &[0], MAX_BODY_BYTES));
        assert!(!append_bounded(&mut data, &[0], MAX_BODY_BYTES));
        assert_eq!(data.len(), MAX_BODY_BYTES);
        assert!(matches!(
            parse_submission(&vec![b' '; MAX_BODY_BYTES + 1]),
            Err(SiteError::TOO_LARGE)
        ));
    }

    #[test]
    fn email_normalization_preserves_local_part() {
        assert_eq!(
            normalize_email(" Person+tag@EXAMPLE.COM ").as_deref(),
            Some("Person+tag@example.com")
        );
        assert!(normalize_email("o'connor@sub.example.com").is_some());
        assert!(normalize_email("user@xn--bcher-kva.example").is_some());
    }

    #[test]
    fn malformed_and_unsupported_emails_are_rejected() {
        for bad in [
            "",
            "a",
            "@example.com",
            "a@",
            "a@@example.com",
            "a b@example.com",
            ".a@example.com",
            "a.@example.com",
            "a..b@example.com",
            "a@localhost",
            "a@.example.com",
            "a@example..com",
            "a@example.com.",
            "a@-example.com",
            "a@example-.com",
            "a@exa_mple.com",
            "a@127.0.0.1",
            "a\n@example.com",
            "a@example.com\r\nBcc:x@y.com",
            "a@éxample.com",
            "ü@example.com",
            "\"a b\"@example.com",
        ] {
            assert!(normalize_email(bad).is_none(), "{bad:?}");
        }
        assert!(normalize_email(&format!("{}@example.com", "x".repeat(64))).is_some());
        assert!(normalize_email(&format!("{}@example.com", "x".repeat(65))).is_none());
        assert!(normalize_email(&format!("x@{}.com", "d".repeat(64))).is_none());
    }

    #[test]
    fn email_total_length_boundary_is_enforced() {
        let max = format!(
            "{}@{}.{}.{}.com",
            "x".repeat(64),
            "d".repeat(63),
            "d".repeat(63),
            "d".repeat(57)
        );
        assert_eq!(max.len(), 254);
        assert!(normalize_email(&max).is_some());
        assert!(normalize_email(&max.replace(".com", "d.com")).is_none());
    }

    #[test]
    fn submission_is_strict_and_honeypot_never_reports_success() {
        assert!(parse_submission(&payload("person@example.com", "token", "")).is_ok());
        for body in [br#"{}"#.as_slice(), br#"null"#, br#"[]"#, br#"{"email":123,"turnstileToken":"token","website":""}"#, br#"{"email":"a@example.com","email":"b@example.com","turnstileToken":"token","website":""}"#, br#"{"email":"a@example.com","turnstileToken":"token","website":"","extra":true}"#] {
            assert!(matches!(parse_submission(body), Err(SiteError::INVALID)));
        }
        assert!(matches!(
            parse_submission(&payload("person@example.com", "token", "https://bot.test")),
            Err(SiteError::INVALID)
        ));
        assert!(matches!(
            parse_submission(&payload("invalid", "token", "")),
            Err(SiteError::EMAIL)
        ));
    }

    #[test]
    fn token_boundary_and_character_validation() {
        assert!(parse_submission(&payload("person@example.com", &"x".repeat(2048), "")).is_ok());
        for token in [
            "".to_owned(),
            "x".repeat(2049),
            "has space".to_owned(),
            "line\nbreak".to_owned(),
            "é".to_owned(),
        ] {
            assert!(matches!(
                parse_submission(&payload("person@example.com", &token, "")),
                Err(SiteError::VERIFICATION)
            ));
        }
    }

    #[test]
    fn clerk_http_payload_uses_snake_case_without_notification() {
        let body = serde_json::to_value(ClerkCreate {
            email_address: "person@example.com",
            notify: false,
        })
        .unwrap();
        assert_eq!(
            body,
            json!({"email_address":"person@example.com","notify":false})
        );
        assert!(body.get("emailAddress").is_none());
    }

    #[test]
    fn clerk_new_and_existing_records_have_the_same_success() {
        let body =
            br#"{"id":"wle_fixture","email_address":"Person@example.com","status":"pending"}"#;
        assert_eq!(confirm_clerk_entry(201, body, "person@example.com"), Ok(()));
        assert_eq!(confirm_clerk_entry(200, body, "person@example.com"), Ok(()));
        for status in [202, 204, 301, 400, 401, 403, 409, 422, 429, 500, 503] {
            assert_eq!(
                confirm_clerk_entry(status, body, "person@example.com"),
                Err(SiteError::UNAVAILABLE)
            );
        }
    }

    #[test]
    fn clerk_malformed_or_mismatched_success_is_not_a_signup() {
        for body in [
            br#"{}"#.as_slice(),
            br#"{"id":"","email_address":"person@example.com"}"#,
            br#"{"id":"wle_fixture","emailAddress":"person@example.com"}"#,
            br#"{"id":"wle_fixture","email_address":"someoneelse@example.com"}"#,
            b"not json",
        ] {
            assert_eq!(
                confirm_clerk_entry(200, body, "person@example.com"),
                Err(SiteError::UNAVAILABLE)
            );
        }
    }

    #[test]
    fn turnstile_requires_success_exact_hostname_and_action() {
        let valid = br#"{"success":true,"hostname":"crossbow.zone","action":"waitlist"}"#;
        assert_eq!(confirm_turnstile(200, valid, "crossbow.zone"), Ok(()));
        assert_eq!(
            confirm_turnstile(200, valid, "preview.example.com"),
            Err(SiteError::VERIFICATION)
        );
        for body in [
            br#"{"success":false}"#.as_slice(),
            br#"{"success":true}"#,
            br#"{"success":true,"hostname":"crossbow.zone","action":"login"}"#,
            br#"{"success":true,"hostname":"crossbow.zone.evil.test","action":"waitlist"}"#,
            br#"{"success":true,"hostname":"crossbow.zone","action":"Waitlist"}"#,
            br#"{"success":false,"error-codes":["timeout-or-duplicate"]}"#,
        ] {
            assert_eq!(
                confirm_turnstile(200, body, "crossbow.zone"),
                Err(SiteError::VERIFICATION)
            );
        }
    }

    #[test]
    fn turnstile_provider_errors_are_retryable_not_false_success() {
        for code in [
            "internal-error",
            "invalid-input-secret",
            "missing-input-secret",
        ] {
            let body = serde_json::to_vec(&json!({"success":false,"error-codes":[code]})).unwrap();
            assert_eq!(
                confirm_turnstile(200, &body, "crossbow.zone"),
                Err(SiteError::UNAVAILABLE)
            );
        }
        assert_eq!(
            confirm_turnstile(500, b"{}", "crossbow.zone"),
            Err(SiteError::UNAVAILABLE)
        );
        assert_eq!(
            confirm_turnstile(200, b"not json", "crossbow.zone"),
            Err(SiteError::UNAVAILABLE)
        );
    }
}
