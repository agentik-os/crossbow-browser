//! The supported syntax against synthetic addresses, the rules skipped, and
//! a 50,000-rule list's compile and match times.

use super::*;
use crate::content_blocking::filters::{RequestType::*, parse};
use std::time::Instant;

const PAGE: &str = "https://news.example/story";

fn one(text: &str) -> Vec<Arc<CompiledList>> {
    vec![compile(text)]
}

fn blocked(lists: &[Arc<CompiledList>], url: &str, kind: RequestType, context: &str) -> bool {
    blocks(lists, &Request::new(url, kind, context).unwrap_or_else(|| panic!("{url} is a web address")))
}

/// Blocked as a script requested by [`PAGE`].
fn script(lists: &[Arc<CompiledList>], url: &str) -> bool {
    blocked(lists, url, Script, PAGE)
}

#[test]
fn host_rules_block_the_host_and_its_subdomains_only() {
    let lists = one("||ads.example^\n");
    for url in ["https://ads.example/a.js", "http://cdn.ads.example/b.js", "https://ads.example:8443/", "https://ADS.Example/c.js", "wss://ads.example/socket"] {
        assert!(script(&lists, url), "{url}");
    }
    for url in ["https://notads.example/a.js", "https://ads.example.net/a.js", "https://news.example/ads.example/a.js", "https://news.example/?u=ads.example"] {
        assert!(!script(&lists, url), "{url}");
    }
}

#[test]
fn anchors_separators_and_wildcards_follow_the_adblock_syntax() {
    let lists = one("|https://cdn.example/ad\nbanner.gif|\n/promo/*.js\n||track.example/pixel^\n||img.example/ads/\n-sponsor^\n");
    let cases = [
        ("https://cdn.example/ad/1.js", true),
        ("https://x.example/?to=https://cdn.example/ad", false),
        ("https://x.example/banner.gif", true),
        ("https://x.example/banner.gif?v=2", false),
        ("https://x.example/promo/a/b.js", true),
        ("https://x.example/promo/a/b.css", false),
        ("https://track.example/pixel?id=1", true),
        ("https://a.track.example/pixel", true),
        ("https://track.example/pixels", false),
        ("https://img.example/ads/1.png", true),
        ("https://img.example:8080/ads/1.png", false),
        ("https://x.example/top-sponsor/", true),
        ("https://x.example/top-sponsor", true),
        ("https://x.example/top-sponsored", false),
    ];
    for (url, expected) in cases {
        assert_eq!(script(&lists, url), expected, "{url}");
    }
}

#[test]
fn case_is_ignored_unless_the_rule_asks_for_it() {
    let lists = one("/BannerAd.\n/TrackMe.$match-case\n");
    assert!(script(&lists, "https://x.example/bannerad.js"));
    assert!(script(&lists, "https://x.example/TrackMe.js"));
    assert!(!script(&lists, "https://x.example/trackme.js"));
}

#[test]
fn third_party_uses_a_simplified_site_rule() {
    // Where it agrees with the public-suffix list.
    assert!(same_site("www.example.com", "cdn.example.com"));
    assert!(same_site("a.example.co.uk", "b.example.co.uk"));
    assert!(!same_site("example.co.uk", "other.co.uk"));
    assert!(same_site("shop.example.com.au", "example.com.au"));
    assert!(!same_site("127.0.0.1", "localhost"));
    assert!(!same_site("10.0.0.1", "10.0.0.2"));
    // Where it differs: shared hosting suffixes count as one site.
    assert!(same_site("alice.github.io", "bob.github.io"));
    assert!(same_site("a.blogspot.com", "b.blogspot.com"));

    let third = one("||widgets.example^$third-party\n");
    assert!(blocked(&third, "https://widgets.example/w.js", Script, "https://news.example/"));
    assert!(!blocked(&third, "https://widgets.example/w.js", Script, "https://www.widgets.example/"));
    assert!(!blocked(&third, "https://widgets.example/w.js", Script, ""), "unknown context: party rules don't apply");
    let first = one("||widgets.example^$~third-party\n");
    assert!(!blocked(&first, "https://widgets.example/w.js", Script, "https://news.example/"));
    assert!(blocked(&first, "https://widgets.example/w.js", Script, "https://www.widgets.example/"));
    for alias in ["$3p", "$~1p", "$~first-party"] {
        let lists = one(&format!("||widgets.example^{alias}\n"));
        assert!(blocked(&lists, "https://widgets.example/w.js", Script, PAGE) && !blocked(&lists, "https://widgets.example/w.js", Script, "https://widgets.example/"), "{alias}");
    }
}

