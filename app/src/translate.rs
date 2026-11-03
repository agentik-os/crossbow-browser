//! Translate Page: an opt-in browser feature that translates a web page's
//! visible text with the saved loopback model, through crossbow-ai.
//!
//! When a tab finishes loading an http(s) page and the feature is on for its
//! profile, the page's language is read from, in order: the root element's
//! `lang`, the response's `Content-Language`, and, when neither names one, a
//! sample of at most [`SAMPLE_BYTES`] of visible text that the provider names
//! with a confidence; below [`MIN_CONFIDENCE`] it stays unknown. A page in
//! another language than the user's gets an offer. Only Translate (or a
//! language the user chose Always for) sends the rest of the text, in batches
//! of numbered items; each answer must name the same numbers, and anything
//! else leaves those nodes as they were. Answers are written into the page
//! as they arrive, as text only, and count once the page confirms them. Show
//! Original puts the page back once the page confirms it; a load or reload
//! ends the translation. Back or Forward to a document the back/forward
//! cache kept is a load too, but its script answers with the number it chose
//! when the document was new, so the document gets back the state it was
//! left in, Show Original included, and sends nothing it hadn't already.
//! The tab keeps a record of the last [`LEFT`] documents it left; one the
//! cache brings back after more says what it did itself, from its script's
//! own record, which comes back with it, so it goes on from there all the
//! same: its sample doesn't go again, its totals go on, and the
//! translation it shows keeps Show Original. An
//! address change within the document, by history.pushState or a #fragment
//! link, keeps everything: nothing is read again, and only for a language
//! the user chose Always is the text the page added since read, once the
//! address settles. The browser takes from each document at most one sample
//! to tell its language, and at most [`MAX_TRANSLATE_SEGMENTS`] nodes and
//! [`MAX_TRANSLATE_CHARS`] UTF-16 units over all its translations, text sent
//! again counting again, whatever the page reports; the page's script keeps
//! the same totals. Turning the feature off or changing the provider cancels
//! a request under way.
//!
//! Never and Always choices are kept per profile in this device's store, and
//! are never synced. Nothing here logs page text; only language codes.

use crate::ai_features::{self, Checked, Pending, Provider};
use crossbow_core::library::Library;
use crossbow_engine::{Earlier, MAX_TRANSLATE_CHARS, MAX_TRANSLATE_SEGMENTS, Segment, TranslateCommand, TranslateReport};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

pub const FEATURE: &str = "translate_pages";
pub const NAME: &str = "Translate Pages";
/// Visible text a probe asks the page for, in UTF-16 units.
pub const SAMPLE_UNITS: usize = 300;
/// The most page text that goes to the provider before the user chooses
/// Translate: the language sample, cleaned and cut on a character boundary.
pub const SAMPLE_BYTES: usize = 600;
/// A named language below this confidence counts as unknown.
pub const MIN_CONFIDENCE: f64 = 0.8;
/// One batch's items as JSON, at most; an item larger than this alone is
/// left untranslated.
pub const BATCH_BYTES: usize = 4000;
pub const BATCH_ITEMS: usize = 40;
/// The provider for language samples and batches. A batch's answer stays
/// inside one 16 KiB chat message.
pub const PROVIDER: Checked = Checked { max_tokens: 6144 };
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the address stays put after a same-document change before an
/// Always language reads what the page added: long enough for most pages
/// to show their next part.
const SETTLE: Duration = Duration::from_secs(1);
const WEB_ONLY: &str = "Translate Page works on web pages (http and https) only.";
const DECLINED: &str = "This page asks not to be translated.";
const SPENT: &str = "This page reached the most text Crossbow translates for one page. Reload it to translate it again.";
const LEFT_NOTICE: &str = "You left the page before it was translated.";
/// Documents a tab keeps a full record of for Back and Forward, at most;
/// past them, a document that comes back carries its own (`Earlier`).
const LEFT: usize = 8;

// ---------------------------------------------------------------------------
// Languages
// ---------------------------------------------------------------------------

/// A BCP 47 tag, normalized to its language, script and region: "fr",
/// "pt-BR", "zh-Hant".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Language(String);

const NAMES: &[(&str, &str)] = &[
    ("af", "Afrikaans"), ("ar", "Arabic"), ("bg", "Bulgarian"), ("bn", "Bengali"), ("ca", "Catalan"), ("cs", "Czech"), ("cy", "Welsh"),
    ("da", "Danish"), ("de", "German"), ("el", "Greek"), ("en", "English"), ("es", "Spanish"), ("et", "Estonian"), ("eu", "Basque"),
    ("fa", "Persian"), ("fi", "Finnish"), ("fil", "Filipino"), ("fr", "French"), ("ga", "Irish"), ("gl", "Galician"), ("gu", "Gujarati"),
    ("he", "Hebrew"), ("hi", "Hindi"), ("hr", "Croatian"), ("hu", "Hungarian"), ("hy", "Armenian"), ("id", "Indonesian"), ("is", "Icelandic"),
    ("it", "Italian"), ("ja", "Japanese"), ("ka", "Georgian"), ("kk", "Kazakh"), ("km", "Khmer"), ("kn", "Kannada"), ("ko", "Korean"),
    ("lt", "Lithuanian"), ("lv", "Latvian"), ("mk", "Macedonian"), ("ml", "Malayalam"), ("mn", "Mongolian"), ("mr", "Marathi"), ("ms", "Malay"),
    ("my", "Burmese"), ("nb", "Norwegian"), ("ne", "Nepali"), ("nl", "Dutch"), ("no", "Norwegian"), ("pa", "Punjabi"), ("pl", "Polish"),
    ("pt", "Portuguese"), ("ro", "Romanian"), ("ru", "Russian"), ("si", "Sinhala"), ("sk", "Slovak"), ("sl", "Slovenian"), ("sq", "Albanian"),
    ("sr", "Serbian"), ("sv", "Swedish"), ("sw", "Swahili"), ("ta", "Tamil"), ("te", "Telugu"), ("th", "Thai"), ("tl", "Tagalog"),
    ("tr", "Turkish"), ("uk", "Ukrainian"), ("ur", "Urdu"), ("uz", "Uzbek"), ("vi", "Vietnamese"), ("zh", "Chinese"), ("zu", "Zulu"),
];

