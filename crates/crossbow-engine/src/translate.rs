//! The page side of Translate Page.
//!
//! In each renderer, `translate.js` is evaluated when an http(s) top frame's
//! script context is created, before any page script. It does nothing until
//! the browser sends a [`TranslateCommand`]: a hello reads nothing and answers
//! with the number the script chose, which tells the browser a document the
//! back/forward cache kept from a new one, and with what the document did
//! before ([`Earlier`]), which comes back with it; a probe answers the same
//! and reads the root element's `lang` and a short sample of visible text;
//! a collect reads the visible
//! text nodes in document order that the translation shown hasn't read yet,
//! numbered from 1; an apply writes translated text back into those nodes,
//! only as text; a restore puts every original back. A document whose root
//! or body says `translate="no"` or `class="notranslate"` is never read: it
//! answers each probe and collect with [`TranslateReport::Declined`]. A
//! document reads at
//! most [`MAX_SEGMENTS`] nodes and [`MAX_CHARS`] UTF-16 units over all its
//! collects, each node counting each time it is sent, so a same-document
//! change can't read the page again. Each answer comes back as a
//! [`TranslateReport`], which the browser parses strictly and drops if
//! anything is out of range; the browser holds each document to the same
//! totals whatever its script says.
//!
//! The browser also keeps the `Content-Language` header of each top-frame
//! document response, see [`Languages`]. Nothing here logs page text.

use cef::*;
use std::cell::RefCell;
use std::collections::HashMap;

pub(crate) const STATE: &str = "crossbow.translate.state";
pub(crate) const COMMAND: &str = "crossbow.translate.command";
const SCRIPT: &str = include_str!("translate.js");
const SCRIPT_URL: &str = "crossbow://translate";
/// The most text nodes one document reads, over all its collects.
pub const MAX_SEGMENTS: usize = 2000;
/// The most UTF-16 units one node may hold to be read; longer ones are skipped.
pub const MAX_SEGMENT_CHARS: usize = 2000;
/// The most UTF-16 units one document reads, over all its collects, text
/// sent again counting again.
pub const MAX_CHARS: usize = 100_000;
/// The longest sample a probe may ask for, in UTF-16 units.
pub const MAX_SAMPLE: usize = 2000;
const MAX_LANG: usize = 64;
/// A report's or a command's most strings.
const MAX_ARGS: usize = 4 + 2 * MAX_SEGMENTS;

/// One text node's trimmed text, as the page holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub id: u32,
    pub text: String,
}

/// What a document's script did before a probe or a hello: whether an
/// earlier probe reported a sample, which the browser may have sent on, the
/// nodes and UTF-16 units its collects read, and the first pass of the
/// translation it shows, or 0. The script keeps it for as long as the
/// document lives, so a document the back/forward cache brings back brings
/// it too, whatever the browser kept. All zero for a new document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Earlier {
    pub sampled: bool,
    pub nodes: usize,
    pub units: usize,
    pub shown: u64,
}

/// What a page's `translate.js` said. `page` is a number the script chose
/// when the document started; `pass` counts its collects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslateReport {
    Probe { page: u64, earlier: Earlier, lang: String, sample: String },
    Segments { page: u64, pass: u64, truncated: bool, segments: Vec<Segment> },
    Applied { page: u64, pass: u64, written: u32, skipped: u32 },
    Restored { page: u64, pass: u64, restored: u32 },
    /// The root or the body says `translate="no"` or `class="notranslate"`:
    /// nothing was read.
    Declined { page: u64 },
    /// The answer to a hello: nothing was read.
    Hello { page: u64, earlier: Earlier },
}

impl TranslateReport {
    pub fn page(&self) -> u64 {
        match self {
            Self::Probe { page, .. } | Self::Segments { page, .. } | Self::Applied { page, .. } | Self::Restored { page, .. } | Self::Declined { page } | Self::Hello { page, .. } => *page,
        }
    }
}

/// What the browser asks of the top frame's `translate.js`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslateCommand {
    /// The number the document's script chose, and nothing else.
    Hello,
    /// The root's `lang` and at most this many UTF-16 units of visible text.
    Probe { sample: usize },
    /// The readable text nodes the translation shown hasn't read, within
    /// what is left of the document's limits; the first after a load or a
    /// restore starts a translation.
    Collect,
    /// Write these texts into the nodes of `pass` with these ids.
    Apply { pass: u64, texts: Vec<(u32, String)> },
    /// Put every original back, ending the translation `pass` belongs to.
    Restore { pass: u64 },
}

