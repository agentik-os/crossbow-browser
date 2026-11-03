//! Links from other apps: which ones Crossbow opens, and Air Traffic Control
//! rules that choose the Space each one opens in. Rules are a device-local
//! preference in the store's key/value table, so they never enter account sync.

use crate::library::Library;
use crate::store::Store;

pub const KEY: &str = "link_routing";
const VERSION: u64 = 1;
/// Chromium's own limit for a URL.
const MAX_URL: usize = 2 * 1024 * 1024;
/// The longest text a rule matches.
pub const MAX_VALUE: usize = 512;

/// A link another app or the command line asked Crossbow to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// An http, https or local HTML file URL, normalized.
    Page(String),
    /// A `crossbow:` link. Crossbow has no pages under its own scheme yet,
    /// so it only comes to the front.
    App,
}

/// Why a link was not opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    TooLong,
    Malformed,
    /// Any scheme other than http, https, file and crossbow.
    Scheme(String),
    /// A file that is not an HTML document.
    NotHtml,
    /// A web URL without a host.
    NoHost,
}

impl Refusal {
    pub fn reason(&self) -> String {
        match self {
            Refusal::Empty => "The link is empty.".into(),
            Refusal::TooLong => "The link is longer than browsers allow.".into(),
            Refusal::Malformed => "This isn't a link Crossbow can read.".into(),
            Refusal::Scheme(scheme) => format!("Crossbow doesn't open {scheme}: links. It opens http, https and HTML file links."),
            Refusal::NotHtml => "Only HTML files on this computer open in Crossbow.".into(),
            Refusal::NoHost => "The link has no site name.".into(),
        }
    }
}

/// The lowercase scheme, when `raw` starts with one.
fn scheme_of(raw: &str) -> Option<String> {
    let (scheme, _) = raw.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = scheme.len() <= 32
        && chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// Check a link before anything opens it.
pub fn accept(raw: &str) -> Result<Link, Refusal> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Refusal::Empty);
    }
    if raw.len() > MAX_URL {
        return Err(Refusal::TooLong);
    }
    let scheme = scheme_of(raw).ok_or(Refusal::Malformed)?;
    match scheme.as_str() {
        "crossbow" => return Ok(Link::App),
        "http" | "https" | "file" => {}
        _ => return Err(Refusal::Scheme(scheme)),
    }
    let url = url::Url::parse(raw).map_err(|_| Refusal::Malformed)?;
    if scheme == "file" {
        let local = url.host_str().is_none_or(|host| host.eq_ignore_ascii_case("localhost"));
        let path = url.path().to_ascii_lowercase();
        let html = [".html", ".htm", ".xhtml", ".xht"].iter().any(|extension| path.ends_with(extension));
        return if local && html { Ok(Link::Page(url.to_string())) } else { Err(Refusal::NotHtml) };
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(Refusal::NoHost);
    }
    Ok(Link::Page(url.to_string()))
}

/// `scheme://host` for logs. Never the path, query, fragment, user name or
/// port, and never any part of a file path.
pub fn redact(raw: &str) -> String {
    let raw = raw.trim();
    let Some(scheme) = scheme_of(raw) else { return "unreadable".into() };
    if matches!(scheme.as_str(), "http" | "https") {
        if let Some(host) = url::Url::parse(raw).ok().and_then(|url| url.host_str().map(str::to_owned)) {
            return format!("{scheme}://{host}");
        }
    }
    format!("{scheme}:")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    UrlContains,
    HostIs,
}

impl Match {
    pub const ALL: [Match; 2] = [Match::UrlContains, Match::HostIs];

    pub fn label(self) -> &'static str {
        match self {
            Match::UrlContains => "URL contains",
            Match::HostIs => "Host is",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Match::UrlContains => "url-contains",
            Match::HostIs => "host-is",
        }
    }
}

