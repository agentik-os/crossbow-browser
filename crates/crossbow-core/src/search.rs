//! Command bar: turning typed text into a URL, and ranking suggestions from open
//! tabs, saved items, history and actions.

use crate::model::SearchEngine;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use serde::{Deserialize, Serialize};

/// A search engine added to one profile on this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomEngine {
    pub id: String,
    pub name: String,
    /// Typing `keyword text` in the command bar searches this engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// An http(s) address with `%s` where the search text goes.
    pub search_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineField {
    Name,
    Keyword,
    SearchUrl,
    SuggestUrl,
}

impl EngineField {
    pub fn label(self) -> &'static str {
        match self {
            EngineField::Name => "Name",
            EngineField::Keyword => "Keyword",
            EngineField::SearchUrl => "Search URL",
            EngineField::SuggestUrl => "Suggestion URL",
        }
    }
}

/// Why an engine was refused: the field, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub field: EngineField,
    pub reason: String,
}

pub const ENGINE_NAME_MAX: usize = 60;
pub const KEYWORD_MAX: usize = 32;
pub const TEMPLATE_MAX: usize = 2048;

impl CustomEngine {
    /// Trimmed, with blank optional fields removed.
    pub fn normalized(self) -> Self {
        let optional = |value: Option<String>| value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        CustomEngine {
            id: self.id,
            name: self.name.trim().to_string(),
            keyword: optional(self.keyword),
            search_url: self.search_url.trim().to_string(),
            suggest_url: optional(self.suggest_url),
        }
    }

    /// Check a normalized engine against the profile's other engines.
    /// `reserved` lists words the command bar already gives a meaning to.
    pub fn validate(&self, others: &[CustomEngine], reserved: &[&str]) -> Result<(), EngineError> {
        let refuse = |field, reason: String| Err(EngineError { field, reason });
        let others = || others.iter().filter(|other| other.id != self.id);
        let name = self.name.trim();
        if name.is_empty() { return refuse(EngineField::Name, "Enter a name.".into()); }
        if name.chars().count() > ENGINE_NAME_MAX { return refuse(EngineField::Name, format!("Use at most {ENGINE_NAME_MAX} characters.")); }
        let lower = name.to_lowercase();
        if SearchEngine::ALL.iter().any(|engine| engine.name().to_lowercase() == lower) || others().any(|other| other.name.trim().to_lowercase() == lower) {
            return refuse(EngineField::Name, format!("“{name}” is already a search engine. Choose another name."));
        }
        if let Some(keyword) = self.keyword.as_deref().map(str::trim).filter(|keyword| !keyword.is_empty()) {
            if keyword.contains(char::is_whitespace) { return refuse(EngineField::Keyword, "Use one word with no spaces.".into()); }
            if keyword.chars().count() > KEYWORD_MAX { return refuse(EngineField::Keyword, format!("Use at most {KEYWORD_MAX} characters.")); }
            let lower = keyword.to_lowercase();
            if let Some(other) = others().find(|other| other.keyword.as_deref().is_some_and(|word| word.trim().to_lowercase() == lower)) {
                return refuse(EngineField::Keyword, format!("“{keyword}” is already the keyword for “{}”.", other.name));
            }
            if reserved.iter().any(|word| word.to_lowercase() == lower) {
                return refuse(EngineField::Keyword, format!("“{keyword}” starts a command-bar command. Choose another keyword."));
            }
        }
        check_template(EngineField::SearchUrl, &self.search_url)?;
        match self.suggest_url.as_deref().map(str::trim).filter(|template| !template.is_empty()) {
            Some(template) => check_template(EngineField::SuggestUrl, template),
            None => Ok(()),
        }
    }

    pub fn search_url(&self, query: &str) -> String {
        fill(&self.search_url, query)
    }

    pub fn suggest_url(&self, query: &str) -> Option<String> {
        self.suggest_url.as_deref().map(|template| fill(template, query))
    }
}

