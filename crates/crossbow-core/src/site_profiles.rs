//! Sites that always open in a chosen profile, as Arc's "open in profile".
//! A rule names one site, an exact host or a host with its subdomains, and
//! the profile whose Space its new tabs open in. Rules are a device-local
//! preference in the store's key/value table, so they never enter account
//! sync; a rule whose profile is missing here routes nothing.
//!
//! There is no public-suffix list without the network, so a site is never
//! widened to its registrable domain: "and subdomains" is chosen explicitly.

use crate::library::Library;
use crate::link_routing::usable_space;
use crate::store::Store;
use url::{Host, Url};

pub const KEY: &str = "site_profiles";
const VERSION: u64 = 1;
/// The longest host name DNS allows.
const MAX_HOST: usize = 253;
const MAX_LABEL: usize = 63;
/// The longest text accepted as a site, a pasted link included.
const MAX_INPUT: usize = 2048;

const UNREADABLE: &str = "Saved site profiles could not be read, so sites open where you are. Changing one here saves over them.";
const NEWER: &str = "Site profiles were saved by a newer version of Crossbow, so sites open where you are. Changing one here replaces them.";
const PARTLY: &str = "Some saved site profiles could not be read, so they were left out. Changing one here saves the list shown.";

/// Lowercase, without a trailing dot or a leading `www.`.
fn bare(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match host.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest.to_owned(),
        _ => host,
    }
}

/// A name of dot-separated labels, each 1–63 letters, digits, `-` or `_`,
/// neither starting nor ending with `-`. One label only for localhost.
fn valid_domain(host: &str) -> bool {
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty() && label.len() <= MAX_LABEL && !label.starts_with('-') && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    });
    !host.is_empty() && host.len() <= MAX_HOST && labels_ok && (host.contains('.') || host == "localhost")
}

fn is_localhost(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    /// Lowercase ASCII (punycode for international names), without a
    /// trailing dot or a leading `www.`; an IPv6 address keeps its brackets.
    pub host: String,
    /// The site's subdomains too. Never for an IP address.
    pub subdomains: bool,
}

/// Why typed text is not a site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteError {
    Empty,
    Invalid,
    /// An IP address or this computer: added only when asked again.
    Local { host: String, ip: bool },
}

impl SiteError {
    pub fn message(&self) -> String {
        match self {
            SiteError::Empty => "Enter a site, such as example.com.".into(),
            SiteError::Invalid => "Enter a site name, such as example.com, or paste a link to it.".into(),
            SiteError::Local { host, ip: true } => format!("“{host}” is an IP address, not a site name. Add it anyway?"),
            SiteError::Local { host, ip: false } => format!("“{host}” is this computer, not a site. Add it anyway?"),
        }
    }
}

impl Site {
    /// A site from typed text: a host name, `*.host` for it and its
    /// subdomains, or a pasted http or https link, whose path and port are
    /// left out. International names are kept as punycode. IP addresses and
    /// localhost need `allow_local`.
    pub fn parse(input: &str, subdomains: bool, allow_local: bool) -> Result<Site, SiteError> {
        let input = input.trim();
        if input.is_empty() { return Err(SiteError::Empty); }
        let (input, subdomains) = match input.strip_prefix("*.") {
            Some(rest) => (rest, true),
            None => (input, subdomains),
        };
        if input.len() > MAX_INPUT || input.chars().any(|c| c.is_whitespace() || c.is_control() || c == '*') {
            return Err(SiteError::Invalid);
        }
        let url = if input.contains("://") { Url::parse(input) } else { Url::parse(&format!("https://{input}")) }.map_err(|_| SiteError::Invalid)?;
        // "mailto:a@site" reads as a user name and password: never a site.
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() { return Err(SiteError::Invalid); }
        match url.host().ok_or(SiteError::Invalid)? {
            Host::Domain(domain) => {
                let host = bare(domain);
                if !valid_domain(&host) { return Err(SiteError::Invalid); }
                if is_localhost(&host) && !allow_local { return Err(SiteError::Local { host, ip: false }); }
                Ok(Site { host, subdomains })
            }
            Host::Ipv4(_) | Host::Ipv6(_) => {
                let host = url.host_str().ok_or(SiteError::Invalid)?.to_owned();
                if !allow_local { return Err(SiteError::Local { host, ip: true }); }
                Ok(Site { host, subdomains: false })
            }
        }
    }

