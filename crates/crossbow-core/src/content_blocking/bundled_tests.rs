//! The lists that come with Crossbow: the files are the ones `SOURCES`
//! names, unchanged; each is compiled once and shared; how much of each
//! applies; and their memory and time. Nothing here fetches.

use super::*;
use crate::content_blocking::filters::{self, RequestType, coverage};
use crate::content_blocking::hiding::Hiding;
use crate::content_blocking::matcher::{Request, blocks, sha256_hex};
use std::time::{Duration, Instant};

/// `SOURCES`' line for `file`, as `key=value` pairs.
fn source(file: &str) -> Vec<(String, String)> {
    let line = SOURCES.lines().find(|line| line.split_whitespace().next() == Some(file)).unwrap_or_else(|| panic!("SOURCES names {file}"));
    line.split_whitespace().skip(1).map(|pair| {
        let (key, value) = pair.split_once('=').expect("key=value");
        (key.to_owned(), value.to_owned())
    }).collect()
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str()).unwrap_or_else(|| panic!("no {key}"))
}

#[test]
fn the_bundled_files_are_the_ones_sources_names_unchanged_under_cc_by_sa() {
    assert_eq!(LISTS.map(|list| (list.id, list.name, list.kind)), [("easylist", "EasyList", ListKind::Ads), ("easyprivacy", "EasyPrivacy", ListKind::Ads)]);
    for list in LISTS {
        let pairs = source(&format!("{}.txt", list.id));
        assert_eq!(field(&pairs, "sha256"), sha256_hex(list.text.as_bytes()), "{} is the file SOURCES names, byte for byte", list.name);
        assert_eq!(field(&pairs, "bytes"), list.text.len().to_string());
        assert_eq!(field(&pairs, "source"), format!("https://easylist.to/easylist/{}.txt", list.id));
        let version = list.text.lines().find_map(|line| line.strip_prefix("! Version: ")).expect("a version line");
        assert_eq!(field(&pairs, "version"), version);
        assert!(list.text.lines().any(|line| line == format!("! Title: {}", list.name)), "{}'s own header is kept", list.name);
    }
    assert_eq!(field(&source("easylist.txt"), "sha256"), "35f280ee4c2cdef9394808deb169abde32f96b82f6628def39c35525ffbbe84f");
    assert_eq!(field(&source("easyprivacy.txt"), "sha256"), "a9f7c152539e448bf067730fa0c91b931e08430841c6ccf455a2d58e23f01d72");
    // The licence chosen, and the attribution Settings shows, in the note beside the files.
    assert!(SOURCES.contains("CC BY-SA 3.0 Unported") && LICENSE.contains("CC BY-SA 3.0 Unported"), "the licence chosen is named");
    assert!(LICENSE.contains("https://creativecommons.org/licenses/by-sa/3.0/") && LICENSE.contains("https://easylist.to/pages/licence.html"));
    assert!(LICENSE.contains(CREDIT), "the note carries the same attribution as Settings");
    assert_eq!(CREDIT, "EasyList and EasyPrivacy by The EasyList authors (https://easylist.to/), used unmodified under CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/).");
}

static FIXTURE: Bundled = Bundled::new("fixture", "Fixture", ListKind::Ads, "! Fixture\n||ads.example^\n##.banner\n", "Fixture list by the Crossbow tests.");

#[test]
fn a_bundled_list_is_compiled_once_and_shared_while_anyone_uses_it() {
    let before = FIXTURE.compiles();
    // Several profiles opening it at once: one compiles, the others wait for it.
    let opened: Vec<Arc<CompiledList>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..4).map(|_| scope.spawn(|| FIXTURE.compiled())).collect();
        threads.into_iter().map(|thread| thread.join().unwrap()).collect()
    });
    assert!(opened.iter().all(|list| Arc::ptr_eq(list, &opened[0])), "one copy");
    assert_eq!(FIXTURE.compiles(), before + 1, "compiled once");
    assert_eq!((opened[0].rules, opened[0].unsupported), (2, 0));
    let again = FIXTURE.compiled();
    assert!(Arc::ptr_eq(&again, &opened[0]) && FIXTURE.compiles() == before + 1, "kept while in use");
    drop((opened, again));
    // Once no profile uses it, its memory goes; the next use compiles it again.
    let _ = FIXTURE.compiled();
    assert_eq!(FIXTURE.compiles(), before + 2);
}