fn check_template(field: EngineField, template: &str) -> Result<(), EngineError> {
    let refuse = |reason: &str| Err(EngineError { field, reason: reason.into() });
    let template = template.trim();
    if template.is_empty() { return refuse("Enter an address with %s where the search text goes."); }
    if template.chars().count() > TEMPLATE_MAX { return refuse("Use at most 2048 characters."); }
    if !template.contains("%s") { return refuse("Include %s where the search text goes."); }
    let Ok(url) = url::Url::parse(&template.replace("%s", "text")) else {
        return refuse("Enter a full address, such as https://example.com/search?q=%s.");
    };
    if !matches!(url.scheme(), "http" | "https") { return refuse("Use an http or https address."); }
    if url.host_str().is_none_or(str::is_empty) { return refuse("Enter a full address, such as https://example.com/search?q=%s."); }
    Ok(())
}

/// Put `query` where a template has `%s`. Spaces become `%20`, which means
/// the same in a path and in a query string.
fn fill(template: &str, query: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    template.replace("%s", &encoded.replace('+', "%20"))
}

/// The engine a search goes to: built in, or added to the profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Engine {
    BuiltIn(SearchEngine),
    Custom(CustomEngine),
}

impl From<SearchEngine> for Engine {
    fn from(engine: SearchEngine) -> Self {
        Engine::BuiltIn(engine)
    }
}

impl Engine {
    pub fn name(&self) -> &str {
        match self {
            Engine::BuiltIn(engine) => engine.name(),
            Engine::Custom(engine) => &engine.name,
        }
    }

    pub fn search_url(&self, query: &str) -> String {
        match self {
            Engine::BuiltIn(engine) => engine.search_url(query),
            Engine::Custom(engine) => engine.search_url(query),
        }
    }

    /// Where suggestions for `query` come from, if the engine offers them.
    pub fn suggest_url(&self, query: &str) -> Option<String> {
        match self {
            Engine::BuiltIn(engine) => engine.suggest_url(query),
            Engine::Custom(engine) => engine.suggest_url(query),
        }
    }
}

/// `keyword text`: the engine with that keyword and the text to search.
pub fn keyword_search<'e, 'q>(query: &'q str, engines: &'e [CustomEngine]) -> Option<(&'e CustomEngine, &'q str)> {
    let (word, text) = query.trim_start().split_once(char::is_whitespace)?;
    let text = text.trim();
    if text.is_empty() { return None; }
    let word = word.to_lowercase();
    let engine = engines.iter().find(|engine| engine.keyword.as_deref().is_some_and(|keyword| keyword.to_lowercase() == word))?;
    Some((engine, text))
}

/// Typed text must be at least this long before Tab offers an engine.
pub const TAB_MIN_CHARS: usize = 2;

/// The engine Tab scopes a search to when `typed` begins its name or keyword,
/// ignoring case: an exact keyword first, then built-in engines, then added
/// ones in order.
pub fn tab_engine(typed: &str, engines: &[CustomEngine]) -> Option<Engine> {
    let typed = typed.trim().to_lowercase();
    if typed.chars().count() < TAB_MIN_CHARS { return None; }
    let keyword = |engine: &CustomEngine| engine.keyword.as_deref().map(str::to_lowercase);
    let begins = |text: &str| text.to_lowercase().starts_with(&typed);
    if let Some(engine) = engines.iter().find(|engine| keyword(engine).as_deref() == Some(typed.as_str())) {
        return Some(Engine::Custom(engine.clone()));
    }
    SearchEngine::ALL.into_iter().find(|engine| begins(engine.name())).map(Engine::BuiltIn)
        .or_else(|| engines.iter().find(|engine| begins(&engine.name) || keyword(engine).is_some_and(|word| begins(&word))).cloned().map(Engine::Custom))
}

/// Sites each profile starts with: (id, name, search address). They have no
/// keyword or suggestion address, and are edited or deleted like added engines.
pub const SITES: [(&str, &str, &str); 5] = [
    ("site-wikipedia", "Wikipedia", "https://en.wikipedia.org/w/index.php?search=%s"),
    ("site-youtube", "YouTube", "https://www.youtube.com/results?search_query=%s"),
    ("site-github", "GitHub", "https://github.com/search?q=%s"),
    ("site-mdn", "MDN", "https://developer.mozilla.org/en-US/search?q=%s"),
    ("site-google-maps", "Google Maps", "https://www.google.com/maps/search/?api=1&query=%s"),
];

