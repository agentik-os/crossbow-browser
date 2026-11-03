use super::*;
use crate::update::fixtures::{keypair, release, sha256};

const URL: &str = "https://updates.example.com/Crossbow-0.2.0.tar.gz";

fn running(version: &str, build: u64) -> Running {
    Running { version: Version::parse(version).unwrap(), build }
}

#[test]
fn a_signed_feed_opens_with_its_key_and_says_exactly_what_was_signed() {
    let (key, public) = keypair();
    let signed = release(URL, b"archive bytes", &key, "0.2.0", 7);
    let text = sign(&signed, &key).unwrap();
    assert!(text.contains(FORMAT));
    assert_eq!(open(text.as_bytes(), &[public]), Ok(signed.clone()));
    let (_, other) = keypair();
    assert_eq!(open(text.as_bytes(), &[other, public]), Ok(signed), "any embedded key may verify");
}

#[test]
fn a_tampered_feed_fails_its_signature() {
    let (key, public) = keypair();
    let text = sign(&release(URL, b"archive bytes", &key, "0.2.0", 7), &key).unwrap();
    // Change one character inside the signed payload: the build number.
    let tampered = text.replacen(r#"\"build\":7"#, r#"\"build\":8"#, 1);
    assert_ne!(tampered, text);
    assert_eq!(open(tampered.as_bytes(), &[public]), Err(FeedError::Signature));
    // Or swap the notes for others, keeping valid JSON.
    let tampered = text.replacen("Faster tabs.", "Install me.!", 1);
    assert_eq!(open(tampered.as_bytes(), &[public]), Err(FeedError::Signature));
}

#[test]
fn a_feed_signed_by_a_key_this_build_does_not_embed_is_refused() {
    let (key, _) = keypair();
    let (_, embedded) = keypair();
    let text = sign(&release(URL, b"archive bytes", &key, "0.2.0", 7), &key).unwrap();
    assert_eq!(open(text.as_bytes(), &[embedded]), Err(FeedError::Signature));
    assert_eq!(open(text.as_bytes(), &[]), Err(FeedError::Signature), "no keys verify nothing");
}

#[test]
fn malformed_and_oversized_feeds_are_refused_before_anything_is_parsed() {
    let (key, public) = keypair();
    assert_eq!(open(b"not json", &[public]), Err(FeedError::Format));
    assert_eq!(open(br#"{"format":"other","payload":"{}","signature":""}"#, &[public]), Err(FeedError::Format));
    assert_eq!(open(&vec![b' '; MAX_FEED + 1], &[public]), Err(FeedError::TooLarge));
    let text = sign(&release(URL, b"archive bytes", &key, "0.2.0", 7), &key).unwrap();
    let extra = text.replacen("\"format\"", "\"extra\": 1, \"format\"", 1);
    assert_eq!(open(extra.as_bytes(), &[public]), Err(FeedError::Format), "unknown fields are refused");
}

#[test]
fn every_field_is_checked_before_signing_and_after_opening() {
    let (key, public) = keypair();
    let good = release(URL, b"archive bytes", &key, "0.2.0", 7);
    let cases: [(&str, fn(&mut Release)); 11] = [
        ("channel", |r| r.channel = "beta".into()),
        ("version", |r| r.version = "0.2".into()),
        ("minimum macOS version", |r| r.minimum_macos = "eleven".into()),
        ("publication time", |r| r.published = "2026-02-30T00:00:00Z".into()),
        ("expiry time", |r| r.expires = "2027-01-01T00:00:00Z".into()),
        ("release notes", |r| r.notes = "x".repeat(MAX_NOTES + 1)),
        ("release notes", |r| r.notes = "<b>bold</b>\u{7}".into()),
        ("archive URL", |r| r.archive.url = "http://updates.example.com/a.tar.gz".into()),
        ("archive size", |r| r.archive.size = MAX_ARCHIVE + 1),
        ("archive SHA-256", |r| r.archive.sha256 = "ABC".into()),
        ("archive signature", |r| r.archive.signature = "AAAA".into()),
    ];
    for (field, break_it) in cases {
        let mut bad = good.clone();
        break_it(&mut bad);
        assert_eq!(sign(&bad, &key), Err(FeedError::Field(field)), "signing checks the {field}");
        // A signer that skipped the checks still can't get it past open().
        let payload = serde_json::to_string(&bad).unwrap();
        let envelope = serde_json::json!({ "format": FORMAT, "payload": payload, "signature": data_encoding::BASE64.encode(key.raw().sign(payload.as_bytes()).as_ref()) });
        assert_eq!(open(envelope.to_string().as_bytes(), &[public]), Err(FeedError::Field(field)), "opening checks the {field}");
    }
    assert!(plain_text(&"é".repeat(MAX_NOTES / 2)), "8 KB of UTF-8 is fine");
    assert!(!plain_text(&"é".repeat(MAX_NOTES / 2 + 1)));
}

#[test]
fn the_archive_signature_covers_its_hash_and_size_under_a_domain() {
    let (key, public) = keypair();
    let signed = release(URL, b"archive bytes", &key, "0.2.0", 7);
    let digest = sha256(b"archive bytes");
    assert!(archive_verifies(&signed, &[public], &digest, 13));
    assert!(!archive_verifies(&signed, &[public], &digest, 14), "another size");
    assert!(!archive_verifies(&signed, &[public], &sha256(b"archive bytez"), 13), "another hash");
    let (_, other) = keypair();
    assert!(!archive_verifies(&signed, &[other], &digest, 13), "another key");
    // The feed key's signature over the same bytes, without the domain, is not an archive signature.
    let mut undomained = signed.clone();
    undomained.archive.signature = data_encoding::BASE64.encode(key.raw().sign(&[&digest[..], &13u64.to_be_bytes()].concat()).as_ref());
    assert!(!archive_verifies(&undomained, &[public], &digest, 13));
}

/// What a Mac running `version` (build) knows a day into the fixture
/// releases' fortnight, with no feed seen before.
fn known(version: &str, build: u64) -> Known {
    Known { running: running(version, build), mac: Some(MacVersion(15, 0, 0)), now: crate::update::fixtures::now(), newest: None, rolled_back: Vec::new() }
}

fn stamp(version: &str, build: u64) -> Stamp {
    Stamp { version: version.into(), build }
}

#[test]
fn judging_a_release_offers_only_newer_builds_this_mac_can_run() {
    let (key, _) = keypair();
    let at = |version: &str, build: u64| release(URL, b"a", &key, version, build);
    let k = known("0.2.0", 7);
    assert_eq!(judge(at("0.2.0", 7), &k), Offer::UpToDate);
    assert_eq!(judge(at("0.2.0", 8), &k), Offer::Available(at("0.2.0", 8)), "a rebuild of the same version");
    assert_eq!(judge(at("0.3.0", 1), &k), Offer::Available(at("0.3.0", 1)));
    assert_eq!(judge(at("0.1.9", 99), &k), Offer::Older(Version(0, 1, 9)), "a downgrade is refused");
    assert_eq!(judge(at("0.2.0", 6), &k), Offer::Older(Version(0, 2, 0)), "an older build too");
    let mut needs_more = at("0.3.0", 1);
    needs_more.minimum_macos = "26.0".into();
    assert_eq!(judge(needs_more.clone(), &k), Offer::Unsupported { version: Version(0, 3, 0), needs: MacVersion(26, 0, 0) });
    assert_eq!(judge(needs_more, &Known { mac: None, ..k }), Offer::Unsupported { version: Version(0, 3, 0), needs: MacVersion(26, 0, 0) }, "an unknown macOS is not assumed new enough");
}

#[test]
fn an_expired_feed_says_nothing_is_known_rather_than_up_to_date() {
    let (key, _) = keypair();
    let current = release(URL, b"a", &key, "0.2.0", 7);
    let expires = parse_time(&current.expires).unwrap();
    for (now, days) in [(expires, 0), (expires + DAY - 1, 0), (expires + 3 * DAY, 3)] {
        let k = Known { now, ..known("0.2.0", 7) };
        assert_eq!(judge(current.clone(), &k), Offer::Expired { days }, "a feed for this very build");
        assert_eq!(judge(release(URL, b"a", &key, "0.3.0", 1), &Known { now, ..known("0.2.0", 7) }), Offer::Expired { days }, "and one offering a newer build");
    }
    assert_eq!(judge(current.clone(), &Known { now: expires - 1, ..known("0.2.0", 7) }), Offer::UpToDate, "current until it expires");
    // A feed may be current for at most 30 days, and must end after it starts.
    for (expires, valid) in [("2026-12-01T11:23:42Z", true), ("2026-12-01T11:23:43Z", false), ("2026-11-01T11:23:42Z", false), ("2026-10-31T00:00:00Z", false), ("soon", false)] {
        let mut feed = current.clone();
        feed.expires = expires.into();
        assert_eq!(feed.check().is_ok(), valid, "{expires}");
        assert_eq!(sign(&feed, &key).is_ok(), valid, "{expires}: signing checks it too");
    }
}

#[test]
fn a_feed_older_than_one_seen_before_is_a_replay() {
    let (key, _) = keypair();
    let at = |version: &str, build: u64| release(URL, b"a", &key, version, build);
    let seen = Known { newest: Some(stamp("0.3.0", 2)), ..known("0.1.0", 1) };
    assert_eq!(judge(at("0.2.0", 7), &seen), Offer::Replayed { offered: Version(0, 2, 0), newest: Version(0, 3, 0) }, "newer than what runs, older than what was seen");
    assert_eq!(judge(at("0.3.0", 1), &seen), Offer::Replayed { offered: Version(0, 3, 0), newest: Version(0, 3, 0) }, "an older build of the newest version");
    assert_eq!(judge(at("0.3.0", 2), &seen), Offer::Available(at("0.3.0", 2)), "the newest itself");
    assert_eq!(judge(at("0.4.0", 1), &seen), Offer::Available(at("0.4.0", 1)));
    // Running what was seen, a replay of the version before is no "up to date" either.
    let running_newest = Known { newest: Some(stamp("0.3.0", 2)), ..known("0.3.0", 2) };
    assert_eq!(judge(at("0.3.0", 2), &running_newest), Offer::UpToDate);
    assert!(matches!(judge(at("0.2.0", 7), &running_newest), Offer::Replayed { .. }));
}

#[test]
fn a_release_rolled_back_here_is_never_offered_again_but_a_later_build_is() {
    let (key, _) = keypair();
    let at = |version: &str, build: u64| release(URL, b"a", &key, version, build);
    let k = Known { rolled_back: vec![stamp("0.2.0", 7)], ..known("0.1.0", 1) };
    assert_eq!(judge(at("0.2.0", 7), &k), Offer::RolledBack(Version(0, 2, 0)));
    assert_eq!(judge(at("0.2.0", 8), &k), Offer::Available(at("0.2.0", 8)), "a later build of the same version");
    assert_eq!(judge(at("0.3.0", 1), &k), Offer::Available(at("0.3.0", 1)));
}

/// The canonical encodings of the eight points of small order.
fn small_order_points() -> Vec<[u8; 32]> {
    let mut points = Vec::new();
    let hex = |text: &str| -> [u8; 32] { data_encoding::HEXLOWER.decode(text.as_bytes()).unwrap().try_into().unwrap() };
    points.push(hex("0100000000000000000000000000000000000000000000000000000000000000"));
    points.push(hex("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"));
    for y in ["0000000000000000000000000000000000000000000000000000000000000000", "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05", "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a"] {
        let point = hex(y);
        let mut negative = point;
        negative[31] |= 0x80;
        points.extend([point, negative]);
    }
    points
}

#[test]
fn signatures_forged_under_a_small_order_key_never_verify_and_such_keys_fail_the_build() {
    let points = small_order_points();
    for key in &points {
        // Under ring alone, a forgery (R of small order, S = 0) verifies for
        // some message: it needs R = -[k]A, and [k]A takes at most 8 values.
        let forgery = (0..256u32).find_map(|n| {
            let message = format!("crossbow-update-feed-1 forged {n}").into_bytes();
            points.iter().map(|r| [&r[..], &[0u8; 32]].concat()).find(|signature| UnparsedPublicKey::new(&ED25519, key).verify(&message, signature).is_ok()).map(|signature| (message, signature))
        });
        let (message, signature) = forgery.unwrap_or_else(|| panic!("{}: a forgery verifies under the bare key", data_encoding::HEXLOWER.encode(key)));
        assert!(super::super::small_order(key));
        assert!(!verifies(&[*key], &message, &signature), "the feed refuses every key of small order");
        assert!(crate::update::check_keys(&data_encoding::HEXLOWER.encode(key)).is_err(), "and the build refuses it");
    }
    // Encodings of y = p and more are refused too, sign bit either way.
    for low in [0xed, 0xee, 0xff] {
        let mut key = [0xff; 32];
        key[0] = low;
        key[31] = 0x7f;
        assert!(super::super::small_order(&key));
        key[31] = 0xff;
        assert!(super::super::small_order(&key));
    }
    // Real keys are not, and still verify.
    let message = b"crossbow-update-feed-1 genuine";
    for _ in 0..64 {
        let (key, public) = keypair();
        assert!(!super::super::small_order(&public));
        assert!(crate::update::check_keys(&data_encoding::HEXLOWER.encode(&public)).is_ok());
        assert!(verifies(&[public], message, key.raw().sign(message).as_ref()));
    }
}

#[test]
fn publication_times_round_trip_and_reject_impossible_dates() {
    assert_eq!(parse_time("2026-11-01T11:23:42Z"), Some(1_793_532_222));
    assert_eq!(format_time(1_793_532_222), "2026-11-01T11:23:42Z");
    for bad in ["2026-11-01 11:23:42Z", "2026-11-01T11:23:42+00:00", "2026-13-01T00:00:00Z", "2026-02-29T00:00:00Z", "2026-11-01T24:00:00Z", "2026-11-1T11:23:42Z"] {
        assert_eq!(parse_time(bad), None, "{bad:?}");
    }
    assert!(parse_time("2028-02-29T00:00:00Z").is_some(), "a leap day");
}

#[test]
fn describing_an_archive_on_disk_signs_exactly_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Crossbow.tar.gz");
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let (key, public) = keypair();
    let bundle = crate::update::archive::Bundle { version: Version(0, 2, 0), build: 7, minimum_macos: MacVersion(11, 0, 0), executable: "crossbow".into() };
    let described = describe(&path, URL, &bundle, "Faster tabs.", 1_793_532_222, 14 * DAY, &key).unwrap();
    assert_eq!((described.version.as_str(), described.build, described.minimum_macos.as_str()), ("0.2.0", 7, "11.0"));
    assert_eq!((described.archive.size, described.published.as_str(), described.expires.as_str()), (200_000, "2026-11-01T11:23:42Z", "2026-11-15T11:23:42Z"));
    assert_eq!(described.archive.sha256, data_encoding::HEXLOWER.encode(&sha256(&bytes)));
    assert!(archive_verifies(&described, &[public], &sha256(&bytes), 200_000));
    let text = sign(&described, &key).unwrap();
    assert_eq!(open(text.as_bytes(), &[public]), Ok(described));
    let bad = describe(&path, "http://updates.example.com/a.tar.gz", &bundle, "Faster tabs.", 1_793_532_222, 14 * DAY, &key);
    assert_eq!(bad.map_err(|error| error.kind()), Err(std::io::ErrorKind::InvalidData), "a release that would not open is not described");
}
