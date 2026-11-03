use super::*;
use std::net::IpAddr;
use std::time::{Duration, Instant};

const PUBLIC_PAGE: &str = "https://news.example.test/today";
const PAGE: [&str; 1] = [PUBLIC_PAGE];
const FIXTURE: [&str; 1] = ["http://127.0.0.1:8123/index.html"];

/// Fetched once `host` is looked up and every address it has is public.
fn named(url: &str, host: &str) -> Result<Checked, &'static str> {
    Ok(Checked { url: url.into(), lookup: Some(host.into()) })
}

/// Fetched without a lookup.
fn direct(url: &str) -> Result<Checked, &'static str> {
    Ok(Checked { url: url.into(), lookup: None })
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn web_links_are_fetched_without_a_user_name_or_password() {
    assert_eq!(destination("https://example.test/a?b=1#c", &PAGE), named("https://example.test/a?b=1#c", "example.test"));
    assert_eq!(destination("http://example.test", &PAGE), named("http://example.test/", "example.test"));
    assert_eq!(destination("https://fixture-user:fixture-secret@example.test/x", &PAGE), named("https://example.test/x", "example.test"));
    assert_eq!(destination("http://8.8.8.8/x", &PAGE), direct("http://8.8.8.8/x"), "a public address literal needs no lookup");
    for other in ["file:///fixture/notes.html", "data:text/html,<p>x</p>", "javascript:alert(1)", "ftp://example.test/", "mailto:a@example.test", "about:blank", "not a link", ""] {
        assert_eq!(destination(other, &PAGE), Err(NOT_WEB), "{other:?}");
    }
}

#[test]
fn local_network_destinations_are_refused() {
    for link in [
        "http://127.0.0.1/", "http://127.9.9.9:8080/", "http://0.0.0.0/", "http://0.1.2.3/",
        "http://10.0.0.1/", "http://172.16.0.1/", "http://172.31.255.255/", "http://192.168.1.1/",
        "http://169.254.169.254/latest/meta-data", "http://100.64.0.1/", "http://100.127.255.255/", "http://255.255.255.255/",
        // Other spellings of loopback that browsers accept.
        "http://2130706433/", "http://0x7f.1/", "http://127.1/",
        "http://[::1]/", "http://[::]/", "http://[fc00::1]/", "http://[fd12:3456::1]/", "http://[fe80::1]/", "http://[fec0::1]/",
        "http://[::ffff:127.0.0.1]/", "http://[::ffff:192.168.0.1]/", "http://[::127.0.0.1]/", "http://[64:ff9b::a00:1]/",
        "http://localhost/", "http://LOCALHOST./", "http://app.localhost/", "http://printer.local/", "http://wiki.corp.internal/", "http://intranet/", "http://router./",
        // Documentation, benchmark, multicast and reserved ranges aren't public either.
        "http://[2001:db8::1]/", "http://192.0.2.1/", "http://198.51.100.7/", "http://203.0.113.9/", "http://198.18.0.1/", "http://224.0.0.251/", "http://240.0.0.1/",
        "http://[ff02::fb]/", "http://[2002:7f00:1::1]/",
    ] {
        assert_eq!(destination(link, &PAGE), Err(LOCAL), "{link}");
    }
    for public in ["http://8.8.8.8/", "http://100.63.255.255/", "http://100.128.0.1/", "http://172.32.0.1/", "http://[2606:4700::1111]/", "http://[::ffff:8.8.8.8]/", "https://localhost.example.test/", "https://internal.example.test/", "https://lan.example.test/", "https://example.test./"] {
        assert!(destination(public, &PAGE).is_ok(), "{public}");
    }
}