/// A typed site name as a lowercase host: no path, port, user name, query or
/// wildcard. A pasted `https://site/` gives its host.
fn host_value(value: &str) -> Option<String> {
    if value.contains(char::is_whitespace) || value.contains('*') {
        return None;
    }
    let url = if value.contains("://") { url::Url::parse(value) } else { url::Url::parse(&format!("https://{value}")) }.ok()?;
    let host_only = matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    (host_only && !host.is_empty()).then_some(host)
}

/// Hosts compare without case, a trailing dot or a leading `www.`.
fn same_host(a: &str, b: &str) -> bool {
    let bare = |host: &str| {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        host.strip_prefix("www.").map(str::to_owned).unwrap_or(host)
    };
    bare(a) == bare(b)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub kind: Match,
    /// Text the URL contains, or a lowercase host name.
    pub value: String,
    pub space: String,
}

impl Rule {
    /// A rule from typed input. `URL contains` keeps the trimmed text; `Host is`
    /// takes a site name only (a pasted `https://site/` is reduced to its host).
    pub fn new(id: String, kind: Match, value: &str, space: String) -> Result<Rule, String> {
        if space.is_empty() {
            return Err("Choose the Space these links open in.".into());
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(match kind {
                Match::UrlContains => "Enter the text a link contains.".into(),
                Match::HostIs => "Enter a site name, such as example.com.".into(),
            });
        }
        if value.chars().count() > MAX_VALUE {
            return Err(format!("Use at most {MAX_VALUE} characters."));
        }
        if value.chars().any(char::is_control) {
            return Err("Remove line breaks and other control characters.".into());
        }
        let value = match kind {
            Match::UrlContains => value.to_string(),
            Match::HostIs => host_value(value).ok_or("Host is takes a site name only, such as example.com. Use URL contains to match part of a link.")?,
        };
        Ok(Rule { id, kind, value, space })
    }

    /// `URL contains “github.com”`
    pub fn describe(&self) -> String {
        format!("{} “{}”", self.kind.label(), self.value)
    }

    fn matches(&self, url: &url::Url) -> bool {
        match self.kind {
            Match::UrlContains => url.as_str().to_lowercase().contains(&self.value.to_lowercase()),
            Match::HostIs => url.host_str().is_some_and(|host| same_host(host, &self.value)),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Destination {
    /// The Space in use when the link arrives.
    #[default]
    MostRecent,
    Space(String),
}

/// Where a link opens, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub space: String,
    /// Index of the first rule that matched.
    pub rule: Option<usize>,
    /// The chosen Space no longer exists or has no profile, so the default,
    /// or failing that the most recent Space, was used instead.
    pub fallback: bool,
}

/// A Space links can open in: it exists and its profile does, which is what
/// creating its tabs' web views requires.
pub fn usable_space(lib: &Library, space: &str) -> bool {
    lib.spaces.get(space).is_some_and(|space| lib.profiles.contains_key(&space.profile_id))
}

const UNREADABLE: &str = "Saved link rules could not be read, so links open in the most recent Space. Changing a rule saves over the unreadable record.";
const NEWER: &str = "Saved link rules are from a newer version of Crossbow, so links open in the most recent Space. Changing a rule here replaces them.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Routing {
    pub rules: Vec<Rule>,
    pub default: Destination,
}

impl Routing {
    /// The first rule that matches wins. A matching rule whose Space is gone
    /// uses the default; a default Space that is gone uses the most recent one.
    /// None only when no Space can take a link.
    pub fn route(&self, lib: &Library, url: &str, most_recent: &str) -> Option<Route> {
        let rule = url::Url::parse(url).ok().and_then(|url| self.rules.iter().position(|rule| rule.matches(&url)));
        if let Some(index) = rule {
            let space = &self.rules[index].space;
            if usable_space(lib, space) {
                return Some(Route { space: space.clone(), rule, fallback: false });
            }
        }
        let (space, fallback) = self.default_space(lib, most_recent)?;
        Some(Route { space, rule, fallback: fallback || rule.is_some() })
    }

