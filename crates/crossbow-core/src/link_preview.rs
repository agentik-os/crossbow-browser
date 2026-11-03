//! 5-Second Previews: which links may be fetched, which redirects may be
//! followed, and the readable text of a fetched page. Addresses on this
//! device or its local network are refused unless every document in the
//! linking page is on that same origin. A name is looked up first and
//! fetched only when every address it has is public. The engine looks the
//! name up again when it fetches, so a name whose answer changes in between
//! (DNS rebinding), or a proxy, can still reach a local address.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use url::{Host, Url};

/// Redirects followed for one preview, after the first request.
pub const MAX_REDIRECTS: usize = 3;
/// Characters kept from a page's title and description.
pub const TITLE_CHARS: usize = 200;
pub const DESCRIPTION_CHARS: usize = 300;

pub const LOCAL: &str = "Previews aren't available for local network addresses.";
pub const NOT_WEB: &str = "Previews are available for http and https links only.";
pub const DOWNGRADE: &str = "The link redirected from https to http, so it wasn't previewed.";
pub const TOO_MANY: &str = "The link redirected more than 3 times, so it wasn't previewed.";
pub const BAD_REDIRECT: &str = "The link redirected to an address that can't be previewed.";
pub const CHARSET: &str = "The page uses a text encoding Crossbow can't read. Nothing was sent to the model.";
pub const UNRESOLVED: &str = "The site's address couldn't be looked up, so it wasn't previewed.";

/// Names that home routers and local resolvers answer, refused with their
/// subdomains without a lookup.
const LOCAL_NAMES: [&str; 10] = ["localhost", "local", "internal", "lan", "home.arpa", "localdomain", "home", "corp", "intranet", "private"];

/// A link or redirect that passed the checks on its address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// What to fetch, without a user name or password.
    pub url: String,
    /// The name to look up first: `url` is fetched only when `addresses`
    /// accepts what it finds. None for an address literal, already checked,
    /// and for a local page's own link.
    pub lookup: Option<String>,
}

/// What to fetch for `link`, hovered on a page made of `documents` (the
/// address of every frame's document), or why it isn't fetched.
pub fn destination<S: AsRef<str>>(link: &str, documents: &[S]) -> Result<Checked, &'static str> {
    let url = Url::parse(link).map_err(|_| NOT_WEB)?;
    checked(url, documents)
}

/// Where a redirect from `current` to `location` goes, or why it isn't
/// followed: the same rules as the link, and never from https to http.
pub fn next_hop<S: AsRef<str>>(current: &str, location: &str, documents: &[S]) -> Result<Checked, &'static str> {
    let current = Url::parse(current).map_err(|_| BAD_REDIRECT)?;
    let next = current.join(location).map_err(|_| BAD_REDIRECT)?;
    if current.scheme() == "https" && next.scheme() == "http" { return Err(DOWNGRADE); }
    checked(next, documents).map_err(|reason| if reason == NOT_WEB { BAD_REDIRECT } else { reason })
}

fn checked<S: AsRef<str>>(mut url: Url, documents: &[S]) -> Result<Checked, &'static str> {
    if !matches!(url.scheme(), "http" | "https") { return Err(NOT_WEB); }
    let lookup = if local(&url) {
        // A local page may preview links to its own origin, as on a fixture
        // server. Hovering doesn't say which frame holds the link, so every
        // document in the page must be on that origin.
        let own = !documents.is_empty() && documents.iter().all(|document| Url::parse(document.as_ref()).is_ok_and(|document| document.origin() == url.origin()));
        if !own { return Err(LOCAL); }
        None
    } else {
        match url.host() {
            Some(Host::Domain(name)) => Some(name.to_owned()),
            _ => None,
        }
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(Checked { url: url.to_string(), lookup })
}

