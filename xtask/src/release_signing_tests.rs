//! `release-key` and `release-sign` with keys made at run time, in folders
//! under the test TMPDIR. Nothing is published and no real key is read.

use super::*;
use crossbow_core::update::MacVersion;

const URL: &str = "https://updates.example.com/Crossbow-0.2.0.tar.gz";
const TEAM: &str = "ABCDE12345";
const NOW: u64 = 1_793_532_222;

/// A fresh folder of its own; removed with `cleanup`.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xtask-release-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    fs::remove_dir_all(dir).unwrap();
}

/// A minimal Crossbow.app packed as an update archive.
fn archive(dir: &Path, version: &str, build: u64) -> PathBuf {
    let app = dir.join(format!("app-{version}")).join(archive::APP);
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>Crossbow</string>
<key>CFBundleIdentifier</key><string>app.crossbow.Crossbow</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{build}</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
"#
    );
    fs::write(app.join("Contents/Info.plist"), plist).unwrap();
    fs::write(app.join("Contents/MacOS/Crossbow"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(app.join("Contents/MacOS/Crossbow"), fs::Permissions::from_mode(0o755)).unwrap();
    let out = dir.join(format!("Crossbow-{version}.tar.gz"));
    archive::pack(&app, &out).unwrap();
    out
}

/// A key pair from `release-key` in `dir/keys`: the private key's path and
/// the public key.
fn keys(dir: &Path, repo: &Path) -> (PathBuf, [u8; 32]) {
    let lines = make_key(&dir.join("keys"), repo).unwrap();
    let public = fs::read_to_string(dir.join("keys").join(PUBLIC)).unwrap();
    let public: [u8; 32] = (0..32).map(|i| u8::from_str_radix(&public.trim()[i * 2..i * 2 + 2], 16).unwrap()).collect::<Vec<_>>().try_into().unwrap();
    assert!(lines.contains(&format!("CROSSBOW_UPDATE_KEYS={}", hex(&public))));
    (dir.join("keys").join(PRIVATE), public)
}

fn options(dir: &Path, archive: PathBuf, key: PathBuf, version: &str) -> Sign {
    let notes = dir.join("notes.txt");
    fs::write(&notes, "Faster tabs.\nFewer crashes.\n").unwrap();
    Sign { archive, version: Version::parse(version).unwrap(), notes, key, url: URL.into(), team: TEAM.into(), valid_days: VALID_DAYS, out: dir.join("stable.json") }
}

fn signed_by_team(_: &Path) -> Result<String, String> {
    Ok(TEAM.into())
}

#[test]
fn release_key_writes_a_private_key_only_its_owner_reads_and_prints_only_the_public_key() {
    let (dir, repo) = (folder("key"), folder("key-repo"));
    let lines = make_key(&dir.join("keys"), &repo).unwrap();
    let private = dir.join("keys").join(PRIVATE);
    assert_eq!(fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(dir.join("keys")).unwrap().permissions().mode() & 0o777, 0o700, "a new folder is private too");
    let document = fs::read(&private).unwrap();
    let key = SigningKey::from_pkcs8(&document).expect("a PKCS#8 Ed25519 key");
    let public = hex(&key.public());
    assert_eq!(fs::read_to_string(dir.join("keys").join(PUBLIC)).unwrap(), format!("{public}\n"));
    assert_eq!(lines[..2], [format!("public key: {public}"), format!("CROSSBOW_UPDATE_KEYS={public}")]);
    // The seed (bytes 16..48 of the 83-byte PKCS#8 v2 document, after its
    // fixed header) is in no printed form: hex, base64 or raw.
    assert_eq!((document.len(), &document[..16]), (83, &[0x30, 0x51, 0x02, 0x01, 0x01, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20][..]));
    let printed = lines.join("\n");
    let seed = &document[16..48];
    for form in [hex(seed), hex(&document), base64(seed), base64(&document), String::from_utf8_lossy(seed).into_owned()] {
        assert!(!printed.contains(&form), "the private key is printed");
    }
    cleanup(&dir);
    cleanup(&repo);
}

/// Unpadded base64, enough to look for a printed key.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    bytes.chunks(3).flat_map(|chunk| {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        (0..=chunk.len()).map(move |i| ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char)
    }).collect()
}