impl Language {
    /// Strictly: a 2- or 3-letter language, then optionally a 4-letter
    /// script and a 2-letter or 3-digit region. Variants, extensions and
    /// private use are dropped; undetermined, multiple or no language
    /// ("und", "mul", "zxx", "mis") is refused.
    pub fn parse(raw: &str) -> Option<Language> {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > 35 { return None; }
        let mut parts = raw.split(['-', '_']);
        let language = parts.next()?;
        if !(2..=3).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic()) { return None; }
        let language = language.to_ascii_lowercase();
        if ["und", "mul", "zxx", "mis"].contains(&language.as_str()) { return None; }
        let mut tag = language;
        let (mut script, mut region) = (false, false);
        for part in parts {
            if part.is_empty() || part.len() > 8 || !part.bytes().all(|b| b.is_ascii_alphanumeric()) { return None; }
            // A singleton starts an extension or private use: the rest is dropped.
            if part.len() == 1 { break; }
            if part.len() == 4 && part.bytes().all(|b| b.is_ascii_alphabetic()) && !script && !region {
                script = true;
                tag.push('-');
                tag.push_str(&part[..1].to_ascii_uppercase());
                tag.push_str(&part[1..].to_ascii_lowercase());
            } else if ((part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic())) || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))) && !region {
                region = true;
                tag.push('-');
                tag.push_str(&part.to_ascii_uppercase());
            } else if part.len() < 4 {
                // An extended language subtag or a malformed one.
                return None;
            }
        }
        Some(Language(tag))
    }

    /// A `Content-Language` value: one language, or a list whose languages
    /// all share one primary language. A list of several is no answer.
    pub fn from_header(value: &str) -> Option<Language> {
        let mut found: Option<Language> = None;
        for part in value.split(',') {
            let language = Language::parse(part)?;
            match &found {
                Some(first) if first.primary() != language.primary() => return None,
                Some(_) => {}
                None => found = Some(language),
            }
        }
        found
    }

    pub fn tag(&self) -> &str { &self.0 }

    /// The language subtag, as choices store it.
    pub fn primary(&self) -> &str { self.0.split('-').next().unwrap_or(&self.0) }

    /// The language's English name, or its tag.
    pub fn name(&self) -> String {
        let primary = self.primary();
        let name = NAMES.iter().find(|(code, _)| *code == primary).map(|(_, name)| *name);
        match (name, self.0.split('-').nth(1)) {
            (Some("Chinese"), Some("Hant")) => "Chinese (Traditional)".into(),
            (Some("Chinese"), Some("Hans")) => "Chinese (Simplified)".into(),
            (Some(name), _) => name.into(),
            (None, _) => self.0.clone(),
        }
    }

    /// Whether a page in `self` reads as `other`: the same language, and for
    /// Chinese the same script when both name one.
    pub fn same(&self, other: &Language) -> bool {
        let script = |language: &Language| language.0.split('-').nth(1).filter(|part| part.len() == 4).map(str::to_owned);
        self.primary() == other.primary() && (self.primary() != "zh" || script(self).is_none() || script(other).is_none() || script(self) == script(other))
    }
}

/// The Mac's first preferred language, which every profile translates into:
/// Crossbow has no language setting of its own. English if it can't be read.
pub fn preferred_language() -> Language {
    #[cfg(target_os = "macos")]
    if let Some(language) = mac::first_language().as_deref().and_then(Language::parse) { return language; }
    Language("en".into())
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CStr, c_char, c_void};

    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFStringGetCString(string: *const c_void, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(value: *const c_void);
    }

    pub(super) fn first_language() -> Option<String> {
        let mut buffer = [0 as c_char; 64];
        // SAFETY: the array is created here and released once; its values are
        // CFStrings it owns, read into a buffer of the size given.
        unsafe {
            let languages = CFLocaleCopyPreferredLanguages();
            if languages.is_null() { return None; }
            let read = CFArrayGetCount(languages) > 0 && CFStringGetCString(CFArrayGetValueAtIndex(languages, 0), buffer.as_mut_ptr(), buffer.len() as isize, UTF8) != 0;
            CFRelease(languages);
            read.then(|| CStr::from_ptr(buffer.as_ptr()).to_string_lossy().into_owned())
        }
    }
}

// ---------------------------------------------------------------------------
// The provider contract
// ---------------------------------------------------------------------------

/// The answer inside one optional Markdown code fence, which small models
/// add on their own; nothing else around it is accepted.
fn unfenced(answer: &str) -> &str {
    let answer = answer.trim();
    let Some(inner) = answer.strip_prefix("```").and_then(|rest| rest.strip_suffix("```")) else { return answer };
    let (first, rest) = inner.split_once('\n').unwrap_or(("", inner));
    if first.trim().is_empty() || first.trim().eq_ignore_ascii_case("json") { rest.trim() } else { answer }
}

/// The language prompt: the sample, cleaned and cut to [`SAMPLE_BYTES`].
pub fn detect_prompt(sample: &str) -> Option<String> {
    let sample = crate::ask_page::extract(sample, false, SAMPLE_BYTES).text;
    (!sample.is_empty()).then(|| format!(
        "Which language is the text below written in? Reply with only a JSON object {{\"language\":\"<BCP 47 code>\",\"confidence\":<number from 0 to 1>}}.\n\nText:\n{sample}"
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detection {
    language: String,
    confidence: f64,
}

/// The provider's typed answer to [`detect_prompt`].
pub fn parse_detection(answer: &str) -> Result<Language, &'static str> {
    let detection: Detection = serde_json::from_str(unfenced(answer)).map_err(|_| "The answer was not the expected JSON.")?;
    if !detection.confidence.is_finite() || !(0.0..=1.0).contains(&detection.confidence) { return Err("The confidence was out of range."); }
    if detection.confidence < MIN_CONFIDENCE { return Err("The provider was not sure of the language."); }
    Language::parse(&detection.language).ok_or("The answer did not name a language.")
}

/// Numbered items that go out together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub items: Vec<Segment>,
}

#[derive(Serialize)]
struct Item<'a> {
    id: u32,
    text: &'a str,
}

fn item_bytes(segment: &Segment) -> usize {
    serde_json::to_string(&Item { id: segment.id, text: &segment.text }).map_or(usize::MAX, |json| json.len())
}

/// The page's text in document order, in batches of at most [`BATCH_ITEMS`]
/// items and [`BATCH_BYTES`] of JSON, and how many items were too large to send.
pub fn batches(segments: &[Segment]) -> (Vec<Batch>, usize) {
    let (mut batches, mut current, mut bytes, mut skipped) = (Vec::new(), Vec::new(), 0, 0);
    for segment in segments {
        let size = item_bytes(segment) + 1;
        if size > BATCH_BYTES {
            skipped += 1;
            continue;
        }
        if !current.is_empty() && (current.len() == BATCH_ITEMS || bytes + size > BATCH_BYTES) {
            batches.push(Batch { items: std::mem::take(&mut current) });
            bytes = 0;
        }
        bytes += size;
        current.push(segment.clone());
    }
    if !current.is_empty() { batches.push(Batch { items: current }); }
    (batches, skipped)
}

