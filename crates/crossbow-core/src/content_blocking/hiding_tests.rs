//! Element hiding: which rules are read, the selector check, and the style
//! sheet each document gets. Every list is a fixture.

use super::*;
use crate::content_blocking::filters::{parse, selector};
use crate::content_blocking::matcher::compile;

const PAGE: &str = "https://www.news.example/story";

fn hiding(text: &str) -> Hiding {
    Hiding::new(&[compile(text)])
}

/// A part of a sheet, one selector per rule.
fn selectors(css: &str) -> Vec<&str> {
    css.split_terminator(RULE_END).collect()
}

fn both(sheet: &Sheet) -> String {
    format!("{}{}", sheet.specific, sheet.generic)
}

#[test]
fn element_hiding_rules_are_read_and_the_rest_counted_and_skipped() {
    let supported = [
        "##.ad",
        "###top-ad",
        "##div[data-ad=\"1\"]",
        "news.example##.sponsor",
        "news.example,~live.news.example##.promo",
        "~news.example##.everywhere-else",
        "#@#.ad",
        "news.example#@#.kept",
        "##a[href^=\"https://ads.example/\"]",
        "##.x:has(> .y)",
        "##.a, .b",
    ];
    let unsupported = [
        "news.example#?#.ad:has-text(Sponsored)",
        "news.example#@?#.ad:has-text(Sponsored)",
        "##+js(nobab)",
        "##^script:has-text(ad)",
        "news.*##.ad",
        "##.ad:style(display: block)",
        "##.ad:remove()",
        "##.ad:upward(2)",
        "news.example#$#.ad { display: none }",
        "news.example#%#window.x = 1",
        "news.example$$script[tag-content=\"ad\"]",
        "bad domain##.ad",
        ",news.example##.ad",
        "news.example,##.ad",
        "##",
        "news.example##",
        "##.a{color:red}",
        "######",
        "## Ads ##",
    ];
    let parsed = parse(&[supported.as_slice(), unsupported.as_slice()].concat().join("\n"));
    assert_eq!((parsed.rules(), parsed.unsupported), (supported.len(), unsupported.len()));
}

#[test]
fn hostile_selectors_are_refused_and_ordinary_ones_kept_as_written() {
    let long = format!(".{}", "a".repeat(MAX_SELECTOR_BYTES));
    let hostile = [
        ".a{background:url(https://evil.example/x)}",
        ".a}body{display:none",
        ".a;color:red",
        "@import url(https://evil.example/x.css)",
        ".a@media",
        ".a/*",
        ".a<script>",
        "[title=\"x\"]{}",
        "[title=\"unterminated]",
        "[title='unterminated]",
        ".a[",
        ".a]",
        ".a(",
        ".a)",
        ":not(.a",
        // Escapes that would spell a brace, a semicolon, an at-sign, a
        // less-than, a quote or a backslash, in or out of a string.
        ".\\7b x",
        ".\\7d x",
        ".\\{x",
        ".\\}x",
        ".\\3b x",
        ".\\40 x",
        ".\\3c x",
        "[title=\"a\\22 b\"]",
        "[title=\"a\\\"b\"]",
        "[title='a\\27 b']",
        "[title=\"a\\5c b\"]",
        ".\\0 x",
        ".\\110000 x",
        ".a\\",
        "[title=\"a\\",
        ".a\u{0}",
        ".a\tb",
        ".a\u{7f}",
        ".a\u{1b}[0m",
        ".a:has-text(x)",
        ".a:-abp-contains(x)",
        ".a:-abp-has(.b)",
        ".a:matches-css(color: red)",
        ".a:xpath(//div)",
        ".a:min-text-length(5)",
        "html > body:style(overflow: auto !important)",
        ".a:remove()",
        ".a:watch-attr(class)",
        "+js(nobab)",
        "^script",
        "",
        "   ",
        long.as_str(),
    ];
    for text in hostile {
        assert_eq!(selector(text), None, "{text:?}");
    }
    let fine = [
        ".ad",
        "#top-ad",
        "div[data-ad=\"1\"]",
        "a[href^='https://ads.example/']",
        "[title=\"{a; b} @ <c> /* d\"]",
        ".x:has(> .y)",
        ".a, .b",
        ".\\31 23",
        ".\\-ad",
        ".a:not(.b):is(.c, .d)",
        "div:nth-child(2n+1)",
        "img[src*=\"/ads/\"]",
        "div > span + p ~ a",
        "[data-x=\"é\"]",
    ];
    for text in fine {
        assert_eq!(selector(text).as_deref(), Some(text), "{text:?}");
        assert_eq!(selector(&format!("  {text}  ")).as_deref(), Some(text), "trimmed: {text:?}");
    }
}

