//! Element hiding in pages, through the renderer's own path.
//!
//! When a frame's script context is created, before any page script runs,
//! the renderer runs `hiding.js`, which keeps the browser's own
//! `CSSStyleSheet` and `adoptedStyleSheets` and returns one function that
//! adopts a style sheet into that document. The function is kept here,
//! never on the page's globals. The renderer then asks the browser for the
//! document's sheet, naming the document and the shared part it already
//! holds. The browser builds the sheet from the frame's own address and the
//! tab's top page, in Rust, and replies; a large reply goes through shared
//! memory. The renderer adopts it only into the document that asked.
//!
//! No matching runs in the page. A page can read and remove the adopted
//! sheet. Shadow roots aren't covered, and a document that never creates a
//! script context gets nothing.

use crate::content_rules::ContentRules;
use cef::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Renderer to browser: `[document, generation held]`.
pub(crate) const ASK: &str = "crossbow.hiding.ask";
/// Browser to renderer: one payload, in a binary argument or shared memory.
pub(crate) const SHEET: &str = "crossbow.hiding.sheet";
/// Replies this large or larger go through shared memory.
pub(crate) const SHARED_FROM: usize = 64 * 1024;
pub(crate) const SCRIPT: &str = include_str!("hiding.js");
const SCRIPT_URL: &str = "crossbow://hiding";
/// The generic part is left out: the renderer holds it.
const HELD: u32 = u32::MAX;

/// A reply: the document it is for, the generic part's generation, the
/// host's rules, and the generic rules unless the renderer holds them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Payload<'a> {
    pub(crate) document: u64,
    pub(crate) generation: u64,
    pub(crate) specific: &'a str,
    pub(crate) generic: Option<&'a str>,
}

/// Little-endian: document, generation, the two lengths (the second
/// [`HELD`] for none), then the two texts.
pub(crate) fn encode(document: u64, generation: u64, specific: &str, generic: Option<&str>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(24 + specific.len() + generic.map_or(0, str::len));
    bytes.extend_from_slice(&document.to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&(specific.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&generic.map_or(HELD, |generic| generic.len() as u32).to_le_bytes());
    bytes.extend_from_slice(specific.as_bytes());
    bytes.extend_from_slice(generic.unwrap_or_default().as_bytes());
    bytes
}

/// None unless `bytes` is exactly one payload with UTF-8 texts.
pub(crate) fn decode(bytes: &[u8]) -> Option<Payload<'_>> {
    let word = |at: usize| bytes.get(at..at + 8).map(|word| u64::from_le_bytes(word.try_into().unwrap()));
    let half = |at: usize| bytes.get(at..at + 4).map(|half| u32::from_le_bytes(half.try_into().unwrap()));
    let (document, generation, specific_len, generic_len) = (word(0)?, word(8)?, half(16)? as usize, half(20)?);
    let generic_bytes = if generic_len == HELD { 0 } else { generic_len as usize };
    if bytes.len() != 24usize.checked_add(specific_len)?.checked_add(generic_bytes)? { return None; }
    let specific = std::str::from_utf8(&bytes[24..24 + specific_len]).ok()?;
    let generic = (generic_len != HELD).then(|| std::str::from_utf8(&bytes[24 + specific_len..])).transpose().ok()?;
    Some(Payload { document, generation, specific, generic })
}

/// Whether a reply of `len` bytes goes through shared memory.
pub(crate) fn shared(len: usize) -> bool {
    len >= SHARED_FROM
}

/// The reply for `document`, at `url` in a tab on `top`, to a renderer that
/// holds generation `held` of the generic part; None when nothing is hidden.
pub(crate) fn reply(rules: Option<&dyn ContentRules>, document: u64, url: &str, top: &str, held: u64) -> Option<Vec<u8>> {
    let sheet = rules?.hiding(url, top)?;
    let generic = (sheet.generation == 0 || sheet.generation != held).then_some(&*sheet.generic);
    Some(encode(document, sheet.generation, &sheet.specific, generic))
}

// ---------------------------------------------------------------------------
// Browser process
// ---------------------------------------------------------------------------

/// Answers `frame`'s ask with its document's sheet, from the frame's own
/// address and the tab's top page.
pub(crate) fn answer(profile: &str, browser: Option<&Browser>, frame: &Frame, message: &ProcessMessage) {
    let Some([document, held]) = crate::media::strings(message).and_then(|args| <[String; 2]>::try_from(args).ok()) else { return };
    let (Ok(document), Ok(held)) = (document.parse::<u64>(), held.parse::<u64>()) else { return };
    let url = CefString::from(&frame.url()).to_string();
    let top = browser.and_then(|browser| browser.main_frame()).map(|top| CefString::from(&top.url()).to_string()).unwrap_or_default();
    let rules = crate::content_rules::rules_for(profile);
    let Some(bytes) = reply(rules.as_deref(), document, &url, &top, held) else { return };
    if let Some(mut message) = outgoing(&bytes) { frame.send_process_message(ProcessId::RENDERER, Some(&mut message)); }
}

