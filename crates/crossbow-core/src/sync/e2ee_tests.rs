use super::*;
use crate::store::Store;
use crate::sync::key_store::MemoryKeyStore;
use crate::sync::test_secrets::{Hidden, failure};

const PLAINTEXT: &str = r#"{"name":"Fixture"}"#;
/// Regression vector for envelope v1 (pins layout, HKDF info, AAD encoding).
/// The AEAD itself is anchored by the published XChaCha20-Poly1305 vector below.
const ENVELOPE: &str = "AQAAAAFAQUJDREVGR0hJSktMTU5PUFFSU1RVVlerenqJZ5viPUlR50Toag2NtxHuY75d1E4J5cPkBjG9DtTi";
const CODE: Hidden<&str> = Hidden::new("CB1-AAAQ-EAYE-AUDA-OCAJ-BIFQ-YDIO-B4IB-CEQT-CQKR-MFYY-DENB-WHA5-DYPT-2RPE");

fn fixed_key() -> LibraryKey {
    LibraryKey::from_slice(&(0u8..32).collect::<Vec<_>>()).unwrap()
}

fn fixed_nonce() -> [u8; NONCE_LEN] {
    std::array::from_fn(|i| 0x40 + i as u8)
}

fn hlc() -> Hlc {
    Hlc::new(1_700_000_000_000, 7, "fixturenode")
}

fn bind(hlc: &Hlc) -> Binding<'_> {
    Binding { account_id: "fixture-account", kind: "space", id: "fixture-space", hlc }
}

fn hex(text: &str) -> Vec<u8> {
    data_encoding::HEXLOWER.decode(text.replace(' ', "").as_bytes()).unwrap()
}

fn reencode(mut envelope: Vec<u8>, edit: impl FnOnce(&mut Vec<u8>)) -> String {
    edit(&mut envelope);
    BASE64URL_NOPAD.encode(&envelope)
}

#[test]
fn xchacha20poly1305_matches_the_published_draft_vector() {
    // draft-irtf-cfrg-xchacha-03, appendix A.3.1.
    let key = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce = hex("404142434445464748494a4b4c4d4e4f5051525354555657");
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let expected = hex("bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b4522f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff921f9664c97637da9768812f615c68b13b52e c0875924c1c7987947deafd8780acf49");
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let sealed = cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: &aad }).unwrap();
    assert_eq!(sealed, expected);
}

