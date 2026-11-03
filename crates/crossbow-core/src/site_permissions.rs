//! Site permissions and links to other apps: what a page may ask for, how a
//! request is worded, and what each profile decided. Decisions live in the
//! library database's `site_permissions` table, on this device only; the sync
//! client never sees them.
//!
//! A decision is keyed by the profile, the requesting origin, the top-level
//! origin it was asked under (the same origin for a top-level page) and the
//! permission. So a site's grant as a top-level page never reaches a frame of
//! it embedded in another site, and a frame's grant applies only under the
//! same top-level site. "Ask" is the absence of a record.

use crate::store::Store;
use rusqlite::{OptionalExtension, params};

/// What a page can ask for. Requests for anything else are refused without a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    Camera,
    Microphone,
    ScreenShare,
    Location,
    Notifications,
    Clipboard,
    MultipleDownloads,
    Midi,
    WindowManagement,
    LocalFonts,
    IdleDetection,
    StorageAccess,
    MotionSensors,
    LocalNetwork,
    FileEditing,
    ProtocolHandlers,
    PointerLock,
    KeyboardLock,
}

impl Permission {
    pub const ALL: [Permission; 18] = [
        Permission::Camera, Permission::Microphone, Permission::ScreenShare, Permission::Location, Permission::Notifications, Permission::Clipboard,
        Permission::MultipleDownloads, Permission::Midi, Permission::WindowManagement, Permission::LocalFonts, Permission::IdleDetection,
        Permission::StorageAccess, Permission::MotionSensors, Permission::LocalNetwork, Permission::FileEditing, Permission::ProtocolHandlers,
        Permission::PointerLock, Permission::KeyboardLock,
    ];

    /// The stored name.
    pub fn key(self) -> &'static str {
        match self {
            Permission::Camera => "camera",
            Permission::Microphone => "microphone",
            Permission::ScreenShare => "screen-share",
            Permission::Location => "location",
            Permission::Notifications => "notifications",
            Permission::Clipboard => "clipboard",
            Permission::MultipleDownloads => "multiple-downloads",
            Permission::Midi => "midi",
            Permission::WindowManagement => "window-management",
            Permission::LocalFonts => "local-fonts",
            Permission::IdleDetection => "idle-detection",
            Permission::StorageAccess => "storage-access",
            Permission::MotionSensors => "motion-sensors",
            Permission::LocalNetwork => "local-network",
            Permission::FileEditing => "file-editing",
            Permission::ProtocolHandlers => "protocol-handlers",
            Permission::PointerLock => "pointer-lock",
            Permission::KeyboardLock => "keyboard-lock",
        }
    }

    pub fn from_key(key: &str) -> Option<Permission> {
        Permission::ALL.into_iter().find(|permission| permission.key() == key)
    }

    /// Its name in Site Settings.
    pub fn label(self) -> &'static str {
        match self {
            Permission::Camera => "Camera",
            Permission::Microphone => "Microphone",
            Permission::ScreenShare => "Screen sharing",
            Permission::Location => "Location",
            Permission::Notifications => "Notifications",
            Permission::Clipboard => "Clipboard",
            Permission::MultipleDownloads => "Multiple downloads",
            Permission::Midi => "MIDI devices",
            Permission::WindowManagement => "Window management",
            Permission::LocalFonts => "Fonts",
            Permission::IdleDetection => "Idle detection",
            Permission::StorageAccess => "Cookies on other sites",
            Permission::MotionSensors => "Motion sensors",
            Permission::LocalNetwork => "Local network",
            Permission::FileEditing => "File editing",
            Permission::ProtocolHandlers => "Link handlers",
            Permission::PointerLock => "Pointer lock",
            Permission::KeyboardLock => "Keyboard lock",
        }
    }

    /// What a prompt says the site wants to do.
    pub fn phrase(self) -> &'static str {
        match self {
            Permission::Camera => "use your camera",
            Permission::Microphone => "use your microphone",
            Permission::ScreenShare => "share your screen",
            Permission::Location => "know your location",
            Permission::Notifications => "show notifications",
            Permission::Clipboard => "see text and images you copy",
            Permission::MultipleDownloads => "download multiple files",
            Permission::Midi => "control your MIDI devices",
            Permission::WindowManagement => "place windows on all your displays",
            Permission::LocalFonts => "use the fonts on this Mac",
            Permission::IdleDetection => "know when you're using this Mac",
            Permission::StorageAccess => "use its cookies on this site",
            Permission::MotionSensors => "use your motion sensors",
            Permission::LocalNetwork => "connect to devices on your local network",
            Permission::FileEditing => "edit files on this Mac",
            Permission::ProtocolHandlers => "open a type of link",
            Permission::PointerLock => "hide and lock your pointer",
            Permission::KeyboardLock => "capture your keyboard shortcuts",
        }
    }

    /// Chromium gives these only to secure pages; Crossbow never asks for
    /// them from an http page other than localhost.
    pub fn needs_secure_origin(self) -> bool {
        matches!(self, Permission::Camera | Permission::Microphone | Permission::ScreenShare | Permission::Location)
    }
}