impl TranslateCommand {
    pub fn encode(&self) -> Vec<String> {
        match self {
            Self::Hello => vec!["hello".into()],
            Self::Probe { sample } => vec!["probe".into(), sample.min(&MAX_SAMPLE).to_string()],
            Self::Collect => vec!["collect".into()],
            Self::Apply { pass, texts } => {
                let mut args = vec!["apply".to_string(), pass.to_string()];
                for (id, text) in texts.iter().take(MAX_SEGMENTS) {
                    args.push(id.to_string());
                    args.push(text.clone());
                }
                args
            }
            Self::Restore { pass } => vec!["restore".into(), pass.to_string()],
        }
    }
}

fn positive(text: &str) -> Option<u64> {
    text.parse().ok().filter(|value| *value > 0)
}

fn count(text: &str) -> Option<u32> {
    text.parse().ok().filter(|value| *value as usize <= MAX_SEGMENTS)
}

fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A document's [`Earlier`], within the limits it reads by.
fn earlier(sampled: &str, nodes: &str, units: &str, shown: &str) -> Option<Earlier> {
    let sampled = match sampled { "0" => false, "1" => true, _ => return None };
    let nodes = nodes.parse().ok().filter(|nodes| *nodes <= MAX_SEGMENTS)?;
    let units = units.parse().ok().filter(|units| *units <= MAX_CHARS)?;
    Some(Earlier { sampled, nodes, units, shown: shown.parse().ok()? })
}

/// Read a [`STATE`] message's strings. Anything malformed or out of range
/// drops the whole report.
pub fn parse_report(args: &[String]) -> Option<TranslateReport> {
    if args.len() > MAX_ARGS { return None; }
    let (kind, rest) = args.split_first()?;
    match (kind.as_str(), rest) {
        ("probe", [page, sampled, nodes, read, shown, lang, sample]) => {
            if units(lang) > MAX_LANG || units(sample) > MAX_SAMPLE { return None; }
            Some(TranslateReport::Probe { page: positive(page)?, earlier: earlier(sampled, nodes, read, shown)?, lang: lang.clone(), sample: sample.clone() })
        }
        ("segments", [page, pass, truncated, pairs @ ..]) => {
            if pairs.len() % 2 != 0 || pairs.len() / 2 > MAX_SEGMENTS { return None; }
            let truncated = match truncated.as_str() { "0" => false, "1" => true, _ => return None };
            let mut segments: Vec<Segment> = Vec::with_capacity(pairs.len() / 2);
            let mut total = 0;
            for pair in pairs.chunks(2) {
                // Numbered in document order from 1, as the script numbers them.
                let id: u32 = pair[0].parse().ok().filter(|id| *id as usize == segments.len() + 1)?;
                let length = units(&pair[1]);
                if pair[1].trim().is_empty() || length > MAX_SEGMENT_CHARS { return None; }
                total += length;
                segments.push(Segment { id, text: pair[1].clone() });
            }
            if total > MAX_CHARS { return None; }
            Some(TranslateReport::Segments { page: positive(page)?, pass: positive(pass)?, truncated, segments })
        }
        ("applied", [page, pass, written, skipped]) => {
            Some(TranslateReport::Applied { page: positive(page)?, pass: positive(pass)?, written: count(written)?, skipped: count(skipped)? })
        }
        ("restored", [page, pass, restored]) => {
            Some(TranslateReport::Restored { page: positive(page)?, pass: positive(pass)?, restored: count(restored)? })
        }
        ("declined", [page]) => Some(TranslateReport::Declined { page: positive(page)? }),
        ("hello", [page, sampled, nodes, read, shown]) => Some(TranslateReport::Hello { page: positive(page)?, earlier: earlier(sampled, nodes, read, shown)? }),
        _ => None,
    }
}

/// The `Content-Language` of the latest top-frame document response, by the
/// address it answered. Written on the IO thread, read on the UI thread.
#[derive(Default)]
pub(crate) struct Languages {
    latest: Option<(String, String)>,
}

impl Languages {
    /// A top-frame document's response and its `Content-Language` values,
    /// joined as one list; none clears what an earlier document said.
    pub(crate) fn response(&mut self, url: &str, values: &[String]) {
        let joined = values.iter().map(|value| value.trim()).filter(|value| !value.is_empty()).collect::<Vec<_>>().join(", ");
        self.latest = (!joined.is_empty() && joined.len() <= 256).then(|| (url.to_owned(), joined));
    }