/// From the review (ai-review1): every spelling of a local address literal.
#[test]
fn every_spelling_of_a_local_address_literal_is_refused() {
    for link in [
        "http://2130706433/", "http://017700000001/", "http://0x7f000001/", "http://0x7F.0.0.1/", "http://127.1/", "http://127.0.1/",
        "http://0177.0.0.1/", "http://127.0.0.1./", "http://%31%32%37.0.0.1/", "http://127%2E0%2E0%2E1/",
        "http://\u{ff11}\u{ff12}\u{ff17}.\u{ff10}.\u{ff10}.\u{ff11}/", "http://\u{2460}\u{2461}\u{2466}.0.0.1/",
        "http://a@127.0.0.1/", "http://a:b@127.0.0.1:8080/", "http://example.test@127.0.0.1/", "http://0/", "http://0x/", "http://0.0.0.0/",
        "http://[::]/", "http://[0:0:0:0:0:0:0:1]/", "http://[::ffff:7f00:1]/", "http://[0:0:0:0:0:ffff:127.0.0.1]/", "http://[::ffff:10.0.0.1]/",
        "http://[64:ff9b::7f00:1]/", "http://[::0.0.0.2]/", "http://LOCALHOST/", "http://localhost../",
        "http://10.1/", "http://192.168.1/", "http://3232235777/", "http://169.254.169.254./", "http://100.64.1/",
        "HTTP://127.0.0.1/", "http://127.0.0.1:65535/", "http://[FE80::1]/", "http://[fd00::]/",
    ] {
        assert_eq!(destination(link, &PAGE), Err(LOCAL), "{link}");
    }
}

/// From the review (ai-review1): names that home routers and local
/// resolvers answer are refused without a lookup, and so are names with an
/// empty label, which resolvers read as the address before the dots.
#[test]
fn router_local_names_and_empty_labels_are_refused_without_a_lookup() {
    for link in [
        "http://router.lan/", "http://nas.home.arpa/", "http://home.arpa/", "http://printer.home/", "http://localhost.localdomain/",
        "http://wiki.corp/", "http://portal.intranet/", "http://files.private/", "http://NAS.LAN./",
        "http://127.0.0.1../", "http://10.0.0.1../", "http://localhost.../", "http://a..example.test/", "http://example.test../",
    ] {
        assert_eq!(destination(link, &PAGE), Err(LOCAL), "{link}");
    }
}

/// From the review (ai-review1): public wildcard names that always answer
/// with a local address pass the name rules, so they're looked up, and the
/// answer refuses them.
#[test]
fn a_name_is_looked_up_and_fetched_only_when_every_address_is_public() {
    for (link, host) in [
        ("http://localtest.me/", "localtest.me"), ("http://127.0.0.1.nip.io:11434/", "127.0.0.1.nip.io"),
        ("http://127-0-0-1.sslip.io/", "127-0-0-1.sslip.io"), ("http://192.168.1.1.nip.io/admin", "192.168.1.1.nip.io"),
    ] {
        assert_eq!(destination(link, &PAGE).map(|checked| checked.lookup), Ok(Some(host.to_owned())), "{link}");
    }
    assert_eq!(addresses(&[ip("93.184.215.14")]), Ok(()));
    assert_eq!(addresses(&[ip("93.184.215.14"), ip("2606:2800:21f:cb07:6820:80da:af6b:8b2c")]), Ok(()));
    assert_eq!(addresses(&[ip("127.0.0.1")]), Err(LOCAL));
    assert_eq!(addresses(&[ip("93.184.215.14"), ip("10.0.0.1")]), Err(LOCAL), "any local answer refuses the name");
    assert_eq!(addresses(&[ip("2606:4700::1111"), ip("::1")]), Err(LOCAL));
    assert_eq!(addresses(&[ip("::ffff:192.168.1.1")]), Err(LOCAL));
    assert_eq!(addresses(&[]), Err(UNRESOLVED), "a failed lookup means no preview");
}

