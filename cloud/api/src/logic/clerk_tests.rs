use super::*;
use crossbow_proto::error_codes;

const ISSUER: &str = "https://fixture-instance-0.clerk.accounts.dev";
const CLIENT: &str = "fixtureClient001";
const NONCE: &str = "nonce-0123456789abcdef";
/// 2026-11-01T00:00:00Z, in milliseconds.
const NOW: u64 = 1_793_491_200_000;

fn provider() -> Provider {
    Provider::from_vars(Some(ISSUER), Some(CLIENT)).unwrap()
}

/// The claims a valid ID token carries, in seconds, as JSON.
fn claims() -> serde_json::Value {
    serde_json::json!({
        "iss": ISSUER, "sub": "user_2abc", "aud": CLIENT, "azp": CLIENT,
        "exp": NOW / 1000 + 300, "iat": NOW / 1000 - 5, "nonce": NONCE,
        "email": "Ada@Example.com", "email_verified": true, "name": " Ada Fixture ",
    })
}

fn jwt(header: &serde_json::Value, claims: &serde_json::Value) -> String {
    let part = |v: &serde_json::Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
    format!("{}.{}.{}", part(header), part(claims), URL_SAFE_NO_PAD.encode([7u8; 256]))
}

fn rs256() -> serde_json::Value {
    serde_json::json!({ "alg": "RS256", "kid": "ins_key", "typ": "JWT" })
}

/// Parse and check a token built from `claims` with `change` applied.
fn checked(change: impl FnOnce(&mut serde_json::Value)) -> Result<Identity, Rejection> {
    let mut c = claims();
    change(&mut c);
    check(&parse_id_token(&jwt(&rs256(), &c))?, &provider(), NONCE, NOW)
}

fn exchange_request() -> ClerkExchangeRequest {
    ClerkExchangeRequest {
        code: "code_AbC-123.x~y".into(),
        code_verifier: "v".repeat(43),
        redirect_uri: "http://127.0.0.1:47232/callback".into(),
        nonce: NONCE.into(),
        device_name: "  Ada's Mac  ".into(),
        platform: "macos".into(),
    }
}

#[test]
fn the_provider_comes_only_from_both_vars_with_an_https_issuer() {
    let p = provider();
    assert_eq!(p.token_endpoint(), format!("{ISSUER}/oauth/token"));
    assert_eq!(p.jwks_uri(), format!("{ISSUER}/.well-known/jwks.json"));
    assert_eq!(p.public(), crossbow_proto::ClerkConfig { issuer: ISSUER.into(), client_id: CLIENT.into(), authorize_endpoint: format!("{ISSUER}/oauth/authorize") });
    // Not configured: the production Worker has neither var.
    for (issuer, client) in [(None, None), (Some(ISSUER), None), (None, Some(CLIENT)), (Some(""), Some(CLIENT)), (Some(ISSUER), Some(" "))] {
        assert!(Provider::from_vars(issuer, client).is_none(), "{issuer:?} {client:?}");
    }
    for issuer in ["http://fixture-instance-0.clerk.accounts.dev", "https://issuer.example/", "https://issuer.example/tenant", "https://issuer.example?x=1", "https://", "https://user@issuer.example", "issuer.example"] {
        assert!(Provider::from_vars(Some(issuer), Some(CLIENT)).is_none(), "{issuer}");
    }
    let unavailable = ApiError::clerk_unavailable();
    assert_eq!((unavailable.status, unavailable.code), (503, error_codes::CLERK_UNAVAILABLE));
}