/// "use your camera and microphone", "know your location and show notifications".
pub fn phrase(permissions: &[Permission]) -> String {
    if permissions == [Permission::Camera, Permission::Microphone] { return "use your camera and microphone".into(); }
    let phrases: Vec<&str> = permissions.iter().map(|permission| permission.phrase()).collect();
    match phrases.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}

impl Decision {
    fn key(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Block => "block",
        }
    }

    fn from_key(key: &str) -> Option<Decision> {
        match key {
            "allow" => Some(Decision::Allow),
            "block" => Some(Decision::Block),
            _ => None,
        }
    }
}

/// The origin of an http(s) URL, `scheme://host[:port]`, as Chromium writes it.
pub fn origin_of(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) { return None; }
    Some(url.origin().ascii_serialization())
}

/// An https origin, or an http one on this Mac: `localhost`, a `.localhost`
/// name, `127.0.0.0/8` or `[::1]`.
pub fn is_secure_origin(origin: &str) -> bool {
    let Ok(url) = url::Url::parse(origin) else { return false };
    match (url.scheme(), url.host()) {
        ("https", Some(_)) => true,
        ("http", Some(url::Host::Domain(host))) => host == "localhost" || host.ends_with(".localhost"),
        ("http", Some(url::Host::Ipv4(ip))) => ip.is_loopback(),
        ("http", Some(url::Host::Ipv6(ip))) => ip.is_loopback(),
        _ => false,
    }
}

// -- links to other apps -----------------------------------------------------------------------

/// The scheme of a link, lowercased, if it is well formed.
pub fn link_scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// Schemes never handed to another app, whatever opens them, with why.
pub const REFUSED_SCHEMES: [(&str, &str); 17] = [
    ("file", "opens files and apps on this Mac"),
    ("javascript", "runs script"),
    ("vbscript", "runs script"),
    ("data", "is page content, not a link to an app"),
    ("blob", "is page content, not a link to an app"),
    ("x-apple.systempreferences", "opens System Settings"),
    ("x-apple-systempreferences", "opens System Settings"),
    ("applescript", "opens a script in Script Editor"),
    ("shortcuts", "runs shortcuts"),
    ("workflow", "runs shortcuts"),
    ("x-man-page", "opens Terminal"),
    ("ssh", "opens a terminal session"),
    ("telnet", "opens a terminal session"),
    ("help", "help pages have run scripts"),
    ("itms-services", "installs apps"),
    ("smb", "mounts a network share"),
    ("afp", "mounts a network share"),
];

/// Apps that never receive a link from a page, by bundle id, with why.
pub const REFUSED_HANDLERS: [(&str, &str); 17] = [
    ("com.apple.Terminal", "runs commands"),
    ("com.googlecode.iterm2", "runs commands"),
    ("dev.warp.Warp-Stable", "runs commands"),
    ("com.mitchellh.ghostty", "runs commands"),
    ("io.alacritty", "runs commands"),
    ("net.kovidgoyal.kitty", "runs commands"),
    ("com.github.wez.wezterm", "runs commands"),
    ("com.apple.ScriptEditor2", "runs scripts"),
    ("com.apple.Automator", "runs scripts"),
    ("com.apple.shortcuts", "runs shortcuts"),
    ("com.apple.installer", "installs apps"),
    ("com.apple.AppStore", "installs apps"),
    ("com.apple.systempreferences", "changes system settings"),
    ("com.apple.Settings", "changes system settings"),
    ("com.apple.finder", "opens files and mounts network shares"),
    ("com.apple.helpviewer", "help pages have run scripts"),
    ("com.apple.ScreenSharing", "controls another computer"),
];

/// Why a link of `scheme` is never handed to another app, if it isn't.
pub fn refused_scheme(scheme: &str) -> Option<&'static str> {
    REFUSED_SCHEMES.iter().find(|(refused, _)| scheme.eq_ignore_ascii_case(refused)).map(|(_, why)| *why)
}

/// Why the app with `bundle_id` never receives a link from a page, if it doesn't.
pub fn refused_handler(bundle_id: &str) -> Option<&'static str> {
    REFUSED_HANDLERS.iter().find(|(refused, _)| bundle_id.eq_ignore_ascii_case(refused)).map(|(_, why)| *why)
}

// -- stored decisions --------------------------------------------------------------------------

/// Links of a scheme a site may always hand to its app.
fn open_key(scheme: &str) -> String {
    format!("open:{scheme}")
}