#[test]
fn only_public_unicast_addresses_are_public() {
    for local in [
        // IPv4: this network, private, loopback, link-local, CGNAT, IETF, documentation, benchmark, 6to4 relay, multicast, reserved, broadcast.
        "0.0.0.0", "0.255.255.255", "10.0.0.1", "10.255.255.255", "127.0.0.1", "127.255.255.254", "169.254.0.1", "169.254.169.254",
        "172.16.0.1", "172.31.255.255", "192.168.0.1", "192.168.255.255", "100.64.0.0", "100.127.255.255", "192.0.0.1", "192.0.0.170",
        "192.0.2.1", "198.51.100.1", "203.0.113.1", "198.18.0.1", "198.19.255.255", "192.88.99.1",
        "224.0.0.1", "239.255.255.250", "240.0.0.1", "254.255.255.255", "255.255.255.255",
        // IPv6: unspecified, loopback, link-local, site-local, unique-local, multicast, discard, documentation, IETF assignments
        // (Teredo, benchmarking, ORCHID), local-use NAT64 and IPv4-translated.
        "::", "::1", "fe80::1", "febf::1", "fec0::1", "fc00::1", "fd12:3456::1", "ff02::1", "ff0e::1", "100::1",
        "2001:db8::1", "2001:db8:ffff::1", "3fff::1", "3fff:fff::1", "2001::1", "2001:0:4136:e378::1", "2001:2::1", "2001:10::1", "2001:20::1", "2001:1ff::1",
        "64:ff9b:1::1", "::ffff:0:808:808",
        // IPv4 inside IPv6, judged by the IPv4 address: mapped, compatible, NAT64 and 6to4.
        "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:192.0.2.1", "::ffff:0.0.0.0", "::127.0.0.1", "::10.0.0.1", "::192.168.1.1",
        "64:ff9b::7f00:1", "64:ff9b::a00:1", "64:ff9b::c000:201", "64:ff9b::a9fe:a9fe",
        "2002:7f00:1::1", "2002:c0a8:101::1", "2002:a9fe:a9fe::1", "2002:a00:1::", "2002:6440:1::1",
    ] {
        assert!(!public(ip(local)), "{local}");
    }
    for public_ip in [
        "8.8.8.8", "1.1.1.1", "93.184.215.14", "100.63.255.255", "100.128.0.1", "172.15.255.255", "172.32.0.1", "192.0.1.1", "192.169.0.1",
        "198.17.255.255", "198.20.0.1", "223.255.255.254",
        "2606:4700::1111", "2a00:1450:4001::1", "2001:200::1", "2001:4860:4860::8888", "3fff:1000::1", "2c0f:ffff::1",
        "::ffff:8.8.8.8", "64:ff9b::808:808", "2002:808:808::1",
    ] {
        assert!(public(ip(public_ip)), "{public_ip}");
    }
}

#[test]
fn a_local_page_may_preview_links_to_its_own_origin_only() {
    assert_eq!(destination("http://127.0.0.1:8123/article.html", &FIXTURE), direct("http://127.0.0.1:8123/article.html"));
    assert_eq!(destination("/article.html", &FIXTURE), Err(NOT_WEB), "links arrive absolute");
    for elsewhere in ["http://localhost:8123/article.html", "http://192.168.1.1/", "http://[::1]:8123/"] {
        assert_eq!(destination(elsewhere, &FIXTURE), Err(LOCAL), "another host: {elsewhere}");
    }
    for other_port in ["http://127.0.0.1:22/", "http://127.0.0.1/", "http://127.0.0.1:8124/article.html"] {
        assert_eq!(destination(other_port, &FIXTURE), Err(LOCAL), "another port: {other_port}");
    }
    assert_eq!(destination("https://127.0.0.1:8123/article.html", &FIXTURE), Err(LOCAL), "another scheme");
    // From the review (ai-review1): the same host on another port or scheme.
    for (link, page) in [
        ("http://localhost:22/", "http://localhost:3000/app"),
        ("http://localhost:631/admin", "http://localhost:3000/app"),
        ("http://127.0.0.1:11434/api/tags", "http://127.0.0.1:5173/"),
        ("https://192.168.1.1:8443/", "http://192.168.1.1/"),
        ("https://192.168.1.1/", "http://192.168.1.1/"),
        ("http://127.0.0.1:22/", "http://localhost:3000/"),
        ("http://192.168.1.2/", "http://192.168.1.1/"),
    ] {
        assert_eq!(destination(link, &[page]), Err(LOCAL), "{link} from {page}");
    }
    assert_eq!(destination("http://192.168.1.1/b", &["http://192.168.1.1/a"]), direct("http://192.168.1.1/b"));
    assert_eq!(destination("http://192.168.1.1:80/b", &["http://192.168.1.1/a"]), direct("http://192.168.1.1/b"), "the default port is that port");
    assert_eq!(destination("http://router.lan/settings", &["http://router.lan/"]), direct("http://router.lan/settings"), "a router-local page's own link");
    assert_eq!(destination("http://127.0.0.1/", &["file:///fixture/index.html"]), Err(LOCAL), "a file page is not that origin");
    assert_eq!(destination("http://localtest.me:8080/x", &["http://localtest.me:8080/"]), named("http://localtest.me:8080/x", "localtest.me"), "a public-looking name is still looked up");
}