#[test]
fn hkdf_sha256_matches_rfc5869_case_1() {
    let mut okm = [0u8; 42];
    Hkdf::<Sha256>::new(Some(&hex("000102030405060708090a0b0c")), &[0x0b; 22]).expand(&hex("f0f1f2f3f4f5f6f7f8f9"), &mut okm).unwrap();
    assert_eq!(okm.to_vec(), hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"));
}

#[test]
fn envelope_v1_fixed_vector_and_layout() {
    let key = fixed_key().library_cipher();
    let hlc = hlc();
    assert_eq!(seal_with_nonce(&key, &bind(&hlc), PLAINTEXT, &fixed_nonce()).unwrap(), ENVELOPE);
    assert_eq!(open(&key, &bind(&hlc), ENVELOPE).unwrap(), PLAINTEXT);
    let bytes = BASE64URL_NOPAD.decode(ENVELOPE.as_bytes()).unwrap();
    assert_eq!(bytes[0], ENVELOPE_VERSION);
    assert_eq!(bytes[1..5], LIBRARY_EPOCH.to_be_bytes());
    assert_eq!(bytes[5..HEADER_LEN], fixed_nonce());
    assert_eq!(bytes.len(), HEADER_LEN + PLAINTEXT.len() + TAG_LEN);
    assert!(!ENVELOPE.contains('=') && !ENVELOPE.contains('+') && !ENVELOPE.contains('/'));
}

#[test]
fn round_trip_uses_a_fresh_nonce_every_time() {
    let key = LibraryKey::generate().unwrap().library_cipher();
    let hlc = hlc();
    let first = seal(&key, &bind(&hlc), PLAINTEXT).unwrap();
    let second = seal(&key, &bind(&hlc), PLAINTEXT).unwrap();
    assert_ne!(first, second);
    assert_eq!(open(&key, &bind(&hlc), &first).unwrap(), PLAINTEXT);
    assert_eq!(open(&key, &bind(&hlc), &second).unwrap(), PLAINTEXT);
    assert_eq!(open(&key, &bind(&hlc), &seal(&key, &bind(&hlc), "").unwrap()).unwrap(), "");
}

#[test]
fn every_bound_field_substitution_fails_closed() {
    let key = fixed_key().library_cipher();
    let hlc = hlc();
    let other_wall = Hlc::new(hlc.wall_ms + 1, hlc.counter, hlc.node.clone());
    let other_counter = Hlc::new(hlc.wall_ms, hlc.counter + 1, hlc.node.clone());
    let other_node = Hlc::new(hlc.wall_ms, hlc.counter, "othernode");
    let cases = [
        Binding { account_id: "other-account", ..bind(&hlc) },
        Binding { kind: "folder", ..bind(&hlc) },
        Binding { id: "other-space", ..bind(&hlc) },
        bind(&other_wall),
        bind(&other_counter),
        bind(&other_node),
    ];
    // Sealed here, not the pinned vector, so a field dropped from the AAD on
    // both sides still fails this test.
    let sealed = seal(&key, &bind(&hlc), PLAINTEXT).unwrap();
    assert_eq!(open(&key, &bind(&hlc), &sealed).unwrap(), PLAINTEXT);
    for binding in cases {
        assert_eq!(open(&key, &binding, &sealed), Err(E2eeError::Integrity), "substituted binding opened: {binding:?}");
    }
    // The epoch is authenticated as well as checked in the header.
    assert_ne!(aad(&bind(&hlc), 1), aad(&bind(&hlc), 2));
    // Length prefixes keep field boundaries unambiguous.
    let joined = Binding { account_id: "fixture-accountspace", kind: "", ..bind(&hlc) };
    assert_ne!(aad(&joined, 1), aad(&bind(&hlc), 1));
    assert_eq!(open(&key, &joined, &sealed), Err(E2eeError::Integrity));
}

#[test]
fn every_bit_flip_truncation_and_wrong_key_fails_closed() {
    let key = fixed_key().library_cipher();
    let hlc = hlc();
    let bytes = BASE64URL_NOPAD.decode(ENVELOPE.as_bytes()).unwrap();
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let tampered = reencode(bytes.clone(), |b| b[index] ^= 1 << bit);
            let error = open(&key, &bind(&hlc), &tampered).unwrap_err();
            let expected = match index {
                0 => E2eeError::UnsupportedVersion(bytes[0] ^ 1 << bit),
                1..=4 => E2eeError::UnknownEpoch(u32::from_be_bytes(BASE64URL_NOPAD.decode(tampered.as_bytes()).unwrap()[1..5].try_into().unwrap())),
                _ => E2eeError::Integrity,
            };
            assert_eq!(error, expected, "byte {index} bit {bit}");
        }
    }
    for len in 0..bytes.len() {
        let truncated = reencode(bytes[..len].to_vec(), |_| {});
        let error = open(&key, &bind(&hlc), &truncated).unwrap_err();
        assert!(matches!(error, E2eeError::Truncated | E2eeError::Integrity), "{len}-byte prefix: {error:?}");
    }
    let appended = reencode(bytes.clone(), |b| b.push(0));
    assert_eq!(open(&key, &bind(&hlc), &appended), Err(E2eeError::Integrity));
    let wrong = LibraryKey::from_slice(&[0x55; 32]).unwrap().library_cipher();
    assert_eq!(open(&wrong, &bind(&hlc), ENVELOPE), Err(E2eeError::Integrity));
}

#[test]
fn unknown_version_epoch_and_malformed_text_are_rejected() {
    let key = fixed_key().library_cipher();
    let hlc = hlc();
    let bytes = BASE64URL_NOPAD.decode(ENVELOPE.as_bytes()).unwrap();
    assert_eq!(open(&key, &bind(&hlc), &reencode(bytes.clone(), |b| b[0] = 2)), Err(E2eeError::UnsupportedVersion(2)));
    assert_eq!(open(&key, &bind(&hlc), &reencode(bytes.clone(), |b| b[1..5].copy_from_slice(&2u32.to_be_bytes()))), Err(E2eeError::UnknownEpoch(2)));
    assert_eq!(open(&key, &bind(&hlc), &reencode(bytes, |b| b[1..5].copy_from_slice(&0u32.to_be_bytes()))), Err(E2eeError::UnknownEpoch(0)));
    for malformed in ["!!!", "AQ==", "AQAA+AFA", "AQAA/AFA", " AQAAAAFA", "{\"name\":\"Fixture\"}"] {
        assert_eq!(open(&key, &bind(&hlc), malformed), Err(E2eeError::Malformed), "{malformed}");
    }
    assert_eq!(open(&key, &bind(&hlc), ""), Err(E2eeError::Truncated));
}