#[test]
fn release_key_refuses_to_overwrite_or_to_write_inside_the_repository() {
    let (dir, repo) = (folder("overwrite"), folder("overwrite-repo"));
    make_key(&dir.join("keys"), &repo).unwrap();
    let before = fs::read(dir.join("keys").join(PRIVATE)).unwrap();
    let error = make_key(&dir.join("keys"), &repo).unwrap_err();
    assert!(error.ends_with("already holds a release key; it is never overwritten"), "{error}");
    assert_eq!(fs::read(dir.join("keys").join(PRIVATE)).unwrap(), before, "the first key is untouched");
    // A public key file alone also stops it.
    fs::create_dir(dir.join("other")).unwrap();
    fs::write(dir.join("other").join(PUBLIC), b"x\n").unwrap();
    assert!(make_key(&dir.join("other"), &repo).is_err());
    assert!(!dir.join("other").join(PRIVATE).exists());
    // Inside the repository, even through a symlink, or in a folder still to be made.
    std::os::unix::fs::symlink(&repo, dir.join("link")).unwrap();
    for inside in [repo.join("keys"), repo.clone(), dir.join("link/keys")] {
        let error = make_key(&inside, &repo).unwrap_err();
        assert!(error.starts_with("a private key never goes inside the repository"), "{error}");
    }
    assert!(!repo.join("keys").exists() && !repo.join(PRIVATE).exists());
    assert!(key(&["--out".into()], &repo).unwrap_err().starts_with("usage:"));
    cleanup(&dir);
    cleanup(&repo);
}

#[test]
fn release_sign_writes_a_feed_that_opens_with_the_public_key_and_matches_the_archive() {
    let (dir, repo) = (folder("sign"), folder("sign-repo"));
    let (key, public) = keys(&dir, &repo);
    let archive = archive(&dir, "0.2.0", 7);
    let options = options(&dir, archive.clone(), key, "0.2.0");
    let lines = sign_release(&options, &signed_by_team, NOW).unwrap();
    assert_eq!(lines[0], format!("Crossbow 0.2.0 (build 7) for macOS 11.0 or later, signed by Team ID {TEAM}"));
    let text = fs::read(&options.out).unwrap();
    let release = feed::open(&text, &[public]).expect("the feed opens with the key's public half");
    let bytes = fs::read(&archive).unwrap();
    assert_eq!((release.version.as_str(), release.build, release.minimum_macos.as_str()), ("0.2.0", 7, "11.0"));
    assert_eq!((release.archive.url.as_str(), release.archive.size, release.published.as_str()), (URL, bytes.len() as u64, "2026-11-01T11:23:42Z"));
    assert_eq!(release.expires, "2026-11-15T11:23:42Z", "current for 14 days");
    assert!(lines.contains(&"the feed expires at 2026-11-15T11:23:42Z: sign it again before then, as Crossbow installs nothing from an expired feed".to_string()));
    assert_eq!(release.archive.sha256, sha256sum(&archive));
    assert_eq!(release.notes, "Faster tabs.\nFewer crashes.", "the notes, without the file's last newline");
    assert!(feed::archive_verifies(&release, &[public], &release.sha256(), bytes.len() as u64));
    assert_eq!(release.minimum_macos(), MacVersion(11, 0, 0));
    // Another key opens nothing.
    let (other, _) = SigningKey::generate().unwrap();
    assert_eq!(feed::open(&text, &[other.public()]), Err(feed::FeedError::Signature));
    cleanup(&dir);
    cleanup(&repo);
}

/// The SHA-256 as macOS's `/sbin/sha256sum` gives it: a second opinion that shares no
/// code with the feed.
fn sha256sum(path: &Path) -> String {
    let out = std::process::Command::new("/sbin/sha256sum").arg(path).output().unwrap();
    String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_owned()
}