    /// What the response for `url` said, if that was the latest.
    pub(crate) fn of(&self, url: &str) -> Option<String> {
        self.latest.as_ref().filter(|(answered, _)| answered == url).map(|(_, value)| value.clone())
    }
}

// ---------------------------------------------------------------------------
// Renderer process
// ---------------------------------------------------------------------------

thread_local! {
    /// Each top frame's script context and the command function
    /// `translate.js` returned, by frame id. Renderer main thread only.
    static COMMANDS: RefCell<HashMap<String, (V8Context, V8Value)>> = RefCell::new(HashMap::new());
}

wrap_v8_handler! {
    struct ReportBuilder;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            report(arguments.unwrap_or_default());
            1
        }
    }
}

/// A report from `translate.js`, sent on from the calling context's own
/// frame, which must be the top one. The browser checks it again.
fn report(arguments: &[Option<V8Value>]) {
    let Some(context) = v8_context_get_current_context() else { return };
    let Some(frame) = context.frame().filter(|frame| frame.is_main() == 1) else { return };
    if arguments.len() > MAX_ARGS { return; }
    let args: Option<Vec<String>> = arguments.iter().map(|value| {
        let value = value.as_ref()?;
        (value.is_string() == 1).then(|| CefString::from(&value.string_value()).to_string())
    }).collect();
    if let Some(mut message) = args.and_then(|args| crate::media::outgoing(STATE, &args)) {
        frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
    }
}

/// Install `translate.js` in an http(s) top frame's new script context.
pub(crate) fn inject(frame: &Frame, context: &mut V8Context) {
    if frame.is_main() != 1 || crate::login::page_origin(&CefString::from(&frame.url()).to_string()).is_none() { return; }
    if context.enter() != 1 { return; }
    let report = v8_value_create_function(Some(&CefString::from("report")), Some(&mut ReportBuilder::new()));
    let (mut factory, mut exception): (Option<V8Value>, Option<V8Exception>) = (None, None);
    let command = (context.eval(Some(&CefString::from(SCRIPT)), Some(&CefString::from(SCRIPT_URL)), 1, Some(&mut factory), Some(&mut exception)) == 1)
        .then_some(factory).flatten()
        .filter(|factory| factory.is_function() == 1)
        .and_then(|factory| factory.execute_function_with_context(Some(context), None, Some(&[report])))
        .filter(|command| command.is_function() == 1);
    context.exit();
    if let Some(command) = command {
        let id = CefString::from(&frame.identifier()).to_string();
        COMMANDS.with_borrow_mut(|commands| commands.insert(id, (context.clone(), command)));
    }
}

pub(crate) fn release(frame: &Frame, context: &mut V8Context) {
    let id = CefString::from(&frame.identifier()).to_string();
    COMMANDS.with_borrow_mut(|commands| {
        if commands.get_mut(&id).is_some_and(|(stored, _)| stored.is_same(Some(&mut *context)) == 1) { commands.remove(&id); }
    });
}

/// A command from the browser, for the top frame it was sent to. Every part
/// reaches the script as a string.
pub(crate) fn command(frame: &Frame, message: &ProcessMessage) -> bool {
    if CefString::from(&message.name()).to_string() != COMMAND { return false; }
    let Some(args) = crate::media::strings(message).filter(|args| !args.is_empty() && args.len() <= MAX_ARGS) else { return true };
    let id = CefString::from(&frame.identifier()).to_string();
    COMMANDS.with_borrow_mut(|commands| {
        let Some((context, command)) = commands.get_mut(&id) else { return };
        if context.is_valid() != 1 || context.enter() != 1 { return; }
        let values: Vec<Option<V8Value>> = args.iter().map(|arg| v8_value_create_string(Some(&CefString::from(arg.as_str())))).collect();
        command.execute_function_with_context(Some(&mut *context), None, Some(&values));
        context.exit();
    });
    true
}

// ---------------------------------------------------------------------------
// Browser process
// ---------------------------------------------------------------------------

/// A [`STATE`] message from `frame`: the report, if it came from the top
/// frame and reads correctly. None for another message.
pub(crate) fn receive(frame: &Frame, message: &ProcessMessage) -> Option<Option<TranslateReport>> {
    if CefString::from(&message.name()).to_string() != STATE { return None; }
    if frame.is_main() != 1 { return Some(None); }
    Some(crate::media::strings(message).as_deref().and_then(parse_report))
}

#[cfg(test)]
#[path = "translate_tests.rs"]
mod tests;