#[test]
fn a_document_gets_the_generic_rules_and_only_its_own_host_s() {
    let h = hiding(concat!(
        "##.ad\n",
        "news.example##.sponsor\n",
        "shop.example##.deal\n",
        "~news.example##.elsewhere\n",
        "news.example,~live.news.example##.promo\n",
        "##.unhidden\n",
        "#@#.unhidden\n",
        "shop.example#@#.ad\n",
        "shop.example##.unhidden\n",
    ));
    let news = h.sheet(PAGE).unwrap();
    assert_eq!(selectors(&news.specific), [".sponsor", ".promo"]);
    assert_eq!(selectors(&news.generic), [".ad"]);
    let live = h.sheet("https://live.news.example/").unwrap();
    assert_eq!(selectors(&live.specific), [".sponsor"], "a subdomain gets its site's rules, but not one that excludes it");
    let shop = h.sheet("http://shop.example:8080/cart?x=1#y").unwrap();
    assert_eq!(selectors(&shop.specific), [".deal", ".elsewhere"]);
    assert!(selectors(&shop.generic).is_empty(), "the site unhides .ad");
    let other = h.sheet("https://other.example/").unwrap();
    assert_eq!((selectors(&other.specific), selectors(&other.generic)), (vec![".elsewhere"], vec![".ad"]));
    // Fingerprinting: what a page can read names no other host's rules.
    for (sheet, foreign) in [(&news, ["deal", "elsewhere"]), (&shop, ["sponsor", "promo"]), (&other, ["sponsor", "deal"])] {
        for name in foreign { assert!(!both(sheet).contains(name), "{name} reached a page it isn't for"); }
        assert!(!both(sheet).contains("unhidden"), "a generic exception unhides everywhere");
    }
    for page in ["file:///fixture/ad.html", "about:blank", "data:text/html,x", "chrome://settings", "not a page", ""] {
        assert!(h.sheet(page).is_none(), "{page:?}");
    }
    assert!(hiding("||ads.example^\n").sheet(PAGE).is_none(), "nothing to hide, no sheet");
    assert!(hiding("shop.example##.deal\n").sheet(PAGE).is_none(), "nothing for this host");
}

#[test]
fn each_rule_hides_with_display_none_and_nothing_else() {
    let h = hiding("##.ad\n##.a, .b\nnews.example##[title=\"{x}\"]\n");
    let sheet = h.sheet(PAGE).unwrap();
    assert_eq!(sheet.specific, "[title=\"{x}\"]{display:none!important}\n");
    assert_eq!(&*sheet.generic, ".ad{display:none!important}\n.a, .b{display:none!important}\n");
}

#[test]
fn the_generic_part_is_shared_until_a_host_unhides_part_of_it() {
    let text = "##.ad\n##.banner\nshop.example#@#.ad\n";
    let h = hiding(text);
    let (a, b, shop) = (h.sheet("https://a.example/").unwrap(), h.sheet("https://b.example/").unwrap(), h.sheet("https://shop.example/").unwrap());
    assert!(Arc::ptr_eq(&a.generic, &b.generic) && a.generation == b.generation && a.generation != 0, "one copy, named by its generation");
    assert_eq!((selectors(&shop.generic), shop.generation), (vec![".banner"], 0), "built for that host alone, never cached");
    assert_ne!(hiding(text).sheet("https://a.example/").unwrap().generation, a.generation, "each set of lists has its own generation");
    assert!(hiding("##.ad\n##.ad\n").sheet(PAGE).is_some_and(|sheet| selectors(&sheet.generic) == [".ad"]), "a selector once");
}