#[test]
fn domain_options_use_the_requesting_document_and_the_most_specific_entry_wins() {
    let lists = one("/ad.js$domain=news.example|~sports.news.example\n/only-off.js$domain=~quiet.example\n");
    assert!(blocked(&lists, "https://cdn.example/ad.js", Script, "https://news.example/"));
    assert!(blocked(&lists, "https://cdn.example/ad.js", Script, "https://world.news.example/"));
    assert!(!blocked(&lists, "https://cdn.example/ad.js", Script, "https://sports.news.example/"));
    assert!(!blocked(&lists, "https://cdn.example/ad.js", Script, "https://news.example.evil/"));
    assert!(!blocked(&lists, "https://cdn.example/ad.js", Script, "https://other.example/"));
    assert!(blocked(&lists, "https://cdn.example/only-off.js", Script, "https://other.example/"));
    assert!(!blocked(&lists, "https://cdn.example/only-off.js", Script, "https://www.quiet.example/"));
    assert!(!blocked(&lists, "https://cdn.example/ad.js", Script, ""), "unknown context: domain rules don't apply");
    let from = one("/ad.js$from=news.example\n");
    assert!(blocked(&from, "https://cdn.example/ad.js", Script, "https://news.example/"));
}

#[test]
fn type_options_select_request_types() {
    let lists = one("||a.example^$script\n||b.example^$~image\n||c.example^$xhr,subdocument\n||d.example^$css,frame,beacon\n||e.example^$websocket\n");
    assert!(blocked(&lists, "https://a.example/x", Script, PAGE) && !blocked(&lists, "https://a.example/x", Image, PAGE));
    assert!(!blocked(&lists, "https://b.example/x", Image, PAGE) && blocked(&lists, "https://b.example/x", Font, PAGE));
    assert!(blocked(&lists, "https://c.example/x", Xhr, PAGE) && blocked(&lists, "https://c.example/x", Subdocument, PAGE) && !blocked(&lists, "https://c.example/x", Media, PAGE));
    assert!(blocked(&lists, "https://d.example/x", Stylesheet, PAGE) && blocked(&lists, "https://d.example/x", Subdocument, PAGE) && blocked(&lists, "https://d.example/x", Ping, PAGE));
    assert!(!blocked(&lists, "https://e.example/x", Xhr, PAGE));
    assert!(blocked(&lists, "wss://e.example/x", Other, PAGE), "a ws or wss address is a websocket whatever the engine calls it");
}

#[test]
fn exceptions_apply_across_lists_and_important_rules_override_them() {
    let ads = compile("||ads.example^\n||must.example^$important\n");
    let allow = compile("@@||ads.example/ok/\n@@||must.example^\n");
    let both = [ads.clone(), allow.clone()];
    assert!(blocked(&both, "https://ads.example/no/1.js", Script, PAGE));
    assert!(!blocked(&both, "https://ads.example/ok/1.js", Script, PAGE), "an exception in another list applies");
    assert!(blocked(&both, "https://must.example/1.js", Script, PAGE), "important beats an exception");
    assert!(!blocked(&[allow], "https://other.example/1.js", Script, PAGE), "an exception alone blocks nothing");
}

