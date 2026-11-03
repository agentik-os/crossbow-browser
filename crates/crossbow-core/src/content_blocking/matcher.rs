//! Compiled lists and the decision for one request.
//!
//! Each list keeps four sets: block rules, `@@` exceptions, `important`
//! block rules and `@@…$generichide` rules, which only element hiding asks.
//! In each set, `||host^` rules without options sit in a hash
//! set looked up with each suffix of the request's host; other `||host…`
//! rules are listed by their host. The rest are listed under their rarest
//! token, a run of letters, digits and `%` that the rule's text bounds on
//! both sides, so that only rules sharing a token with the address are
//! tried; a rule without one is tried for every request. Patterns are
//! matched in place, without regular expressions or backtracking.

use super::filters::{ALL_TYPES, Anchor, HidingRule, NetworkRule, Party, Pattern, RequestType, parse};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, Weak};

/// One list's rules, compiled. Shared by every profile that added the same
/// text, and read without locks.
#[derive(Debug, Default)]
pub struct CompiledList {
    /// The SHA-256 of the list's text, in hex.
    pub id: String,
    pub rules: usize,
    pub unsupported: usize,
    block: Set,
    allow: Set,
    important: Set,
    generic_hide: Set,
    /// Element hiding rules, in list order.
    pub(crate) hiding: Vec<HidingRule>,
}

impl CompiledList {
    /// Roughly what the compiled rules hold on the heap.
    pub fn heap_bytes(&self) -> usize {
        let hiding: usize = self.hiding.iter().map(HidingRule::heap_bytes).sum();
        self.block.heap_bytes() + self.allow.heap_bytes() + self.important.heap_bytes() + self.generic_hide.heap_bytes() + hiding
    }

    /// Whether an `@@…$generichide` rule matches `page`, so it gets only
    /// its own host's element hiding.
    pub(crate) fn generic_hidden(&self, page: &Request<'_>) -> bool {
        self.generic_hide.matches(page)
    }

    pub(crate) fn has_generic_hide(&self) -> bool {
        !self.generic_hide.rules.is_empty() || !self.generic_hide.plain.is_empty()
    }
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Lists compiled in this process, by id, while any profile uses them.
static COMPILED: LazyLock<Mutex<HashMap<String, Weak<CompiledList>>>> = LazyLock::new(Default::default);

/// `text` compiled, or the copy already compiled for the same text.
pub fn compile(text: &str) -> Arc<CompiledList> {
    let id = sha256_hex(text.as_bytes());
    let cached = |id: &str| COMPILED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id).and_then(Weak::upgrade);
    if let Some(list) = cached(&id) { return list; }
    // Compiled outside the lock; if another thread finished first, its copy wins.
    let parsed = parse(text);
    let mut list = CompiledList { id: id.clone(), rules: parsed.rules(), unsupported: parsed.unsupported, ..Default::default() };
    let (mut block, mut allow, mut important, mut generic_hide) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for rule in parsed.network {
        if rule.generic_hide { generic_hide.push(rule) } else if rule.exception { allow.push(rule) } else if rule.important { important.push(rule) } else { block.push(rule) }
    }
    list.block = Set::new(block);
    list.allow = Set::new(allow);
    list.important = Set::new(important);
    list.generic_hide = Set::new(generic_hide);
    list.hiding = parsed.hiding;
    let mut compiled = COMPILED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(list) = compiled.get(&id).and_then(Weak::upgrade) { return list; }
    compiled.retain(|_, list| list.strong_count() > 0);
    let list = Arc::new(list);
    compiled.insert(id, Arc::downgrade(&list));
    list
}

/// A request as the rules see it.
#[derive(Debug)]
pub struct Request<'a> {
    url: &'a str,
    lower: String,
    /// The host's byte range in the address.
    host: (usize, usize),
    kind: u16,
    /// The requesting document's host, lowercase.
    context: Option<String>,
    third_party: Option<bool>,
    tokens: Vec<u32>,
}