pub fn translate_prompt(batch: &Batch, target: &Language) -> String {
    let items: Vec<Item> = batch.items.iter().map(|segment| Item { id: segment.id, text: &segment.text }).collect();
    format!(
        "Translate the \"text\" of each item into {} ({}). Keep names, numbers, addresses and symbols as they are. Reply with only a JSON object {{\"translations\":[{{\"id\":<id>,\"text\":\"<translation>\"}}]}}: one entry for each item, with the same id, in plain text, without markup or notes.\n\nItems:\n{}",
        target.name(), target.tag(), serde_json::to_string(&items).unwrap_or_default(),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    translations: Vec<Translated>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Translated {
    id: u32,
    text: String,
}

fn bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A translation as written into the page: trimmed, one plain run of text no
/// longer than four times its source, with no control characters but tabs
/// and line breaks, and bidirectional controls only where the source had them.
fn usable(source: &str, text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > source.len() * 4 + 64 { return None; }
    if text.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || (bidi_control(c) && !source.contains(bidi_control))) { return None; }
    Some(text.to_owned())
}

/// The provider's answer to [`translate_prompt`] for `batch`: the
/// translation of each item it answered correctly, in the batch's order.
/// Invented ids are ignored; an id answered twice, or with unusable text, is
/// left out, so its node keeps its text. Err when the answer isn't the
/// contract at all.
pub fn parse_translations(answer: &str, batch: &Batch) -> Result<Vec<(u32, String)>, &'static str> {
    let answer: Answer = serde_json::from_str(unfenced(answer)).map_err(|_| "The answer was not the expected JSON.")?;
    let mut found: HashMap<u32, Option<String>> = HashMap::new();
    for translated in answer.translations {
        let Some(source) = batch.items.iter().find(|item| item.id == translated.id) else { continue };
        let usable = usable(&source.text, &translated.text);
        found.entry(translated.id).and_modify(|kept| *kept = None).or_insert(usable);
    }
    Ok(batch.items.iter().filter_map(|item| found.remove(&item.id).flatten().map(|text| (item.id, text))).collect())
}

// ---------------------------------------------------------------------------
// Choices
// ---------------------------------------------------------------------------

/// Never and Always, per profile, on this device only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choices {
    /// Hosts, as `search::host_of` gives them.
    #[serde(default)]
    pub never_sites: BTreeSet<String>,
    /// Language subtags.
    #[serde(default)]
    pub never_languages: BTreeSet<String>,
    #[serde(default)]
    pub always_languages: BTreeSet<String>,
}

impl Choices {
    pub fn count(&self) -> usize {
        self.never_sites.len() + self.never_languages.len() + self.always_languages.len()
    }
}

fn choices_key(profile: &str) -> String { format!("translate.choices.v1.{profile}") }

/// This profile's choices. An unreadable or unknown record is an error, so a
/// Never is never silently lost.
pub fn choices(lib: &Library, profile: &str) -> Result<Choices, String> {
    const UNREADABLE: &str = "Could not read your translation choices. Reopen Settings and retry.";
    let raw = lib.store.kv_get(&choices_key(profile)).map_err(|_| UNREADABLE.to_string())?;
    raw.map_or(Ok(Choices::default()), |raw| serde_json::from_str(&raw).map_err(|_| UNREADABLE.to_string()))
}

fn save_choices(lib: &Library, profile: &str, choices: &Choices) -> Result<(), String> {
    const UNSAVED: &str = "Could not save this choice. Nothing changed. Check disk space and permissions, then try again.";
    let raw = serde_json::to_string(choices).map_err(|_| UNSAVED.to_string())?;
    lib.store.kv_set(&choices_key(profile), &raw).map_err(|_| UNSAVED.to_string())
}

/// Forget every Never and Always of this profile. On failure they all stay.
pub fn reset_choices(lib: &Library, profile: &str) -> Result<(), String> {
    lib.store.kv_delete(&choices_key(profile))
        .map_err(|_| "Could not reset translation choices. They are unchanged. Check disk space and permissions, then try again.".to_string())
}

/// Read, change and save this profile's choices; on any failure nothing changes.
fn change(lib: &Library, profile: &str, edit: impl FnOnce(&mut Choices)) -> Result<(), String> {
    let mut choices = choices(lib, profile)?;
    edit(&mut choices);
    save_choices(lib, profile, &choices)
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// Which document a translation belongs to: a tab's view and profile, and
/// the view's page loads so far. A load or reload starts a new document; an
/// address change within one, by history.pushState or a #fragment link,
/// doesn't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub tab: String,
    pub view: u64,
    pub loads: u64,
    pub profile: String,
}

/// One tab's page, as the translator needs it. Tests substitute a fake.
pub trait Page {
    fn owner(&self) -> Owner;
    /// The view's address changes and loads so far: a change while the
    /// owner stays is a same-document navigation.
    fn navigations(&self) -> u64;
    fn url(&self) -> String;
    fn loading(&self) -> bool;
    /// False when the page couldn't be reached.
    fn send(&self, command: &TranslateCommand) -> bool;
    fn content_language(&self) -> Option<String>;
}

pub struct TabPage<'a> {
    pub tab: &'a str,
    pub view: &'a crossbow_engine::WebView,
}

impl Page for TabPage<'_> {
    fn owner(&self) -> Owner {
        Owner { tab: self.tab.into(), view: self.view.serial(), loads: self.view.page_loads(), profile: self.view.profile_id().into() }
    }
    fn navigations(&self) -> u64 { self.view.navigations() }
    fn url(&self) -> String { self.view.nav().url }
    fn loading(&self) -> bool { self.view.nav().loading }
    fn send(&self, command: &TranslateCommand) -> bool { self.view.translate(command) }
    fn content_language(&self) -> Option<String> { self.view.content_language() }
}

/// http(s) pages with a host only: never file:, data:, about:, crossbow: or
/// other internal pages.
pub fn web_page(url: &str) -> bool {
    crate::ask_page::supported(url).is_ok()
}

/// The host a Never for this site names.
pub fn site(url: &str) -> Option<String> {
    crossbow_core::search::host_of(url)
}

// ---------------------------------------------------------------------------
// The translator
// ---------------------------------------------------------------------------

pub enum Phase {
    Idle,
    /// A page that isn't read on its own was asked only for its number, in
    /// case it is a document the tab showed a translation in before.
    Asking,
    /// The page was asked for its language and a sample.
    Probing { since: Instant },
    /// The sample went to the provider.
    Detecting { task: Box<dyn Pending> },
    /// Translate this page from {language}?
    Offer,
    /// The page was asked for its text; with `shown`, for text added since
    /// that translation, which stays shown meanwhile.
    Collecting { since: Instant, shown: Option<Progress> },
    Translating(Progress),
    Translated(Progress),
    /// Keep Show Original available until the renderer confirms the restore.
    Restoring { progress: Progress, since: Instant },
    /// A provider error stopped it; what was translated stays. Retry goes on.
    Stopped { progress: Progress, notice: String },
    /// After Show Original.
    Original,
    Notice(String),
}