/// On this device or its local network, by its address or its name: an
/// address that isn't `public`, a single-label name, one of `LOCAL_NAMES`
/// or below one, or a name with an empty label, which resolvers read as the
/// address before the dots.
pub fn local(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => !public_v4(ip),
        Some(Host::Ipv6(ip)) => !public_v6(ip),
        Some(Host::Domain(name)) => {
            let name = name.to_ascii_lowercase();
            let name = name.strip_suffix('.').unwrap_or(&name);
            !name.contains('.') || name.split('.').any(str::is_empty)
                || LOCAL_NAMES.iter().any(|local| name == *local || name.strip_suffix(local).is_some_and(|rest| rest.ends_with('.')))
        }
        None => true,
    }
}

/// Every address a name has is public, or why it isn't fetched.
pub fn addresses(found: &[IpAddr]) -> Result<(), &'static str> {
    if found.is_empty() { return Err(UNRESOLVED); }
    if !found.iter().all(|ip| public(*ip)) { return Err(LOCAL); }
    Ok(())
}

/// A public unicast address. Refused: this network, loopback, private,
/// link-local, CGNAT, unique-local, site-local, multicast, broadcast,
/// reserved, documentation and benchmark ranges, and IETF assignments such
/// as Teredo. IPv4 inside IPv6 (mapped, compatible, NAT64 and 6to4) is
/// judged by its IPv4 address.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0 || a == 10 || a == 127 || a >= 224
        || (a == 100 && (64..128).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && matches!(c, 0 | 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && matches!(b, 18 | 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let [a, b, c, d, e, f, g, h] = ip.segments();
    let v4 = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    match [a, b, c, d, e, f] {
        // Mapped (::ffff:0:0/96), compatible (::/96, with :: and ::1) and NAT64 (64:ff9b::/96).
        [0, 0, 0, 0, 0, 0xffff] | [0, 0, 0, 0, 0, 0] | [0x64, 0xff9b, 0, 0, 0, 0] => public_v4(v4(g, h)),
        // 6to4 (2002::/16).
        [0x2002, ..] => public_v4(v4(b, c)),
        // Global unicast (2000::/3), less IETF assignments (2001::/23),
        // documentation (2001:db8::/32 and 3fff::/20).
        _ => a & 0xe000 == 0x2000 && !(a == 0x2001 && (b < 0x200 || b == 0xdb8)) && !(a == 0x3fff && b < 0x1000),
    }
}

/// A name lookup under way.
pub trait Lookup {
    /// None while waiting; then every address found, or none when the name
    /// has none or the lookup failed.
    fn poll(&self) -> Option<Vec<IpAddr>>;
}

/// Looks names up. Tests substitute a fake.
pub trait Resolver {
    fn lookup(&self, host: &str) -> Box<dyn Lookup>;
}

/// The system's resolver, each lookup on its own thread. It can't be
/// cancelled: one given up on keeps its thread until the system answers,
/// and the answer is dropped.
pub struct SystemResolver;

struct Asking(Receiver<Vec<IpAddr>>);

impl Lookup for Asking {
    fn poll(&self) -> Option<Vec<IpAddr>> {
        match self.0.try_recv() {
            Ok(found) => Some(found),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Vec::new()),
        }
    }
}

impl Resolver for SystemResolver {
    fn lookup(&self, host: &str) -> Box<dyn Lookup> {
        let (sender, receiver) = mpsc::channel();
        let host = host.to_owned();
        // If the thread can't start, the lookup reads as failed.
        let _ = std::thread::Builder::new().name("preview-lookup".into()).spawn(move || {
            let found = (host.as_str(), 0).to_socket_addrs().map(|found| found.map(|address| address.ip()).collect()).unwrap_or_default();
            let _ = sender.send(found);
        });
        Box::new(Asking(receiver))
    }
}

/// A page's title, description and visible text, each on one line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageText {
    pub title: String,
    pub description: String,
    pub text: String,
}