/// Hovering doesn't say which frame holds the link, so every document in the
/// page must share the link's origin for the local exception.
#[test]
fn the_local_exception_needs_every_document_in_the_page_to_share_the_links_origin() {
    let link = "http://127.0.0.1:8123/admin";
    assert_eq!(destination(link, &["http://127.0.0.1:8123/index.html", "http://127.0.0.1:8123/frame.html"]), direct(link), "frames on the same origin");
    assert_eq!(destination(link, &[PUBLIC_PAGE, "http://127.0.0.1:8123/frame.html"]), Err(LOCAL), "a local frame inside a public page");
    assert_eq!(destination(link, &["http://127.0.0.1:8123/index.html", "https://ads.example.test/frame"]), Err(LOCAL), "a public frame inside a local page");
    assert_eq!(destination(link, &["http://127.0.0.1:8123/index.html", "http://127.0.0.1:8124/frame"]), Err(LOCAL), "a frame on another port");
    assert_eq!(destination(link, &["http://127.0.0.1:8123/index.html", "data:text/html,<a>x</a>"]), Err(LOCAL), "a frame with an opaque origin");
    assert_eq!(destination(link, &[] as &[&str]), Err(LOCAL), "no document known");
    assert_eq!(destination("https://example.test/", &[PUBLIC_PAGE, "http://127.0.0.1:8123/"]), named("https://example.test/", "example.test"), "public links are unaffected");
    assert_eq!(destination("https://news.example.test/next", &PAGE), named("https://news.example.test/next", "news.example.test"), "a public page's own link is still looked up");
}

#[test]
fn redirects_follow_the_same_rules_and_never_downgrade() {
    assert_eq!(next_hop("https://example.test/a/b", "/c?d=1", &PAGE), named("https://example.test/c?d=1", "example.test"));
    assert_eq!(next_hop("https://example.test/a/b", "c", &PAGE), named("https://example.test/a/c", "example.test"));
    assert_eq!(next_hop("http://example.test/", "https://www.example.test/", &PAGE), named("https://www.example.test/", "www.example.test"), "an upgrade is fine");
    assert_eq!(next_hop("https://example.test/", "http://example.test/", &PAGE), Err(DOWNGRADE));
    assert_eq!(next_hop("https://example.test/", "//example.test/x", &PAGE), named("https://example.test/x", "example.test"));
    assert_eq!(next_hop("https://example.test/", "https://8.8.8.8/x", &PAGE), direct("https://8.8.8.8/x"));
    for local in ["https://127.0.0.1/", "https://192.168.0.10/admin", "https://[::1]/", "https://localhost/", "https://router.lan/", "https://127.0.0.1../", "https://[2001:db8::1]/"] {
        assert_eq!(next_hop("https://example.test/", local, &PAGE), Err(LOCAL), "{local}");
    }
    for bad in ["javascript:alert(1)", "data:text/html,x", "file:///etc/hosts", "ftp://example.test/"] {
        assert_eq!(next_hop("https://example.test/", bad, &PAGE), Err(BAD_REDIRECT), "{bad}");
    }
    assert_eq!(next_hop("https://example.test/", "https://fixture-user:fixture-secret@example.test/in", &PAGE), named("https://example.test/in", "example.test"));
    // On the fixture server, its own origin stays allowed, and only that.
    assert_eq!(next_hop("http://127.0.0.1:8123/hop1", "/hop2", &FIXTURE), direct("http://127.0.0.1:8123/hop2"));
    assert_eq!(next_hop("http://127.0.0.1:8123/hop1", "http://192.168.1.1/", &FIXTURE), Err(LOCAL));
    assert_eq!(next_hop("http://127.0.0.1:8123/hop1", "http://127.0.0.1:22/", &FIXTURE), Err(LOCAL));
    assert_eq!(next_hop("http://localhost:3000/a", "http://localhost:22/", &["http://localhost:3000/app"]), Err(LOCAL));
}