/// One pass over the page's text.
pub struct Progress {
    pub pass: u64,
    queue: VecDeque<Batch>,
    task: Option<(Box<dyn Pending>, Batch)>,
    pub done: usize,
    pub total: usize,
    /// Items left as they were: too large, unanswered or unusable.
    pub missed: usize,
    /// The page held more text than one pass reads.
    pub truncated: bool,
    /// Writes sent to the page and not yet confirmed, by item count.
    applying: VecDeque<usize>,
    applying_since: Option<Instant>,
}

impl Progress {
    fn cancel(&mut self) {
        if let Some((task, _)) = self.task.take() { task.cancel(); }
    }
}

/// One document of one tab.
pub struct Translation {
    owner: Owner,
    url: String,
    /// The page's navigations when its text was last read, or when this
    /// document was first seen.
    address: u64,
    /// A later address, and when it was first seen.
    moved: Option<(u64, Instant)>,
    /// The number the page's script chose, from its first report.
    page: Option<u64>,
    pub language: Option<Language>,
    settled: bool,
    probed: bool,
    /// Its sample went to the provider: it never goes again.
    sampled: bool,
    /// Nodes and UTF-16 units taken from the page for translation so far,
    /// whatever its script counts.
    taken_nodes: usize,
    taken_units: usize,
    /// Translate was asked for before the language was settled.
    manual: bool,
    pub phase: Phase,
    /// Close: the card stays away for this load.
    pub hidden: bool,
    /// A choice that couldn't be saved.
    pub error: Option<String>,
    /// The finished card offers Always for this language.
    pub offer_always: bool,
}

impl Translation {
    fn new(owner: Owner, url: String, address: u64) -> Self {
        Self { owner, url, address, moved: None, page: None, language: None, settled: false, probed: false, sampled: false, taken_nodes: 0, taken_units: 0, manual: false, phase: Phase::Idle, hidden: false, error: None, offer_always: false }
    }

    /// The page's text, as far as the document's limits go: in order, up to
    /// the first node that would pass them. True when one did.
    fn take(&mut self, segments: Vec<Segment>) -> (Vec<Segment>, bool) {
        let mut taken = Vec::new();
        for segment in segments {
            let units = segment.text.encode_utf16().count();
            if self.taken_nodes + 1 > MAX_TRANSLATE_SEGMENTS || self.taken_units + units > MAX_TRANSLATE_CHARS { return (taken, true); }
            self.taken_nodes += 1;
            self.taken_units += units;
            taken.push(segment);
        }
        (taken, false)
    }

    /// The user went to another document, which the back/forward cache may
    /// keep: nothing more is sent for this one while it is away, and what it
    /// shows stays shown. A sample or text still being read is asked for
    /// again, or offered again, if it comes back.
    fn pause(&mut self) {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Asking | Phase::Probing { .. } => self.probed = false,
            Phase::Detecting { task } => {
                task.cancel();
                self.probed = false;
            }
            Phase::Collecting { shown: None, .. } => self.phase = Phase::Offer,
            Phase::Collecting { shown: Some(progress), .. } => self.phase = Phase::Translated(progress),
            Phase::Translating(mut progress) => {
                if let Some((task, batch)) = progress.task.take() {
                    task.cancel();
                    progress.queue.push_front(batch);
                }
                self.phase = Phase::Translating(progress);
                stop(self, LEFT_NOTICE.into());
            }
            other => self.phase = other,
        }
        // Writes it never confirmed count as left as they were.
        if let Phase::Stopped { progress, .. } = &mut self.phase {
            progress.missed += progress.applying.drain(..).sum::<usize>();
            progress.applying_since = None;
        }
    }

    fn cancel(&mut self) {
        match &mut self.phase {
            Phase::Detecting { task } => task.cancel(),
            Phase::Translating(progress) | Phase::Translated(progress) | Phase::Stopped { progress, .. } | Phase::Restoring { progress, .. } | Phase::Collecting { shown: Some(progress), .. } => progress.cancel(),
            _ => {}
        }
    }

    /// The pass shown on the page; the page restores every pass of a
    /// translation for any one of them.
    fn pass(&self) -> Option<u64> {
        match &self.phase {
            Phase::Translating(progress) | Phase::Translated(progress) | Phase::Stopped { progress, .. } | Phase::Restoring { progress, .. } | Phase::Collecting { shown: Some(progress), .. } => Some(progress.pass),
            _ => None,
        }
    }

    pub fn translated(&self) -> bool { self.pass().is_some() }

    /// A new record's first answer, with what its document did before. A
    /// new document did nothing; one the back/forward cache brought back
    /// after the tab stopped keeping its record goes on from there: its
    /// sample doesn't go again, its totals go on, and the translation it
    /// shows gets Show Original, in the language the page names, if any,
    /// so Always reads what it adds. True for that last: nothing else is
    /// read.
    fn resume(&mut self, earlier: Earlier, named: Option<Language>) -> bool {
        if earlier == Earlier::default() { return false; }
        log::info!("translate: a document the tab no longer kept came back from the back/forward cache");
        self.sampled |= earlier.sampled;
        self.taken_nodes = self.taken_nodes.max(earlier.nodes);
        self.taken_units = self.taken_units.max(earlier.units);
        if earlier.shown == 0 { return false; }
        self.language = named;
        self.settled = true;
        self.phase = Phase::Translated(Progress { pass: earlier.shown, queue: VecDeque::new(), task: None, done: 0, total: 0, missed: 0, truncated: false, applying: VecDeque::new(), applying_since: None });
        true
    }
}

impl Drop for Translation {
    fn drop(&mut self) { self.cancel(); }
}

/// Which button on the card was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Translate,
    NeverSite,
    NeverLanguage,
    Always,
    ShowOriginal,
    Retry,
    Close,
}

/// What the card shows: a title, an optional line, and its buttons, the
/// first of which is the primary one when `primary` says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub title: String,
    pub detail: Option<String>,
    pub buttons: Vec<(Choice, String)>,
    pub primary: bool,
    pub error: Option<String>,
}

pub struct Translator {
    tabs: HashMap<String, Translation>,
    /// Each tab's documents the user went away from, paused, oldest first.
    left: HashMap<String, VecDeque<Translation>>,
    /// Each tab's views and profiles that stopped keeping a document that
    /// showed a translation: their pages are asked for their number.
    forgot: HashMap<String, BTreeSet<(u64, String)>>,
    /// The language pages are translated into.
    pub preferred: Language,
}

impl Translator {
    pub fn new(preferred: Language) -> Self {
        Self { tabs: HashMap::new(), left: HashMap::new(), forgot: HashMap::new(), preferred }
    }

    #[cfg(test)]
    pub fn get(&self, tab: &str) -> Option<&Translation> { self.tabs.get(tab) }

