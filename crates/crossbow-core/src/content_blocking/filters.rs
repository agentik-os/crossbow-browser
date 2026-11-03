//! The filter syntax Crossbow reads, one line at a time:
//!
//! - `!` comments, `[Adblock Plus …]` headers and `#` hosts-file comments;
//! - network rules: `||host^`, `|start`, `end|`, plain text with `*` and `^`,
//!   `@@` exceptions, and the options `third-party`/`3p`, `~third-party`/
//!   `first-party`/`1p`, `domain=` (or `from=`), `match-case`, `important`
//!   and the request types below, each also with `~`;
//! - `@@…$generichide` (or `ghide`), with no option but `domain=`: pages it
//!   matches get only their own host's element hiding rules;
//! - hosts-file lines (`0.0.0.0 host`, `127.0.0.1 host`) and lines that are
//!   only a host name, both read as `||host^`;
//! - element hiding: `##selector` on every page, `host,~other##selector` on
//!   some, and `#@#` to unhide, with selectors that pass [`selector`].
//!
//! Anything else is unsupported: regular expressions, other options such as
//! `redirect`, `csp`, `popup` or `document`, procedural and scriptlet
//! filters (`#?#`, `#$#`, `#%#`, `##+js(…)`, `:has-text()`, `:style()`…)
//! and HTML filters. So is a rule that would block every request with
//! nothing to narrow it. An unsupported rule is counted and skipped;
//! [`coverage`] says why.

use std::borrow::Cow;
use std::collections::BTreeMap;

/// What a request is, for a rule's type options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestType {
    Subdocument,
    Script,
    Image,
    Stylesheet,
    Font,
    Object,
    Media,
    Xhr,
    Ping,
    Websocket,
    Other,
}