/// Each list's network and cosmetic rules the matcher applies, and the
/// reasons for the rest, in the test log.
#[test]
fn most_of_each_bundled_list_applies_and_the_rest_is_counted_by_reason() {
    // Floors a little under the 2026-11-01 lists; EasyPrivacy's few cosmetic
    // rules are nearly all scriptlets.
    for (list, network_floor, cosmetic_floor) in [(&EASYLIST, 0.92, 0.96), (&EASYPRIVACY, 0.99, 0.0)] {
        let coverage = coverage(list.text);
        let share = |share: filters::Share| share.supported as f64 / share.total.max(1) as f64;
        println!(
            "coverage: {}: network {} of {} ({:.1}%), cosmetic {} of {} ({:.1}%)",
            list.name, coverage.network.supported, coverage.network.total, share(coverage.network) * 100.0,
            coverage.cosmetic.supported, coverage.cosmetic.total, share(coverage.cosmetic) * 100.0,
        );
        let mut reasons: Vec<(&String, &usize)> = coverage.reasons.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (reason, count) in reasons.iter().take(12) { println!("coverage: {}:   {count:>6}  {reason}", list.name); }
        assert!(share(coverage.network) >= network_floor, "{}: network {:.3}", list.name, share(coverage.network));
        assert!(share(coverage.cosmetic) >= cosmetic_floor, "{}: cosmetic {:.3}", list.name, share(coverage.cosmetic));
        let skipped: usize = coverage.reasons.values().sum();
        assert_eq!(skipped, coverage.network.total - coverage.network.supported + coverage.cosmetic.total - coverage.cosmetic.supported, "every skipped rule has a reason");
        let compiled = list.compiled();
        assert_eq!((compiled.rules, compiled.unsupported), (coverage.network.supported + coverage.cosmetic.supported, skipped), "the same count Settings shows");
    }
}

#[test]
fn coverage_names_why_each_rule_is_skipped() {
    let text = "! comment\n[Adblock Plus 2.0]\n||ads.example^\n||pop.example^$popup\n/ad[0-9]+/\n@@||site.example^$generichide\n@@$generichide,domain=forum.example\n||x.example^$domain=*.example\n##.ad\nexample.*##.ad\nexample.com#?#.ad:has-text(x)\n##.ad:-abp-has(.b)\nexample.com##+js(nobab)\nexample.com#$#.ad { display: none }\nexample.com#%#window.x=1\nexample.com$$script\n0.0.0.0 bad_host!\n";
    let coverage = coverage(text);
    assert_eq!((coverage.network.supported, coverage.network.total), (3, 7));
    assert_eq!((coverage.cosmetic.supported, coverage.cosmetic.total), (1, 8));
    let reasons: Vec<(&str, usize)> = coverage.reasons.iter().map(|(reason, count)| (reason.as_str(), *count)).collect();
    assert_eq!(reasons, [
        ("$domain= value that isn't a host name", 1),
        ("$popup option", 1),
        ("HTML filter ($$, ##^)", 1),
        ("hosts-file name that isn't a host name", 1),
        ("injected CSS (#$#)", 1),
        ("procedural filter (#?#)", 1),
        ("procedural pseudo-class (:-abp-has)", 1),
        ("regular expression", 1),
        ("scriptlet (+js, #%#)", 2),
        ("site pattern that isn't a host name (example.*)", 1),
    ]);
}