    /// Whether the tab shows a translation, in part or in full.
    pub fn translated(&self, tab: &str) -> bool { self.tabs.get(tab).is_some_and(Translation::translated) }

    /// Drop tabs that closed, cancelling their requests.
    pub fn retain(&mut self, open: impl Fn(&str) -> bool) {
        self.tabs.retain(|tab, _| open(tab));
        self.left.retain(|tab, _| open(tab));
        self.forgot.retain(|tab, _| open(tab));
    }

    /// The tab's state for the document it shows now: a new load starts
    /// over, a same-document change doesn't. The document left is kept,
    /// paused, in case Back or Forward brings it back.
    fn current(&mut self, tab: &str, page: &dyn Page) -> &mut Translation {
        let owner = page.owner();
        if self.tabs.get(tab).is_some_and(|translation| translation.owner != owner) {
            let mut old = self.tabs.remove(tab).expect("checked");
            // Without its number, a document can't be told when it comes back.
            if old.page.is_some() {
                old.pause();
                let left = self.left.entry(tab.to_owned()).or_default();
                left.push_back(old);
                if left.len() > LEFT && let Some(gone) = left.pop_front() && gone.translated() {
                    self.forgot.entry(tab.to_owned()).or_default().insert((gone.owner.view, gone.owner.profile.clone()));
                }
            }
        }
        self.tabs.entry(tab.to_owned()).or_insert_with(|| Translation::new(owner, page.url(), page.navigations()))
    }

    /// Whether the tab left a document of this view and profile that showed
    /// a translation, kept or no longer kept.
    fn left_shown(&self, tab: &str, owner: &Owner) -> bool {
        self.left.get(tab).is_some_and(|left| left.iter().any(|old| old.translated() && old.owner.view == owner.view && old.owner.profile == owner.profile))
            || self.forgot.get(tab).is_some_and(|forgot| forgot.contains(&(owner.view, owner.profile.clone())))
    }

    /// The page's script answered with `number`: if it is a document the
    /// tab left, which the back/forward cache kept, that document's state
    /// comes back as it was left, and a Translate asked for meanwhile goes
    /// on. False for a new document.
    fn returned(&mut self, tab: &str, number: u64, page: &dyn Page, lib: &Library, now: Instant) -> bool {
        let owner = page.owner();
        let Some(left) = self.left.get_mut(tab) else { return false };
        let Some(at) = left.iter().position(|old| old.page == Some(number) && old.owner.view == owner.view && old.owner.profile == owner.profile) else { return false };
        let mut translation = left.remove(at).expect("found");
        log::info!("translate: a document came back from the back/forward cache");
        translation.owner = owner;
        translation.url = page.url();
        translation.address = page.navigations();
        translation.moved = None;
        // Show Original may have reached the page before it was left; the
        // page answers it again either way.
        if let Phase::Restoring { progress, since } = &mut translation.phase {
            *since = now;
            page.send(&TranslateCommand::Restore { pass: progress.pass });
        }
        let manual = self.tabs.insert(tab.to_owned(), translation).is_some_and(|new| new.manual);
        if manual { let _ = self.translate(tab, page, lib, now); }
        true
    }

    /// Once a frame for each tab with a page: start detection when a load
    /// ends, and move requests along.
    pub fn tick(&mut self, tab: &str, page: &dyn Page, lib: &Library, provider: &dyn Provider, ctx: &egui::Context, now: Instant) {
        let preferred = self.preferred.clone();
        if !self.current(tab, page).probed && !page.loading() {
            let left_shown = self.left_shown(tab, &page.owner());
            let translation = self.current(tab, page);
            translation.probed = true;
            // A page read on its own tells its number with its probe; any
            // other is asked for it only if it may be one that showed a
            // translation, so Show Original comes back with it.
            if web_page(&translation.url) && (translation.manual || wanted(lib, &translation.owner.profile, &translation.url)) {
                translation.phase = if page.send(&TranslateCommand::Probe { sample: SAMPLE_UNITS }) { Phase::Probing { since: now } } else { Phase::Idle };
            } else if web_page(&translation.url) && left_shown && page.send(&TranslateCommand::Hello) {
                translation.phase = Phase::Asking;
            }
        }
        let translation = self.current(tab, page);
        // Turning the feature off or changing the provider cancels a request
        // at once, so its answer is never used.
        let pending = matches!(&translation.phase, Phase::Detecting { .. } | Phase::Translating(Progress { task: Some(_), .. }));
        if pending && let Err(error) = ai_features::allowed(lib, &translation.owner.profile, FEATURE, NAME) {
            match std::mem::replace(&mut translation.phase, Phase::Idle) {
                Phase::Detecting { task } => {
                    task.cancel();
                    translation.phase = if translation.manual { Phase::Notice(error) } else { Phase::Idle };
                }
                Phase::Translating(mut progress) => {
                    if let Some((task, batch)) = progress.task.take() {
                        task.cancel();
                        progress.queue.push_front(batch);
                    }
                    translation.phase = Phase::Translating(progress);
                    stop(translation, error);
                }
                other => translation.phase = other,
            }
            return;
        }
        match &mut translation.phase {
            Phase::Probing { since } | Phase::Collecting { since, .. } if now.saturating_duration_since(*since) >= READ_TIMEOUT => {
                match std::mem::replace(&mut translation.phase, Phase::Idle) {
                    Phase::Probing { .. } => settle(translation, None, page, lib, &preferred, now),
                    // What is translated stays; the next change asks again.
                    Phase::Collecting { shown: Some(progress), .. } => translation.phase = Phase::Translated(progress),
                    _ => translation.phase = Phase::Notice("The page did not return its text in time. Reload it, then try again.".into()),
                }
            }
            Phase::Probing { since } | Phase::Collecting { since, .. } => ctx.request_repaint_after(READ_TIMEOUT - now.saturating_duration_since(*since)),
            Phase::Restoring { since, .. } if now.saturating_duration_since(*since) >= READ_TIMEOUT => {
                let Phase::Restoring { progress, .. } = std::mem::replace(&mut translation.phase, Phase::Idle) else { unreachable!() };
                translation.phase = Phase::Translated(progress);
                translation.hidden = false;
                translation.error = Some("The page did not confirm Show Original. Reload it to see the original.".into());
            }
            Phase::Restoring { since, .. } => ctx.request_repaint_after(READ_TIMEOUT - now.saturating_duration_since(*since)),
            Phase::Detecting { task } => {
                let Some(result) = task.poll() else { return };
                let language = result.ok().and_then(|answer| parse_detection(&answer).ok());
                log::info!("translate: detected {}", language.as_ref().map_or("no language", Language::tag));
                settle(translation, language, page, lib, &preferred, now);
            }
            Phase::Translating(progress) | Phase::Stopped { progress, .. } if progress.applying_since.is_some_and(|since| now.saturating_duration_since(since) >= READ_TIMEOUT) => {
                progress.missed += progress.applying.drain(..).sum::<usize>();
                progress.applying_since = None;
                if matches!(translation.phase, Phase::Translating(_)) {
                    stop(translation, "The page did not confirm the translation.".into());
                }
            }
            Phase::Translating(_) => advance(translation, page, lib, provider, &preferred, ctx, now),
            Phase::Translated(_) => moved(translation, page, lib, ctx, now),
            _ => {}
        }
    }