/// From the review (ai-review1): redirect spellings of local addresses.
#[test]
fn redirect_spellings_of_local_addresses_are_refused_and_names_are_looked_up() {
    for (current, location, want) in [
        ("https://pub.example.test/a", "//127.0.0.1/x", Err(LOCAL)),
        ("https://pub.example.test/a", "https://[::ffff:7f00:1]/", Err(LOCAL)),
        ("https://pub.example.test/a", "https://0x7f.1/", Err(LOCAL)),
        ("https://pub.example.test/a", "https://nas.home.arpa/", Err(LOCAL)),
        ("https://pub.example.test/a", "data:text/html,<p>x</p>", Err(BAD_REDIRECT)),
        ("https://pub.example.test/a", "javascript:alert(1)", Err(BAD_REDIRECT)),
        ("https://pub.example.test/a", "http://pub.example.test/b", Err(DOWNGRADE)),
        ("https://pub.example.test/a", "/relative", named("https://pub.example.test/relative", "pub.example.test")),
        ("https://pub.example.test/a", "https://u:p@other.example.test/", named("https://other.example.test/", "other.example.test")),
        ("http://pub.example.test/a", "https://localtest.me/", named("https://localtest.me/", "localtest.me")),
    ] {
        assert_eq!(next_hop(current, location, &PAGE), want, "{location}");
    }
}

#[test]
fn page_text_skips_code_and_markup_and_decodes_entities() {
    let html = r#"<!DOCTYPE html><html><head>
        <meta charset="utf-8"><title> Fixture &amp; Friends </title>
        <meta name="description" content="A page about &quot;fixtures&quot;.">
        <style>body { color: red }</style><script>var secret = "<p>not text</p>";</script>
        </head><body><!-- a comment <p>hidden</p> -->
        <h1>Heading</h1><p>First&nbsp;paragraph &lt;with&gt; &#8220;quotes&#8221; &#x1F600; &unknown; a &lt b</p>
        <noscript>Enable scripts</noscript><template><p>Template</p></template><svg><text>Chart</text></svg>
        <ul><li>One</li><li>Two</li></ul><p>Café<br>naïve	text</p>
        <textarea>typed</textarea><a href="/x" title="a > b">Link</a>
        </body></html>"#;
    let page = page_text(html.as_bytes(), "", 6_000).unwrap();
    assert_eq!(page.title, "Fixture & Friends");
    assert_eq!(page.description, "A page about \"fixtures\".");
    assert_eq!(page.text, "Heading First paragraph <with> “quotes” 😀 &unknown; a &lt b One Two Café naïve text Link");
}

#[test]
fn a_reference_is_read_only_when_its_semicolon_is_within_ten_bytes() {
    let page = page_text(b"<p>&amp; &#38; &#x26; &#0000038; &#00000038; &#000000038; &hellip; &amp &;x &#;</p>", "", 6_000).unwrap();
    assert_eq!(page.text, "& & & & & &#000000038; … &amp &;x &#;");
}

#[test]
fn control_and_invisible_characters_never_reach_the_text() {
    let page = page_text("<title>A\u{202E}B</title><p>x&#7;y\u{200B}z&#0;w</p>".as_bytes(), "utf-8", 6_000).unwrap();
    assert_eq!(page.title, "AB");
    assert_eq!(page.text, "xyz&#0;w");
}

#[test]
fn the_text_stops_at_its_byte_limit_on_a_character_boundary() {
    let body = format!("<p>{}</p>", "é".repeat(5_000));
    let page = page_text(body.as_bytes(), "utf-8", 6_000).unwrap();
    assert_eq!(page.text.len(), 6_000);
    let page = page_text(body.as_bytes(), "utf-8", 5_999).unwrap();
    assert_eq!(page.text.len(), 5_998, "never half a character");
    let spaced = format!("<p>{}</p>", "word        \n\n        ".repeat(2_000));
    assert!(page_text(spaced.as_bytes(), "", 6_000).unwrap().text.len() >= 5_990, "collapsed whitespace doesn't use up the limit");
    let long = format!("<title>{}</title><meta name=description content='{}'>", "t".repeat(500), "d".repeat(500));
    let page = page_text(long.as_bytes(), "", 6_000).unwrap();
    assert_eq!((page.title.chars().count(), page.description.chars().count()), (TITLE_CHARS, DESCRIPTION_CHARS));
}