#[test]
fn oversize_plaintext_fails_before_push_and_oversize_input_before_decoding() {
    let key = fixed_key().library_cipher();
    let hlc = hlc();
    // The largest plaintext whose encoded envelope fits one sync row.
    let fits = (0..MAX_PAYLOAD_BYTES).rev().find(|n| BASE64URL_NOPAD.encode_len(HEADER_LEN + n + TAG_LEN) <= MAX_PAYLOAD_BYTES).unwrap();
    let sealed = seal(&key, &bind(&hlc), &"x".repeat(fits)).unwrap();
    assert!(sealed.len() <= MAX_PAYLOAD_BYTES);
    assert_eq!(open(&key, &bind(&hlc), &sealed).unwrap().len(), fits);
    assert_eq!(seal(&key, &bind(&hlc), &"x".repeat(fits + 1)), Err(E2eeError::TooLarge));
    assert_eq!(open(&key, &bind(&hlc), &"A".repeat(MAX_PAYLOAD_BYTES + 1)), Err(E2eeError::TooLarge));
}

#[test]
fn authentic_non_text_plaintext_is_rejected() {
    let key = fixed_key();
    let hlc = hlc();
    let subkey = key.subkey(Purpose::Library, LIBRARY_EPOCH);
    let sealed = XChaCha20Poly1305::new(Key::from_slice(subkey.as_slice()))
        .encrypt(XNonce::from_slice(&fixed_nonce()), Payload { msg: &[0xff, 0xfe], aad: &aad(&bind(&hlc), LIBRARY_EPOCH) }).unwrap();
    let mut envelope = vec![ENVELOPE_VERSION];
    envelope.extend_from_slice(&LIBRARY_EPOCH.to_be_bytes());
    envelope.extend_from_slice(&fixed_nonce());
    envelope.extend_from_slice(&sealed);
    assert_eq!(open(&key.library_cipher(), &bind(&hlc), &BASE64URL_NOPAD.encode(&envelope)), Err(E2eeError::NotText));
}

#[test]
fn purposes_and_epochs_derive_independent_keys() {
    let key = fixed_key();
    let library = Hidden::new(*key.subkey(Purpose::Library, LIBRARY_EPOCH));
    assert_ne!(library, Hidden::from(&key));
    assert_ne!(library, Hidden::new(*key.subkey(Purpose::Vault, LIBRARY_EPOCH)));
    assert_ne!(library, Hidden::new(*key.subkey(Purpose::Library, 2)));
    let prefix: Hidden<[u8; 16]> = library.map(|bytes| bytes[..16].try_into().unwrap());
    assert_ne!(prefix, Hidden::new(key.key_id()));
    assert_eq!(BASE64URL_NOPAD.encode(&key.key_id()), "y1hRzQRU3nqVO9sAfagS-A");
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let key = fixed_key();
    let code = RecoveryCode::for_key(&key);
    assert_eq!(format!("{key:?}"), "LibraryKey(..)");
    assert_eq!(format!("{code:?}"), "RecoveryCode(..)");
}

/// The recovery code `e2ee` holds, hidden, as tests compare it.
fn held(e2ee: &LibraryEncryption) -> Option<Hidden<String>> {
    e2ee.recovery_code().as_ref().map(Hidden::from)
}

/// AD4: a failed assert over the codes and keys these tests compare prints
/// neither, whichever assert it is.
#[test]
fn a_failed_assert_on_fixture_codes_or_keys_prints_neither() {
    let (key, other) = (fixed_key(), LibraryKey::from_slice(&[0xA5; 32]).unwrap());
    let messages = [
        failure(|| assert_eq!(Hidden::from(&RecoveryCode::for_key(&key)), Hidden::from(&RecoveryCode::for_key(&other)))),
        failure(|| assert_ne!(CODE, CODE, "the fixture code")),
        failure(|| assert_eq!(Hidden::from(&key), Hidden::from(&other))),
        failure(|| panic!("{CODE} {:?}", Hidden::from(&other))),
    ];
    for (n, message) in messages.iter().enumerate() {
        assert!(!message.contains("CB1-") && !message.contains("165") && !message.contains("0, 1, 2"), "failure {n} printed a code or key bytes");
        assert!(message.contains("Hidden(..)"), "failure {n} printed no redacted value");
    }
}

#[test]
fn recovery_code_fixed_vector_and_round_trip() {
    let code = RecoveryCode::for_key(&fixed_key());
    assert_eq!(Hidden::from(&code), CODE);
    let groups = code.as_str().split('-').count();
    assert_eq!(groups, 15);
    assert_eq!(Hidden::from(&RecoveryCode::parse(CODE.reveal()).unwrap()), Hidden::from(&fixed_key()));
    for _ in 0..16 {
        let key = LibraryKey::generate().unwrap();
        let code = RecoveryCode::for_key(&key);
        assert_eq!(Hidden::from(&RecoveryCode::parse(code.as_str()).unwrap()), Hidden::from(&key));
    }
}

