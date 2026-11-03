//! Element hiding: the style sheet each document gets from a profile's lists.
//!
//! [`Hiding`] is built once for a set of lists. A document's sheet holds the
//! rules for its own host, then the rules for every page, which are built
//! once and shared; a host that unhides one of them gets its own copy. A
//! page can read its sheet, so it never holds another host's rules.
//!
//! Each rule hides with `display:none` and nothing else, one selector a
//! rule, so a selector the browser can't read fails alone. A sheet holds at
//! most [`MAX_SELECTORS`] rules and [`MAX_BYTES`]; rules for every page past
//! that are counted in [`Hiding::skipped`] and left out, in list order.
//!
//! A page an `@@…$generichide` rule matches gets only its own host's
//! rules, as EasyList asks on sites that rules for every page break.

use super::filters::{HidingRule, RequestType};
use super::matcher::{CompiledList, Request};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use super::filters::MAX_SELECTOR_BYTES;

pub const MAX_SELECTORS: usize = 50_000;
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// What follows each selector in a sheet.
pub const RULE_END: &str = "{display:none!important}\n";

/// A profile's element hiding, for its lists that are in use.
pub struct Hiding {
    /// Rules for every page that nothing unhides everywhere, within the
    /// bounds, in list order.
    generic: Vec<Box<str>>,
    generic_sheet: Arc<str>,
    /// Names `generic_sheet`, so a renderer can keep one copy.
    generation: u64,
    /// Rules for every page left out: too many.
    pub skipped: usize,
    /// Rules naming hosts, to hide or to unhide.
    specific: Vec<HidingRule>,
    unhide: Vec<HidingRule>,
    /// The lists with `@@…$generichide` rules.
    generic_off: Vec<Arc<CompiledList>>,
}

/// A document's sheet: its host's rules, then the rules for every page.
#[derive(Debug)]
pub struct Sheet {
    pub specific: String,
    pub generic: Arc<str>,
    /// The generic part's [`Hiding`] generation; 0 when it was built for
    /// this document alone.
    pub generation: u64,
}

static GENERATIONS: AtomicU64 = AtomicU64::new(1);

impl Hiding {
    pub fn new(lists: &[Arc<CompiledList>]) -> Hiding {
        let rules = || lists.iter().flat_map(|list| list.hiding.iter());
        // `#@#selector` with no host unhides it everywhere.
        let everywhere: HashSet<&str> = rules().filter(|rule| rule.exception && rule.domains.is_empty()).map(|rule| &*rule.selector).collect();
        let (mut generic, mut seen, mut bytes, mut skipped) = (Vec::new(), HashSet::new(), 0, 0);
        let (mut specific, mut unhide) = (Vec::new(), Vec::new());
        for rule in rules() {
            if rule.exception {
                if !rule.domains.is_empty() { unhide.push(rule.clone()); }
            } else if everywhere.contains(&*rule.selector) {
                continue;
            } else if !rule.domains.is_empty() {
                specific.push(rule.clone());
            } else if seen.insert(&*rule.selector) {
                if generic.len() < MAX_SELECTORS && bytes + rule_len(&rule.selector) <= MAX_BYTES {
                    bytes += rule_len(&rule.selector);
                    generic.push(rule.selector.clone());
                } else {
                    skipped += 1;
                }
            }
        }
        let generic_sheet: Arc<str> = sheet_of(generic.iter().map(|selector| &**selector)).into();
        let generic_off = lists.iter().filter(|list| list.has_generic_hide()).cloned().collect();
        Hiding { generic, generic_sheet, generation: GENERATIONS.fetch_add(1, Ordering::Relaxed), skipped, specific, unhide, generic_off }
    }

    /// The sheet for a web document at `url`; None for another address or
    /// when nothing is hidden there.
    pub fn sheet(&self, url: &str) -> Option<Sheet> {
        let parsed = url::Url::parse(url).ok().filter(|parsed| matches!(parsed.scheme(), "http" | "https"))?;
        let host = parsed.host_str()?.trim_end_matches('.');
        let unhidden: HashSet<&str> = self.unhide.iter().filter(|rule| applies(rule, host)).map(|rule| &*rule.selector).collect();
        let (mut seen, mut count, mut bytes) = (HashSet::new(), 0, 0);
        let mut specific = String::new();
        for rule in self.specific.iter().filter(|rule| applies(rule, host) && !unhidden.contains(&*rule.selector)) {
            if count == MAX_SELECTORS || bytes + rule_len(&rule.selector) > MAX_BYTES { break; }
            if !seen.insert(&*rule.selector) { continue; }
            push_rule(&mut specific, &rule.selector);
            (count, bytes) = (count + 1, bytes + rule_len(&rule.selector));
        }
        let shared = self.generic.len() + count <= MAX_SELECTORS && self.generic_sheet.len() + bytes <= MAX_BYTES
            && !self.generic.iter().any(|selector| unhidden.contains(&**selector));
        let generic_off = !self.generic_off.is_empty() && Request::new(url, RequestType::Other, url).is_some_and(|page| self.generic_off.iter().any(|list| list.generic_hidden(&page)));
        let (generic, generation) = if generic_off {
            (Arc::from(""), 0)
        } else if shared {
            (self.generic_sheet.clone(), self.generation)
        } else {
            // Room left after the host's rules, without what it unhides.
            let kept = self.generic.iter().filter(|selector| !unhidden.contains(&***selector)).take_while(|selector| {
                (count, bytes) = (count + 1, bytes + rule_len(selector));
                count <= MAX_SELECTORS && bytes <= MAX_BYTES
            });
            (sheet_of(kept.map(|selector| &**selector)).into(), 0)
        };
        (!specific.is_empty() || !generic.is_empty()).then_some(Sheet { specific, generic, generation })
    }

    /// Roughly what it holds on the heap, besides its lists.
    pub fn heap_bytes(&self) -> usize {
        let generic: usize = self.generic.iter().map(|selector| std::mem::size_of::<Box<str>>() + selector.len()).sum();
        generic + self.generic_sheet.len() + self.specific.iter().chain(&self.unhide).map(HidingRule::heap_bytes).sum::<usize>()
    }
}

/// Whether a rule naming hosts applies on `host`: on one of the hosts it
/// includes (or on any, when it only excludes), and on none it excludes. A
/// host includes its subdomains.
fn applies(rule: &HidingRule, host: &str) -> bool {
    let on = |domain: &str| host.len() >= domain.len() && host.ends_with(domain) && (host.len() == domain.len() || host.as_bytes()[host.len() - domain.len() - 1] == b'.');
    let mut includes = rule.domains.iter().filter(|(_, included)| *included).peekable();
    let included = includes.peek().is_none() || includes.any(|(domain, _)| on(domain));
    included && !rule.domains.iter().any(|(domain, included)| !included && on(domain))
}

fn rule_len(selector: &str) -> usize {
    selector.len() + RULE_END.len()
}

fn push_rule(sheet: &mut String, selector: &str) {
    sheet.push_str(selector);
    sheet.push_str(RULE_END);
}

fn sheet_of<'a>(selectors: impl Iterator<Item = &'a str>) -> String {
    let mut sheet = String::new();
    for selector in selectors { push_rule(&mut sheet, selector); }
    sheet
}

#[cfg(test)]
#[path = "hiding_tests.rs"]
mod tests;