/// From the review (ai-review1): a body of 1 MiB, the most a preview reads,
/// made of whatever costs the reader most. Each must be read in linear time:
/// the bound leaves a debug build on a busy machine ten times its usual time.
#[test]
fn a_body_at_the_size_limit_is_read_in_bounded_time_whatever_it_holds() {
    const CAP: usize = 1 << 20;
    let filled = |prefix: &str, unit: &str, suffix: &str| {
        let mut body = String::with_capacity(CAP + unit.len());
        body.push_str(prefix);
        while body.len() + unit.len() + suffix.len() <= CAP { body.push_str(unit); }
        body.push_str(suffix);
        body.into_bytes()
    };
    let mut slow = Vec::new();
    for (label, body) in [
        ("ampersands", filled("", "&", "")),
        ("&#", filled("", "&#", "")),
        ("&#x", filled("", "&#x", "")),
        ("&# and digits", filled("", "&#1234", "")),
        ("a title of ampersands", filled("<title>", "&", "")),
        ("a description of ampersands", filled("<meta name=description content='", "&", "'>")),
        ("text of ampersands after markup", filled("<p>", "&", "</p>")),
        ("nested inline tags", filled("", "<b>", "")),
        ("nested blocks", filled("", "<div>", "")),
        ("only <", filled("", "<", "")),
        ("unclosed tags", filled("", "<a ", "")),
        ("a quote never closed", filled("<a title=\"", "<", "")),
        ("comment openers", filled("", "<!--", "")),
        ("script openers", filled("", "<script>", "")),
        ("title openers", filled("", "<title>", "")),
        ("closing tags", filled("", "</p>", "")),
        ("meta tags", filled("", "<meta name=x content=y>", "")),
    ] {
        let started = Instant::now();
        let page = page_text(&body, "utf-8", 6_000).unwrap();
        let took = started.elapsed();
        eprintln!("{label}: {} bytes read in {} ms", body.len(), took.as_millis());
        if took >= Duration::from_secs(3) { slow.push((label, took)); }
        assert!(page.text.len() <= 6_000 && page.title.chars().count() <= TITLE_CHARS && page.description.chars().count() <= DESCRIPTION_CHARS, "{label}");
    }
    assert!(slow.is_empty(), "over 3 s: {slow:?}");
}

#[test]
fn utf8_latin1_and_windows_1252_are_read_and_other_encodings_are_refused() {
    let latin1 = b"<p>Caf\xE9 \x93quoted\x94 \x80 5</p>";
    for label in ["iso-8859-1", "ISO-8859-1", "latin1", "windows-1252", "\"windows-1252\"", "us-ascii"] {
        assert_eq!(page_text(latin1, label, 6_000).unwrap().text, "Café “quoted” € 5", "{label}");
    }
    let meta = b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1252\"><p>Caf\xE9</p>";
    assert_eq!(page_text(meta, "", 6_000).unwrap().text, "Café", "the page's own meta tag");
    assert_eq!(page_text("<p>Café</p>".as_bytes(), "", 6_000).unwrap().text, "Café", "UTF-8 by default");
    assert_eq!(page_text(b"\xEF\xBB\xBF<p>Caf\xC3\xA9</p>", "iso-8859-1", 6_000).unwrap().text, "Café", "a byte order mark wins");
    assert_eq!(page_text(b"<p>Caf\xC3</p>", "utf-8", 6_000).unwrap().text, "Caf\u{FFFD}", "bad bytes are replaced");
    for label in ["shift_jis", "utf-16", "koi8-r"] {
        assert_eq!(page_text(b"<p>x</p>", label, 6_000), Err(CHARSET), "{label}");
    }
    assert_eq!(page_text(b"<meta charset=gb2312><p>x</p>", "", 6_000), Err(CHARSET));
    assert_eq!(page_text(b"\xFF\xFE<\x00p\x00>\x00", "", 6_000), Err(CHARSET));
}

#[test]
fn broken_markup_still_gives_text() {
    let page = page_text(b"<p>open <b>bold <i unclosed>end <!-- never closed", "", 6_000).unwrap();
    assert_eq!(page.text, "open bold end");
    let page = page_text(b"<script>never closed <p>x</p>", "", 6_000).unwrap();
    assert_eq!(page.text, "");
    let page = page_text(b"<a title=\"x > y\" href='z'>link</a> 3 < 4 > 2", "", 6_000).unwrap();
    assert_eq!(page.text, "link 3 < 4 > 2");
}

/// The system resolver answers on its own thread. An address literal is
/// answered without asking DNS, so this does no network lookup.
#[test]
fn the_system_resolver_answers_on_its_own_thread() {
    let lookup = SystemResolver.lookup("127.0.0.1");
    let started = Instant::now();
    let found = loop {
        if let Some(found) = lookup.poll() { break found; }
        assert!(started.elapsed() < Duration::from_secs(5), "no answer");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(found, [ip("127.0.0.1")]);
}