#[test]
fn recovery_code_tolerates_case_spaces_dashes_and_line_breaks() {
    let code: &str = CODE.reveal();
    let body: String = code.trim_start_matches("CB1-").replace('-', "");
    let groups: Vec<&str> = code.split('-').collect();
    let variants = [
        code.to_ascii_lowercase(),
        code.replace('-', " "),
        code.replace('-', ""),
        ["  ", code, "\n"].concat(),
        [groups[..8].join("-"), groups[8..].join("-")].join("\n"),
        body.clone(),
        body.to_ascii_lowercase(),
        groups[1..].join(" - "),
    ];
    for (n, variant) in variants.iter().enumerate() {
        assert_eq!(Hidden::from(&RecoveryCode::parse(variant).unwrap()), Hidden::from(&fixed_key()), "variant {n}");
    }
}

#[test]
fn recovery_code_typos_are_detected_before_decryption() {
    let code: &str = CODE.reveal();
    let body: Vec<u8> = code.trim_start_matches("CB1-").replace('-', "").into_bytes();
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    for index in 0..body.len() {
        for &replacement in alphabet.iter().filter(|c| **c != body[index]) {
            let mut typo = body.clone();
            typo[index] = replacement;
            assert_eq!(RecoveryCode::parse(std::str::from_utf8(&typo).unwrap()).unwrap_err(), RecoveryError::Checksum, "substitution at {index}");
        }
        if index + 1 < body.len() && body[index] != body[index + 1] {
            let mut swapped = body.clone();
            swapped.swap(index, index + 1);
            assert_eq!(RecoveryCode::parse(std::str::from_utf8(&swapped).unwrap()).unwrap_err(), RecoveryError::Checksum, "transposition at {index}");
        }
    }
    for invalid in ["0", "1", "8", "9", "@"] {
        let typo = [&code[..10], invalid, &code[11..]].concat();
        assert_eq!(RecoveryCode::parse(&typo).unwrap_err(), RecoveryError::Character, "{invalid}");
    }
    assert_eq!(RecoveryCode::parse(&code[..code.len() - 1]).unwrap_err(), RecoveryError::Length);
    assert_eq!(RecoveryCode::parse(&[code, "A"].concat()).unwrap_err(), RecoveryError::Length);
    assert_eq!(RecoveryCode::parse("").unwrap_err(), RecoveryError::Length);
}

#[test]
fn aad_bytes_are_pinned_because_account_ids_and_clock_text_are_sealed_into_every_row() {
    // Changing how account ids or clocks are written would make every sealed
    // row fail its integrity check, so these exact bytes must never change.
    let hlc = Hlc::new(1_700_000_000_000, 7, "fixturenode");
    assert_eq!(hlc.encode(), "1700000000000-000007-fixturenode");
    let expected = [
        &b"\x00\x00\x00\x1ccrossbow.library.envelope.v1"[..],
        b"\x00\x00\x00\x0ffixture-account",
        b"\x00\x00\x00\x05space",
        b"\x00\x00\x00\x0dfixture-space",
        b"\x00\x00\x00\x201700000000000-000007-fixturenode",
        b"\x00\x00\x00\x04\x00\x00\x00\x01",
    ].concat();
    assert_eq!(aad(&bind(&hlc), LIBRARY_EPOCH), expected);
}

// -- local state without a network -----------------------------------------------------

fn library() -> Library {
    let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    lib
}

fn caught_up(lib: &mut Library, keys: &MemoryKeyStore, account: &str) -> LibraryEncryption {
    let (mut e2ee, warning) = LibraryEncryption::load(account, lib, keys);
    assert!(warning.is_none());
    e2ee.apply_page(lib, &[], 0, false).unwrap();
    e2ee
}

/// Begin, one complete pull, confirm: the order the UI enforces.
fn turn_on(e2ee: &mut LibraryEncryption, lib: &mut Library, keys: &MemoryKeyStore) -> RecoveryCode {
    e2ee.begin(lib, keys).unwrap();
    let code = e2ee.recovery_code().unwrap();
    assert!(e2ee.confirm(lib).unwrap_err().contains("Checking your other devices"), "confirm waits for a pull that started after Turn On");
    e2ee.apply_page(lib, &[], 0, false).unwrap();
    e2ee.confirm(lib).unwrap();
    code
}