    /// How specific a match `host` (already bare) is, or None: a longer site
    /// is more specific, and an exact match beats a subdomain one.
    fn matches(&self, host: &str) -> Option<(usize, bool)> {
        if host == self.host { return Some((self.host.len(), true)); }
        let below = self.subdomains && host.len() > self.host.len() + 1
            && host.ends_with(self.host.as_str()) && host.as_bytes()[host.len() - self.host.len() - 1] == b'.';
        below.then_some((self.host.len(), false))
    }

    /// The host as people read it: international names in their own script.
    pub fn name(&self) -> String {
        url::quirks::domain_to_unicode(&self.host)
    }

    /// "github.com", or "github.com and subdomains".
    pub fn label(&self) -> String {
        if self.subdomains { format!("{} and subdomains", self.name()) } else { self.name() }
    }
}

/// The bare host of an http or https link, as rules compare it.
pub fn link_host(url: &str) -> Option<String> {
    let url = Url::parse(url).ok().filter(|url| matches!(url.scheme(), "http" | "https"))?;
    let host = bare(url.host_str()?);
    (!host.is_empty()).then_some(host)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteRule {
    pub site: Site,
    pub profile: String,
}

/// A profile pages can open in on this device.
pub fn usable_profile(lib: &Library, profile: &str) -> bool {
    lib.effective_profile_preferences(profile).is_some()
}

/// Why a site was not added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddError {
    /// Another profile has this site; add again with `take_over` to move it.
    Taken(SiteRule),
    Save(String),
}

/// This device's site profiles.
#[derive(Clone, Debug, Default)]
pub struct SiteProfiles {
    rules: Vec<SiteRule>,
    /// Why saved rules were not fully read; cleared by the next save.
    pub notice: Option<String>,
}

impl SiteProfiles {
    /// Read this device's rules. A bad record never blocks links and is left
    /// untouched until the next save.
    pub fn load(store: &Store) -> Self {
        let raw = match store.kv_get(KEY) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Self::default(),
            Err(_) => return SiteProfiles { rules: Vec::new(), notice: Some(UNREADABLE.into()) },
        };
        match decode(&raw) {
            Ok((rules, 0)) => SiteProfiles { rules, notice: None },
            Ok((rules, _)) => SiteProfiles { rules, notice: Some(PARTLY.into()) },
            Err(notice) => SiteProfiles { rules: Vec::new(), notice: Some(notice.into()) },
        }
    }

    pub fn rules(&self) -> &[SiteRule] {
        &self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The rule for exactly this site, whatever its profile.
    pub fn owner(&self, host: &str) -> Option<&SiteRule> {
        self.rules.iter().find(|rule| rule.site.host == host)
    }

    /// The most specific rule matching `url`, whatever its profile.
    pub fn rule_for(&self, url: &str) -> Option<&SiteRule> {
        let host = link_host(url)?;
        self.rules.iter().filter_map(|rule| rule.site.matches(&host).map(|rank| (rank, rule))).max_by_key(|(rank, _)| *rank).map(|(_, rule)| rule)
    }

    /// The rule that routes `url`: the most specific match, while its
    /// profile is usable here. A rule for a missing profile routes nothing,
    /// not even through a broader rule.
    pub fn route(&self, lib: &Library, url: &str) -> Option<&SiteRule> {
        self.rule_for(url).filter(|rule| usable_profile(lib, &rule.profile))
    }

    /// Add `rule`, or change the subdomains of its profile's rule for the
    /// site. Another profile's rule for it moves only with `take_over`.
    pub fn add(&mut self, store: &Store, rule: SiteRule, take_over: bool) -> Result<(), AddError> {
        let mut rules = self.rules.clone();
        match rules.iter_mut().find(|old| old.site.host == rule.site.host) {
            Some(old) if old.profile != rule.profile && !take_over => return Err(AddError::Taken(old.clone())),
            Some(old) => *old = rule,
            None => rules.push(rule),
        }
        self.save(store, rules).map_err(AddError::Save)
    }

    /// Remove the rule for exactly this site.
    pub fn remove(&mut self, store: &Store, host: &str) -> Result<(), String> {
        let rules: Vec<SiteRule> = self.rules.iter().filter(|rule| rule.site.host != host).cloned().collect();
        self.save(store, rules)
    }

    /// Write `rules` in one statement. On failure nothing changes here.
    fn save(&mut self, store: &Store, rules: Vec<SiteRule>) -> Result<(), String> {
        store.kv_set(KEY, &encode(&rules)).map_err(|error| error.to_string())?;
        self.rules = rules;
        self.notice = None;
        Ok(())
    }
}