/// The readable text of `body`, decoded by the charset its headers name,
/// else its own meta tag, else UTF-8. Scripts, styles, templates, SVG and
/// comments are skipped. The text stops at `max_bytes`.
pub fn page_text(body: &[u8], header_charset: &str, max_bytes: usize) -> Result<PageText, &'static str> {
    let html = decode(body, header_charset)?;
    Ok(extract(&html, max_bytes))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Utf8,
    Windows1252,
}

fn charset(label: &str) -> Option<Option<Charset>> {
    let label = label.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    match label.as_str() {
        "" => Some(None),
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Some(Charset::Utf8)),
        // As browsers do, Latin-1 and ASCII read as windows-1252.
        "windows-1252" | "cp1252" | "x-cp1252" | "iso-8859-1" | "iso8859-1" | "latin1" | "l1" | "us-ascii" | "ascii" => Some(Some(Charset::Windows1252)),
        _ => None,
    }
}

fn decode(body: &[u8], header: &str) -> Result<String, &'static str> {
    if let Some(rest) = body.strip_prefix(b"\xEF\xBB\xBF") { return Ok(String::from_utf8_lossy(rest).into_owned()); }
    if body.starts_with(b"\xFF\xFE") || body.starts_with(b"\xFE\xFF") { return Err(CHARSET); }
    let named = match charset(header).ok_or(CHARSET)? {
        Some(named) => Some(named),
        None => charset(&meta_charset(&body[..body.len().min(1024)])).ok_or(CHARSET)?,
    };
    Ok(match named.unwrap_or(Charset::Utf8) {
        Charset::Utf8 => String::from_utf8_lossy(body).into_owned(),
        Charset::Windows1252 => body.iter().map(|&byte| windows_1252(byte)).collect(),
    })
}

/// `<meta charset=…>` or `content="…; charset=…"` near the top.
fn meta_charset(head: &[u8]) -> String {
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    let Some(at) = head.find("charset=") else { return String::new() };
    head[at + 8..].trim_start_matches(['"', '\'', ' ']).chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')).collect()
}

fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    if (0x80..0xA0).contains(&byte) { HIGH[usize::from(byte - 0x80)] } else { char::from(byte) }
}

const SKIPPED: [&str; 8] = ["script", "style", "noscript", "template", "svg", "iframe", "object", "math"];
const BLOCKS: [&str; 28] = [
    "address", "article", "aside", "blockquote", "br", "dd", "div", "dl", "dt", "figcaption", "footer", "h1", "h2", "h3",
    "h4", "h5", "h6", "header", "hr", "li", "main", "nav", "ol", "p", "pre", "section", "td", "tr",
];

fn extract(html: &str, max_bytes: usize) -> PageText {
    let mut page = PageText::default();
    let mut text = String::new();
    let mut rest = html;
    // The raw text runs longer than the result: spaces collapse and entities shrink.
    while !rest.is_empty() && text.len() <= max_bytes.saturating_mul(4) {
        let Some(open) = rest.find('<') else {
            text.push_str(rest);
            break;
        };
        text.push_str(&rest[..open]);
        rest = &rest[open..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some((tag, after)) = tag(rest) else {
            // A lone `<` is text.
            text.push('<');
            rest = &rest[1..];
            continue;
        };
        rest = after;
        if tag.closing { continue; }
        if SKIPPED.contains(&tag.name.as_str()) || tag.name == "title" || tag.name == "textarea" {
            let end = find_ignore_case(rest, &format!("</{}", tag.name)).unwrap_or(rest.len());
            if tag.name == "title" && page.title.is_empty() { page.title = clip(&one_line(&entities(&rest[..end])), TITLE_CHARS); }
            rest = &rest[end..];
            continue;
        }
        if tag.name == "meta" && page.description.is_empty() {
            let named = attribute(tag.attributes, "name").or_else(|| attribute(tag.attributes, "property")).map(|name| name.to_ascii_lowercase());
            if matches!(named.as_deref(), Some("description" | "og:description")) {
                page.description = clip(&one_line(&entities(&attribute(tag.attributes, "content").unwrap_or_default())), DESCRIPTION_CHARS);
            }
        }
        if BLOCKS.contains(&tag.name.as_str()) { text.push(' '); }
    }
    page.text = cut(&one_line(&entities(&text)), max_bytes);
    page
}

struct Tag<'a> {
    name: String,
    closing: bool,
    attributes: &'a str,
}