    /// A report from the tab's page.
    pub fn report(&mut self, tab: &str, report: TranslateReport, page: &dyn Page, lib: &Library, provider: &dyn Provider, ctx: &egui::Context, now: Instant) {
        let preferred = self.preferred.clone();
        let translation = self.current(tab, page);
        // Only an answer to what was asked; the first names the document.
        let asked = match (&report, &translation.phase) {
            (TranslateReport::Probe { .. }, Phase::Probing { .. }) | (TranslateReport::Segments { .. }, Phase::Collecting { .. }) | (TranslateReport::Hello { .. }, Phase::Asking) => true,
            (TranslateReport::Declined { .. }, Phase::Probing { .. } | Phase::Collecting { .. }) => true,
            (TranslateReport::Applied { pass, .. }, Phase::Translating(progress) | Phase::Stopped { progress, .. }) => *pass == progress.pass,
            (TranslateReport::Restored { pass, .. }, Phase::Restoring { progress, .. }) => *pass == progress.pass,
            _ => false,
        };
        let first = translation.page.is_none();
        if !asked || *translation.page.get_or_insert(report.page()) != report.page() { return; }
        // A document's first answer; before its sample is used, whether it
        // is one the tab left.
        if matches!(report, TranslateReport::Probe { .. } | TranslateReport::Hello { .. }) && self.returned(tab, report.page(), page, lib, now) { return; }
        let translation = self.current(tab, page);
        // Or one it left longer ago than it keeps a record of, which says
        // what it did itself.
        if let TranslateReport::Probe { earlier, .. } | TranslateReport::Hello { earlier, .. } = &report
            && first
            && translation.resume(*earlier, named(&report, page))
        {
            if translation.manual { let _ = self.translate(tab, page, lib, now); }
            return;
        }
        match report {
            TranslateReport::Hello { .. } => translation.phase = Phase::Idle,
            TranslateReport::Probe { ref sample, .. } => {
                let named = named(&report, page);
                if named.is_some() { return settle(translation, named, page, lib, &preferred, now); }
                let allowed = ai_features::allowed(lib, &translation.owner.profile, FEATURE, NAME);
                let started = match (allowed, detect_prompt(&sample)) {
                    // A document's sample goes once, even when it comes back unsettled.
                    (Ok(ready), Some(prompt)) if !translation.sampled => provider.start(&ready, prompt, ctx).ok(),
                    _ => None,
                };
                match started {
                    Some(task) => {
                        translation.sampled = true;
                        translation.phase = Phase::Detecting { task };
                    }
                    None => settle(translation, None, page, lib, &preferred, now),
                }
            }
            TranslateReport::Segments { pass, truncated, segments, .. } => {
                let Phase::Collecting { shown, .. } = std::mem::replace(&mut translation.phase, Phase::Idle) else { unreachable!() };
                let (segments, cut) = translation.take(segments);
                let truncated = truncated || cut;
                let (queue, skipped) = batches(&segments);
                // Text added to a translated page adds to what its card says.
                let (missed, truncated) = shown.as_ref().map_or((skipped, truncated), |shown| (shown.missed + skipped, shown.truncated || truncated));
                let total = queue.len();
                let progress = Progress { pass, queue: queue.into(), task: None, done: 0, total, missed, truncated, applying: VecDeque::new(), applying_since: None };
                translation.phase = match shown {
                    _ if total > 0 => Phase::Translating(progress),
                    Some(_) => Phase::Translated(progress),
                    None if truncated => Phase::Notice(SPENT.into()),
                    None => Phase::Notice("This page has no text to translate.".into()),
                };
                advance(translation, page, lib, provider, &preferred, ctx, now);
            }
            TranslateReport::Applied { written, skipped, .. } => {
                let (Phase::Translating(progress) | Phase::Stopped { progress, .. }) = &mut translation.phase else { return };
                // The page answers each write in order, for all of its items.
                let (written, skipped) = (written as usize, skipped as usize);
                if progress.applying.front() != Some(&(written + skipped)) { return; }
                progress.applying.pop_front();
                progress.missed += skipped;
                progress.applying_since = (!progress.applying.is_empty()).then_some(now);
                if matches!(translation.phase, Phase::Translating(_)) { advance(translation, page, lib, provider, &preferred, ctx, now); }
            }
            TranslateReport::Restored { .. } => {
                translation.phase = Phase::Original;
                translation.hidden = true;
                translation.error = None;
            }
            // The page's root or body opts out: no offer, and nothing is
            // read. Asked by the user, the card says why.
            TranslateReport::Declined { .. } => {
                log::info!("translate: the page declined");
                translation.phase = match std::mem::replace(&mut translation.phase, Phase::Idle) {
                    Phase::Probing { .. } => {
                        translation.language = None;
                        translation.settled = true;
                        if translation.manual { Phase::Notice(DECLINED.into()) } else { Phase::Idle }
                    }
                    Phase::Collecting { shown: Some(progress), .. } => Phase::Translated(progress),
                    _ => Phase::Notice(DECLINED.into()),
                };
            }
        }
    }

    /// Translate, from the card, the command bar or the tab menu. Err says
    /// why it can't.
    pub fn translate(&mut self, tab: &str, page: &dyn Page, lib: &Library, now: Instant) -> Result<(), String> {
        if !web_page(&page.url()) { return Err(WEB_ONLY.into()); }
        let profile = page.owner().profile;
        if ai_features::private_profile(&profile) { return Err(format!("{NAME} is unavailable in private profiles.")); }
        ai_features::allowed(lib, &profile, FEATURE, NAME)?;
        let translation = self.current(tab, page);
        translation.hidden = false;
        translation.error = None;
        match translation.phase {
            // Asked again: whatever the page added since.
            Phase::Translated(_) => recollect(translation, page, lib, now),
            Phase::Translating(_) | Phase::Stopped { .. } | Phase::Restoring { .. } | Phase::Collecting { .. } => {}
            _ if translation.settled => collect(translation, page, lib, now),
            // Already reading the language: use it when it comes.
            Phase::Probing { .. } | Phase::Detecting { .. } => translation.manual = true,
            _ => {
                translation.manual = true;
                // A load still under way is read when it ends; a finished one,
                // even one that was skipped while the feature was off or the
                // site was a Never, is read now.
                translation.probed = !page.loading();
                if translation.probed {
                    translation.phase = if page.send(&TranslateCommand::Probe { sample: SAMPLE_UNITS }) { Phase::Probing { since: now } } else { Phase::Notice("Crossbow could not reach this page. Reload it, then try again.".into()) };
                }
            }
        }
        Ok(())
    }