/// One remembered decision, as Site Settings lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Remembered {
    Permission(Permission, Decision),
    /// "Always allow {site} to open links of this type".
    OpenLinks(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteRecord {
    pub origin: String,
    /// The top-level origin a frame asked under; equal to `origin` for a page.
    pub top_origin: String,
    pub remembered: Remembered,
}

/// What `profile` decided for `permission` on `origin` under `top_origin`.
/// A value this build can't read is Ask.
pub fn decision(store: &Store, profile: &str, origin: &str, top_origin: &str, permission: Permission) -> rusqlite::Result<Option<Decision>> {
    let value: Option<String> = store.connection().query_row(
        "SELECT decision FROM site_permissions WHERE profile_id = ?1 AND origin = ?2 AND top_origin = ?3 AND permission = ?4",
        params![profile, origin, top_origin, permission.key()],
        |row| row.get(0),
    ).optional()?;
    Ok(value.as_deref().and_then(Decision::from_key))
}

/// Remember `decision`, or forget it with None, which is Ask.
pub fn set_decision(store: &Store, profile: &str, origin: &str, top_origin: &str, permission: Permission, decision: Option<Decision>, now_ms: u64) -> rusqlite::Result<()> {
    write(store, profile, origin, top_origin, permission.key(), decision.map(Decision::key), now_ms)
}

/// Remember one decision for several permissions at once: all of them, or
/// none if any write fails.
pub fn set_decisions(store: &Store, profile: &str, origin: &str, top_origin: &str, permissions: &[Permission], decision: Option<Decision>, now_ms: u64) -> rusqlite::Result<()> {
    let transaction = store.connection().unchecked_transaction()?;
    for permission in permissions {
        write(store, profile, origin, top_origin, permission.key(), decision.map(Decision::key), now_ms)?;
    }
    transaction.commit()
}

/// Whether `profile` lets `top_origin` open links of `scheme` without asking.
pub fn opens_links(store: &Store, profile: &str, top_origin: &str, scheme: &str) -> rusqlite::Result<bool> {
    let value: Option<String> = store.connection().query_row(
        "SELECT decision FROM site_permissions WHERE profile_id = ?1 AND origin = ?2 AND top_origin = ?2 AND permission = ?3",
        params![profile, top_origin, open_key(scheme)],
        |row| row.get(0),
    ).optional()?;
    Ok(value.as_deref() == Some(Decision::Allow.key()))
}

pub fn set_opens_links(store: &Store, profile: &str, top_origin: &str, scheme: &str, allowed: bool, now_ms: u64) -> rusqlite::Result<()> {
    write(store, profile, top_origin, top_origin, &open_key(scheme), allowed.then_some(Decision::Allow.key()), now_ms)
}

fn write(store: &Store, profile: &str, origin: &str, top_origin: &str, key: &str, decision: Option<&str>, now_ms: u64) -> rusqlite::Result<()> {
    let conn = store.connection();
    match decision {
        Some(decision) => conn.execute(
            "INSERT INTO site_permissions (profile_id, origin, top_origin, permission, decision, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (profile_id, origin, top_origin, permission) DO UPDATE SET decision = excluded.decision, updated_ms = excluded.updated_ms",
            params![profile, origin, top_origin, key, decision, now_ms as i64],
        )?,
        None => conn.execute(
            "DELETE FROM site_permissions WHERE profile_id = ?1 AND origin = ?2 AND top_origin = ?3 AND permission = ?4",
            params![profile, origin, top_origin, key],
        )?,
    };
    Ok(())
}

/// Everything `profile` remembered, by site and then top-level site. Rows
/// this build can't read are left out.
pub fn records(store: &Store, profile: &str) -> rusqlite::Result<Vec<SiteRecord>> {
    let mut statement = store.connection().prepare(
        "SELECT origin, top_origin, permission, decision FROM site_permissions WHERE profile_id = ?1 ORDER BY origin, top_origin, permission",
    )?;
    let rows = statement.query_map([profile], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)))?;
    let mut records = Vec::new();
    for row in rows {
        let (origin, top_origin, permission, decision) = row?;
        let remembered = match (permission.strip_prefix("open:"), Permission::from_key(&permission), Decision::from_key(&decision)) {
            (Some(scheme), _, Some(Decision::Allow)) if origin == top_origin && link_scheme(&format!("{scheme}:")).as_deref() == Some(scheme) => Remembered::OpenLinks(scheme.to_owned()),
            (None, Some(permission), Some(decision)) => Remembered::Permission(permission, decision),
            _ => continue,
        };
        records.push(SiteRecord { origin, top_origin, remembered });
    }
    Ok(records)
}

/// Forget everything `profile` remembered for `origin`, including as a frame
/// under other sites. Returns how many decisions went.
pub fn reset_site(store: &Store, profile: &str, origin: &str) -> rusqlite::Result<usize> {
    store.connection().execute("DELETE FROM site_permissions WHERE profile_id = ?1 AND origin = ?2", params![profile, origin])
}

/// Forget everything `profile` remembered. Other profiles keep theirs.
pub fn reset_all(store: &Store, profile: &str) -> rusqlite::Result<usize> {
    store.connection().execute("DELETE FROM site_permissions WHERE profile_id = ?1", [profile])
}

#[cfg(test)]
#[path = "site_permissions_tests.rs"]
mod tests;