pub fn default_sites() -> Vec<CustomEngine> {
    SITES.iter().map(|(id, name, search_url)| CustomEngine { id: (*id).into(), name: (*name).into(), keyword: None, search_url: (*search_url).into(), suggest_url: None }).collect()
}

/// Interpret command-bar input as a URL when it clearly is one.
pub fn input_to_url(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() || s.contains(char::is_whitespace) {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    for scheme in ["http://", "https://", "file://", "about:", "chrome://", "data:", "view-source:"] {
        if lower.starts_with(scheme) {
            return url::Url::parse(s).ok().map(|u| u.to_string());
        }
    }
    let host_part = s.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_part.rsplit_once(':').map(|(h, _)| h).unwrap_or(host_part);
    let is_local = host == "localhost" || host.parse::<std::net::Ipv4Addr>().is_ok();
    let looks_like_domain = host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.rsplit('.').next().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !(is_local || looks_like_domain) {
        return None;
    }
    let scheme = if is_local { "http" } else { "https" };
    url::Url::parse(&format!("{scheme}://{s}")).ok().map(|u| u.to_string())
}

/// URL to open for command-bar input: the URL itself, or a search.
pub fn resolve_input(input: &str, engine: &Engine) -> String {
    input_to_url(input).unwrap_or_else(|| engine.search_url(input.trim()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum SuggestionKind {
    /// Go to the typed URL or search for the text.
    Navigate,
    /// Switch to an already-open tab.
    Tab { tab_id: String, space_id: String },
    /// Open a pinned item or favorite.
    Item { item_id: String },
    History,
    /// A search the engine suggested for the typed text.
    EngineSuggestion,
    /// Run a named command (the UI owns the action ids).
    Action { action_id: &'static str },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    pub title: String,
    pub subtitle: String,
    pub url: Option<String>,
    pub score: u32,
}

pub struct Candidate {
    pub kind: SuggestionKind,
    pub title: String,
    pub url: Option<String>,
    /// Added to the fuzzy score (e.g. open tabs rank above history).
    pub boost: u32,
}

/// Rank candidates against the query. The navigate/search row is always first
/// unless the top match is an exact URL hit.
pub fn rank(query: &str, candidates: &[Candidate], engine: &Engine, limit: usize) -> Vec<Suggestion> {
    let q = query.trim();
    let mut out = Vec::new();
    if !q.is_empty() {
        let (title, subtitle, url) = match input_to_url(q) {
            Some(u) => (q.to_string(), "Open URL".to_string(), u),
            None => (q.to_string(), format!("Search {}", engine.name()), engine.search_url(q)),
        };
        out.push(Suggestion { kind: SuggestionKind::Navigate, title, subtitle, url: Some(url), score: u32::MAX });
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<Suggestion> = candidates
        .iter()
        .filter_map(|c| {
            let hay = match &c.url {
                Some(u) => format!("{} {}", c.title, display_url(u)),
                None => c.title.clone(),
            };
            let score = if q.is_empty() {
                Some(0)
            } else {
                pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher)
            }?;
            Some(Suggestion {
                kind: c.kind.clone(),
                title: c.title.clone(),
                subtitle: c.url.as_deref().map(display_url).unwrap_or_default(),
                url: c.url.clone(),
                score: score + c.boost,
            })
        })
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score));
    let mut seen = std::collections::HashSet::new();
    for s in scored {
        if let Some(u) = &s.url {
            // One row per URL, keeping the best-ranked source (tab > item > history).
            if !seen.insert(u.clone()) {
                continue;
            }
        }
        out.push(s);
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Order ranked rows for a profile with suggestions on. The typed-text row and
/// an exact command keep their places; local rows that match the typed text at
/// the start of a word come next, then the engine's suggestions, then other
/// local rows. Local rows keep this order while a reply is awaited.
pub fn blend(mut results: Vec<Suggestion>, query: &str, suggestions: &[String], engine: &Engine, limit: usize) -> Vec<Suggestion> {
    let head = results.iter().take_while(|row| row.kind == SuggestionKind::Navigate || row.score >= u32::MAX - 1).count();
    let local = results.split_off(head);
    let (matched, other): (Vec<_>, Vec<_>) = local.into_iter().partition(|row| word_start(&row.title, query) || word_start(&row.subtitle, query));
    let typed = results.first().map(|row| row.title.to_lowercase());
    let mut suggested = Vec::new();
    for text in suggestions {
        let url = engine.search_url(text);
        let repeated = typed.as_deref() == Some(text.to_lowercase().as_str()) || query.trim().eq_ignore_ascii_case(text)
            || results.iter().chain(&matched).chain(&suggested).any(|row: &Suggestion| row.url.as_deref() == Some(url.as_str()));
        if !repeated {
            suggested.push(Suggestion { kind: SuggestionKind::EngineSuggestion, title: text.clone(), subtitle: format!("Search {}", engine.name()), url: Some(url), score: 0 });
        }
    }
    results.extend(matched);
    results.extend(suggested);
    results.extend(other);
    results.truncate(limit);
    results
}

/// Whether `text` contains `query` at the start of a word, ignoring case.
fn word_start(text: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() { return false; }
    let text = text.to_lowercase();
    text.match_indices(&query).any(|(at, _)| text[..at].chars().next_back().is_none_or(|before| !before.is_alphanumeric()))
}

/// `https://www.example.com/path?q` → `example.com/path?q`
pub fn display_url(url: &str) -> String {
    let s = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    let s = s.strip_prefix("www.").unwrap_or(s);
    s.trim_end_matches('/').to_string()
}

/// Host of a URL without `www.`, for favicons and boosts.
pub fn host_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let h = u.host_str()?;
    Some(h.strip_prefix("www.").unwrap_or(h).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_detection() {
        assert_eq!(input_to_url("github.com").as_deref(), Some("https://github.com/"));
        assert_eq!(input_to_url("localhost:3000/x").as_deref(), Some("http://localhost:3000/x"));
        assert_eq!(input_to_url("https://a.b/c?d=1").as_deref(), Some("https://a.b/c?d=1"));
        assert_eq!(input_to_url("192.168.1.1").as_deref(), Some("http://192.168.1.1/"));
        assert_eq!(input_to_url("rust lang"), None);
        assert_eq!(input_to_url("hello"), None);
        assert_eq!(input_to_url("v1.2"), None);
        assert!(resolve_input("rust lang", &SearchEngine::DuckDuckGo.into()).starts_with("https://duckduckgo.com/?q=rust+lang"));
    }

    #[test]
    fn ranking_prefers_tabs_and_dedupes() {
        let cands = vec![
            Candidate {
                kind: SuggestionKind::History,
                title: "GitHub".into(),
                url: Some("https://github.com/".into()),
                boost: 0,
            },
            Candidate {
                kind: SuggestionKind::Tab { tab_id: "t".into(), space_id: "s".into() },
                title: "GitHub".into(),
                url: Some("https://github.com/".into()),
                boost: 1000,
            },
            Candidate { kind: SuggestionKind::Action { action_id: "new_space" }, title: "New Space".into(), url: None, boost: 0 },
        ];
        let r = rank("git", &cands, &SearchEngine::Google.into(), 8);
        assert_eq!(r[0].kind, SuggestionKind::Navigate);
        assert!(matches!(r[1].kind, SuggestionKind::Tab { .. }));
        assert_eq!(r.iter().filter(|s| s.url.as_deref() == Some("https://github.com/")).count(), 1);
        let r = rank("new sp", &cands, &SearchEngine::Google.into(), 8);
        assert!(r.iter().any(|s| s.kind == SuggestionKind::Action { action_id: "new_space" }));
    }

    fn engine(id: &str, name: &str, keyword: Option<&str>, search: &str, suggest: Option<&str>) -> CustomEngine {
        CustomEngine { id: id.into(), name: name.into(), keyword: keyword.map(Into::into), search_url: search.into(), suggest_url: suggest.map(Into::into) }
    }

    #[test]
    fn custom_engines_are_refused_with_the_field_and_reason() {
        let saved = [engine("a", "Intranet", Some("in"), "https://search.example.test/?q=%s", None)];
        let refused = |candidate: CustomEngine| candidate.normalized().validate(&saved, &["new", "find"]).unwrap_err();
        let good = || engine("b", "Docs", Some("docs"), "https://docs.example.test/find?q=%s", Some("https://docs.example.test/suggest?q=%s"));
        assert_eq!(good().normalized().validate(&saved, &["new", "find"]), Ok(()));
        // Editing an engine keeps its own name and keyword.
        assert_eq!(saved[0].clone().validate(&saved, &[]), Ok(()));

        let cases = [
            (CustomEngine { name: "  ".into(), ..good() }, EngineField::Name, "Enter a name."),
            (CustomEngine { name: "intranet".into(), ..good() }, EngineField::Name, "“intranet” is already a search engine. Choose another name."),
            (CustomEngine { name: "Google".into(), ..good() }, EngineField::Name, "“Google” is already a search engine. Choose another name."),
            (CustomEngine { name: "x".repeat(61), ..good() }, EngineField::Name, "Use at most 60 characters."),
            (CustomEngine { keyword: Some("IN".into()), ..good() }, EngineField::Keyword, "“IN” is already the keyword for “Intranet”."),
            (CustomEngine { keyword: Some("go now".into()), ..good() }, EngineField::Keyword, "Use one word with no spaces."),
            (CustomEngine { keyword: Some("Find".into()), ..good() }, EngineField::Keyword, "“Find” starts a command-bar command. Choose another keyword."),
            (CustomEngine { keyword: Some("k".repeat(33)), ..good() }, EngineField::Keyword, "Use at most 32 characters."),
            (CustomEngine { search_url: "".into(), ..good() }, EngineField::SearchUrl, "Enter an address with %s where the search text goes."),
            (CustomEngine { search_url: "https://docs.example.test/find".into(), ..good() }, EngineField::SearchUrl, "Include %s where the search text goes."),
            (CustomEngine { search_url: "ftp://docs.example.test/%s".into(), ..good() }, EngineField::SearchUrl, "Use an http or https address."),
            (CustomEngine { search_url: "javascript:alert(%s)".into(), ..good() }, EngineField::SearchUrl, "Use an http or https address."),
            (CustomEngine { search_url: "docs.example.test/?q=%s".into(), ..good() }, EngineField::SearchUrl, "Enter a full address, such as https://example.com/search?q=%s."),
            (CustomEngine { search_url: format!("https://docs.example.test/?q=%s&pad={}", "p".repeat(2048)), ..good() }, EngineField::SearchUrl, "Use at most 2048 characters."),
            (CustomEngine { suggest_url: Some("https://docs.example.test/suggest".into()), ..good() }, EngineField::SuggestUrl, "Include %s where the search text goes."),
            (CustomEngine { suggest_url: Some("file:///tmp/%s".into()), ..good() }, EngineField::SuggestUrl, "Use an http or https address."),
        ];
        for (candidate, field, reason) in cases {
            assert_eq!(refused(candidate), EngineError { field, reason: reason.into() });
        }
        // Blank optional fields are dropped, not refused.
        let blank = CustomEngine { keyword: Some("  ".into()), suggest_url: Some(" ".into()), ..good() }.normalized();
        assert_eq!((blank.keyword.as_deref(), blank.suggest_url.as_deref()), (None, None));
        assert_eq!(blank.validate(&saved, &[]), Ok(()));
    }

    #[test]
    fn templates_put_encoded_text_where_percent_s_is() {
        let docs = engine("b", "Docs", None, "https://docs.example.test/%s/find?q=%s", Some("http://127.0.0.1:9/s?q=%s&type=list"));
        assert_eq!(docs.search_url("a b&c/é+"), "https://docs.example.test/a%20b%26c%2F%C3%A9%2B/find?q=a%20b%26c%2F%C3%A9%2B");
        assert_eq!(docs.suggest_url("rust lang").as_deref(), Some("http://127.0.0.1:9/s?q=rust%20lang&type=list"));
        let engine = Engine::Custom(docs);
        assert_eq!(engine.name(), "Docs");
        assert_eq!(resolve_input("example.com", &engine), "https://example.com/");
        assert_eq!(resolve_input(" rust ", &engine), "https://docs.example.test/rust/find?q=rust");
    }

    #[test]
    fn tab_offers_the_engine_whose_name_or_keyword_the_text_begins() {
        let added = [
            engine("gh", "GitHub", Some("gh"), "https://github.com/search?q=%s", None),
            engine("docs", "Docs", Some("du"), "https://docs.example.test/?q=%s", None),
            engine("wiki", "Team Wiki", Some("teamwiki"), "https://wiki.example.test/?q=%s", None),
        ];
        let offered = |typed: &str| tab_engine(typed, &added).map(|engine| engine.name().to_string());
        for (typed, name) in [("gi", "GitHub"), ("GITH", "GitHub"), ("gh", "GitHub"), (" github ", "GitHub"), ("go", "Google"), ("googl", "Google"),
            ("du", "Docs"), ("duc", "DuckDuckGo"), ("Bi", "Bing"), ("te", "Team Wiki"), ("team w", "Team Wiki"), ("teamw", "Team Wiki")] {
            assert_eq!(offered(typed).as_deref(), Some(name), "{typed:?}");
        }
        for typed in ["", " ", "g", " G ", "gix", "github stars", "wiki", "docs.example.test"] {
            assert_eq!(offered(typed), None, "{typed:?}");
        }
        assert_eq!(tab_engine("go", &[]), Some(Engine::BuiltIn(SearchEngine::Google)));
        assert_eq!(tab_engine("gi", &[]), None);
    }

    #[test]
    fn default_sites_are_valid_and_escape_the_search_into_percent_s() {
        let sites = default_sites();
        let shown: Vec<_> = sites.iter().map(|site| (site.id.as_str(), site.name.as_str(), site.search_url.as_str())).collect();
        assert_eq!(shown, SITES);
        assert_eq!(shown, [
            ("site-wikipedia", "Wikipedia", "https://en.wikipedia.org/w/index.php?search=%s"),
            ("site-youtube", "YouTube", "https://www.youtube.com/results?search_query=%s"),
            ("site-github", "GitHub", "https://github.com/search?q=%s"),
            ("site-mdn", "MDN", "https://developer.mozilla.org/en-US/search?q=%s"),
            ("site-google-maps", "Google Maps", "https://www.google.com/maps/search/?api=1&query=%s"),
        ]);
        for site in &sites {
            assert_eq!(site.clone().normalized(), *site);
            assert_eq!(site.validate(&sites, &[]), Ok(()), "{}", site.name);
            assert!(site.keyword.is_none() && site.suggest_url.is_none(), "{} has no keyword and sends nothing while typing", site.name);
            assert!(site.search_url.starts_with("https://"));
        }
        let maps = Engine::Custom(sites[4].clone());
        let typed = "café & crêpes #1 100% a+b";
        let url = maps.search_url(typed);
        assert_eq!(url, "https://www.google.com/maps/search/?api=1&query=caf%C3%A9%20%26%20cr%C3%AApes%20%231%20100%25%20a%2Bb");
        let query = |url: &str| url::Url::parse(url).unwrap().query_pairs().find(|(name, _)| name == "query").map(|(_, value)| value.into_owned());
        assert_eq!(query(&url).as_deref(), Some(typed), "the site receives exactly what was typed");
        assert_eq!(url::Url::parse(&url).unwrap().fragment(), None, "# stays in the search");
        let long = format!("{} end", "ü".repeat(3000));
        let url = maps.search_url(&long);
        assert!(url.ends_with("%20end"), "long text is not cut");
        assert_eq!(query(&url), Some(long));
    }

    #[test]
    fn built_in_engines_name_their_suggestion_endpoints() {
        let expected = [
            (SearchEngine::Google, Some("https://www.google.com/complete/search?client=firefox&q=rust+lang")),
            (SearchEngine::DuckDuckGo, Some("https://duckduckgo.com/ac/?q=rust+lang&type=list")),
            (SearchEngine::Bing, Some("https://www.bing.com/osjson.aspx?query=rust+lang")),
            (SearchEngine::Kagi, Some("https://kagi.com/api/autosuggest?q=rust+lang")),
            (SearchEngine::Ecosia, Some("https://ac.ecosia.org/autocomplete?q=rust+lang&type=list")),
            (SearchEngine::Perplexity, None),
        ];
        for (engine, url) in expected {
            assert_eq!(Engine::from(engine).suggest_url("rust lang").as_deref(), url, "{}", engine.name());
        }
    }

    #[test]
    fn keywords_need_a_word_then_text() {
        let engines = [engine("a", "GitHub", Some("gh"), "https://github.com/search?q=%s", None), engine("b", "Plain", None, "https://plain.example.test/?q=%s", None)];
        let found = |query: &str| keyword_search(query, &engines).map(|(engine, text)| (engine.name.clone(), text.to_string()));
        assert_eq!(found("gh serde"), Some(("GitHub".into(), "serde".into())));
        assert_eq!(found("  GH   rust lang  "), Some(("GitHub".into(), "rust lang".into())));
        assert_eq!(found("gh"), None);
        assert_eq!(found("gh   "), None);
        assert_eq!(found("ghx serde"), None);
        assert_eq!(found("serde gh"), None);
        assert_eq!(keyword_search("gh serde", &[]), None);
    }

    #[test]
    fn suggestions_follow_word_matches_and_precede_other_local_rows() {
        let cands = vec![
            Candidate { kind: SuggestionKind::History, title: "Trusty tools".into(), url: Some("https://trusty.example.test/".into()), boost: 400 },
            Candidate { kind: SuggestionKind::Tab { tab_id: "t".into(), space_id: "s".into() }, title: "The Rust Book".into(), url: Some("https://book.example.test/".into()), boost: 0 },
        ];
        let engine = Engine::from(SearchEngine::DuckDuckGo);
        let ranked = rank("rust", &cands, &engine, 9);
        assert_eq!(ranked.iter().map(|row| row.title.as_str()).collect::<Vec<_>>(), ["rust", "Trusty tools", "The Rust Book"], "fuzzy ranking alone");
        let suggestions = ["rust lang".to_string(), "RUST".into(), "rustup".into()];
        let blended = blend(ranked.clone(), "rust", &suggestions, &engine, 9);
        let rows: Vec<_> = blended.iter().map(|row| (row.title.as_str(), row.kind.clone())).collect();
        assert_eq!(rows, [
            ("rust", SuggestionKind::Navigate),
            ("The Rust Book", SuggestionKind::Tab { tab_id: "t".into(), space_id: "s".into() }),
            ("rust lang", SuggestionKind::EngineSuggestion),
            ("rustup", SuggestionKind::EngineSuggestion),
            ("Trusty tools", SuggestionKind::History),
        ]);
        assert_eq!(blended[2].subtitle, "Search DuckDuckGo");
        assert_eq!(blended[2].url, Some(SearchEngine::DuckDuckGo.search_url("rust lang")));
        // The limit drops other local rows first, then suggestions, never the typed text.
        let short: Vec<_> = blend(ranked.clone(), "rust", &suggestions, &engine, 3).into_iter().map(|row| row.title).collect();
        assert_eq!(short, ["rust", "The Rust Book", "rust lang"]);
        // Before a reply, local rows already sit in their final order, so the
        // reply only inserts rows and never reorders what is shown.
        let waiting = blend(ranked.clone(), "rust", &[], &engine, 9);
        let local: Vec<_> = blended.iter().filter(|row| row.kind != SuggestionKind::EngineSuggestion).cloned().collect();
        assert_eq!(waiting, local);
        // An exact command keeps its place right after the typed text.
        let mut with_command = ranked.clone();
        with_command.insert(1, Suggestion { kind: SuggestionKind::Action { action_id: "find" }, title: "Find on Page".into(), subtitle: String::new(), url: None, score: u32::MAX - 1 });
        let blended = blend(with_command, "rust", &suggestions, &engine, 9);
        assert_eq!(blended[1].kind, SuggestionKind::Action { action_id: "find" });
        assert_eq!(blended[3].kind, SuggestionKind::EngineSuggestion);
    }

    #[test]
    fn display_urls() {
        assert_eq!(display_url("https://www.example.com/"), "example.com");
        assert_eq!(host_of("https://www.example.com/a").as_deref(), Some("example.com"));
    }
}
