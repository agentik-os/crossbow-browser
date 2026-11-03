//! `update-lists` against list files served on a loopback port: what it
//! writes when both lists check out, and that any failed check changes
//! nothing. Nothing leaves this Mac.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;

/// A server on an unused loopback port answering each path with a status
/// and body; any other path is a 404. The base address to fetch from.
fn serve(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() { continue; }
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|read| read > 2) { header.clear(); }
            let path = line.split_whitespace().nth(1).unwrap_or_default().to_owned();
            let (status, body) = routes.iter().find(|(route, ..)| *route == path).map_or((404, b"Not here\n".to_vec()), |(_, status, body)| (*status, body.clone()));
            let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}/lists/")
}

/// A list as EasyList's server sends it: header, then `rules` rules.
fn list(title: &str, version: &str, rules: usize) -> Vec<u8> {
    let mut text = format!("[Adblock Plus 2.0]\n! Version: {version}\n! Title: {title}\n! Expires: 4 days (update frequency)\n!\n");
    for i in 0..rules { text.push_str(&format!("||ad{i}.{}.example^\n", title.to_ascii_lowercase())); }
    text.into_bytes()
}

fn both(version: &str, rules: usize) -> Vec<(&'static str, u16, Vec<u8>)> {
    vec![("/lists/easylist.txt", 200, list("EasyList", version, rules)), ("/lists/easyprivacy.txt", 200, list("EasyPrivacy", version, rules))]
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crossbow-update-lists-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The files in `dir` and their bytes.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir).unwrap().flatten().map(|entry| (entry.file_name().to_string_lossy().into_owned(), std::fs::read(entry.path()).unwrap())).collect();
    files.sort();
    files
}

fn shasum(bytes: &[u8]) -> String {
    let path = std::env::temp_dir().join(format!("crossbow-update-lists-sum-{}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("shasum").args(["-a", "256"]).arg(&path).output().unwrap();
    std::fs::remove_file(&path).unwrap();
    String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_owned()
}

#[test]
fn both_lists_and_their_sources_are_written_when_both_check_out() {
    let dir = dir("written");
    let base = serve(both("202610010000", 100));
    let summary = update(&base, &dir, "2026-10-01").unwrap();
    assert_eq!(summary.len(), 2, "{summary:?}");
    for (file, title) in LISTS {
        let bytes = std::fs::read(dir.join(file)).unwrap();
        assert_eq!(bytes, list(title, "202610010000", 100), "{file} is written unchanged");
    }
    let sources = std::fs::read_to_string(dir.join("SOURCES")).unwrap();
    let easylist = list("EasyList", "202610010000", 100);
    let line = format!("easylist.txt version=202610010000 fetched=2026-10-01 bytes={} sha256={} source={base}easylist.txt", easylist.len(), shasum(&easylist));
    assert!(sources.lines().any(|l| l == line), "{sources}");
    assert!(sources.contains("CC BY-SA 3.0 Unported"), "SOURCES names the licence chosen");
    assert!(!dir.read_dir().unwrap().flatten().any(|entry| entry.file_name().to_string_lossy().contains("partial")), "no partial file is left");

    // A newer version with a few rules fewer replaces both.
    let base = serve(both("202611010000", 95));
    update(&base, &dir, "2026-11-01").unwrap();
    assert_eq!(std::fs::read(dir.join("easyprivacy.txt")).unwrap(), list("EasyPrivacy", "202611010000", 95));
    assert!(std::fs::read_to_string(dir.join("SOURCES")).unwrap().contains("easyprivacy.txt version=202611010000 fetched=2026-11-01 "));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_list_that_fails_any_check_changes_nothing() {
    let dir = dir("refused");
    update(&serve(both("202610010000", 100)), &dir, "2026-10-01").unwrap();
    let before = snapshot(&dir);
    let good = list("EasyList", "202611010000", 100);
    let mut not_utf8 = list("EasyPrivacy", "202611010000", 100);
    not_utf8.extend_from_slice(b"\xff\xfe\n");
    let cases: Vec<(Vec<u8>, u16, &str)> = vec![
        (Vec::new(), 404, "couldn't fetch"),
        (b"[Adblock Plus 2.0]\n! Title: EasyPrivacy\n\x00\x01".to_vec(), 200, "isn't text"),
        (not_utf8, 200, "isn't text"),
        (b"<html>Moved</html>\n".to_vec(), 200, "isn't an Adblock Plus list"),
        (list("Fanboy", "202611010000", 100), 200, "isn't EasyPrivacy"),
        (b"[Adblock Plus 2.0]\n! Title: EasyPrivacy\n||ad.example^\n".to_vec(), 200, "has no version"),
        (list("EasyPrivacy", "202609010000", 100), 200, "older than"),
        (list("EasyPrivacy", "202611010000", 70), 200, "fewer rules"),
    ];
    for (body, status, reason) in cases {
        let base = serve(vec![("/lists/easylist.txt", 200, good.clone()), ("/lists/easyprivacy.txt", status, body)]);
        let error = update(&base, &dir, "2026-11-01").unwrap_err();
        assert!(error.contains("easyprivacy.txt") && error.contains(reason), "{reason}: {error}");
        assert_eq!(snapshot(&dir), before, "{reason}: nothing changed, EasyList included");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_https_is_fetched_except_from_loopback_fixtures() {
    for url in ["http://example.invalid/easylist.txt", "file:///etc/hosts", "ftp://example.invalid/easylist.txt"] {
        assert!(fetch(url).is_err(), "{url} is refused before any connection");
    }
    assert_eq!(BASE, "https://easylist.to/easylist/");
}

#[test]
fn dates_are_written_in_utc() {
    assert_eq!(civil(0), "1970-01-01");
    assert_eq!(civil(19_723), "2024-01-01");
    assert_eq!(civil(19_782), "2024-02-29");
    assert_eq!(civil(20_757), "2026-10-31");
    assert_eq!(civil(20_758), "2026-11-01");
}