#[test]
fn unsupported_rules_are_counted_and_never_applied() {
    let skipped = [
        "/banner[0-9]+\\.gif/",
        "||redirect.example^$redirect=noop.js",
        "||popup.example^$popup",
        "||csp.example^$csp=script-src 'none'",
        "||doc.example^$document",
        "||bad.example^$badfilter",
        "||unknown.example^$some-new-option",
        "||entity.example^$domain=news.*",
        "@@||important.example^$important",
        "*",
        "$important",
        "news.example#?#.ad:has-text(Sponsored)",
        "news.example##+js(nobab)",
        "@@||elem.example^$elemhide",
        // Read only bare or with `domain=`: see hiding_tests.rs.
        "@@||generic.example^$generichide,script",
    ];
    let parsed = parse(&skipped.join("\n"));
    assert_eq!((parsed.rules(), parsed.unsupported), (0, skipped.len()));
    let lists = one(&format!("{}\n||real.example^\n", skipped.join("\n")));
    assert_eq!((lists[0].rules, lists[0].unsupported), (1, skipped.len()));
    for url in ["https://x.example/banner1.gif", "https://redirect.example/a.js", "https://popup.example/", "https://csp.example/", "https://doc.example/", "https://bad.example/", "https://unknown.example/", "https://entity.example/", "https://anything.example/"] {
        assert!(!blocked(&lists, url, Script, PAGE), "{url}");
    }
    assert!(blocked(&lists, "https://real.example/", Script, PAGE));
}

#[test]
fn comments_headers_hosts_files_and_plain_host_names_are_read() {
    let text = "[Adblock Plus 2.0]\n! Title: Fixture list\n# hosts comment\n\n127.0.0.1 localhost\n0.0.0.0 0.0.0.0\n::1 ip6-localhost\n0.0.0.0 tracker.example  # inline comment\n127.0.0.1 pixel.example beacon.example\nplain.example\n";
    let parsed = parse(text);
    assert_eq!((parsed.rules(), parsed.unsupported), (4, 0), "four hosts; comments and the hosts file's own names are neither");
    let lists = one(text);
    for url in ["https://tracker.example/t.gif", "https://pixel.example/p", "https://a.beacon.example/b", "https://plain.example/x"] {
        assert!(script(&lists, url), "{url}");
    }
    assert!(!script(&lists, "http://localhost/fixture.js"), "the hosts file's own names are never rules");
    assert!(!script(&lists, "https://notplain.example/x"), "a plain host name is a host rule, not text");
}

#[test]
fn only_web_addresses_are_matched() {
    for url in ["data:text/javascript,1", "blob:https://news.example/1", "about:blank", "file:///tmp/a.js", "chrome-extension://x/a.js", "not an address"] {
        assert!(Request::new(url, Script, PAGE).is_none(), "{url}");
    }
    assert!(Request::new("https://news.example/a.js", Script, "").is_some());
}

#[test]
fn the_same_text_compiles_once_and_is_shared() {
    let text = "||shared-fixture.example^\n";
    let a = compile(text);
    let b = compile(text);
    assert!(Arc::ptr_eq(&a, &b), "one compiled copy for the same text");
    assert_eq!(a.id, sha256_hex(text.as_bytes()));
    assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let c = compile("||other-fixture.example^\n");
    assert!(!Arc::ptr_eq(&a, &c));
}

