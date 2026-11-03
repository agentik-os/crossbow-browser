//! Where New Document goes: a known service's new-document address, an
//! https address the user typed, or nowhere yet. Chosen per profile and kept
//! on this device only.

use crate::store::Store;

/// Longest custom address accepted, in characters.
pub const ADDRESS_MAX: usize = 2048;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DocumentApp {
    /// Not chosen: New Document asks for one.
    #[default]
    None,
    GoogleDocs,
    GoogleSheets,
    GoogleSlides,
    Word,
    Notion,
    /// A checked https address.
    Custom(String),
}

impl DocumentApp {
    /// The services offered by name, in menu order.
    pub const KNOWN: [DocumentApp; 5] = [DocumentApp::GoogleDocs, DocumentApp::GoogleSheets, DocumentApp::GoogleSlides, DocumentApp::Word, DocumentApp::Notion];

    pub fn label(&self) -> &'static str {
        match self {
            DocumentApp::None => "None",
            DocumentApp::GoogleDocs => "Google Docs",
            DocumentApp::GoogleSheets => "Google Sheets",
            DocumentApp::GoogleSlides => "Google Slides",
            DocumentApp::Word => "Microsoft Word",
            DocumentApp::Notion => "Notion",
            DocumentApp::Custom(_) => "Custom",
        }
    }

    /// The address a new document opens at; None until one is chosen.
    pub fn address(&self) -> Option<&str> {
        Some(match self {
            DocumentApp::None => return None,
            DocumentApp::GoogleDocs => "https://docs.new",
            DocumentApp::GoogleSheets => "https://sheets.new",
            DocumentApp::GoogleSlides => "https://slides.new",
            DocumentApp::Word => "https://word.new",
            DocumentApp::Notion => "https://notion.new",
            DocumentApp::Custom(address) => address,
        })
    }

    fn raw(&self) -> String {
        match self {
            DocumentApp::None => "none".into(),
            DocumentApp::GoogleDocs => "google-docs".into(),
            DocumentApp::GoogleSheets => "google-sheets".into(),
            DocumentApp::GoogleSlides => "google-slides".into(),
            DocumentApp::Word => "word".into(),
            DocumentApp::Notion => "notion".into(),
            DocumentApp::Custom(address) => format!("custom:{address}"),
        }
    }

    /// A saved value. A custom address is checked again, so one saved by
    /// any other means is never opened unchecked.
    fn from_raw(raw: &str) -> Option<DocumentApp> {
        if let Some(address) = raw.strip_prefix("custom:") {
            return custom_address(address).ok().filter(|checked| checked == address).map(DocumentApp::Custom);
        }
        std::iter::once(DocumentApp::None).chain(DocumentApp::KNOWN).find(|app| app.raw() == raw)
    }
}

pub const EMPTY: &str = "Enter the address that starts a new document, such as https://example.com/new.";
pub const TOO_LONG: &str = "Use an address of at most 2,048 characters.";
pub const SPACES: &str = "Remove spaces and line breaks from the address.";
pub const UNSAFE: &str = "javascript:, data: and file: addresses can't be used. Use an https address.";
pub const NOT_HTTPS: &str = "Use an https address.";
pub const CREDENTIALS: &str = "Remove the user name and password from the address.";
pub const NOT_ADDRESS: &str = "Enter a full address, such as https://example.com/new.";

/// A typed custom address, checked: https only, with a host, no user name
/// or password, and at most `ADDRESS_MAX` characters before and after it is
/// normalized. Returns the normalized address, or why it was refused.
pub fn custom_address(input: &str) -> Result<String, &'static str> {
    let input = input.trim();
    if input.is_empty() { return Err(EMPTY); }
    if input.chars().count() > ADDRESS_MAX { return Err(TOO_LONG); }
    // The URL parser drops tabs and line breaks silently, so they're refused first.
    if input.chars().any(|c| c.is_whitespace() || c.is_control()) { return Err(SPACES); }
    let lower = input.to_ascii_lowercase();
    if ["javascript:", "data:", "file:"].iter().any(|scheme| lower.starts_with(scheme)) { return Err(UNSAFE); }
    let url = url::Url::parse(input).map_err(|_| NOT_ADDRESS)?;
    if url.scheme() != "https" { return Err(NOT_HTTPS); }
    if !url.username().is_empty() || url.password().is_some() { return Err(CREDENTIALS); }
    if url.host_str().is_none_or(str::is_empty) { return Err(NOT_ADDRESS); }
    let address = url.to_string();
    if address.chars().count() > ADDRESS_MAX { return Err(TOO_LONG); }
    Ok(address)
}

fn key(profile: &str) -> String { format!("document_app.{profile}") }

pub const UNREADABLE: &str = "The saved document app could not be read, so New Document asks for one. Choosing one saves over it.";

/// `profile`'s choice on this device, and why it was not read if it wasn't.
pub fn load(store: &Store, profile: &str) -> (DocumentApp, Option<&'static str>) {
    match store.kv_get(&key(profile)) {
        Ok(None) => (DocumentApp::None, None),
        Ok(Some(raw)) => DocumentApp::from_raw(&raw).map_or((DocumentApp::None, Some(UNREADABLE)), |app| (app, None)),
        Err(_) => (DocumentApp::None, Some(UNREADABLE)),
    }
}

pub fn save(store: &Store, profile: &str, app: &DocumentApp) -> crate::store::Result<()> {
    store.kv_set(&key(profile), &app.raw())
}

#[cfg(test)]
#[path = "document_app_tests.rs"]
mod tests;
