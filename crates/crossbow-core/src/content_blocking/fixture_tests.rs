//! The lists `cargo xtask blocking-fixture` writes, through the real parser,
//! matcher and element hiding, against the fixture page's own addresses.
//! xtask doesn't depend on this crate, so the lists are read from its folder.

use super::filters::{RequestType, coverage};
use super::hiding::{Hiding, RULE_END};
use super::matcher::{CompiledList, Request, blocks, compile};
use std::sync::Arc;

const ADS: &str = include_str!("../../../../xtask/src/blocking_fixture/fixture-ads.txt");
const COOKIES: &str = include_str!("../../../../xtask/src/blocking_fixture/fixture-cookies.txt");
/// The fixture page; its "third-party" requests go to localhost.
const PAGE: &str = "http://127.0.0.1:8737/";
const FRAME: &str = "http://localhost:8737/frame.html";

fn blocked(lists: &[Arc<CompiledList>], url: &str, kind: RequestType) -> bool {
    blocks(lists, &Request::new(url, kind, PAGE).unwrap())
}

/// The selectors hidden in a document at `url`.
fn hidden(lists: &[Arc<CompiledList>], url: &str) -> Vec<String> {
    let Some(sheet) = Hiding::new(lists).sheet(url) else { return Vec::new() };
    format!("{}{}", sheet.specific, sheet.generic).split_terminator(RULE_END).map(str::to_owned).collect()
}

#[test]
fn each_list_skips_only_what_the_fixture_says_it_skips() {
    let ads = coverage(ADS);
    assert_eq!(ads.reasons.into_iter().collect::<Vec<_>>(), [("procedural filter (#?#)".to_owned(), 1)]);
    assert_eq!((ads.network.supported, ads.network.total), (1, 1));
    let cookies = coverage(COOKIES);
    assert!(cookies.reasons.is_empty(), "{:?}", cookies.reasons);
    let (ads, cookies) = (compile(ADS), compile(COOKIES));
    assert_eq!((ads.rules, ads.unsupported), (5, 1), "Settings: 5 rules · 1 unsupported");
    assert_eq!((cookies.rules, cookies.unsupported), (2, 0));
}

#[test]
fn the_ads_list_blocks_the_localhost_ad_and_not_the_first_party_requests() {
    let (ads, cookies) = ([compile(ADS)], [compile(COOKIES)]);
    assert!(blocked(&ads, "http://localhost:8737/fixture-ad/ad.js", RequestType::Script));
    assert!(blocked(&ads, "http://localhost:8737/fixture-ad/banner.svg", RequestType::Image));
    let both = [compile(ADS), compile(COOKIES)];
    assert!(!blocked(&both, "http://127.0.0.1:8737/fixture-ad/own.svg", RequestType::Image), "$third-party");
    assert!(!blocked(&both, "http://127.0.0.1:8737/first-party.js", RequestType::Script));
    assert!(!blocked(&both, FRAME, RequestType::Subdocument));
    assert!(blocked(&cookies, "http://127.0.0.1:8737/fixture-cmp.js", RequestType::Script));
    assert!(!blocked(&ads, "http://127.0.0.1:8737/fixture-cmp.js", RequestType::Script), "the cookie list's rule only");
    assert!(!blocked(&cookies, "http://localhost:8737/fixture-ad/ad.js", RequestType::Script), "the ads list's rule only");
}

#[test]
fn hiding_follows_the_expectations_the_page_states() {
    let (ads, cookies) = ([compile(ADS)], [compile(COOKIES)]);
    let page = hidden(&ads, PAGE);
    for selector in [".fixture-ad", ".fixture-site-ad"] {
        assert!(page.iter().any(|hidden| hidden == selector), "{selector} on 127.0.0.1: {page:?}");
    }
    assert!(!page.iter().any(|hidden| hidden == ".fixture-kept"), "unhidden on 127.0.0.1: {page:?}");
    // The localhost frame: the generic rule only; the unhiding is 127.0.0.1's.
    let frame = hidden(&ads, FRAME);
    assert!(frame.iter().any(|hidden| hidden == ".fixture-ad"), "{frame:?}");
    assert!(!frame.iter().any(|hidden| hidden == ".fixture-site-ad"), "{frame:?}");
    assert!(frame.iter().any(|hidden| hidden == ".fixture-kept"), "{frame:?}");
    assert!(!page.iter().chain(&frame).any(|hidden| hidden.contains("has-text")), "procedural rules are skipped");
    assert_eq!(hidden(&cookies, PAGE), ["#fixture-cookie-banner"]);
    assert!(!hidden(&ads, PAGE).iter().any(|hidden| hidden == "#fixture-cookie-banner"));
}