    /// The default destination's Space, and whether it fell back.
    pub fn default_space(&self, lib: &Library, most_recent: &str) -> Option<(String, bool)> {
        let chosen = match &self.default {
            Destination::Space(space) if usable_space(lib, space) => return Some((space.clone(), false)),
            Destination::Space(_) => true,
            Destination::MostRecent => false,
        };
        if usable_space(lib, most_recent) {
            return Some((most_recent.to_string(), chosen));
        }
        lib.spaces_ordered().into_iter().map(|(id, _)| id).find(|id| usable_space(lib, id)).map(|id| (id, true))
    }

    fn encode(&self) -> String {
        let rules: Vec<_> = self.rules.iter().map(|rule| serde_json::json!({
            "id": rule.id,
            "match": rule.kind.name(),
            "value": rule.value,
            "space": rule.space,
        })).collect();
        let default = match &self.default {
            Destination::MostRecent => serde_json::json!({ "kind": "most-recent" }),
            Destination::Space(space) => serde_json::json!({ "kind": "space", "space": space }),
        };
        serde_json::json!({ "version": VERSION, "default": default, "rules": rules }).to_string()
    }

    /// Saved routing and how many rows were ignored. Rules whose Space is
    /// gone are kept; Settings shows them as needing attention.
    fn decode(raw: &str) -> Result<(Routing, usize), &'static str> {
        let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| UNREADABLE)?;
        match value["version"].as_u64() {
            Some(VERSION) => {}
            Some(version) if version > VERSION => return Err(NEWER),
            _ => return Err(UNREADABLE),
        }
        let rows = value["rules"].as_array().ok_or(UNREADABLE)?;
        let mut ignored = 0;
        let mut rules: Vec<Rule> = Vec::new();
        for row in rows {
            let kind = Match::ALL.into_iter().find(|kind| row["match"].as_str() == Some(kind.name()));
            let rule = match (row["id"].as_str(), kind, row["value"].as_str(), row["space"].as_str()) {
                (Some(id), Some(kind), Some(value), Some(space)) if !id.is_empty() && !rules.iter().any(|rule| rule.id == id) => {
                    Rule::new(id.into(), kind, value, space.into()).ok()
                }
                _ => None,
            };
            match rule {
                Some(rule) => rules.push(rule),
                None => ignored += 1,
            }
        }
        let default = match (value["default"]["kind"].as_str(), value["default"]["space"].as_str()) {
            (Some("most-recent"), _) => Destination::MostRecent,
            (Some("space"), Some(space)) if !space.is_empty() => Destination::Space(space.into()),
            _ => {
                ignored += 1;
                Destination::MostRecent
            }
        };
        Ok((Routing { rules, default }, ignored))
    }
}

/// This device's saved routing.
#[derive(Clone, Debug, Default)]
pub struct LinkRouting {
    saved: Routing,
    /// Why saved rules were not fully read; cleared by the next save.
    pub notice: Option<String>,
}

impl LinkRouting {
    /// Read this device's rules. A bad record never blocks links and is left
    /// untouched until the next save.
    pub fn load(store: &Store) -> Self {
        let raw = match store.kv_get(KEY) {
            Ok(raw) => raw,
            Err(_) => return LinkRouting { saved: Routing::default(), notice: Some("Saved link rules could not be read, so links open in the most recent Space. Reopen Crossbow to try again; changing a rule saves over them.".into()) },
        };
        let Some(raw) = raw else { return Self::default() };
        match Routing::decode(&raw) {
            Ok((saved, 0)) => LinkRouting { saved, notice: None },
            Ok((saved, _)) => LinkRouting { saved, notice: Some("Some saved link rules were unreadable, so they were ignored. Changing a rule saves the list shown here.".into()) },
            Err(notice) => LinkRouting { saved: Routing::default(), notice: Some(notice.into()) },
        }
    }

    pub fn saved(&self) -> &Routing {
        &self.saved
    }