/// A deterministic synthetic list, in the proportions of real ones: 60 %
/// host rules, 15 % host-and-path rules, 15 % text rules with options, 5 %
/// path rules (a few with no usable token) and 5 % exceptions.
fn synthetic(count: usize) -> String {
    let mut text = String::from("! Synthetic list for the benchmark\n");
    for i in 0..count {
        let line = match i % 20 {
            0..=11 => format!("||ad{i}.track{}.example^", i % 97),
            12..=14 => format!("||cdn{}.example/banners/{i}/", i % 89),
            15..=17 => format!("/promo-{i}-slot^$script,third-party,domain=site{}.example|~safe{}.example", i % 13, i % 7),
            18 if i % 1000 == 18 => format!("*banner*{i}*"),
            18 => format!("/ads/{i}/*.js|"),
            _ => format!("@@||ad{}.track{}.example/allowed/", i - 19, (i - 19) % 97),
        };
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Requests and the documents that make them: blocked hosts, blocked
/// paths, text rules on their own sites, unrelated addresses and
/// exceptions, in turn.
fn requests(count: usize) -> Vec<(String, String)> {
    (0..count).map(|i| {
        let k = |offset: usize| (i * 20 + offset) % 50_000;
        match i % 5 {
            0 => (format!("https://ad{}.track{}.example/x.js?i={i}", k(i % 12), k(i % 12) % 97), "https://news.example/".into()),
            1 => (format!("https://cdn{}.example/banners/{}/a.png", k(12) % 89, k(12)), "https://news.example/".into()),
            2 => (format!("https://ads.network.example/promo-{}-slot/a.js", k(15)), format!("https://site{}.example/", k(15) % 13)),
            3 => (format!("https://www.example{i}.com/assets/app.{i}.js?v={i}"), "https://news.example/".into()),
            _ => (format!("https://ad{}.track{}.example/allowed/x.js", k(0), k(0) % 97), "https://news.example/".into()),
        }
    }).collect()
}

struct Measured {
    compiled: std::time::Duration,
    mb: f64,
    median: std::time::Duration,
    p99: std::time::Duration,
}

#[test]
fn fifty_thousand_rules_compile_and_match_within_budget() {
    let measured = measure();
    // The median survives a loaded machine; the compile time and p99 don't,
    // so the hand-run benchmark below checks those.
    assert!(measured.mb < 24.0, "{:.1} MB", measured.mb);
    assert!(measured.median.as_micros() < 50, "median {:?}", measured.median);
}

#[test]
#[ignore = "checks compile time and p99 against their budgets; run by hand on an idle machine with --nocapture"]
fn fifty_thousand_rules_meet_the_time_budget() {
    let Measured { compiled, mb, median, p99 } = measure();
    // Release targets are 250 ms, 24 MB, 5 µs and 50 µs; a debug build gets ten times as long.
    assert!(compiled.as_millis() < 2_500, "compiled in {compiled:?}");
    assert!(mb < 24.0, "{mb:.1} MB");
    assert!(median.as_micros() < 50, "median {median:?}");
    assert!(p99.as_micros() < 500, "p99 {p99:?}");
}

fn measure() -> Measured {
    let text = synthetic(50_000);
    let started = Instant::now();
    let list = compile(&text);
    let compiled = started.elapsed();
    assert_eq!((list.rules, list.unsupported), (50_000, 0));
    let requests = requests(20_000);
    let mut times = Vec::with_capacity(requests.len());
    let mut matched = [0; 5];
    let lists = [list.clone()];
    for (i, (url, context)) in requests.iter().enumerate() {
        let started = Instant::now();
        let request = Request::new(url, Script, context).unwrap();
        let blocked = blocks(&lists, &request);
        times.push(started.elapsed());
        matched[i % 5] += usize::from(blocked);
    }
    times.sort();
    let (median, p99) = (times[times.len() / 2], times[times.len() * 99 / 100]);
    let mb = list.heap_bytes() as f64 / 1_048_576.0;
    // Kept in the test log: these are debug-build times.
    println!("benchmark: 50,000 rules compiled in {compiled:?}, {mb:.1} MB; {} requests, blocked by kind {matched:?}, median {median:?}, p99 {p99:?}", requests.len());
    let each = requests.len() / 5;
    assert_eq!(matched, [each, each, each, 0, 0], "hosts, paths and text rules block; unrelated addresses and exceptions don't");
    Measured { compiled, mb, median, p99 }
}