impl<'a> Request<'a> {
    /// None for an address that isn't http, https, ws or wss: those are never
    /// blocked. `context` is the address of the document that made the
    /// request; empty when unknown.
    pub fn new(url: &'a str, kind: RequestType, context: &str) -> Option<Request<'a>> {
        let (scheme, host) = split(url)?;
        let kind = if matches!(scheme, "ws" | "wss") { RequestType::Websocket } else { kind };
        let lower = url.to_ascii_lowercase();
        let context = split(context).map(|(_, (start, end))| context[start..end].to_ascii_lowercase());
        let third_party = context.as_deref().map(|context| !same_site(context, &lower[host.0..host.1]));
        let mut tokens = Vec::new();
        let bytes = lower.as_bytes();
        let mut start = None;
        for (i, &c) in bytes.iter().chain([&b' ']).enumerate() {
            match (token_char(c), start) {
                (true, None) => start = Some(i),
                (false, Some(from)) => {
                    tokens.push(hash(&bytes[from..i]));
                    start = None;
                }
                _ => {}
            }
        }
        Some(Request { url, lower, host, kind: kind.bit(), context, third_party, tokens })
    }

    fn host(&self) -> &str {
        &self.lower[self.host.0..self.host.1]
    }
}

/// An address's lowercase scheme and its host's byte range; None unless
/// the scheme is http, https, ws or wss and there is a host.
fn split(url: &str) -> Option<(&'static str, (usize, usize))> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = ["http", "https", "ws", "wss"].into_iter().find(|known| scheme.eq_ignore_ascii_case(known))?;
    let start = url.len() - rest.len();
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let after_user = authority.rfind('@').map_or(0, |at| at + 1);
    let host = &authority[after_user..];
    let host_len = if host.starts_with('[') { host.find(']').map_or(host.len(), |end| end + 1) } else { host.find(':').unwrap_or(host.len()) };
    (host_len > 0).then_some((scheme, (start + after_user, start + after_user + host_len)))
}

/// Whether `lists` block `request`: a matching `important` rule, or a
/// matching rule and no matching exception in any of them.
pub fn blocks(lists: &[Arc<CompiledList>], request: &Request<'_>) -> bool {
    if lists.iter().any(|list| list.important.matches(request)) { return true; }
    lists.iter().any(|list| list.block.matches(request)) && !lists.iter().any(|list| list.allow.matches(request))
}

/// Second levels that are generic under a two-letter country domain.
const GENERIC_SECOND: [&str; 16] = ["co", "com", "net", "org", "gov", "edu", "ac", "or", "ne", "go", "gob", "mil", "nic", "ltd", "plc", "sch"];

/// Whether two hosts are the same site, by a simplified rule: the last two
/// labels, or three under a two-letter top-level domain whose second level
/// is generic, as in `co.uk`. Without the public-suffix list, hosts under a
/// shared suffix such as `github.io` count as one site.
pub fn same_site(a: &str, b: &str) -> bool {
    site(a) == site(b)
}

/// A host's site by the rule [`same_site`] uses.
pub fn site(host: &str) -> &str {
    let host = host.trim_end_matches('.');
    if host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok() { return host; }
    let labels: Vec<&str> = host.rsplit('.').collect();
    let keep = match labels.as_slice() {
        [tld, second, _, ..] if tld.len() == 2 && GENERIC_SECOND.contains(second) => 3,
        _ => 2,
    };
    if labels.len() <= keep { return host; }
    let cut: usize = labels[..keep].iter().map(|label| label.len() + 1).sum();
    &host[host.len() + 1 - cut..]
}

fn token_char(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'%'
}

/// FNV-1a: a token's key in the index. A collision only tries more rules.
fn hash(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, &byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193))
}

/// Not a letter, digit or `_ - . %`: what `^` matches.
fn separator(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'%'))
}

#[derive(Debug, Default)]
struct Set {
    /// `||host^` rules without options.
    plain: HashSet<Box<str>>,
    /// Other `||host…` rules, by host.
    hosts: HashMap<Box<str>, Vec<u32>>,
    /// The rest, by their rarest token.
    tokens: HashMap<u32, Vec<u32>>,
    /// Rules without a usable token, tried for every request.
    fallback: Vec<u32>,
    rules: Vec<Rule>,
}

#[derive(Debug)]
struct Rule {
    party: Party,
    types: u16,
    match_case: bool,
    domains: Box<[(Box<str>, bool)]>,
    pattern: Compiled,
}

#[derive(Debug)]
enum Compiled {
    Host,
    HostThen(Glob),
    Glob(Anchor, Glob),
}