#[test]
fn turn_on_waits_for_the_first_sync_and_a_storage_failure_enables_nothing() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let (mut e2ee, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(e2ee.status(), E2eeStatus::Off);
    assert!(e2ee.begin(&lib, &keys).unwrap_err().contains("finish syncing"));
    e2ee.apply_page(&mut lib, &[], 0, false).unwrap();
    keys.fail_writes(true);
    assert!(e2ee.begin(&lib, &keys).unwrap_err().contains("Encryption is still off"));
    assert_eq!(e2ee.status(), E2eeStatus::Off);
    assert!(e2ee.recovery_code().is_none());
    assert!(!keys.contains("fixture-a"));
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap(), None);
}

#[test]
fn plaintext_waits_visibly_for_a_complete_pull_after_every_reconnect() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let (mut e2ee, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(e2ee.upload_hold(), Some("Waiting for the first sync before uploading."));
    assert!(e2ee.outgoing(&lib, 100).unwrap().is_empty());
    e2ee.apply_page(&mut lib, &[], 0, true).unwrap();
    assert!(e2ee.upload_hold().is_some(), "a partial pull is not enough");
    e2ee.apply_page(&mut lib, &[], 0, false).unwrap();
    assert_eq!(e2ee.upload_hold(), None);
    assert!(!e2ee.outgoing(&lib, 100).unwrap().is_empty());
    e2ee.reconnecting();
    assert_eq!(e2ee.upload_hold(), Some("Waiting to catch up with the server before uploading."));
    assert!(e2ee.outgoing(&lib, 100).unwrap().is_empty());
}

#[test]
fn cancel_before_confirmation_removes_the_key_and_keeps_json() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    e2ee.begin(&lib, &keys).unwrap();
    assert_eq!(e2ee.status(), E2eeStatus::Confirming);
    assert!(keys.contains("fixture-a"));
    assert!(RecoveryCode::parse(e2ee.recovery_code().unwrap().as_str()).is_ok());
    // Nothing is encrypted until the user confirms.
    e2ee.apply_page(&mut lib, &[], 0, false).unwrap();
    assert!(e2ee.outgoing(&lib, 100).unwrap().iter().all(|c| c.enc == Encoding::Json));
    e2ee.cancel(&keys).unwrap();
    assert_eq!(e2ee.status(), E2eeStatus::Off);
    assert!(!keys.contains("fixture-a"));
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap(), None);
    let outgoing = e2ee.outgoing(&lib, 100).unwrap();
    assert!(!outgoing.is_empty() && outgoing.iter().all(|c| c.enc == Encoding::Json && c.kind != MARKER_KIND));
}

#[test]
fn an_abandoned_setup_key_is_removed_on_the_next_load() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    e2ee.begin(&lib, &keys).unwrap();
    drop(e2ee); // Quit before confirming.
    let (e2ee, warning) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert!(warning.is_none());
    assert_eq!(e2ee.status(), E2eeStatus::Off);
    assert!(!keys.contains("fixture-a"));
}

#[test]
fn confirmed_state_survives_reload_and_a_missing_key_locks() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    let live = lib.store.all_entities().unwrap().iter().filter(|r| !r.deleted).count();
    let code = turn_on(&mut e2ee, &mut lib, &keys);
    assert_eq!(e2ee.status(), E2eeStatus::On);
    assert_eq!(lib.store.all_entities().unwrap().iter().filter(|r| !r.deleted && r.kind != MARKER_KIND).count(), live);
    assert_eq!(lib.store.entity_payload(MARKER_KIND, MARKER_ID).unwrap().as_deref(), Some(MARKER_PLAINTEXT));
    let (reloaded, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(reloaded.status(), E2eeStatus::On);
    assert_eq!(held(&reloaded), Some(Hidden::from(&code)));

    // Losing the stored key never downgrades: the device locks.
    keys.clear("fixture-a").unwrap();
    let (mut locked, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(locked.status(), E2eeStatus::Locked);
    assert!(locked.outgoing(&lib, 100).unwrap().is_empty());
    let wrong = RecoveryCode::for_key(&LibraryKey::generate().unwrap());
    assert!(locked.unlock(wrong.as_str(), &lib, &keys).unwrap_err().contains("doesn't unlock"));
    assert!(!keys.contains("fixture-a"), "a wrong code stores nothing");
    assert!(locked.unlock("CB1-TYPO", &lib, &keys).is_err());
    assert!(!keys.contains("fixture-a"));
    locked.unlock(code.as_str(), &lib, &keys).unwrap();
    assert_eq!(locked.status(), E2eeStatus::On);
    assert!(keys.contains("fixture-a"));
}

#[test]
fn turn_on_supersedes_each_row_just_above_its_own_clock() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    let before: Vec<_> = lib.store.all_entities().unwrap();
    turn_on(&mut e2ee, &mut lib, &keys);
    for row in before.iter().filter(|r| !r.deleted) {
        let after = lib.store.entity_hlc(&row.kind, &row.id).unwrap().unwrap();
        assert_eq!((after.wall_ms, after.counter, after.node.as_str()), (row.hlc.wall_ms, row.hlc.counter + 1, lib.node()), "{} {}", row.kind, row.id);
        assert!(after.encode() > row.hlc.encode());
    }
    // The clock never falls behind what it wrote.
    let next = lib.create_profile("After Turn On").unwrap();
    assert!(before.iter().all(|row| lib.store.entity_hlc(kinds::PROFILE, &next).unwrap().unwrap() > row.hlc));
}