fn encode(rules: &[SiteRule]) -> String {
    let rows: Vec<_> = rules.iter().map(|rule| serde_json::json!({
        "host": rule.site.host,
        "subdomains": rule.site.subdomains,
        "profile": rule.profile,
    })).collect();
    serde_json::json!({ "version": VERSION, "rules": rows }).to_string()
}

/// Saved rules and how many rows were left out. Rules whose profile is
/// missing are kept; Settings shows them.
fn decode(raw: &str) -> Result<(Vec<SiteRule>, usize), &'static str> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| UNREADABLE)?;
    match value["version"].as_u64() {
        Some(VERSION) => {}
        Some(version) if version > VERSION => return Err(NEWER),
        _ => return Err(UNREADABLE),
    }
    let rows = value["rules"].as_array().ok_or(UNREADABLE)?;
    let mut rules: Vec<SiteRule> = Vec::new();
    let mut ignored = 0;
    for row in rows {
        let site = match (row["host"].as_str(), row["subdomains"].as_bool()) {
            // Saved hosts were checked when added; only the same host reads back.
            (Some(host), Some(subdomains)) => Site::parse(host, subdomains, true).ok().filter(|site| site.host == host && site.subdomains == subdomains),
            _ => None,
        };
        match (site, row["profile"].as_str()) {
            (Some(site), Some(profile)) if !profile.is_empty() && !rules.iter().any(|rule| rule.site.host == site.host) => {
                rules.push(SiteRule { site, profile: profile.into() });
            }
            _ => ignored += 1,
        }
    }
    Ok((rules, ignored))
}

/// Remove `profile`'s rules. Library::delete_profile calls this inside its
/// transaction, so they go with the profile. An unreadable record is left:
/// it routes nothing.
pub fn forget_profile(store: &Store, profile: &str) -> crate::store::Result<()> {
    let Some(raw) = store.kv_get(KEY)? else { return Ok(()) };
    let Ok((rules, _)) = decode(&raw) else { return Ok(()) };
    if !rules.iter().any(|rule| rule.profile == profile) { return Ok(()); }
    let kept: Vec<SiteRule> = rules.into_iter().filter(|rule| rule.profile != profile).collect();
    store.kv_set(KEY, &encode(&kept))
}

/// The Space `profile`'s sites open in: the one whose tabs were used last,
/// by `recent` (Space, last-active time) pairs, else its first in sidebar
/// order. None when no usable Space has that profile.
pub fn target_space<'a>(lib: &Library, profile: &str, recent: impl IntoIterator<Item = (&'a str, u64)>) -> Option<String> {
    let owned = |space: &str| usable_space(lib, space) && lib.spaces.get(space).is_some_and(|owner| owner.profile_id == profile);
    let latest = recent.into_iter().filter(|(space, _)| owned(space)).max_by_key(|(_, at)| *at).map(|(space, _)| space.to_owned());
    latest.or_else(|| lib.spaces_ordered().into_iter().map(|(id, _)| id).find(|id| owned(id)))
}

#[cfg(test)]
#[path = "site_profiles_tests.rs"]
mod tests;