fn outgoing(bytes: &[u8]) -> Option<ProcessMessage> {
    if shared(bytes.len()) {
        let builder = shared_process_message_builder_create(Some(&CefString::from(SHEET)), bytes.len())?;
        if builder.is_valid() != 1 || builder.size() < bytes.len() || builder.memory().is_null() { return None; }
        // SAFETY: the builder's region is writable and at least `bytes.len()` long.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), builder.memory().cast::<u8>(), bytes.len()) };
        return builder.build();
    }
    let message = process_message_create(Some(&CefString::from(SHEET)))?;
    let list = message.argument_list()?;
    list.set_size(1);
    list.set_binary(0, binary_value_create(Some(bytes)).as_mut());
    Some(message)
}

// ---------------------------------------------------------------------------
// Renderer process
// ---------------------------------------------------------------------------

thread_local! {
    /// Each frame's script context, its adopt function and the document it
    /// asked for, by frame id. Renderer main thread only.
    static DOCUMENTS: RefCell<HashMap<String, (V8Context, V8Value, u64)>> = RefCell::new(HashMap::new());
    static NEXT_DOCUMENT: Cell<u64> = const { Cell::new(1) };
    /// The generic part last received, by generation.
    static HELD_SHEET: RefCell<(u64, String)> = const { RefCell::new((0, String::new())) };
}

/// Keeps an adopt function for a web document's new context, then asks for
/// its sheet.
pub(crate) fn inject(frame: &Frame, context: &mut V8Context) {
    let url = CefString::from(&frame.url()).to_string();
    if crate::login::page_origin(&url).is_none() { return; }
    if context.enter() != 1 { return; }
    let (mut adopt, mut exception): (Option<V8Value>, Option<V8Exception>) = (None, None);
    let ran = context.eval(Some(&CefString::from(SCRIPT)), Some(&CefString::from(SCRIPT_URL)), 1, Some(&mut adopt), Some(&mut exception)) == 1;
    context.exit();
    let Some(adopt) = adopt.filter(|adopt| ran && adopt.is_function() == 1) else { return };
    let document = NEXT_DOCUMENT.with(|next| next.replace(next.get() + 1));
    let id = CefString::from(&frame.identifier()).to_string();
    DOCUMENTS.with_borrow_mut(|documents| documents.insert(id, (context.clone(), adopt, document)));
    ask(frame, document, HELD_SHEET.with_borrow(|(generation, _)| *generation));
}

fn ask(frame: &Frame, document: u64, held: u64) {
    if let Some(mut message) = crate::media::outgoing(ASK, &[document.to_string(), held.to_string()]) {
        frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
    }
}

pub(crate) fn release(frame: &Frame, context: &mut V8Context) {
    let id = CefString::from(&frame.identifier()).to_string();
    DOCUMENTS.with_borrow_mut(|documents| {
        if documents.get_mut(&id).is_some_and(|(stored, ..)| stored.is_same(Some(&mut *context)) == 1) { documents.remove(&id); }
    });
}

/// A sheet from the browser, adopted into the frame's document if it is
/// still the one that asked.
pub(crate) fn sheet(frame: &Frame, message: &ProcessMessage) -> bool {
    if CefString::from(&message.name()).to_string() != SHEET { return false; }
    let region = message.shared_memory_region().filter(|region| region.is_valid() == 1 && !region.memory().is_null());
    let binary = message.argument_list().and_then(|list| list.binary(0));
    // SAFETY: each view lives while `region` or `binary` does, here.
    let bytes: &[u8] = match (&region, &binary) {
        (Some(region), _) => unsafe { std::slice::from_raw_parts(region.memory().cast::<u8>(), region.size()) },
        (None, Some(binary)) if !binary.raw_data().is_null() => unsafe { std::slice::from_raw_parts(binary.raw_data().cast::<u8>(), binary.size()) },
        _ => return true,
    };
    let Some(payload) = decode(bytes) else { return true };
    let id = CefString::from(&frame.identifier()).to_string();
    let css = HELD_SHEET.with_borrow_mut(|held| {
        let generic = match payload.generic {
            Some(generic) => {
                if payload.generation != 0 { *held = (payload.generation, generic.to_owned()); }
                generic
            }
            None if held.0 == payload.generation => held.1.as_str(),
            None => return None,
        };
        Some(format!("{}{}", payload.specific, generic))
    });
    let Some(css) = css else {
        // The held part changed since the ask: ask again for all of it.
        ask(frame, payload.document, 0);
        return true;
    };
    DOCUMENTS.with_borrow_mut(|documents| {
        let Some((context, adopt, asked)) = documents.get_mut(&id) else { return };
        if *asked != payload.document || context.is_valid() != 1 || context.enter() != 1 { return; }
        adopt.execute_function_with_context(Some(&mut *context), None, Some(&[v8_value_create_string(Some(&CefString::from(css.as_str())))]));
        context.exit();
    });
    true
}

#[cfg(test)]
#[path = "hiding_tests.rs"]
mod tests;