#[test]
fn a_sheet_holds_at_most_50_000_selectors_and_2_mb_and_the_rest_are_counted() {
    assert_eq!((MAX_SELECTORS, MAX_BYTES), (50_000, 2 * 1024 * 1024));
    let many: String = (0..50_010).map(|i| format!("##.g{i}\n")).collect();
    let h = hiding(&many);
    assert_eq!(h.skipped, 10);
    let sheet = h.sheet(PAGE).unwrap();
    assert_eq!(selectors(&sheet.generic).len(), 50_000);
    assert_eq!(selectors(&sheet.generic).last(), Some(&".g49999"), "the first rules in list order are kept");

    let long: String = (0..2_500).map(|i| format!("##.{}{i}\n", "x".repeat(1_000))).collect();
    let h = hiding(&long);
    let sheet = h.sheet(PAGE).unwrap();
    assert!(sheet.generic.len() <= MAX_BYTES && sheet.generic.len() > MAX_BYTES - 1_100, "{}", sheet.generic.len());
    assert_eq!(selectors(&sheet.generic).len() + h.skipped, 2_500);

    // A host's own rules come first; the generic part makes room.
    let h = hiding(&format!("{}news.example##.mine\n", &many));
    let news = h.sheet(PAGE).unwrap();
    assert_eq!(selectors(&news.specific), [".mine"]);
    assert_eq!((selectors(&news.generic).len(), news.generation), (49_999, 0));
    let other = h.sheet("https://other.example/").unwrap();
    assert_eq!((selectors(&other.generic).len(), other.generation != 0), (50_000, true));
}

/// `@@…$generichide`, as EasyList uses on sites that rules for every page
/// break: those pages get only their own host's rules. It allows no request.
#[test]
fn a_generichide_exception_leaves_a_page_its_own_rules_only() {
    let h = hiding("##.ad\nnews.example##.sponsor\nforum.example##.promo\n@@||news.example^$generichide\n@@://10.0.0.$generichide\n@@$generichide,domain=forum.example\n||ads.example^\n");
    let news = h.sheet(PAGE).unwrap();
    assert_eq!((selectors(&news.specific), selectors(&news.generic), news.generation), (vec![".sponsor"], vec![], 0));
    let forum = h.sheet("https://www.forum.example/thread").unwrap();
    assert_eq!((selectors(&forum.specific), selectors(&forum.generic)), (vec![".promo"], vec![]), "by the page's own site");
    assert!(h.sheet("http://10.0.0.1/admin").is_none(), "a router page gets nothing");
    let other = h.sheet("https://other.example/").unwrap();
    assert_eq!(selectors(&other.generic), [".ad"]);
    assert!(other.generation != 0, "everyone else still shares the generic part");
    let lists = [compile("||ads.example^\n||news.example/ad.js\n@@||news.example^$generichide\n")];
    for url in ["https://ads.example/x.js", "https://news.example/ad.js"] {
        let request = crate::content_blocking::matcher::Request::new(url, crate::content_blocking::filters::RequestType::Script, PAGE).unwrap();
        assert!(crate::content_blocking::matcher::blocks(&lists, &request), "{url}: generichide isn't an allow rule");
    }
    let parsed = parse("||x.example^$generichide\n@@||x.example^$generichide,script\n@@||x.example^$~generichide\n@@$generichide\n");
    assert_eq!((parsed.rules(), parsed.unsupported), (0, 4), "only an @@ rule, narrowed by its address or sites, turns generic hiding off");
}