impl RequestType {
    pub(crate) const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// Every type: a rule without type options.
pub(crate) const ALL_TYPES: u16 = (1 << 11) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Party {
    Any,
    First,
    Third,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    None,
    /// `|`: the address's first character.
    Start,
    /// `||`: the start of the host or of one of its labels.
    Host,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pattern {
    /// `||host^`: the host or any subdomain of it.
    Host(Box<str>),
    /// `||host` then the rest, which starts with `^` or `/` and is matched
    /// from the end of the request's host.
    HostThen { host: Box<str>, rest: Box<str>, end: bool },
    /// `*` matches any run and `^` one separator or the end; `end` is a
    /// closing `|`.
    Glob { anchor: Anchor, glob: Box<str>, end: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetworkRule {
    pub(crate) exception: bool,
    pub(crate) important: bool,
    /// `@@…$generichide`: turns off rules for every page, never blocks or
    /// allows a request.
    pub(crate) generic_hide: bool,
    pub(crate) match_case: bool,
    pub(crate) party: Party,
    pub(crate) types: u16,
    /// `domain=` entries, lowercase: the host and whether it is included.
    pub(crate) domains: Box<[(Box<str>, bool)]>,
    pub(crate) pattern: Pattern,
}

/// An element hiding rule: `##` hides what `selector` matches, `#@#`
/// unhides it. Without `domains` it applies on every page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HidingRule {
    pub(crate) exception: bool,
    /// Lowercase hosts and whether each is included, as in `domain=`.
    pub(crate) domains: Box<[(Box<str>, bool)]>,
    pub(crate) selector: Box<str>,
}

impl HidingRule {
    /// Roughly what it holds, on the heap and in place.
    pub(crate) fn heap_bytes(&self) -> usize {
        std::mem::size_of::<HidingRule>() + self.selector.len() + self.domains.iter().map(|(host, _)| host.len() + std::mem::size_of::<(Box<str>, bool)>()).sum::<usize>()
    }
}

/// A list's rules Crossbow can use, and how many it skipped.
#[derive(Debug, Default)]
pub struct Parsed {
    pub(crate) network: Vec<NetworkRule>,
    pub(crate) hiding: Vec<HidingRule>,
    pub unsupported: usize,
}

impl Parsed {
    pub fn rules(&self) -> usize {
        self.network.len() + self.hiding.len()
    }
}

pub fn parse(text: &str) -> Parsed {
    read(text, |_, _| {})
}

/// How much of a list applies: its network and element hiding rules
/// (`cosmetic`), how many of each Crossbow reads, and why it skips the rest.
#[derive(Debug, Default)]
pub struct Coverage {
    pub network: Share,
    pub cosmetic: Share,
    /// Skipped rules by reason.
    pub reasons: BTreeMap<String, usize>,
}

/// How many rules of one kind are read, of how many.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Share {
    pub supported: usize,
    pub total: usize,
}

/// What [`parse`] reads of `text`, counted as [`Coverage`].
pub fn coverage(text: &str) -> Coverage {
    let mut coverage = Coverage::default();
    read(text, |kind, skip| {
        let share = match kind {
            Kind::Network => &mut coverage.network,
            Kind::Cosmetic => &mut coverage.cosmetic,
        };
        share.total += 1;
        match skip {
            None => share.supported += 1,
            Some(reason) => *coverage.reasons.entry(reason.into_owned()).or_default() += 1,
        }
    });
    coverage
}

/// Why a rule is skipped.
type Skip = Cow<'static, str>;

#[derive(Clone, Copy)]
enum Kind {
    Network,
    Cosmetic,
}

const PROCEDURAL_FILTER: &str = "procedural filter (#?#)";
const INJECTED_CSS: &str = "injected CSS (#$#)";
const SCRIPTLET: &str = "scriptlet (+js, #%#)";
const HTML_FILTER: &str = "HTML filter ($$, ##^)";
const UNBALANCED: &str = "selector with unbalanced quotes, brackets or parentheses";

/// Each rule `text` holds, read or counted as unsupported; `seen` hears of
/// each, with why when it was skipped.
fn read(text: &str, mut seen: impl FnMut(Kind, Option<Skip>)) -> Parsed {
    let mut parsed = Parsed::default();
    for line in text.strip_prefix('\u{feff}').unwrap_or(text).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('!') || (line.starts_with('[') && line.ends_with(']')) { continue; }
        if cosmetic(line) {
            match hiding(line) {
                Some(rule) => {
                    parsed.hiding.push(rule);
                    seen(Kind::Cosmetic, None);
                }
                None => {
                    parsed.unsupported += 1;
                    seen(Kind::Cosmetic, Some(cosmetic_skip(line)));
                }
            }
            continue;
        }
        if line.starts_with('#') { continue; }
        if let Some(hosts) = hosts_line(line) {
            for host in hosts {
                match host {
                    Some(host) => {
                        parsed.network.push(host_rule(host));
                        seen(Kind::Network, None);
                    }
                    None => {
                        parsed.unsupported += 1;
                        seen(Kind::Network, Some("hosts-file name that isn't a host name".into()));
                    }
                }
            }
            continue;
        }
        match plain_host(line).map_or_else(|| network(line), |host| Ok(host_rule(host))) {
            Ok(rule) => {
                parsed.network.push(rule);
                seen(Kind::Network, None);
            }
            Err(skip) => {
                parsed.unsupported += 1;
                seen(Kind::Network, Some(skip));
            }
        }
    }
    parsed
}

/// `domains##selector` or `domains#@#selector`, when every part is in the
/// subset.
fn hiding(line: &str) -> Option<HidingRule> {
    let (exception, domains, selector_text) = split_hiding(line)?;
    let domains: Box<[(Box<str>, bool)]> = if domains.is_empty() {
        Box::new([])
    } else {
        domains.split(',').map(|entry| match entry.strip_prefix('~') {
            Some(host) => Some((host_name(host)?, false)),
            None => Some((host_name(entry)?, true)),
        }).collect::<Option<_>>()?
    };
    Some(HidingRule { exception, domains, selector: selector(selector_text)? })
}

/// Whether a hiding line unhides, its sites and its selector.
fn split_hiding(line: &str) -> Option<(bool, &str, &str)> {
    let (at, exception, marker) = [("#@#", true), ("##", false)].into_iter().find_map(|(marker, exception)| line.find(marker).map(|at| (at, exception, marker)))?;
    Some((exception, &line[..at], &line[at + marker.len()..]))
}

/// Why a line with a cosmetic marker isn't read.
fn cosmetic_skip(line: &str) -> Skip {
    const OTHER: [(&str, &str); 8] = [
        ("#?#", PROCEDURAL_FILTER), ("#@?#", PROCEDURAL_FILTER), ("#$#", INJECTED_CSS), ("#@$#", INJECTED_CSS),
        ("#%#", SCRIPTLET), ("#@%#", SCRIPTLET), ("$$", HTML_FILTER), ("$@$", HTML_FILTER),
    ];
    if let Some((_, reason)) = OTHER.iter().find(|(marker, _)| line.contains(marker)) { return (*reason).into(); }
    let Some((_, domains, selector_text)) = split_hiding(line) else { return "cosmetic filter".into() };
    let entries = || domains.split(',').map(|entry| entry.strip_prefix('~').unwrap_or(entry));
    if !domains.is_empty() && entries().any(|entry| host_name(entry).is_none()) {
        let reason = if entries().any(|entry| entry.ends_with(".*")) { "site pattern that isn't a host name (example.*)" } else { "site name that isn't a host name" };
        return reason.into();
    }
    check_selector(selector_text).err().unwrap_or_else(|| "cosmetic filter".into())
}

/// The longest selector kept.
pub const MAX_SELECTOR_BYTES: usize = 4096;

/// Extended pseudo-classes that run logic, which plain CSS can't do.
const PROCEDURAL: [&str; 25] = [
    "has-text", "contains", "-abp-contains", "-abp-has", "-abp-properties", "matches-css", "matches-css-before", "matches-css-after",
    "matches-attr", "matches-media", "matches-path", "matches-prop", "min-text-length", "nth-ancestor", "others", "remove", "remove-attr",
    "remove-class", "style", "upward", "watch-attr", "xpath", "if", "if-not", "shadow-root",
];

/// `text`, trimmed, when it is safe to place before `{display:none!important}`:
/// nothing that could end the rule or start another (`{ } ; @ /* <` outside a
/// string), no escape that spells one of those or a quote or backslash,
/// balanced strings, brackets and parentheses, no control characters, no
/// procedural pseudo-class, and at most [`MAX_SELECTOR_BYTES`]. Anything
/// that isn't a valid selector then only fails on its own rule.
pub(crate) fn selector(text: &str) -> Option<Box<str>> {
    check_selector(text).ok()
}

/// [`selector`], or why not.
fn check_selector(text: &str) -> Result<Box<str>, Skip> {
    let text = text.trim_matches(' ');
    if text.starts_with("+js(") { return Err(SCRIPTLET.into()); }
    if text.starts_with('^') { return Err(HTML_FILTER.into()); }
    if text.is_empty() { return Err("empty selector".into()); }
    if text.len() > MAX_SELECTOR_BYTES { return Err("selector longer than 4,096 bytes".into()); }
    let mut chars = text.char_indices().peekable();
    let (mut quote, mut brackets, mut parens) = (None::<char>, 0usize, 0usize);
    while let Some((at, c)) = chars.next() {
        if c.is_control() { return Err("selector with a control character".into()); }
        if c == '\\' {
            escape(&mut chars).ok_or("selector escape that spells a quote, backslash or rule end")?;
            continue;
        }
        if let Some(open) = quote {
            if c == open { quote = None; }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '}' | ';' | '@' | '<' => return Err("selector with { } ; @ or <".into()),
            // An id needs a name: `## Ads ##` and `######` are banners, not rules.
            '#' if !text[at + 1..].starts_with(|n: char| n.is_alphanumeric() || n == '-' || n == '_' || n == '\\' || !n.is_ascii()) => return Err("# with no id after it".into()),
            '/' if text[at + 1..].starts_with('*') => return Err("comment in a selector (/*)".into()),
            '[' => brackets += 1,
            ']' => brackets = brackets.checked_sub(1).ok_or(UNBALANCED)?,
            '(' => parens += 1,
            ')' => parens = parens.checked_sub(1).ok_or(UNBALANCED)?,
            ':' => {
                let name = text[at + 1..].trim_start_matches(':').chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>().to_ascii_lowercase();
                if PROCEDURAL.contains(&name.as_str()) { return Err(format!("procedural pseudo-class (:{name})").into()); }
            }
            _ => {}
        }
    }
    if quote.is_none() && brackets == 0 && parens == 0 { Ok(text.into()) } else { Err(UNBALANCED.into()) }
}

/// One CSS escape after its backslash: up to six hex digits and an optional
/// space, or one character. None when it is cut off or spells a character
/// that could close a string or a rule.
fn escape(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Option<()> {
    const SPECIAL: [char; 9] = ['{', '}', ';', '@', '<', '"', '\'', '\\', '/'];
    let (_, first) = chars.next()?;
    let decoded = if first.is_ascii_hexdigit() {
        let mut digits = String::from(first);
        while digits.len() < 6 && let Some((_, c)) = chars.next_if(|(_, c)| c.is_ascii_hexdigit()) { digits.push(c); }
        chars.next_if(|(_, c)| *c == ' ');
        char::from_u32(u32::from_str_radix(&digits, 16).ok()?).filter(|c| *c != '\0')?
    } else {
        first
    };
    (!decoded.is_control() && !SPECIAL.contains(&decoded)).then_some(())
}

/// Other page-changing syntax: not applied.
fn cosmetic(line: &str) -> bool {
    const MARKERS: [&str; 10] = ["##", "#@#", "#?#", "#@?#", "#$#", "#@$#", "#%#", "#@%#", "$$", "$@$"];
    MARKERS.iter().any(|marker| line.contains(marker))
}

/// A hosts-file line's names: None when the line isn't one, and None for a
/// name that isn't a valid host. The file's own names are left out.
fn hosts_line(line: &str) -> Option<Vec<Option<Box<str>>>> {
    const OWN: [&str; 12] = ["localhost", "localhost.localdomain", "local", "broadcasthost", "0.0.0.0", "ip6-localhost", "ip6-loopback", "ip6-localnet", "ip6-mcastprefix", "ip6-allnodes", "ip6-allrouters", "ip6-allhosts"];
    let mut words = line.split_whitespace();
    if !matches!(words.next()?, "0.0.0.0" | "127.0.0.1" | "::" | "::1") { return None; }
    let names = words.take_while(|word| !word.starts_with('#'));
    Some(names.filter(|name| !OWN.contains(&name.to_ascii_lowercase().as_str())).map(host_name).collect())
}

/// A line that is only a host name, as in a list of domains. One whose last
/// label is a file extension stays text, as in `ads.js`.
fn plain_host(line: &str) -> Option<Box<str>> {
    const EXTENSIONS: [&str; 22] = ["js", "css", "gif", "png", "jpg", "jpeg", "svg", "webp", "ico", "html", "htm", "php", "asp", "aspx", "jsp", "cgi", "swf", "mp4", "json", "xml", "txt", "woff"];
    let (_, last) = line.rsplit_once('.')?;
    if EXTENSIONS.contains(&last.to_ascii_lowercase().as_str()) || !last.bytes().all(|c| c.is_ascii_alphabetic()) { return None; }
    host_name(line)
}

/// Lowercase, without a trailing dot: labels of 1–63 letters, digits, `-`
/// or `_`, none starting or ending with `-`.
pub(crate) fn host_name(name: &str) -> Option<Box<str>> {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    let valid = !name.is_empty() && name.len() <= 253 && name.split('.').all(|label| {
        !label.is_empty() && label.len() <= 63 && !label.starts_with('-') && !label.ends_with('-')
            && label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    });
    valid.then(|| name.into_boxed_str())
}

fn host_rule(host: Box<str>) -> NetworkRule {
    NetworkRule { exception: false, important: false, generic_hide: false, match_case: false, party: Party::Any, types: ALL_TYPES, domains: Box::new([]), pattern: Pattern::Host(host) }
}

/// A network rule, or why it is outside the subset.
fn network(line: &str) -> Result<NetworkRule, Skip> {
    let (exception, body) = match line.strip_prefix("@@") {
        Some(body) => (true, body),
        None => (false, line),
    };
    let (text, options) = split_options(body);
    if text.len() > 1 && text.starts_with('/') && text.ends_with('/') { return Err("regular expression".into()); }
    let mut rule = NetworkRule { exception, important: false, generic_hide: false, match_case: false, party: Party::Any, types: ALL_TYPES, domains: Box::new([]), pattern: Pattern::Host("".into()) };
    let (mut include, mut exclude) = (0u16, 0u16);
    let unsupported = |negated: bool, name: &str, value: bool| -> Skip { format!("${}{name}{} option", if negated { "~" } else { "" }, if value { "=" } else { "" }).into() };
    for option in options.into_iter().flat_map(|options| options.split(',')) {
        let (name, value) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (option, None),
        };
        let name = name.to_ascii_lowercase();
        let (negated, name) = match name.strip_prefix('~') {
            Some(name) => (true, name),
            None => (false, name.as_str()),
        };
        match (name, value) {
            ("third-party" | "3p", None) => rule.party = if negated { Party::First } else { Party::Third },
            ("first-party" | "1p", None) => rule.party = if negated { Party::Third } else { Party::First },
            ("domain" | "from", Some(value)) if !negated => rule.domains = domains(value).ok_or("$domain= value that isn't a host name")?,
            ("match-case", None) if !negated => rule.match_case = true,
            ("important", None) if !negated && !exception => rule.important = true,
            ("generichide" | "ghide", None) if !negated && exception => rule.generic_hide = true,
            (name, None) => {
                let bit = request_type(name).ok_or_else(|| unsupported(negated, name, false))?.bit();
                if negated { exclude |= bit } else { include |= bit }
            }
            (name, Some(_)) => return Err(unsupported(negated, name, true)),
        }
    }
    // Generic hiding is turned off for pages, so a type or party can't narrow it.
    if rule.generic_hide && (include != 0 || exclude != 0 || rule.party != Party::Any) { return Err("$generichide with an option other than $domain=".into()); }
    rule.types = if include == 0 { ALL_TYPES } else { include } & !exclude;
    if rule.types == 0 { return Err("type options that leave no request type".into()); }
    rule.pattern = pattern(text, rule.match_case);
    // A rule that would match every request needs something to narrow it.
    let everything = match &rule.pattern {
        Pattern::Glob { glob, .. } => glob.bytes().all(|c| c == b'*'),
        _ => false,
    };
    if everything && rule.types == ALL_TYPES && rule.party == Party::Any && rule.domains.is_empty() { return Err("rule that would match every request".into()); }
    Ok(rule)
}

/// The text before the last `$` and the options after it, when they read as
/// options; otherwise the whole line is text.
fn split_options(body: &str) -> (&str, Option<&str>) {
    let Some((text, options)) = body.rsplit_once('$') else { return (body, None) };
    let shaped = options.split(',').all(|option| {
        let name = option.split_once('=').map_or(option, |(name, _)| name);
        let name = name.strip_prefix('~').unwrap_or(name);
        !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    });
    if shaped { (text, Some(options)) } else { (body, None) }
}

fn request_type(name: &str) -> Option<RequestType> {
    Some(match name {
        "script" => RequestType::Script,
        "image" => RequestType::Image,
        "stylesheet" | "css" => RequestType::Stylesheet,
        "subdocument" | "frame" => RequestType::Subdocument,
        "xmlhttprequest" | "xhr" => RequestType::Xhr,
        "media" => RequestType::Media,
        "font" => RequestType::Font,
        "object" => RequestType::Object,
        "ping" | "beacon" => RequestType::Ping,
        "websocket" => RequestType::Websocket,
        "other" => RequestType::Other,
        _ => return None,
    })
}

/// `a.example|~b.example`: hosts only, so no entities or expressions.
fn domains(value: &str) -> Option<Box<[(Box<str>, bool)]>> {
    value.split('|').map(|entry| match entry.strip_prefix('~') {
        Some(host) => Some((host_name(host)?, false)),
        None => Some((host_name(entry)?, true)),
    }).collect()
}

fn pattern(text: &str, match_case: bool) -> Pattern {
    let text = if match_case { text.to_owned() } else { text.to_ascii_lowercase() };
    let (anchor, rest) = if let Some(rest) = text.strip_prefix("||") {
        (Anchor::Host, rest)
    } else if let Some(rest) = text.strip_prefix('|') {
        (Anchor::Start, rest)
    } else {
        (Anchor::None, text.as_str())
    };
    let (rest, end) = match rest.strip_suffix('|') {
        Some(rest) => (rest, true),
        None => (rest, false),
    };
    let mut glob = String::with_capacity(rest.len());
    for c in rest.chars() {
        if !(c == '*' && glob.ends_with('*')) { glob.push(c); }
    }
    if anchor == Anchor::Host {
        let host_end = glob.find(['^', '/', '*', ':', '?', '&', '=']).unwrap_or(glob.len());
        let (host, after) = glob.split_at(host_end);
        if let Some(host) = host_name(host) {
            if after == "^" && !end { return Pattern::Host(host); }
            if after.starts_with(['^', '/']) { return Pattern::HostThen { host, rest: after.into(), end }; }
        }
    }
    Pattern::Glob { anchor, glob: glob.into_boxed_str(), end }
}
