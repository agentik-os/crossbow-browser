//! Unread badges for pinned s and favorites, as Arc's Show Unread Badge.
//! A loaded tab whose title changes while it is not on screen gets a dot, or
//! the count its title starts with, such as "(3) Inbox". Viewing it clears
//! the badge. Session-only: nothing here is saved.

use std::collections::HashMap;

/// Counts above this show as "99+".
pub const MAX_COUNT: u64 = 99;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Dot,
    Count(u64),
}

impl Badge {
    /// A count's text; None for a dot.
    pub fn label(self) -> Option<String> {
        match self {
            Badge::Dot => None,
            Badge::Count(n) if n > MAX_COUNT => Some(format!("{MAX_COUNT}+")),
            Badge::Count(n) => Some(n.to_string()),
        }
    }
}

/// The count a title starts with: "(3) Inbox", "[12] Chat", "(1,234) Feed",
/// or "(99+) Mail" for more than 99. Direction marks before it are skipped.
pub fn title_count(title: &str) -> Option<u64> {
    split_count(title).map(|(count, _)| count)
}

/// The count and the rest of the title after it.
fn split_count(title: &str) -> Option<(u64, &str)> {
    let text = title.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'));
    let close = match text.chars().next()? {
        '(' => ')',
        '[' => ']',
        _ => return None,
    };
    let rest = &text[1..];
    // A count is short: never scan a long title for the bracket.
    let end = rest.char_indices().take(24).find(|(_, c)| *c == close)?.0;
    let (digits, more) = match rest[..end].strip_suffix('+') {
        Some(digits) => (digits, true),
        None => (&rest[..end], false),
    };
    // Groups after the first have three digits: "1,234" or "1.234".
    let mut groups = digits.split([',', '.']);
    let first = groups.next()?;
    let grouped = groups.all(|group| group.len() == 3 && group.bytes().all(|b| b.is_ascii_digit()));
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) || !grouped { return None; }
    let n = digits.bytes().filter(u8::is_ascii_digit).fold(0u64, |n, b| n.saturating_mul(10).saturating_add(u64::from(b - b'0')));
    Some((if more { n.saturating_add(1) } else { n }, rest[end + close.len_utf8()..].trim()))
}

/// A pinned tab or favorite, as one frame's badge pass sees it.
#[derive(Clone, Copy, Debug)]
pub struct Watched<'a> {
    pub tab: &'a str,
    pub title: &'a str,
    /// Has a live page. Unloaded tabs have no badge.
    pub loaded: bool,
    pub loading: bool,
    /// Shown in a pane.
    pub visible: bool,
}

#[derive(Debug, Default)]
pub struct Unread {
    /// The title each watched tab was last seen with, and the pass that saw it.
    seen: HashMap<String, (String, u64)>,
    badges: HashMap<String, Badge>,
    pass: u64,
}

impl Unread {
    pub fn badge(&self, tab: &str) -> Option<Badge> {
        self.badges.get(tab).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.badges.is_empty()
    }