#[test]
fn successor_clocks_sort_above_in_order_and_as_server_text() {
    use crate::library::successor;
    for hlc in [Hlc::new(1_700_000_000_000, 0, "zzzzzzzz"), Hlc::new(1_700_000_000_000, 41, "a"), Hlc::new(1_700_000_000_000, 999_999, "z"), Hlc::new(9_999_999_999_998, 999_999, "z"), Hlc::new(9_999_999_999_999, 999_998, "z")] {
        let next = successor(&hlc, "a").unwrap();
        assert!(next > hlc && next.encode() > hlc.encode(), "{hlc} -> {next}");
        assert_eq!(Hlc::parse(&next.encode()).unwrap(), next, "the successor must stay a valid clock");
    }
    assert_eq!(successor(&Hlc::new(5, 999_999, "z"), "a"), Some(Hlc::new(6, 0, "a")));
    // The largest encodable clock has no successor: rolling over would need a
    // 14-digit wall time that sorts below it as text and doesn't parse.
    let largest = Hlc::parse("9999999999999-999999-legacy").unwrap();
    assert_eq!(successor(&largest, "a"), None);
    assert_eq!(successor(&Hlc::new(u64::MAX, 999_999, "x"), "a"), None, "never panics on overflow");
}

#[test]
fn a_reset_never_writes_a_marker_clock_past_the_encodable_limit() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    turn_on(&mut e2ee, &mut lib, &keys);
    // Observing a deletion at the largest clock carries this device's own
    // clock past six counter digits.
    let largest = Hlc::parse("9999999999999-999999-legacy").unwrap();
    let deletion = ServerChange { seq: 1, device_id: "fixture-server".into(), change: Change { kind: kinds::NOTE.into(), id: "at-the-limit".into(), hlc: largest, deleted: true, enc: Encoding::Json, payload: String::new() } };
    e2ee.apply_page(&mut lib, &[deletion], 1, false).unwrap();
    e2ee.begin_reset(&lib).unwrap();
    // A scan that found nothing past the cursor.
    e2ee.scan_page(&lib, &[], false);
    e2ee.confirm_reset(&mut lib, &keys).unwrap();
    // entity_hlc reads back only clocks that parse.
    assert!(lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().is_some(), "the new marker's clock must stay valid");
}

#[test]
fn a_mismatched_stored_key_is_never_used() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    turn_on(&mut e2ee, &mut lib, &keys);
    keys.save("fixture-a", &LibraryKey::generate().unwrap()).unwrap();
    let (reloaded, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(reloaded.status(), E2eeStatus::Locked);
    assert!(reloaded.outgoing(&lib, 100).unwrap().is_empty());
}

#[test]
fn another_account_never_adopts_or_erases_an_encrypted_accounts_state() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut a = caught_up(&mut lib, &keys, "fixture-a");
    turn_on(&mut a, &mut lib, &keys);
    let record = lib.store.kv_get(FLAG_KEY).unwrap();
    // Account A's session ended without a clean sign-out.
    let (mut b, warning) = LibraryEncryption::load("fixture-b", &lib, &keys);
    assert!(warning.is_none());
    assert_eq!(b.status(), E2eeStatus::OtherAccount);
    assert_eq!(LibraryEncryption::owner(&lib).unwrap().as_deref(), Some("fixture-a"));
    assert!(b.recovery_code().is_none());
    assert!(b.outgoing(&lib, 100).unwrap().is_empty(), "nothing of A's library is queued for B");
    assert_eq!(b.apply_page(&mut lib, &[], 0, false), Err(PageError::OtherAccount));
    assert!(b.begin(&lib, &keys).is_err() && b.begin_reset(&lib).is_err());
    assert!(keys.contains("fixture-a") && !keys.contains("fixture-b"));
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap(), record);
    assert_eq!(lib.store.entity_payload(MARKER_KIND, MARKER_ID).unwrap().as_deref(), Some(MARKER_PLAINTEXT));
    assert_eq!(LibraryEncryption::load("fixture-a", &lib, &keys).0.status(), E2eeStatus::On);
}