    /// Put the page's own text back, once the page confirms it.
    pub fn show_original(&mut self, tab: &str, page: &dyn Page, now: Instant) {
        let translation = self.current(tab, page);
        let Some(pass) = translation.pass() else { return };
        if matches!(translation.phase, Phase::Restoring { .. }) { return; }
        if !page.send(&TranslateCommand::Restore { pass }) {
            translation.hidden = false;
            translation.error = Some("Crossbow could not reach this page. Reload it to see the original.".into());
            return;
        }
        translation.cancel();
        let progress = match std::mem::replace(&mut translation.phase, Phase::Idle) {
            Phase::Translating(progress) | Phase::Translated(progress) | Phase::Stopped { progress, .. } | Phase::Collecting { shown: Some(progress), .. } => progress,
            _ => unreachable!(),
        };
        translation.phase = Phase::Restoring { progress, since: now };
        translation.hidden = false;
        translation.error = None;
    }

    /// A button on the card. Returns a notice for a toast, if any.
    pub fn choose(&mut self, tab: &str, choice: Choice, page: &dyn Page, lib: &Library, provider: &dyn Provider, ctx: &egui::Context, now: Instant) -> Option<String> {
        let preferred = self.preferred.clone();
        let profile = page.owner().profile;
        match choice {
            Choice::Translate => return self.translate(tab, page, lib, now).err(),
            Choice::ShowOriginal => self.show_original(tab, page, now),
            Choice::Close => {
                let translation = self.current(tab, page);
                translation.hidden = true;
                if matches!(translation.phase, Phase::Offer | Phase::Notice(_)) { translation.phase = Phase::Idle; }
            }
            Choice::Retry => {
                let translation = self.current(tab, page);
                if let Phase::Stopped { .. } = translation.phase {
                    let Phase::Stopped { progress, .. } = std::mem::replace(&mut translation.phase, Phase::Idle) else { unreachable!() };
                    translation.phase = Phase::Translating(progress);
                    advance(translation, page, lib, provider, &preferred, ctx, now);
                }
            }
            Choice::NeverSite | Choice::NeverLanguage | Choice::Always => {
                let translation = self.current(tab, page);
                let result = match (&choice, site(&translation.url), translation.language.clone()) {
                    (Choice::NeverSite, Some(host), _) => change(lib, &profile, |choices| { choices.never_sites.insert(host); }),
                    (Choice::NeverLanguage, _, Some(language)) => change(lib, &profile, |choices| {
                        choices.always_languages.remove(language.primary());
                        choices.never_languages.insert(language.primary().to_owned());
                    }),
                    (Choice::Always, _, Some(language)) => change(lib, &profile, |choices| {
                        choices.never_languages.remove(language.primary());
                        choices.always_languages.insert(language.primary().to_owned());
                    }),
                    _ => return None,
                };
                match result {
                    Ok(()) if choice == Choice::Always => {
                        translation.offer_always = false;
                        translation.error = None;
                    }
                    Ok(()) => {
                        translation.phase = Phase::Idle;
                        translation.hidden = true;
                        translation.error = None;
                    }
                    Err(error) => translation.error = Some(error),
                }
            }
        }
        None
    }

    /// What the tab's card shows now, if anything.
    pub fn card(&self, tab: &str) -> Option<Card> {
        let translation = self.tabs.get(tab).filter(|translation| !translation.hidden)?;
        let name = translation.language.as_ref().map(Language::name);
        let from = name.as_ref().map(|name| format!(" from {name}")).unwrap_or_default();
        let close = (Choice::Close, "Close".to_string());
        let original = (Choice::ShowOriginal, "Show Original".to_string());
        let card = |title: String, detail: Option<String>, buttons: Vec<(Choice, String)>, primary: bool| {
            Some(Card { title, detail, buttons, primary, error: translation.error.clone() })
        };
        match &translation.phase {
            Phase::Offer => {
                let mut buttons = vec![(Choice::Translate, "Translate".to_string())];
                // A long host would not fit on its button.
                if let Some(host) = site(&translation.url) { buttons.push((Choice::NeverSite, if host.chars().count() <= 24 { format!("Never for {host}") } else { "Never for This Site".into() })); }
                if let Some(name) = &name { buttons.push((Choice::NeverLanguage, format!("Never for {name}"))); }
                buttons.push(close);
                card(format!("Translate this page{from}?"), None, buttons, true)
            }
            Phase::Collecting { shown: Some(_), .. } => card(format!("Translating{from}…"), None, vec![original, close], false),
            Phase::Collecting { shown: None, .. } => card(format!("Translating{from}…"), None, vec![close], false),
            Phase::Restoring { .. } => card("Showing the original…".into(), None, vec![close], false),
            Phase::Translating(progress) => card(format!("Translating{from}…"), Some(format!("{} of {} parts", progress.done, progress.total)), vec![original, close], false),
            Phase::Translated(progress) => {
                let mut buttons = vec![original];
                let always = translation.language.as_ref().filter(|_| translation.offer_always);
                if let Some(language) = always { buttons.push((Choice::Always, format!("Always Translate {}", language.name()))); }
                buttons.push(close);
                card(format!("Translated{from}"), notes(progress), buttons, false)
            }
            Phase::Stopped { progress, notice } => {
                let detail = format!("{notice} What was translated stays.{}", notes(progress).map(|notes| format!(" {notes}")).unwrap_or_default());
                card("Translation stopped".into(), Some(detail), vec![(Choice::Retry, "Retry".into()), original, close], true)
            }
            Phase::Notice(text) => card(text.clone(), None, vec![close], false),
            Phase::Idle | Phase::Asking | Phase::Probing { .. } | Phase::Detecting { .. } | Phase::Original => None,
        }
    }
}