    /// One frame's pass. Tabs left out (closed, archived, or in a profile
    /// with badges off) lose their badge and what was seen.
    pub fn update<'a>(&mut self, tabs: impl IntoIterator<Item = Watched<'a>>) {
        self.pass += 1;
        let pass = self.pass;
        for tab in tabs {
            if !tab.loaded { continue; }
            if tab.visible { self.badges.remove(tab.tab); }
            let Some((seen, at)) = self.seen.get_mut(tab.tab) else {
                // The first title seen is not news.
                self.seen.insert(tab.tab.to_owned(), (tab.title.to_owned(), pass));
                continue;
            };
            *at = pass;
            if seen == tab.title { continue; }
            // "(5) Inbox" becoming "Inbox": the count went away, so nothing is unread.
            let read = split_count(seen).is_some_and(|(_, rest)| rest == tab.title.trim());
            tab.title.clone_into(seen);
            if tab.visible { continue; }
            match title_count(tab.title) {
                Some(0) => { self.badges.remove(tab.tab); }
                Some(n) => { self.badges.insert(tab.tab.to_owned(), Badge::Count(n)); }
                None if read => { self.badges.remove(tab.tab); }
                // A navigation's titles are not news.
                None if tab.loading => {}
                None => { self.badges.insert(tab.tab.to_owned(), Badge::Dot); }
            }
        }
        self.seen.retain(|_, (_, at)| *at == pass);
        let seen = &self.seen;
        self.badges.retain(|tab, _| seen.contains_key(tab));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab<'a>(title: &'a str, visible: bool) -> Watched<'a> {
        Watched { tab: "mail", title, loaded: true, loading: false, visible }
    }

    #[test]
    fn titles_give_the_count_they_start_with() {
        for (title, count) in [
            ("(3) Inbox", Some(3)),
            ("[12] Chat", Some(12)),
            ("  (7)Inbox", Some(7)),
            ("(1,234) Feed", Some(1_234)),
            ("(1.234) Feed", Some(1_234)),
            ("(99+) Mail", Some(100)),
            ("(0) Inbox", Some(0)),
            ("\u{200f}(5) صندوق الوارد", Some(5)),
            ("\u{2067}[2] רשימה\u{2069}", Some(2)),
            ("(99999999999999999999) Spam", Some(u64::MAX)),
            ("Inbox (3)", None),
            ("(beta) Release notes", None),
            ("(3 new) Mail", None),
            ("(12,34) Odd", None),
            ("(,3) Odd", None),
            ("() Empty", None),
            ("(3", None),
            ("", None),
        ] {
            assert_eq!(title_count(title), count, "{title:?}");
        }
        // A long title is not scanned for a closing bracket.
        let long = format!("({}", "1".repeat(100_000));
        assert_eq!(title_count(&long), None);
        assert_eq!(title_count(&format!("(4) {}", "Very long title ".repeat(10_000))), Some(4));
        assert_eq!([Badge::Count(1), Badge::Count(99), Badge::Count(100), Badge::Count(u64::MAX), Badge::Dot].map(Badge::label),
            [Some("1".into()), Some("99".into()), Some("99+".into()), Some("99+".into()), None]);
    }

    #[test]
    fn a_background_title_change_badges_the_tab_until_it_is_viewed() {
        let mut unread = Unread::default();
        unread.update([tab("Inbox", false)]);
        assert_eq!(unread.badge("mail"), None, "the first title seen is not news");
        unread.update([tab("Inbox", false)]);
        assert_eq!(unread.badge("mail"), None);
        unread.update([tab("(3) Inbox", false)]);
        assert_eq!(unread.badge("mail"), Some(Badge::Count(3)));
        unread.update([tab("(1,234) Inbox", false)]);
        assert_eq!((unread.badge("mail"), unread.badge("mail").and_then(Badge::label)), (Some(Badge::Count(1_234)), Some("99+".into())));
        unread.update([tab("Someone replied", false)]);
        assert_eq!(unread.badge("mail"), Some(Badge::Dot), "a title without a count gives a dot");
        assert!(!unread.is_empty() && unread.badge("other").is_none());

        // Viewing clears it, and the title seen there is not news afterwards.
        unread.update([tab("Someone replied", true)]);
        assert_eq!(unread.badge("mail"), None);
        unread.update([tab("Someone replied", false)]);
        assert_eq!(unread.badge("mail"), None);
        // A change while on screen is seen, not unread.
        unread.update([tab("(2) Inbox", true)]);
        unread.update([tab("(2) Inbox", false)]);
        assert_eq!(unread.badge("mail"), None);
        // Zero unread clears a count, and so does a count going away.
        unread.update([tab("(5) Inbox", false)]);
        unread.update([tab("(0) Inbox", false)]);
        assert_eq!(unread.badge("mail"), None);
        unread.update([tab("(5) Inbox", false)]);
        unread.update([tab("Inbox", false)]);
        assert_eq!(unread.badge("mail"), None);
        unread.update([tab("Inbox — 1 new", false)]);
        assert_eq!(unread.badge("mail"), Some(Badge::Dot));
    }

    #[test]
    fn loading_pages_unloaded_and_closed_tabs_have_no_badge() {
        let mut unread = Unread::default();
        unread.update([tab("Inbox", false)]);
        // A navigation's titles are not unread news, unless they carry a count.
        unread.update([Watched { loading: true, ..tab("Loading…", false) }]);
        assert_eq!(unread.badge("mail"), None);
        unread.update([Watched { loading: true, ..tab("(4) Inbox", false) }]);
        assert_eq!(unread.badge("mail"), Some(Badge::Count(4)));

        // Unloading drops the badge and what was seen; reloading starts again.
        unread.update([Watched { loaded: false, ..tab("(4) Inbox", false) }]);
        assert_eq!(unread.badge("mail"), None);
        unread.update([tab("(6) Inbox", false)]);
        assert_eq!(unread.badge("mail"), None, "the first title after loading is not news");
        unread.update([tab("(7) Inbox", false)]);
        assert_eq!(unread.badge("mail"), Some(Badge::Count(7)));

        // Closed, archived or badges off: the pass leaves the tab out.
        unread.update([]);
        assert_eq!(unread.badge("mail"), None);
        assert!(unread.seen.is_empty() && unread.badges.is_empty(), "nothing is kept for tabs no longer watched");
        unread.update([tab("(8) Inbox", false)]);
        assert_eq!(unread.badge("mail"), None);
    }

    #[test]
    fn each_tab_has_its_own_badge() {
        let mut unread = Unread::default();
        let both = |a: &'static str, b: &'static str| [Watched { tab: "a", ..tab(a, false) }, Watched { tab: "b", ..tab(b, false) }];
        unread.update(both("A", "B"));
        unread.update(both("(1) A", "B"));
        assert_eq!((unread.badge("a"), unread.badge("b")), (Some(Badge::Count(1)), None));
        // Only "b" is still watched: "a" is in a profile that turned badges off.
        unread.update([Watched { tab: "b", ..tab("(2) B", false) }]);
        assert_eq!((unread.badge("a"), unread.badge("b")), (None, Some(Badge::Count(2))));
    }
}