#[test]
fn an_exchange_names_a_registered_loopback_address_and_well_formed_secrets() {
    let x = exchange(exchange_request()).unwrap();
    assert_eq!((x.device_name.as_str(), x.redirect_uri.as_str()), ("Ada's Mac", "http://127.0.0.1:47232/callback"));
    for uri in REDIRECT_URIS {
        assert!(exchange(ClerkExchangeRequest { redirect_uri: uri.into(), ..exchange_request() }).is_ok(), "{uri}");
    }
    let refused: [(&str, fn(&mut ClerkExchangeRequest)); 12] = [
        ("an unregistered port", |r| r.redirect_uri = "http://127.0.0.1:47234/callback".into()),
        ("another host", |r| r.redirect_uri = "http://localhost:47231/callback".into()),
        ("another path", |r| r.redirect_uri = "http://127.0.0.1:47231/callback/".into()),
        ("no code", |r| r.code = String::new()),
        ("a code with a space", |r| r.code = "a b".into()),
        ("an oversized code", |r| r.code = "c".repeat(513)),
        ("a short verifier", |r| r.code_verifier = "v".repeat(42)),
        ("a long verifier", |r| r.code_verifier = "v".repeat(129)),
        ("a verifier outside the PKCE alphabet", |r| r.code_verifier = format!("{}+", "v".repeat(43))),
        ("a short nonce", |r| r.nonce = "n".repeat(15)),
        ("a bad platform", |r| r.platform = "beos".into()),
        ("an empty device name", |r| r.device_name = " ".into()),
    ];
    for (case, change) in refused {
        let mut r = exchange_request();
        change(&mut r);
        assert_eq!(exchange(r).unwrap_err().status, 400, "{case}");
    }
}

#[test]
fn the_token_request_is_a_public_pkce_client_redeeming_its_code() {
    let x = exchange(ClerkExchangeRequest { code: "a/b+c=d&e".into(), ..exchange_request() }).unwrap();
    assert_eq!(
        token_request(&provider(), &x),
        format!("grant_type=authorization_code&code=a%2Fb%2Bc%3Dd%26e&redirect_uri=http%3A%2F%2F127.0.0.1%3A47232%2Fcallback&client_id={CLIENT}&code_verifier={}", "v".repeat(43)),
    );
    assert!(!token_request(&provider(), &x).contains("secret"), "no client secret");
}