#[test]
fn release_sign_refuses_what_crossbow_would_refuse_and_writes_nothing() {
    let (dir, repo) = (folder("refuse"), folder("refuse-repo"));
    let (key, _) = keys(&dir, &repo);
    let good = archive(&dir, "0.2.0", 7);
    let refused = |options: &Sign, team: &dyn Fn(&Path) -> Result<String, String>| {
        let error = sign_release(options, team, NOW).unwrap_err();
        assert!(!options.out.exists(), "nothing is written when {error}");
        error
    };
    // A key file others can read.
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(refused(&options(&dir, good.clone(), key.clone(), "0.2.0"), &signed_by_team).ends_with("must be a file only you can read (chmod 600)"));
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    // A file that is not a key.
    let junk = dir.join("junk.pk8");
    fs::write(&junk, b"not a key").unwrap();
    fs::set_permissions(&junk, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(refused(&options(&dir, good.clone(), junk, "0.2.0"), &signed_by_team).ends_with("is not a release key from cargo xtask release-key"));
    // Another version than the archive's.
    assert_eq!(refused(&options(&dir, good.clone(), key.clone(), "0.3.0"), &signed_by_team), "the archive holds Crossbow 0.2.0, not 0.3.0");
    // Notes that aren't plain text, or are too long.
    let mut bad_notes = options(&dir, good.clone(), key.clone(), "0.2.0");
    for notes in ["Click\u{1b}[31m here".to_string(), "x".repeat(feed::MAX_NOTES + 1)] {
        fs::write(&bad_notes.notes, notes).unwrap();
        assert!(refused(&bad_notes, &signed_by_team).starts_with("the release notes must be plain text"));
    }
    bad_notes.notes = dir.join("notes-elsewhere.txt");
    assert!(refused(&bad_notes, &signed_by_team).contains("notes-elsewhere.txt"));
    // An archive URL Crossbow would not fetch, or fetches only in its tests.
    for url in ["http://updates.example.com/Crossbow.tar.gz", "http://127.0.0.1:8080/Crossbow.tar.gz", "updates.example.com/Crossbow.tar.gz"] {
        let mut plain_http = options(&dir, good.clone(), key.clone(), "0.2.0");
        plain_http.url = url.into();
        assert_eq!(refused(&plain_http, &signed_by_team), "--url must be an https URL: Crossbow downloads updates only over https", "{url}");
    }
    // An app signed by another team than the one named.
    let other_team = |_: &Path| Ok::<String, String>("ZYXWV98765".into());
    assert_eq!(refused(&options(&dir, good.clone(), key.clone(), "0.2.0"), &other_team), "the app in the archive is signed by Team ID ZYXWV98765, not ABCDE12345");
    // A feed current for longer than Crossbow accepts.
    let mut too_long = options(&dir, good.clone(), key.clone(), "0.2.0");
    too_long.valid_days = MAX_VALID_DAYS + 1;
    assert_eq!(refused(&too_long, &signed_by_team), "a feed stays current for 1 to 30 days");
    // An archive that isn't a .tar.gz of Crossbow.app.
    let not_tar = dir.join("not.tar.gz");
    fs::write(&not_tar, b"plain bytes").unwrap();
    assert_eq!(refused(&options(&dir, not_tar, key.clone(), "0.2.0"), &signed_by_team), "the archive is refused: it is not a .tar.gz");
    // An app without a Developer ID signature, as the system reads it.
    assert!(refused(&options(&dir, good.clone(), key.clone(), "0.2.0"), &developer_id).starts_with("the app in the archive has no Team ID"));
    // An existing feed is never overwritten.
    let exists = options(&dir, good, key, "0.2.0");
    fs::write(&exists.out, b"old feed").unwrap();
    assert!(sign_release(&exists, &signed_by_team, NOW).unwrap_err().ends_with("already exists; choose a new name"));
    assert_eq!(fs::read(&exists.out).unwrap(), b"old feed");
    // Every scratch folder is gone.
    let left = fs::read_dir(std::env::temp_dir()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&format!("crossbow-release-sign-{}-", std::process::id()))).count();
    assert_eq!(left, 0, "unpacked apps are removed");
    cleanup(&dir);
    cleanup(&repo);
}

#[test]
fn release_sign_takes_each_argument_once() {
    let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let full = ["--archive", "a.tar.gz", "--version", "0.2.0", "--notes", "n.txt", "--key", "k.pk8", "--url", URL, "--team", TEAM, "--out", "f.json"];
    let parsed = Sign::parse(&args(&full)).unwrap();
    assert_eq!((parsed.version, parsed.url.as_str(), parsed.team.as_str(), parsed.valid_days, parsed.out.as_path()), (Version(0, 2, 0), URL, TEAM, 14, Path::new("f.json")));
    assert!(Sign::parse(&args(&full[..12])).unwrap_err().starts_with("usage:"), "--out is required");
    assert!(Sign::parse(&args(&full[..13])).unwrap_err().starts_with("usage:"), "a flag without its value");
    assert!(Sign::parse(&args(&[&full[..10], &full[12..]].concat())).unwrap_err().starts_with("usage:"), "--team is required");
    for (days, ok) in [("1", true), ("30", true), ("0", false), ("31", false), ("14.5", false), ("", false)] {
        let parsed = Sign::parse(&args(&[&full[..], &["--valid-days", days]].concat()));
        assert_eq!(parsed.map(|sign| sign.valid_days.to_string()), if ok { Ok(days.to_string()) } else { Err("--valid-days is a whole number of days from 1 to 30".to_string()) }, "{days:?}");
    }
    for team in ["abcde12345", "ABCDE1234", "ABCDE\"1234"] {
        let mut bad = full;
        bad[11] = team;
        assert_eq!(Sign::parse(&args(&bad)).unwrap_err(), "--team is a Team ID: 10 capital letters and digits", "{team}");
    }
    assert_eq!(Sign::parse(&args(&[&full[..], &["--key", "other.pk8"]].concat())).unwrap_err(), "--key is given twice");
    assert!(Sign::parse(&args(&[&full[..], &["--force", "1"]].concat())).unwrap_err().starts_with("usage:"));
    let mut bad = full;
    bad[3] = "0.2";
    assert_eq!(Sign::parse(&args(&bad)).unwrap_err(), "--version is MAJOR.MINOR.PATCH, digits only");
}