#[test]
fn forgetting_the_key_keeps_the_record_so_the_account_signs_in_locked() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    let code = turn_on(&mut e2ee, &mut lib, &keys);
    keys.save("fixture-b", &LibraryKey::generate().unwrap()).unwrap();
    LibraryEncryption::forget_key("fixture-a", &keys).unwrap();
    assert!(!keys.contains("fixture-a"));
    assert!(keys.contains("fixture-b"), "only this account's key is removed");
    assert!(lib.store.kv_get(FLAG_KEY).unwrap().is_some());
    assert_eq!(lib.store.entity_payload(MARKER_KIND, MARKER_ID).unwrap().as_deref(), Some(MARKER_PLAINTEXT));
    let (mut again, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(again.status(), E2eeStatus::Locked);
    // An empty page (a withholding server) changes nothing.
    assert_eq!(again.apply_page(&mut lib, &[], 0, false), Err(PageError::Locked));
    assert!(again.outgoing(&lib, 100).unwrap().is_empty());
    again.unlock(code.as_str(), &lib, &keys).unwrap();
    assert_eq!(again.status(), E2eeStatus::On);
    keys.fail_writes(true);
    assert!(LibraryEncryption::forget_key("fixture-b", &keys).is_err(), "a failed removal is reported");
}

#[test]
fn a_locked_device_checks_codes_against_the_newest_marker_it_has_seen() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let (old, new) = (LibraryKey::from_slice(&[1; 32]).unwrap(), LibraryKey::from_slice(&[2; 32]).unwrap());
    let sealed = |key: &LibraryKey, kind: &str, id: &str, hlc: Hlc, text: &str| {
        let payload = seal(&key.library_cipher(), &Binding { account_id: "fixture-a", kind, id, hlc: &hlc }, text).unwrap();
        ServerChange { seq: 1, device_id: "other".into(), change: Change { kind: kind.into(), id: id.into(), hlc, deleted: false, enc: Encoding::Xchacha20V1, payload } }
    };
    let (mut e2ee, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    let first = [sealed(&old, "space", "s1", Hlc::new(10, 0, "a"), "{}")];
    assert_eq!(e2ee.apply_page(&mut lib, &first, 1, false), Err(PageError::Locked));
    assert!(e2ee.has_check_row());
    // After a reset elsewhere the newer marker replaces the old check row.
    let reset = [sealed(&new, MARKER_KIND, MARKER_ID, Hlc::new(20, 0, "b"), MARKER_PLAINTEXT), sealed(&old, "space", "s2", Hlc::new(30, 0, "a"), "{}")];
    assert_eq!(e2ee.apply_page(&mut lib, &reset, 2, false), Err(PageError::Locked));
    assert!(e2ee.unlock(RecoveryCode::for_key(&old).as_str(), &lib, &keys).unwrap_err().contains("doesn't unlock"));
    // An older marker never replaces a newer one.
    let stale = [sealed(&old, MARKER_KIND, MARKER_ID, Hlc::new(15, 0, "a"), MARKER_PLAINTEXT)];
    assert_eq!(e2ee.apply_page(&mut lib, &stale, 3, false), Err(PageError::Locked));
    e2ee.unlock(RecoveryCode::for_key(&new).as_str(), &lib, &keys).unwrap();
    assert_eq!(e2ee.status(), E2eeStatus::On);
}

