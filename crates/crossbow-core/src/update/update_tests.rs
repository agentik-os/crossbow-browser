use super::*;

const KEY_A: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
const KEY_B: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";

#[test]
fn a_build_without_a_feed_url_or_keys_has_no_updater() {
    assert_eq!(build(None, None), None);
    assert_eq!(build(Some("https://updates.example.com/stable.json"), None), None);
    assert_eq!(build(None, Some(KEY_A)), None);
    assert_eq!(build(Some(""), Some(KEY_A)), None);
    assert_eq!(build(Some("https://updates.example.com/stable.json"), Some("")), None);
    assert_eq!(NO_UPDATER, "This build has no updater.");
}

#[test]
fn a_build_with_both_has_an_updater_with_every_key() {
    let both = format!("{KEY_A},{KEY_B}");
    let leaked: &'static str = Box::leak(both.into_boxed_str());
    let updater = build(Some("https://updates.example.com/stable.json"), Some(leaked)).unwrap();
    assert_eq!(updater.url, "https://updates.example.com/stable.json");
    let keys = updater.keys();
    assert_eq!(keys.len(), 2);
    assert_eq!(data_encoding::HEXLOWER.encode(&keys[0]), KEY_A);
    assert_eq!(data_encoding::HEXLOWER.encode(&keys[1]), KEY_B);
    assert!(build(Some("http://127.0.0.1:8790/stable.json"), Some(KEY_A)).is_some(), "loopback http, for tests");
}

#[test]
fn malformed_keys_are_refused() {
    assert_eq!(check_keys(KEY_A), Ok(()));
    assert_eq!(check_keys(&KEY_A.to_uppercase()), Ok(()));
    for bad in [KEY_A[..63].to_string(), format!("{KEY_A}0"), format!("{KEY_A},"), format!(",{KEY_A}"), format!("{KEY_A} "), KEY_A.replace('d', "g"), [KEY_A; 9].join(",")] {
        assert!(check_keys(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_degenerate_key_fails_the_build() {
    let identity = format!("01{}", "0".repeat(62));
    assert!(check_keys(&identity).is_err(), "the identity point");
    assert!(check_keys(&format!("{KEY_A},{}", "0".repeat(64))).is_err(), "one of small order among good ones");
    assert!(check_keys(&format!("ED{}7F", "F".repeat(60))).is_err(), "y = p, in capitals");
    let leaked: &'static str = Box::leak(identity.into_boxed_str());
    assert!(std::panic::catch_unwind(|| build(Some("https://updates.example.com/stable.json"), Some(leaked))).is_err());
}

#[test]
fn only_a_plain_version_can_have_an_updater() {
    for good in ["0.1.0", "1.20.300", "999999999.0.0"] {
        assert!(plain_version(good) && Version::parse(good).is_some(), "{good}");
    }
    for bad in ["1.0.0-beta.1", "1.0", "1.0.0.0", "01.0.0", "1..0", "", "1.0.0+build", "1000000000.0.0", "1.0.a"] {
        assert!(!plain_version(bad) && Version::parse(bad).is_none(), "{bad}");
    }
    assert!(plain_version(VERSION), "this build's own version");
}

#[test]
#[should_panic(expected = "CROSSBOW_UPDATE_URL must be")]
fn a_plain_http_feed_fails_the_build() {
    build(Some("http://updates.example.com/stable.json"), Some(KEY_A));
}

#[test]
#[should_panic(expected = "CROSSBOW_UPDATE_KEYS must be")]
fn a_malformed_key_fails_the_build() {
    build(Some("https://updates.example.com/stable.json"), Some("abc"));
}

#[test]
fn build_numbers_are_digits() {
    assert_eq!(build_number(None), 0);
    assert_eq!(build_number(Some("42")), 42);
    assert!(std::panic::catch_unwind(|| build_number(Some("4.2"))).is_err());
    assert!(std::panic::catch_unwind(|| build_number(Some(""))).is_err());
}

#[test]
fn versions_compare_by_number_and_reject_anything_else() {
    assert!(Version::parse("0.10.0").unwrap() > Version::parse("0.9.9").unwrap());
    assert_eq!(Version::parse("1.2.3"), Some(Version(1, 2, 3)));
    assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
    for bad in ["", "1", "1.2", "1.2.3.4", "1.2.x", "01.2.3", "1.2.3-beta", " 1.2.3", "1..3"] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Version::parse(VERSION).map(|v| v.to_string()).as_deref(), Some(VERSION), "the workspace version parses");
}

#[test]
fn macos_versions_take_two_or_three_parts() {
    assert_eq!(MacVersion::parse("11.0"), Some(MacVersion(11, 0, 0)));
    assert_eq!(MacVersion::parse("14.6.1"), Some(MacVersion(14, 6, 1)));
    assert!(MacVersion::parse("26.0").unwrap() > MacVersion::parse("15.7.2").unwrap());
    assert_eq!(MacVersion(15, 0, 0).to_string(), "15.0");
    for bad in ["", "15", "15.", "a.b", "15.0.0.0"] {
        assert_eq!(MacVersion::parse(bad), None, "{bad:?}");
    }
    #[cfg(target_os = "macos")]
    assert!(MacVersion::running().is_some_and(|mac| mac >= MacVersion(11, 0, 0)), "reads the running macOS");
}

#[test]
fn automatic_checks_run_at_most_once_a_day_plus_jitter_never_overlap_and_stop_when_off() {
    use std::time::Duration;
    let schedule = Schedule { jitter: 600 };
    let ready = Some(GRACE);
    let last = 1_000_000;
    assert!(schedule.due(last, None, true, ready, false), "never checked: due once interactive");
    assert!(!schedule.due(last + DAY, Some(last), true, ready, false), "a day is not enough with jitter");
    assert!(!schedule.due(last + DAY + 599, Some(last), true, ready, false));
    assert!(schedule.due(last + DAY + 600, Some(last), true, ready, false), "a day plus the jitter");
    assert!(!schedule.due(last + 3600, Some(last), true, ready, false), "an hour after a check, manual or automatic");
    assert!(!schedule.due(last + 2 * DAY, Some(last), true, ready, true), "never while a check or download runs");
    assert!(!schedule.due(last + 2 * DAY, Some(last), false, ready, false), "never with the switch off");
    assert!(!schedule.due(last + 2 * DAY, Some(last), true, None, false), "never before the window is interactive");
    assert!(!schedule.due(last + 2 * DAY, Some(last), true, Some(GRACE - Duration::from_secs(1)), false), "nor in its first minute");
    assert!(!schedule.due(last - 3600, Some(last), true, ready, false), "a clock an hour behind waits");
    assert!(schedule.due(last - 2 * DAY, Some(last), true, ready, false), "a last check far in the future doesn't block checks for good");
    for _ in 0..32 {
        assert!(Schedule::random().jitter <= MAX_JITTER);
    }
}
