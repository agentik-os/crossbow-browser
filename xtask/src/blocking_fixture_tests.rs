//! The fixture server on a loopback port: its page, its consent log and its
//! lists. Nothing leaves this Mac.

use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// A server on an unused loopback port, and its log.
fn start() -> (u16, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Log::default();
    let served = log.clone();
    std::thread::spawn(move || serve(listener, served));
    (port, log)
}

/// One GET over loopback: the status code and the body.
fn get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").expect("a response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

#[test]
fn the_page_has_first_and_third_party_requests_hiding_targets_and_a_banner() {
    let (port, _) = start();
    let (status, page) = get(port, "/");
    assert_eq!(status, 200);
    for part in [
        "src=\"/first-party.js\"",
        &format!("src=\"http://localhost:{port}/fixture-ad/ad.js\""),
        &format!("src=\"http://localhost:{port}/fixture-ad/banner.svg\""),
        "src=\"/fixture-ad/own.svg\"",
        &format!("src=\"http://localhost:{port}/frame.html\""),
        "class=\"fixture-ad\"",
        "class=\"fixture-site-ad\"",
        "class=\"fixture-kept\"",
        "attachShadow",
        "id=\"fixture-cookie-banner\"",
        "src=\"/fixture-cmp.js\"",
        "overflow: hidden",
    ] {
        assert!(page.contains(part), "{part}");
    }
    let (status, frame) = get(port, "/frame.html");
    assert!(status == 200 && frame.contains("class=\"fixture-ad\""));
    for path in ["/first-party.js", "/fixture-ad/ad.js", "/fixture-cmp.js", "/fixture-ad/banner.svg", "/fixture-ad/own.svg"] {
        assert_eq!(get(port, path).0, 200, "{path}");
    }
    assert_eq!(get(port, "/missing").0, 404);
}

#[test]
fn every_request_and_each_banner_answer_is_logged() {
    let (port, log) = start();
    get(port, "/first-party.js");
    assert_eq!(get(port, "/consent?answer=accept").0, 204);
    assert_eq!(get(port, "/consent?answer=reject").0, 204);
    assert_eq!(get(port, "/consent?answer=other").0, 400);
    let lines = log.lines();
    assert!(lines.contains(&"GET /first-party.js".to_owned()), "{lines:?}");
    assert_eq!(lines.iter().filter(|line| line.starts_with("consent:")).cloned().collect::<Vec<_>>(), ["consent: accept", "consent: reject"]);
    let (status, shown) = get(port, "/log");
    assert!(status == 200 && shown.contains("consent: accept\nconsent: reject\n"), "{shown}");
}

#[test]
fn the_fixture_lists_are_served_and_written_where_asked() {
    let (port, _) = start();
    assert_eq!(get(port, "/lists/fixture-ads.txt"), (200, ADS_LIST.to_owned()));
    assert_eq!(get(port, "/lists/fixture-cookies.txt"), (200, COOKIES_LIST.to_owned()));
    let dir = std::env::temp_dir().join(format!("crossbow-fixture-lists-{}", std::process::id()));
    write_lists(&dir).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("fixture-ads.txt")).unwrap(), ADS_LIST);
    assert_eq!(std::fs::read_to_string(dir.join("fixture-cookies.txt")).unwrap(), COOKIES_LIST);
    std::fs::remove_dir_all(&dir).unwrap();
}