    /// Write `candidate` in one statement. On failure nothing changes here.
    pub fn save(&mut self, store: &Store, candidate: Routing) -> Result<(), String> {
        store.kv_set(KEY, &candidate.encode()).map_err(|error| error.to_string())?;
        self.saved = candidate;
        self.notice = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Theme;

    fn page(raw: &str) -> String {
        match accept(raw) {
            Ok(Link::Page(url)) => url,
            other => panic!("{raw} should open as a page: {other:?}"),
        }
    }

    #[test]
    fn only_web_html_files_and_crossbow_links_are_accepted() {
        assert_eq!(page("https://Example.COM/a b?q=1#x"), "https://example.com/a%20b?q=1#x");
        assert_eq!(page("  http://127.0.0.1:8080/path  "), "http://127.0.0.1:8080/path");
        assert_eq!(page("HTTPS://example.com"), "https://example.com/");
        for file in ["file:///Users/someone/Site/index.html", "file:///tmp/A.HTM", "file:///tmp/page.xhtml", "file://localhost/tmp/x.html"] {
            assert!(page(file).starts_with("file:///"), "{file}");
        }
        assert_eq!(accept("crossbow://anything?x=1"), Ok(Link::App));
        assert_eq!(accept("CROSSBOW:open"), Ok(Link::App));
        for (raw, refusal) in [
            ("", Refusal::Empty),
            ("   ", Refusal::Empty),
            ("mailto:someone@example.com", Refusal::Scheme("mailto".into())),
            ("javascript:alert(1)", Refusal::Scheme("javascript".into())),
            ("data:text/html,<p>x</p>", Refusal::Scheme("data".into())),
            ("chrome://settings", Refusal::Scheme("chrome".into())),
            ("about:blank", Refusal::Scheme("about".into())),
            ("ftp://example.com/file", Refusal::Scheme("ftp".into())),
            ("file:///etc/passwd", Refusal::NotHtml),
            ("file:///tmp/notes.html.txt", Refusal::NotHtml),
            ("file://server/share/page.html", Refusal::NotHtml),
            ("example.com", Refusal::Malformed),
            ("http://", Refusal::Malformed),
            ("https://exa mple.com/", Refusal::Malformed),
            ("not a url at all", Refusal::Malformed),
        ] {
            assert_eq!(accept(raw), Err(refusal), "{raw:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL));
        assert_eq!(accept(&long), Err(Refusal::TooLong));
        for refusal in [Refusal::Empty, Refusal::TooLong, Refusal::Malformed, Refusal::Scheme("mailto".into()), Refusal::NotHtml, Refusal::NoHost] {
            assert!(!refusal.reason().is_empty());
        }
    }

    #[test]
    fn logs_name_the_scheme_and_host_only() {
        assert_eq!(redact("https://user:secret@Example.com:8443/private/path?token=abc#frag"), "https://example.com");
        assert_eq!(redact("http://127.0.0.1:9/x?y"), "http://127.0.0.1");
        assert_eq!(redact("file:///Users/someone/Secret%20Plans/index.html"), "file:");
        assert_eq!(redact("mailto:someone@example.com?subject=private"), "mailto:");
        assert_eq!(redact("javascript:alert(document.cookie)"), "javascript:");
        assert_eq!(redact("crossbow://open?url=https://example.com/private"), "crossbow:");
        assert_eq!(redact("not a url /with/path?q=1"), "unreadable");
        assert_eq!(redact(""), "unreadable");
        for raw in ["https://example.com/private?q=secret", "mailto:secret@example.com", "file:///secret.html", "x-custom:secret-payload"] {
            let line = redact(raw);
            assert!(!line.contains("secret") && !line.contains("private") && !line.contains("payload"), "{raw} → {line}");
        }
    }

    struct Fixture {
        lib: Library,
        work: String,
        personal: String,
        other: String,
        profile_b: String,
    }

    /// Personal and Work use profile A; Other uses profile B.
    fn fixture(store: Store) -> Fixture {
        let mut lib = Library::open(store).unwrap();
        lib.ensure_defaults().unwrap();
        let personal = lib.spaces_ordered()[0].0.clone();
        let profile_a = lib.spaces[&personal].profile_id.clone();
        let profile_b = lib.create_profile("B").unwrap();
        let work = lib.create_space("Work", "W", Theme::preset(1), &profile_a).unwrap();
        let other = lib.create_space("Other", "O", Theme::preset(2), &profile_b).unwrap();
        Fixture { lib, work, personal, other, profile_b }
    }

    fn rule(kind: Match, value: &str, space: &str) -> Rule {
        Rule::new(crate::new_id(), kind, value, space.into()).unwrap()
    }

    #[test]
    fn typed_rules_are_validated_and_normalized() {
        let space = || "space".to_string();
        let ok = |kind, value: &str| Rule::new("id".into(), kind, value, space()).map(|rule| rule.value);
        assert_eq!(ok(Match::UrlContains, "  github.com/Rust  "), Ok("github.com/Rust".into()));
        assert_eq!(ok(Match::HostIs, " Example.COM. "), Ok("example.com".into()));
        assert_eq!(ok(Match::HostIs, "https://www.GitHub.com/"), Ok("www.github.com".into()));
        assert_eq!(ok(Match::HostIs, "localhost"), Ok("localhost".into()));
        for (kind, value) in [
            (Match::UrlContains, ""),
            (Match::UrlContains, "   "),
            (Match::HostIs, ""),
            (Match::HostIs, "github.com/rust-lang"),
            (Match::HostIs, "https://github.com/rust-lang"),
            (Match::HostIs, "example.com?q=1"),
            (Match::HostIs, "localhost:3000"),
            (Match::HostIs, "user@example.com"),
            (Match::HostIs, "two words"),
        ] {
            assert!(Rule::new("id".into(), kind, value, space()).is_err_and(|error| !error.is_empty()), "{kind:?} {value:?}");
        }
        let long = "a".repeat(MAX_VALUE + 1);
        assert!(Rule::new("id".into(), Match::UrlContains, &long, space()).is_err());
        assert!(Rule::new("id".into(), Match::UrlContains, "x", String::new()).is_err(), "a rule needs a Space");
    }

    #[test]
    fn first_matching_rule_wins_and_unmatched_links_use_the_default() {
        let f = fixture(Store::open_in_memory().unwrap());
        let mut routing = Routing {
            rules: vec![
                rule(Match::HostIs, "github.com", &f.work),
                rule(Match::UrlContains, "GITHUB.COM/rust", &f.other),
                rule(Match::UrlContains, "docs", &f.other),
            ],
            default: Destination::MostRecent,
        };
        let route = |routing: &Routing, url: &str, recent: &str| routing.route(&f.lib, &page(url), recent).unwrap();
        // Rule order decides, not specificity.
        assert_eq!(route(&routing, "https://github.com/rust-lang/rust", &f.personal), Route { space: f.work.clone(), rule: Some(0), fallback: false });
        assert_eq!(route(&routing, "https://www.github.com/x", &f.personal).rule, Some(0), "Host is ignores www.");
        assert_eq!(route(&routing, "https://GitHub.com./x", &f.personal).rule, Some(0), "and case and a trailing dot");
        assert_eq!(route(&routing, "https://gist.github.com/rust", &f.personal).rule, Some(1), "Host is does not take other subdomains");
        assert_eq!(route(&routing, "https://notgithub.com/x", &f.personal).rule, None);
        assert_eq!(route(&routing, "https://example.com/docs/a", &f.personal), Route { space: f.other.clone(), rule: Some(2), fallback: false });
        routing.rules.swap(0, 1);
        assert_eq!(route(&routing, "https://github.com/rust-lang/rust", &f.personal).rule, Some(0), "URL contains is case-insensitive and now first");
        assert_eq!(route(&routing, "https://github.com/rust-lang/rust", &f.personal).space, f.other);
        // No rule: the most recent Space, or the chosen default.
        assert_eq!(route(&routing, "https://example.com/", &f.work), Route { space: f.work.clone(), rule: None, fallback: false });
        assert_eq!(route(&routing, "https://example.com/", &f.personal).space, f.personal);
        routing.default = Destination::Space(f.other.clone());
        assert_eq!(route(&routing, "https://example.com/", &f.personal), Route { space: f.other.clone(), rule: None, fallback: false });
        assert_eq!(routing.default_space(&f.lib, &f.personal), Some((f.other.clone(), false)));
        // Local HTML files only match URL contains.
        assert_eq!(route(&routing, "file:///tmp/docs/index.html", &f.personal).rule, Some(2));
    }

    #[test]
    fn a_deleted_space_falls_back_to_the_default_and_then_to_the_most_recent() {
        let mut f = fixture(Store::open_in_memory().unwrap());
        let gone = f.lib.create_space("Gone", "G", Theme::preset(3), &f.profile_b).unwrap();
        let routing = Routing {
            rules: vec![rule(Match::HostIs, "example.com", &gone), rule(Match::UrlContains, "example", &f.other)],
            default: Destination::Space(f.work.clone()),
        };
        let url = page("https://example.com/");
        assert_eq!(routing.route(&f.lib, &url, &f.personal).unwrap(), Route { space: gone.clone(), rule: Some(0), fallback: false });
        f.lib.delete_space(&gone).unwrap();
        assert!(!usable_space(&f.lib, &gone));
        // The first match still decides; its missing Space never passes the link to the next rule.
        assert_eq!(routing.route(&f.lib, &url, &f.personal).unwrap(), Route { space: f.work.clone(), rule: Some(0), fallback: true });
        f.lib.delete_space(&f.work).unwrap();
        assert_eq!(routing.default_space(&f.lib, &f.personal), Some((f.personal.clone(), true)));
        assert_eq!(routing.route(&f.lib, &url, &f.personal).unwrap(), Route { space: f.personal.clone(), rule: Some(0), fallback: true });
        // A most recent Space that is gone too uses the first usable one in sidebar order.
        let most_recent = Routing::default();
        assert_eq!(most_recent.route(&f.lib, &url, "deleted-space").unwrap(), Route { space: f.lib.spaces_ordered()[0].0.clone(), rule: None, fallback: true });
        // A Space whose profile is gone cannot take links.
        f.lib.profiles.remove(&f.profile_b);
        assert!(!usable_space(&f.lib, &f.other));
        let to_other = Routing { rules: vec![rule(Match::UrlContains, "example", &f.other)], default: Destination::Space(f.other.clone()) };
        assert_eq!(to_other.route(&f.lib, &url, &f.personal).unwrap(), Route { space: f.personal.clone(), rule: Some(0), fallback: true });
        // With no usable Space at all, nothing opens.
        for (id, _) in f.lib.spaces_ordered() { f.lib.spaces.remove(&id); }
        assert_eq!(to_other.route(&f.lib, &url, &f.personal), None);
    }

    #[test]
    fn rules_survive_reopening_and_never_enter_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.sqlite");
        let f = fixture(Store::open(&path).unwrap());
        let before = f.lib.store.dirty_count().unwrap();
        let mut routing = LinkRouting::load(&f.lib.store);
        assert_eq!(routing.saved(), &Routing::default());
        assert!(routing.notice.is_none());
        let candidate = Routing {
            rules: vec![rule(Match::HostIs, "github.com", &f.work), rule(Match::UrlContains, "zoom.us/j/", &f.other)],
            default: Destination::Space(f.personal.clone()),
        };
        routing.save(&f.lib.store, candidate.clone()).unwrap();
        assert_eq!(routing.saved(), &candidate);
        drop(f);
        let store = Store::open(&path).unwrap();
        let reopened = LinkRouting::load(&store);
        assert_eq!(reopened.saved(), &candidate);
        assert!(reopened.notice.is_none());
        assert_eq!(store.dirty_count().unwrap(), before, "no sync change is queued");
        assert!(store.all_entities().unwrap().iter().all(|row| !row.kind.contains("link")));
    }

    #[test]
    fn a_failed_write_keeps_the_previous_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.sqlite");
        let f = fixture(Store::open(&path).unwrap());
        let mut routing = LinkRouting::load(&f.lib.store);
        let first = Routing { rules: vec![rule(Match::HostIs, "github.com", &f.work)], default: Destination::MostRecent };
        routing.save(&f.lib.store, first.clone()).unwrap();
        let raw = f.lib.store.kv_get(KEY).unwrap();
        let read_only = {
            let path = path.to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
            Store::open(std::path::Path::new(&format!("file:{path}?mode=ro"))).unwrap()
        };
        let second = Routing { rules: vec![], default: Destination::Space(f.other.clone()) };
        assert!(routing.save(&read_only, second.clone()).is_err_and(|error| !error.is_empty()));
        assert_eq!(routing.saved(), &first, "the rules in use stay");
        assert_eq!(f.lib.store.kv_get(KEY).unwrap(), raw);
        routing.save(&f.lib.store, second.clone()).unwrap();
        assert_eq!(LinkRouting::load(&Store::open(&path).unwrap()).saved(), &second);
    }

    #[test]
    fn malformed_rows_are_ignored_and_bad_records_stay_untouched() {
        let f = fixture(Store::open_in_memory().unwrap());
        let row = |id: serde_json::Value, kind: &str, value: &str, space: &str| serde_json::json!({ "id": id, "match": kind, "value": value, "space": space });
        let raw = serde_json::json!({ "version": 1, "default": { "kind": "space", "space": f.other }, "rules": [
            row("a".into(), "host-is", "github.com", &f.work),
            row("a".into(), "url-contains", "duplicate id", &f.work),
            row("b".into(), "regex", ".*", &f.work),
            row("c".into(), "host-is", "github.com/path", &f.work),
            row("d".into(), "url-contains", "", &f.work),
            row(serde_json::Value::Null, "url-contains", "no id", &f.work),
            row("e".into(), "url-contains", "no space", ""),
            row("f".into(), "url-contains", "deleted space", "deleted-space"),
            serde_json::json!("not a row"),
            row("g".into(), "url-contains", "zoom.us", &f.other),
        ]}).to_string();
        f.lib.store.kv_set(KEY, &raw).unwrap();
        let loaded = LinkRouting::load(&f.lib.store);
        let saved = loaded.saved();
        assert_eq!(saved.rules.iter().map(|rule| rule.id.as_str()).collect::<Vec<_>>(), ["a", "f", "g"], "a rule for a deleted Space is kept");
        assert_eq!(saved.rules[0], Rule { id: "a".into(), kind: Match::HostIs, value: "github.com".into(), space: f.work.clone() });
        assert_eq!(saved.default, Destination::Space(f.other.clone()));
        assert!(loaded.notice.as_deref().is_some_and(|notice| notice.contains("ignored")));
        assert_eq!(f.lib.store.kv_get(KEY).unwrap().as_deref(), Some(raw.as_str()), "loading never rewrites the record");

        let bad_default = serde_json::json!({ "version": 1, "default": { "kind": "little-arc" }, "rules": [] }).to_string();
        for (raw, newer) in [("not json", false), ("{\"version\":2,\"rules\":[]}", true), ("{\"version\":1}", false), ("[]", false), (bad_default.as_str(), false)] {
            f.lib.store.kv_set(KEY, raw).unwrap();
            let loaded = LinkRouting::load(&f.lib.store);
            assert_eq!(loaded.saved(), &Routing::default(), "{raw}");
            assert!(loaded.notice.is_some(), "{raw}");
            assert_eq!(loaded.notice.as_deref().is_some_and(|notice| notice.contains("newer version")), newer, "{raw}");
            assert_eq!(f.lib.store.kv_get(KEY).unwrap().as_deref(), Some(raw));
        }
    }
}