/// What both lists may hold on the heap, compiled and with their element
/// hiding: 11.7 MB on 2026-11-01.
const MEMORY_BUDGET_MB: f64 = 16.0;

struct Measured {
    compiled: Duration,
    mb: f64,
    median: Duration,
    p99: Duration,
}

/// Both lists compiled from scratch and their element hiding built, then
/// 20,000 requests: their memory, compile time and per-request times.
fn measure() -> Measured {
    let started = Instant::now();
    let lists = [crate::content_blocking::matcher::compile(EASYLIST.text), crate::content_blocking::matcher::compile(EASYPRIVACY.text)];
    let compiled = started.elapsed();
    let started = Instant::now();
    let hiding = Hiding::new(&lists);
    let hiding_built = started.elapsed();
    let started = Instant::now();
    let sheet = hiding.sheet("https://www.news.example/story").map_or(0, |sheet| sheet.specific.len() + sheet.generic.len());
    let sheet_built = started.elapsed();
    let known = [
        ("https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js", true),
        ("https://www.google-analytics.com/analytics.js", true),
        ("https://www.example.com/assets/app.js", false),
    ];
    for (url, blocked) in known {
        let request = Request::new(url, RequestType::Script, "https://news.example/").unwrap();
        assert_eq!(blocks(&lists, &request), blocked, "{url}");
    }
    let mut times = Vec::with_capacity(20_000);
    for i in 0..20_000 {
        let url = match i % 4 {
            0 => format!("https://cdn{}.example.com/assets/app.{i}.js?v={i}", i % 97),
            1 => format!("https://images{}.example.net/photos/{i}/large.jpg", i % 89),
            2 => format!("https://ads{}.example.org/banner/{i}/slot.js?ref=news", i % 13),
            _ => format!("https://api.example{}.com/v1/items?page={i}&utm_source=x", i % 7),
        };
        let started = Instant::now();
        let request = Request::new(&url, RequestType::Script, "https://news.example/").unwrap();
        let _ = blocks(&lists, &request);
        times.push(started.elapsed());
    }
    times.sort();
    let (median, p99) = (times[times.len() / 2], times[times.len() * 99 / 100]);
    let lists_mb = lists.iter().map(|list| list.heap_bytes()).sum::<usize>() as f64 / 1_048_576.0;
    let hiding_mb = hiding.heap_bytes() as f64 / 1_048_576.0;
    // Kept in the test log: these are debug-build times.
    println!(
        "benchmark: EasyList and EasyPrivacy ({} rules) compiled in {compiled:?}, {lists_mb:.1} MB; element hiding built in {hiding_built:?}, {hiding_mb:.1} MB, a page's {sheet}-byte sheet in {sheet_built:?}; 20,000 requests, median {median:?}, p99 {p99:?}",
        lists.iter().map(|list| list.rules).sum::<usize>(),
    );
    Measured { compiled, mb: lists_mb + hiding_mb, median, p99 }
}

#[test]
fn the_bundled_lists_fit_their_memory_and_median_budget() {
    let measured = measure();
    // The median survives a loaded machine; the compile time and p99 don't,
    // so the hand-run benchmark below checks those.
    assert!(measured.mb < MEMORY_BUDGET_MB, "{:.1} MB", measured.mb);
    assert!(measured.median.as_micros() < 50, "median {:?}", measured.median);
}

#[test]
#[ignore = "checks compile time and p99 against their budgets; run by hand on an idle machine with --nocapture"]
fn the_bundled_lists_meet_the_time_budget() {
    let Measured { compiled, mb, median, p99 } = measure();
    // A debug build; release is several times faster.
    assert!(compiled.as_millis() < 10_000, "compiled in {compiled:?}");
    assert!(mb < MEMORY_BUDGET_MB, "{mb:.1} MB");
    assert!(median.as_micros() < 50, "median {median:?}");
    assert!(p99.as_micros() < 500, "p99 {p99:?}");
}