/// `*`-separated parts; in a part, `^` matches one separator or the end.
#[derive(Debug)]
struct Glob {
    parts: Box<[Box<[u8]>]>,
    end: bool,
}

impl Glob {
    fn new(text: &str, end: bool) -> Glob {
        Glob { parts: text.as_bytes().split(|&c| c == b'*').map(Box::from).collect(), end }
    }

    /// Whether the glob matches `text` from exactly `at`.
    fn from(&self, text: &[u8], at: usize) -> bool {
        let Some(mut at) = part_at(&self.parts[0], text, at) else { return false };
        let last = self.parts.len() - 1;
        for (i, part) in self.parts.iter().enumerate().skip(1) {
            if i == last && self.end {
                return (at..=text.len()).any(|start| part_at(part, text, start) == Some(text.len()));
            }
            if part.is_empty() { continue; }
            match (at..=text.len()).find_map(|start| part_at(part, text, start)) {
                Some(next) => at = next,
                None => return false,
            }
        }
        !self.end || last > 0 || at == text.len()
    }

    /// Whether it matches starting anywhere.
    fn anywhere(&self, text: &[u8]) -> bool {
        match self.parts[0].first() {
            // A leading `*`: the first part is empty and the rest is searched.
            None => self.from(text, 0),
            Some(b'^') => (0..=text.len()).any(|at| self.from(text, at)),
            Some(&first) => (0..text.len()).filter(|&at| text[at] == first).any(|at| self.from(text, at)),
        }
    }
}

/// Where `part` ends when it matches `text` from `at`.
fn part_at(part: &[u8], text: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    for (k, &c) in part.iter().enumerate() {
        let Some(&t) = text.get(i) else {
            // Only `^` matches the end, and only once nothing else follows.
            return part[k..].iter().all(|&c| c == b'^').then_some(i);
        };
        if if c == b'^' { !separator(t) } else { c != t } { return None; }
        i += 1;
    }
    Some(i)
}

impl Set {
    fn new(rules: Vec<NetworkRule>) -> Set {
        let mut set = Set::default();
        // Tokens each rule could be listed under, then the rarest of them.
        let mut candidates: Vec<(u32, Vec<(u32, usize)>)> = Vec::new();
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for rule in rules {
            let plain = rule.party == Party::Any && rule.types == ALL_TYPES && rule.domains.is_empty();
            let id = set.rules.len() as u32;
            let pattern = match rule.pattern {
                Pattern::Host(host) if plain => {
                    set.plain.insert(host);
                    continue;
                }
                Pattern::Host(host) => {
                    set.hosts.entry(host).or_default().push(id);
                    Compiled::Host
                }
                Pattern::HostThen { host, rest, end } => {
                    set.hosts.entry(host).or_default().push(id);
                    Compiled::HostThen(Glob::new(&rest, end))
                }
                Pattern::Glob { anchor, glob, end } => {
                    let tokens = tokens(&glob.to_ascii_lowercase(), anchor != Anchor::None, end);
                    for (token, _) in &tokens { *counts.entry(*token).or_default() += 1; }
                    candidates.push((id, tokens));
                    Compiled::Glob(anchor, Glob::new(&glob, end))
                }
            };
            set.rules.push(Rule { party: rule.party, types: rule.types, match_case: rule.match_case, domains: rule.domains, pattern });
        }
        for (id, tokens) in candidates {
            match tokens.iter().min_by_key(|(token, len)| (counts[token], std::cmp::Reverse(*len))) {
                Some((token, _)) => set.tokens.entry(*token).or_default().push(id),
                None => set.fallback.push(id),
            }
        }
        set.plain.shrink_to_fit();
        set.hosts.shrink_to_fit();
        set.tokens.shrink_to_fit();
        set
    }

    fn matches(&self, request: &Request<'_>) -> bool {
        if self.rules.is_empty() && self.plain.is_empty() { return false; }
        let host = request.host();
        let mut from = 0;
        loop {
            let suffix = &host[from..];
            if self.plain.contains(suffix) { return true; }
            if let Some(ids) = self.hosts.get(suffix) && ids.iter().any(|&id| self.rules[id as usize].matches(request)) { return true; }
            match suffix.find('.') {
                Some(dot) => from += dot + 1,
                None => break,
            }
        }
        request.tokens.iter().filter_map(|token| self.tokens.get(token)).flatten().chain(&self.fallback)
            .any(|&id| self.rules[id as usize].matches(request))
    }

    fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        let ids = |lists: &mut dyn Iterator<Item = &Vec<u32>>| lists.map(|ids| ids.capacity() * 4).sum::<usize>();
        let plain = self.plain.capacity() * (size_of::<Box<str>>() + 8) + self.plain.iter().map(|host| host.len()).sum::<usize>();
        let hosts = self.hosts.capacity() * (size_of::<(Box<str>, Vec<u32>)>() + 8) + self.hosts.keys().map(|host| host.len()).sum::<usize>() + ids(&mut self.hosts.values());
        let tokens = self.tokens.capacity() * (size_of::<(u32, Vec<u32>)>() + 8) + ids(&mut self.tokens.values());
        let rules = self.rules.capacity() * size_of::<Rule>() + self.rules.iter().map(Rule::heap_bytes).sum::<usize>();
        plain + hosts + tokens + rules + self.fallback.capacity() * 4
    }
}

/// The runs of a pattern that bound a whole token of the address: not next
/// to `*`, and at the pattern's start or end only when it is anchored there.
fn tokens(glob: &str, anchored: bool, end: bool) -> Vec<(u32, usize)> {
    let bytes = glob.as_bytes();
    let mut tokens = Vec::new();
    let mut start = None;
    for i in 0..=bytes.len() {
        let token = bytes.get(i).is_some_and(|&c| token_char(c));
        match (token, start) {
            (true, None) => start = Some(i),
            (false, Some(from)) => {
                let left = if from == 0 { anchored } else { bytes[from - 1] != b'*' };
                let right = if i == bytes.len() { end } else { bytes[i] != b'*' };
                if left && right { tokens.push((hash(&bytes[from..i]), i - from)); }
                start = None;
            }
            _ => {}
        }
    }
    tokens
}

impl Rule {
    fn matches(&self, request: &Request<'_>) -> bool {
        if self.types & request.kind == 0 { return false; }
        let party = match self.party {
            Party::Any => true,
            Party::First => request.third_party == Some(false),
            Party::Third => request.third_party == Some(true),
        };
        if !party || !self.domains_allow(request) { return false; }
        let text = if self.match_case { request.url.as_bytes() } else { request.lower.as_bytes() };
        match &self.pattern {
            Compiled::Host => true,
            Compiled::HostThen(rest) => rest.from(text, request.host.1),
            Compiled::Glob(Anchor::Start, glob) => glob.from(text, 0),
            Compiled::Glob(Anchor::Host, glob) => {
                let (start, end) = request.host;
                std::iter::once(start).chain((start..end).filter(|&i| text[i] == b'.').map(|i| i + 1)).any(|at| glob.from(text, at))
            }
            Compiled::Glob(Anchor::None, glob) => glob.anywhere(text),
        }
    }

    /// `domain=`: the most specific entry naming the requesting document's
    /// host decides; with none, only a rule that includes nothing applies.
    fn domains_allow(&self, request: &Request<'_>) -> bool {
        if self.domains.is_empty() { return true; }
        let Some(context) = request.context.as_deref() else { return false };
        let within = |domain: &str| context == domain || context.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'));
        match self.domains.iter().filter(|(domain, _)| within(domain)).max_by_key(|(domain, _)| domain.len()) {
            Some((_, include)) => *include,
            None => !self.domains.iter().any(|(_, include)| *include),
        }
    }

    fn heap_bytes(&self) -> usize {
        let glob = |glob: &Glob| glob.parts.len() * std::mem::size_of::<Box<[u8]>>() + glob.parts.iter().map(|part| part.len()).sum::<usize>();
        let domains = self.domains.len() * std::mem::size_of::<(Box<str>, bool)>() + self.domains.iter().map(|(domain, _)| domain.len()).sum::<usize>();
        domains + match &self.pattern {
            Compiled::Host => 0,
            Compiled::HostThen(rest) | Compiled::Glob(_, rest) => glob(rest),
        }
    }
}

#[cfg(test)]
#[path = "matcher_tests.rs"]
mod tests;