#[test]
fn only_an_id_token_is_taken_from_the_token_response() {
    let body = br#"{"access_token":"at","refresh_token":"rt","id_token":"a.b.c","token_type":"Bearer","expires_in":3600}"#;
    assert_eq!(id_token(200, body).unwrap(), "a.b.c");
    for (status, body, expected) in [
        // A provider that fails is not a sign-in to start again.
        (200, &br#"{"access_token":"at"}"#[..], (502, error_codes::PROVIDER_UNAVAILABLE)),
        (200, &b"not json"[..], (502, error_codes::PROVIDER_UNAVAILABLE)),
        (400, &br#"{"error":"invalid_grant"}"#[..], (401, error_codes::SIGN_IN_REJECTED)),
        (401, &br#"{"error":"invalid_client"}"#[..], (401, error_codes::SIGN_IN_REJECTED)),
        (500, &b""[..], (502, error_codes::PROVIDER_UNAVAILABLE)),
    ] {
        let error = id_token(status, body).unwrap_err();
        assert_eq!((error.status, error.code), expected, "{status}");
        assert!(!error.message.contains("at") || !error.message.contains("rt"), "no token is echoed");
    }
}

#[test]
fn a_valid_id_token_gives_the_identity() {
    let identity = checked(|_| {}).unwrap();
    assert_eq!(identity, Identity {
        issuer: ISSUER.into(),
        subject: "user_2abc".into(),
        email: Some("ada@example.com".into()),
        email_verified: true,
        name: Some("Ada Fixture".into()),
        // For a step-up's age: claims are in seconds.
        issued_at_ms: NOW - 5_000,
    });
    // An audience list naming the client, no azp, and a time inside the skew.
    let identity = checked(|c| {
        c["aud"] = serde_json::json!(["other", CLIENT]);
        c.as_object_mut().unwrap().remove("azp");
        c["exp"] = serde_json::json!(NOW / 1000 - 30);
        c["iat"] = serde_json::json!(NOW / 1000 + 30);
        c.as_object_mut().unwrap().remove("name");
    }).unwrap();
    assert_eq!(identity.name, None);
}

#[test]
fn each_claim_failure_is_refused_on_its_own() {
    let cases: [(Rejection, fn(&mut serde_json::Value)); 13] = [
        (Rejection::Issuer, |c| c["iss"] = serde_json::json!("https://evil.example")),
        (Rejection::Issuer, |c| c["iss"] = serde_json::json!(format!("{ISSUER}/"))),
        (Rejection::Audience, |c| c["aud"] = serde_json::json!("other-client")),
        (Rejection::Audience, |c| c["aud"] = serde_json::json!(["a", "b"])),
        (Rejection::Audience, |c| { c.as_object_mut().unwrap().remove("aud"); }),
        (Rejection::AuthorizedParty, |c| c["azp"] = serde_json::json!("other-client")),
        (Rejection::Expired, |c| c["exp"] = serde_json::json!(NOW / 1000 - 61)),
        (Rejection::Expired, |c| { c.as_object_mut().unwrap().remove("exp"); }),
        (Rejection::IssuedInFuture, |c| c["iat"] = serde_json::json!(NOW / 1000 + 61)),
        (Rejection::Nonce, |c| c["nonce"] = serde_json::json!("nonce-fedcba9876543210")),
        (Rejection::Nonce, |c| { c.as_object_mut().unwrap().remove("nonce"); }),
        (Rejection::Subject, |c| { c.as_object_mut().unwrap().remove("sub"); }),
        (Rejection::Subject, |c| c["sub"] = serde_json::json!("")),
    ];
    for (expected, change) in cases {
        assert_eq!(checked(change).unwrap_err(), expected);
    }
    // The header's algorithm: only RS256.
    for alg in ["HS256", "none", "RS512", "ES256"] {
        let token = parse_id_token(&jwt(&serde_json::json!({ "alg": alg }), &claims())).unwrap();
        assert_eq!(check(&token, &provider(), NONCE, NOW).unwrap_err(), Rejection::Algorithm, "{alg}");
    }
    let refused = Rejection::Nonce.error();
    assert_eq!((refused.status, refused.code), (401, error_codes::SIGN_IN_REJECTED));
}

#[test]
fn a_token_that_isnt_a_signed_jwt_is_malformed() {
    let valid = jwt(&rs256(), &claims());
    let parts: Vec<&str> = valid.split('.').collect();
    for token in [
        String::new(),
        parts[..2].join("."),
        format!("{valid}.x"),
        format!("{}.{}.", parts[0], parts[1]),
        format!("!!.{}.{}", parts[1], parts[2]),
        format!("{}.{}.{}", URL_SAFE_NO_PAD.encode("not json"), parts[1], parts[2]),
        format!("{}.{}.{}", parts[0], URL_SAFE_NO_PAD.encode("[1]"), parts[2]),
        format!("{}.{}.{}", parts[0], parts[1], "s".repeat(MAX_ID_TOKEN_CHARS)),
    ] {
        assert_eq!(parse_id_token(&token).unwrap_err(), Rejection::Malformed, "{token:.40}");
    }
    let token = parse_id_token(&valid).unwrap();
    assert_eq!((token.alg.as_str(), token.kid.as_deref(), token.signature.len()), ("RS256", Some("ins_key"), 256));
    assert_eq!(token.signed, format!("{}.{}", parts[0], parts[1]));
}

#[test]
fn an_identity_signs_in_to_its_own_account_and_never_merges_by_email() {
    let identity = checked(|_| {}).unwrap();
    // Known: that account, unchanged, whatever owns the email now.
    assert_eq!(sign_in(Some("acc_1234".into()), &identity, Some("acc_9999".into())), SignIn::Account("acc_1234".into()));
    // New, with a verified email nobody uses: a new account for it.
    assert_eq!(sign_in(None, &identity, None), SignIn::Create { email: "ada@example.com".into() });
    // An account with that email exists: link from it, never merge.
    assert_eq!(sign_in(None, &identity, Some("acc_9999".into())), SignIn::LinkRequired);
    // An unverified or missing email creates nothing and says nothing about who uses it.
    let unverified = checked(|c| c["email_verified"] = serde_json::json!(false)).unwrap();
    assert_eq!(sign_in(None, &unverified, Some("acc_9999".into())), SignIn::EmailUnverified);
    assert_eq!(sign_in(None, &unverified, None), SignIn::EmailUnverified);
    let missing = checked(|c| { c.as_object_mut().unwrap().remove("email_verified"); }).unwrap();
    assert_eq!(sign_in(None, &missing, None), SignIn::EmailUnverified);
    let no_email = checked(|c| { c.as_object_mut().unwrap().remove("email"); }).unwrap();
    assert_eq!(sign_in(None, &no_email, None), SignIn::EmailUnverified);
    // Known accounts don't need the email at all.
    assert_eq!(sign_in(Some("acc_1234".into()), &no_email, None), SignIn::Account("acc_1234".into()));
    let refused = SignIn::EmailUnverified.error().unwrap();
    assert_eq!((refused.status, refused.code), (403, error_codes::EMAIL_UNVERIFIED));
    let refused = SignIn::LinkRequired.error().unwrap();
    assert_eq!((refused.status, refused.code), (409, error_codes::LINK_REQUIRED));
}

#[test]
fn a_link_binds_a_new_identity_to_the_callers_account_only() {
    assert_eq!(link(None, "acc_1234"), Link::Pending);
    assert_eq!(link(Some("acc_1234"), "acc_1234"), Link::Already);
    assert_eq!(link(Some("acc_9999"), "acc_1234"), Link::InUse);
    let refused = ApiError::identity_in_use();
    assert_eq!((refused.status, refused.code), (409, error_codes::IDENTITY_IN_USE));
}

#[test]
fn the_signing_key_is_the_issuers_rsa_key_with_the_tokens_kid() {
    let jwks: Jwks = serde_json::from_value(serde_json::json!({ "keys": [
        { "kty": "RSA", "kid": "old", "alg": "RS256", "use": "sig", "n": "n-old", "e": "AQAB" },
        { "kty": "RSA", "kid": "new", "n": "n-new", "e": "AQAB" },
        { "kty": "EC", "kid": "ec", "crv": "P-256", "x": "x", "y": "y" },
        { "kty": "RSA", "kid": "enc", "use": "enc", "n": "n-enc", "e": "AQAB" },
        { "kty": "RSA", "kid": "hs", "alg": "RS512", "n": "n-hs", "e": "AQAB" },
    ] })).unwrap();
    // A rotated key set: each token finds its own key.
    assert_eq!(signing_key(&jwks, Some("old")).map(|k| k.n.as_str()), Some("n-old"));
    assert_eq!(signing_key(&jwks, Some("new")).map(|k| k.n.as_str()), Some("n-new"));
    for kid in [Some("ec"), Some("enc"), Some("hs"), Some("gone")] {
        assert!(signing_key(&jwks, kid).is_none(), "{kid:?}");
    }
    // Without a kid, only a set with one usable key will do.
    assert!(signing_key(&jwks, None).is_none());
    let one: Jwks = serde_json::from_value(serde_json::json!({ "keys": [{ "kty": "RSA", "n": "n-one", "e": "AQAB" }] })).unwrap();
    assert_eq!(signing_key(&one, None).map(|k| k.n.as_str()), Some("n-one"));
}

#[test]
fn a_link_names_the_identity_by_its_email_only_when_verified() {
    let identity = checked(|_| {}).unwrap();
    assert_eq!(identity.verified_email(), Some("ada@example.com"));
    let unverified = checked(|c| c["email_verified"] = serde_json::json!(false)).unwrap();
    assert_eq!(unverified.email.as_deref(), Some("ada@example.com"), "the claim is read");
    assert_eq!(unverified.verified_email(), None, "but never shown or kept");
    let unsaid = checked(|c| { c.as_object_mut().unwrap().remove("email_verified"); }).unwrap();
    assert_eq!(unsaid.verified_email(), None, "no claim is not verified");
}