/// The tag at the start of `html` and what follows it, or None if `<` doesn't
/// open one. Quoted attribute values may hold `>`.
fn tag(html: &str) -> Option<(Tag<'_>, &str)> {
    let inner = &html[1..];
    let (closing, inner) = match inner.strip_prefix('/') { Some(inner) => (true, inner), None => (false, inner) };
    let first = inner.chars().next()?;
    if !(first.is_ascii_alphabetic() || (!closing && matches!(first, '!' | '?'))) { return None; }
    let name_len = inner.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '!' || c == '?')).unwrap_or(inner.len());
    let name = inner[..name_len].to_ascii_lowercase();
    let mut quote = None;
    for (at, c) in inner[name_len..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => {
                let end = name_len + at;
                return Some((Tag { name, closing, attributes: &inner[name_len..end] }, &inner[end + 1..]));
            }
            _ => {}
        }
    }
    Some((Tag { name, closing, attributes: &inner[name_len..] }, ""))
}

fn attribute(attributes: &str, wanted: &str) -> Option<String> {
    let mut rest = attributes;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() { return None; }
        let name_len = rest.find(|c: char| c.is_whitespace() || c == '=' || c == '/').unwrap_or(rest.len());
        let name = &rest[..name_len];
        rest = rest[name_len..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (value, next) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(q).unwrap_or(body.len());
                    (&body[..end], body.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            rest = next;
            value
        } else {
            ""
        };
        if name.eq_ignore_ascii_case(wanted) { return Some(value.to_owned()); }
    }
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    haystack.as_bytes().windows(needle.len()).position(|window| window.eq_ignore_ascii_case(needle))
}

/// Character references: numeric ones and the common named ones. Others
/// stay as written. A reference ends within 10 bytes of its `&`, so only
/// those are searched: the time is linear in the text.
fn entities(text: &str) -> String {
    const NAMED: [(&str, char); 28] = [
        ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''), ("nbsp", ' '), ("ndash", '–'), ("mdash", '—'),
        ("hellip", '…'), ("lsquo", '‘'), ("rsquo", '’'), ("ldquo", '“'), ("rdquo", '”'), ("laquo", '«'), ("raquo", '»'),
        ("copy", '©'), ("reg", '®'), ("trade", '™'), ("middot", '·'), ("bull", '•'), ("eacute", 'é'), ("egrave", 'è'),
        ("agrave", 'à'), ("ccedil", 'ç'), ("uuml", 'ü'), ("ouml", 'ö'), ("auml", 'ä'), ("szlig", 'ß'),
    ];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.as_bytes()[1..rest.len().min(11)].iter().position(|byte| *byte == b';').map(|end| end + 1);
        let decoded = end.and_then(|end| {
            let name = &rest[1..end];
            let number = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")).map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| name.strip_prefix('#').map(str::parse::<u32>));
            let decoded = match number {
                Some(code) => code.ok().and_then(char::from_u32).filter(|c| *c != '\0'),
                None => NAMED.iter().find(|(named, _)| *named == name).map(|(_, c)| *c),
            };
            decoded.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Spaces collapsed; control and format characters that can disguise
/// text dropped.
fn one_line(text: &str) -> String {
    let kept: String = text.chars().filter_map(|c| {
        if c.is_whitespace() { Some(' ') } else if c.is_control() || invisible(c) { None } else { Some(c) }
    }).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

fn clip(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// At most `max_bytes`, on a character boundary.
fn cut(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].trim_end().to_owned()
}

#[cfg(test)]
#[path = "link_preview_tests.rs"]
mod tests;
