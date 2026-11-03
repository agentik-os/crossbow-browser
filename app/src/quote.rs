//! Copy as Quote: the page menu copies the selected text with the page's
//! title and address, as plain text and as an HTML block quote. Off by
//! default and kept on this device only. The text is the selection CEF gives
//! the menu; nothing runs in the page, and nothing is uploaded or stored.

use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crossbow_core::store::Store;
use crossbow_engine::ContextMenuInfo;

/// The page menu offers Copy as Quote for selected text.
#[derive(Debug)]
pub struct CopyAsQuote;

impl SwitchSpec for CopyAsQuote {
    const KEY: &'static str = "copy_as_quote";
    const DEFAULT: bool = false;
    const TITLE: &'static str = "Offer Copy as Quote for selected text";
    const DESCRIPTION: &'static str = "The page menu copies the selection in quotes with the page's title and address, and as a block quote for apps that paste formatting. This device only.";
    const STILL_OFF: &'static str = "The page menu still has no Copy as Quote";
    const STILL_ON: &'static str = "The page menu still offers Copy as Quote";
    const UNREADABLE: &'static str = "The saved choice for Copy as Quote could not be read, so it is off. Changing it saves over it.";
}

pub const LABEL: &str = "Copy as Quote";
pub const COPIED: &str = "Copied quote";
pub const COPY_FAILED: &str = "Couldn't copy the quote. Try Copy as Quote again.";
pub const NOTHING: &str = "There's no text to quote. Select text and try again.";
/// Characters of quoted text, the ellipsis included.
pub const QUOTE_MAX: usize = 1000;
/// Characters of the page's title, the ellipsis included.
pub const TITLE_MAX: usize = 200;
pub(crate) const PLAIN_TEXT: &str = "public.utf8-plain-text";
pub(crate) const HTML: &str = "public.html";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub plain: String,
    pub html: String,
}

/// Where quotes are copied. The production one is the macOS general
/// pasteboard; tests use a fake or a private pasteboard, never the person's
/// clipboard.
pub trait QuoteBoard {
    /// Replace the contents with one item: the plain text and the HTML.
    fn copy(&mut self, quote: &Quote) -> Result<(), String>;
}

pub fn system_board() -> Box<dyn QuoteBoard> {
    #[cfg(target_os = "macos")]
    if let Some(board) = crate::capture::mac::Pasteboard::general() { return Box::new(board); }
    Box::new(NoBoard)
}

struct NoBoard;

impl QuoteBoard for NoBoard {
    fn copy(&mut self, _: &Quote) -> Result<(), String> {
        Err(COPY_FAILED.into())
    }
}

#[cfg(target_os = "macos")]
impl QuoteBoard for crate::capture::mac::Pasteboard {
    fn copy(&mut self, quote: &Quote) -> Result<(), String> {
        if self.write_texts(&[(PLAIN_TEXT, &quote.plain), (HTML, &quote.html)]) { Ok(()) } else { Err(COPY_FAILED.into()) }
    }
}

/// The switch, read once when first needed, and where quotes are copied.
#[derive(Default)]
pub struct QuoteHost {
    switch: Option<DeviceSwitch<CopyAsQuote>>,
    board: Option<Box<dyn QuoteBoard>>,
}

impl QuoteHost {
    #[cfg(test)]
    pub fn with_board(board: Box<dyn QuoteBoard>) -> QuoteHost {
        QuoteHost { switch: None, board: Some(board) }
    }

    pub fn switch(&mut self, store: &Store) -> &mut DeviceSwitch<CopyAsQuote> {
        self.switch.get_or_insert_with(|| DeviceSwitch::load(store))
    }

    pub fn enabled(&mut self, store: &Store) -> bool {
        self.switch(store).enabled()
    }

    /// Copy a menu's selection, unless the switch was turned off since.
    /// None when nothing was written for that reason.
    pub fn copy(&mut self, store: &Store, selection: &str, title: &str, page_url: &str) -> Option<Result<(), String>> {
        if !self.enabled(store) { return None; }
        let board = self.board.get_or_insert_with(system_board);
        Some(quote(selection, title, page_url).ok_or_else(|| NOTHING.to_string()).and_then(|quote| board.copy(&quote)))
    }
}

/// The page menu offers Copy as Quote: the switch is on and text outside a
/// text field is selected.
pub fn offered(enabled: bool, info: &ContextMenuInfo) -> bool {
    enabled && !info.is_editable && !clean(&info.selection_text).is_empty()
}

/// The title and address to quote: the address of the frame the selection
/// is in, and the tab's title while the tab still shows the page the menu
/// was opened on and that frame shares the page's origin. Otherwise no
/// title, and the quote names the frame's site.
pub fn source(info: &ContextMenuInfo, shown_url: &str, shown_title: &str) -> (String, String) {
    let frame = if info.frame_url.is_empty() { &info.page_url } else { &info.frame_url };
    let origin = crossbow_core::site_permissions::origin_of;
    let same_origin = origin(frame).zip(origin(&info.page_url)).is_some_and(|(frame, page)| frame == page);
    let title = if shown_url == info.page_url && same_origin { shown_title.to_owned() } else { String::new() };
    (title, frame.clone())
}

/// The quote of `selection` on the page at `page_url`, or None when nothing
/// is left of the selection. Only an http or https address is included, and
/// without a user name or password. A missing title, or one that is just the
/// address, gives way to the site's name.
pub fn quote(selection: &str, title: &str, page_url: &str) -> Option<Quote> {
    let text = shorten(clean(selection), QUOTE_MAX);
    if text.is_empty() { return None; }
    let address = crossbow_core::captures::page_url(page_url);
    let title = shorten(clean(title), TITLE_MAX);
    let source = if title.is_empty() || title == address || title == page_url {
        if address.is_empty() { String::new() } else { crossbow_core::search::host_of(&address).unwrap_or_default() }
    } else {
        title
    };
    let plain = match (source.is_empty(), address.is_empty()) {
        (_, false) => format!("“{text}” — {source} ({address})"),
        (false, true) => format!("“{text}” — {source}"),
        (true, true) => format!("“{text}”"),
    };
    let (text, source, address) = (escape(&text), escape(&source), escape(&address));
    let html = match (source.is_empty(), address.is_empty()) {
        (_, false) => format!("<blockquote cite=\"{address}\"><p>{text}</p><p>— <a href=\"{address}\">{source}</a></p></blockquote>"),
        (false, true) => format!("<blockquote><p>{text}</p><p>— {source}</p></blockquote>"),
        (true, true) => format!("<blockquote><p>{text}</p></blockquote>"),
    };
    // Without it, some apps read the HTML as Latin-1.
    Some(Quote { plain, html: format!("<meta charset=\"utf-8\">{html}") })
}

/// One line: line breaks, tabs and other spaces become one space, and other
/// control or invisible characters are dropped.
fn clean(text: &str) -> String {
    let kept: String = text.chars().filter_map(|c| {
        if c.is_whitespace() { Some(' ') } else if c.is_control() || crate::ai_features::invisible(c) { None } else { Some(c) }
    }).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters, ending in an ellipsis when cut.
fn shorten(text: String, max: usize) -> String {
    if text.chars().count() <= max { return text; }
    let kept: String = text.chars().take(max - 1).collect();
    format!("{}…", kept.trim_end())
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
#[path = "quote_tests.rs"]
mod tests;