#[test]
fn unreadable_local_state_fails_closed_until_a_reset() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    lib.store.kv_set(FLAG_KEY, "not json").unwrap();
    let (mut e2ee, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(e2ee.status(), E2eeStatus::Unreadable);
    assert!(e2ee.outgoing(&lib, 100).unwrap().is_empty());
    assert_eq!(e2ee.apply_page(&mut lib, &[], 0, false), Err(PageError::Unreadable));
    let code = RecoveryCode::for_key(&LibraryKey::generate().unwrap());
    assert!(e2ee.unlock(code.as_str(), &lib, &keys).is_err());
    assert!(e2ee.begin(&lib, &keys).is_err());

    e2ee.begin_reset(&lib).unwrap();
    assert_eq!(e2ee.status(), E2eeStatus::Resetting);
    assert!(!keys.contains("fixture-a"), "the new key is stored only once the reset is confirmed");
    let code = e2ee.recovery_code().unwrap();
    // Confirm waits for a scan to the end of the stream.
    assert!(e2ee.confirm_reset(&mut lib, &keys).unwrap_err().starts_with("Checking the server"));
    assert!(!e2ee.reset_ready() && e2ee.needs_scan());
    let marker = Hlc::new(crate::now_ms() + 600_000, 0, "elsewhere");
    let seen = ServerChange { seq: 9, device_id: "other".into(), change: Change { kind: MARKER_KIND.into(), id: MARKER_ID.into(), hlc: marker.clone(), deleted: false, enc: Encoding::Xchacha20V1, payload: "AAAA".into() } };
    e2ee.scan_page(&lib, &[seen], true);
    assert!(!e2ee.reset_ready());
    e2ee.scan_page(&lib, &[], false);
    assert!(e2ee.reset_ready());
    // A failed save puts the unreadable record back and removes the new
    // marker, so nothing changed; confirming again repeats the reset.
    keys.fail_writes(true);
    assert_eq!(e2ee.confirm_reset(&mut lib, &keys).unwrap_err(), RESET_UNSAVED);
    assert_eq!(e2ee.status(), E2eeStatus::Resetting);
    assert!(e2ee.outgoing(&lib, 100).unwrap().is_empty());
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap().as_deref(), Some("not json"));
    assert_eq!(lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap(), None);
    keys.fail_writes(false);
    e2ee.confirm_reset(&mut lib, &keys).unwrap();
    assert!(lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap() > marker, "the new marker lands above the newest one the scan saw");
    assert_eq!(e2ee.status(), E2eeStatus::On);
    assert_eq!(held(&e2ee), Some(Hidden::from(&code)));
    let (reloaded, _) = LibraryEncryption::load("fixture-a", &lib, &keys);
    assert_eq!(reloaded.status(), E2eeStatus::On);
}

#[test]
fn a_cancelled_reset_keeps_the_current_key() {
    let mut lib = library();
    let keys = MemoryKeyStore::default();
    let mut e2ee = caught_up(&mut lib, &keys, "fixture-a");
    let code = turn_on(&mut e2ee, &mut lib, &keys);
    e2ee.begin_reset(&lib).unwrap();
    assert_ne!(held(&e2ee), Some(Hidden::from(&code)));
    e2ee.cancel_reset();
    assert_eq!(e2ee.status(), E2eeStatus::On);
    assert_eq!(held(&e2ee), Some(Hidden::from(&code)));
    assert_eq!(held(&LibraryEncryption::load("fixture-a", &lib, &keys).0), Some(Hidden::from(&code)));
}

#[test]
fn deleting_the_server_data_retires_only_that_accounts_record() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(Store::open(&dir.path().join("library.sqlite")).unwrap()).unwrap();
    let marker = || lib.store.raw_entity(MARKER_KIND, MARKER_ID).unwrap();
    let flag = |account: &str| flag_text(&Flag { account: account.into(), key_id: None, check: None, reseal: false, undo: None, epoch: None }).unwrap();
    let counts = |account: &str| serde_json::to_string(&Counts { account: account.into(), skipped: 2, ..Counts::default() }).unwrap();
    let setup = |account: &str| {
        lib.store.kv_set(FLAG_KEY, &flag(account)).unwrap();
        lib.store.kv_set(NOTICE_KEY, &counts(account)).unwrap();
        lib.store.put_raw_entity(MARKER_KIND, MARKER_ID, Some(&crate::store::RawEntity { hlc: hlc().encode(), deleted: false, payload: MARKER_PLAINTEXT.into(), dirty: false })).unwrap();
    };

    setup("account-a");
    LibraryEncryption::retire("account-a", &lib).unwrap();
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap(), None);
    assert_eq!(lib.store.kv_get(NOTICE_KEY).unwrap(), None);
    assert!(marker().is_none(), "the old marker would upload again with the library");

    // Another account's record, and one that can't be read, stay.
    setup("account-b");
    LibraryEncryption::retire("account-a", &lib).unwrap();
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap(), Some(flag("account-b")));
    assert_eq!(lib.store.kv_get(NOTICE_KEY).unwrap(), Some(counts("account-b")));
    assert!(marker().is_some());
    lib.store.kv_set(FLAG_KEY, "not json").unwrap();
    LibraryEncryption::retire("account-a", &lib).unwrap();
    assert_eq!(lib.store.kv_get(FLAG_KEY).unwrap().as_deref(), Some("not json"));
}