fn notes(progress: &Progress) -> Option<String> {
    let mut notes = Vec::new();
    if progress.missed > 0 { notes.push(format!("{} {} left untranslated.", progress.missed, if progress.missed == 1 { "passage was" } else { "passages were" })); }
    if progress.truncated { notes.push("Only the first part of this long page was translated.".to_string()); }
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// Whether this profile turned the feature on (one store read).
pub fn enabled(lib: &Library, profile: &str) -> bool {
    matches!(ai_features::consented(lib, profile, FEATURE), Ok(Some(_)))
}

/// Whether a load should look for its language: the feature is on with the
/// current provider, the choices read, and the site isn't a Never.
fn wanted(lib: &Library, profile: &str, url: &str) -> bool {
    if ai_features::private_profile(profile) || ai_features::allowed(lib, profile, FEATURE, NAME).is_err() { return false; }
    let Ok(choices) = choices(lib, profile) else { return false };
    site(url).is_some_and(|host| !choices.never_sites.contains(&host))
}

/// The language is as known as it gets: offer, translate or stay quiet.
/// The language a probe says its page is in, by the root's lang attribute
/// or else the response's Content-Language; none for any other report.
fn named(report: &TranslateReport, page: &dyn Page) -> Option<Language> {
    let TranslateReport::Probe { lang, .. } = report else { return None };
    Language::parse(lang).or_else(|| page.content_language().as_deref().and_then(Language::from_header))
}

fn settle(translation: &mut Translation, language: Option<Language>, page: &dyn Page, lib: &Library, preferred: &Language, now: Instant) {
    translation.language = language;
    translation.settled = true;
    translation.phase = Phase::Idle;
    if translation.manual { return collect(translation, page, lib, now); }
    let Some(language) = &translation.language else { return };
    if language.same(preferred) || !wanted(lib, &translation.owner.profile, &translation.url) { return; }
    let Ok(choices) = choices(lib, &translation.owner.profile) else { return };
    if choices.never_languages.contains(language.primary()) { return; }
    if choices.always_languages.contains(language.primary()) {
        log::info!("translate: always {} to {}", language.tag(), preferred.tag());
        return collect(translation, page, lib, now);
    }
    translation.phase = Phase::Offer;
}

/// Ask the page for its text: the user agreed, or chose Always.
fn collect(translation: &mut Translation, page: &dyn Page, lib: &Library, now: Instant) {
    translation.cancel();
    translation.address = page.navigations();
    translation.moved = None;
    translation.phase = match ai_features::allowed(lib, &translation.owner.profile, FEATURE, NAME) {
        Err(error) => Phase::Notice(error),
        Ok(_) if page.send(&TranslateCommand::Collect) => Phase::Collecting { since: now, shown: None },
        Ok(_) => Phase::Notice("Crossbow could not reach this page. Reload it, then try again.".into()),
    };
}

/// Ask a translated page for the text it added since: the page reads only
/// nodes this translation hasn't, within what is left of the document's
/// limits. The translation stays shown, with Show Original, meanwhile; if
/// the page can't be reached or the feature is off, it stays as it is.
fn recollect(translation: &mut Translation, page: &dyn Page, lib: &Library, now: Instant) {
    translation.address = page.navigations();
    translation.moved = None;
    if !matches!(translation.phase, Phase::Translated(_)) || ai_features::allowed(lib, &translation.owner.profile, FEATURE, NAME).is_err() { return; }
    if !page.send(&TranslateCommand::Collect) { return; }
    let Phase::Translated(progress) = std::mem::replace(&mut translation.phase, Phase::Idle) else { unreachable!() };
    translation.phase = Phase::Collecting { since: now, shown: Some(progress) };
}

/// A translated page whose address changed within the document: once the
/// address has stayed put for [`SETTLE`], read what the page added if its
/// language is one the user chose Always for. Otherwise nothing more is read.
fn moved(translation: &mut Translation, page: &dyn Page, lib: &Library, ctx: &egui::Context, now: Instant) {
    let address = page.navigations();
    if address == translation.address { return; }
    let since = match translation.moved {
        Some((seen, since)) if seen == address => since,
        _ => {
            translation.moved = Some((address, now));
            now
        }
    };
    let waited = now.saturating_duration_since(since);
    if waited < SETTLE { return ctx.request_repaint_after(SETTLE - waited); }
    if page.loading() { return; }
    let always = translation.language.as_ref().is_some_and(|language| {
        wanted(lib, &translation.owner.profile, &translation.url) && choices(lib, &translation.owner.profile).is_ok_and(|choices| choices.always_languages.contains(language.primary()))
    });
    if always {
        log::info!("translate: always, reading what the page added");
        return recollect(translation, page, lib, now);
    }
    translation.address = address;
    translation.moved = None;
}

/// Poll the batch under way, write its answer, and start the next. A
/// provider error stops the pass; an answer that isn't the contract leaves
/// its batch untranslated and the pass goes on.
fn advance(translation: &mut Translation, page: &dyn Page, lib: &Library, provider: &dyn Provider, preferred: &Language, ctx: &egui::Context, now: Instant) {
    let profile = translation.owner.profile.clone();
    let source = translation.language.as_ref().map_or("unknown", Language::tag).to_owned();
    let Phase::Translating(progress) = &mut translation.phase else { return };
    loop {
        if let Some((task, batch)) = &progress.task {
            let Some(result) = task.poll() else { return };
            let batch = batch.clone();
            progress.task = None;
            match result {
                Ok(answer) => {
                    let texts = parse_translations(&answer, &batch).unwrap_or_default();
                    progress.missed += batch.items.len() - texts.len();
                    progress.done += 1;
                    if !texts.is_empty() {
                        let count = texts.len();
                        if !page.send(&TranslateCommand::Apply { pass: progress.pass, texts }) {
                            progress.missed += count;
                            return stop(translation, "Crossbow could not reach this page. Reload it, then try again.".into());
                        }
                        progress.applying.push_back(count);
                        progress.applying_since.get_or_insert(now);
                    }
                }
                Err(error) => {
                    progress.queue.push_front(batch);
                    return stop(translation, error.to_string());
                }
            }
        }
        if progress.queue.is_empty() && !progress.applying.is_empty() { return; }
        let Some(batch) = progress.queue.pop_front() else {
            log::info!("translate: {source} to {} done", preferred.tag());
            let Phase::Translating(progress) = std::mem::replace(&mut translation.phase, Phase::Idle) else { unreachable!() };
            translation.phase = Phase::Translated(progress);
            // Read once: the card asks only for a language not already Always.
            translation.offer_always = translation.language.as_ref().is_some_and(|language| {
                choices(lib, &profile).is_ok_and(|choices| !choices.always_languages.contains(language.primary()))
            });
            return;
        };
        // The switch or the provider may have changed since the pass began.
        let ready = match ai_features::allowed(lib, &profile, FEATURE, NAME) {
            Ok(ready) => ready,
            Err(error) => {
                progress.queue.push_front(batch);
                return stop(translation, error);
            }
        };
        match provider.start(&ready, translate_prompt(&batch, preferred), ctx) {
            Ok(task) => progress.task = Some((task, batch)),
            Err(error) => {
                progress.queue.push_front(batch);
                return stop(translation, error.to_string());
            }
        }
    }
}

fn stop(translation: &mut Translation, notice: String) {
    let Phase::Translating(progress) = std::mem::replace(&mut translation.phase, Phase::Idle) else { return };
    translation.phase = Phase::Stopped { progress, notice };
    translation.hidden = false;
}

#[cfg(test)]
#[path = "translate_tests.rs"]
pub(crate) mod tests;
