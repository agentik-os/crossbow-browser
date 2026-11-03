//! Synced entity payloads. Every struct here is stored as the JSON payload of a
//! `crossbow_proto::Change` with the matching kind.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Profile {
    pub name: String,
    pub created_ms: u64,
    /// Absent/null preferences inherit the legacy account-wide values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferences: Option<ProfilePreferences>,
}

impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            name: String,
            created_ms: u64,
            preferences: Option<ProfilePreferences>,
        }
        let fields: Fields = deserialize_object(deserializer)?;
        Ok(Profile { name: fields.name, created_ms: fields.created_ms, preferences: fields.preferences })
    }
}

/// Nonsensitive overrides in the existing JSON-synced profile payload. Each
/// absent/null field inherits independently; reading never creates an override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProfilePreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_engine: Option<SearchEngine>,
    /// Hours from ARCHIVE_CHOICES; Some(0) explicitly disables auto-archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_archive_hours: Option<u32>,
}

impl ProfilePreferences {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.auto_archive_hours.is_some_and(|hours| !ARCHIVE_CHOICES.contains(&hours)) {
            return Err("unsupported profile auto-archive duration");
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ProfilePreferences {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            search_engine: Option<SearchEngine>,
            auto_archive_hours: Option<u32>,
        }
        let fields: Fields = deserialize_object(deserializer)?;
        let preferences = ProfilePreferences { search_engine: fields.search_engine, auto_archive_hours: fields.auto_archive_hours };
        preferences.validate().map_err(serde::de::Error::custom)?;
        Ok(preferences)
    }
}

fn deserialize_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> {
    struct ObjectVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }
    // Derived struct deserialization also accepts arrays; malformed profiles or
    // preferences must not silently inherit and re-enable automatic archive.
    deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
}

/// Effective values for one existing profile, including legacy inheritance.
/// Unlike an explicit override, an inherited duration may be a custom value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedProfilePreferences {
    pub search_engine: SearchEngine,
    pub auto_archive_hours: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let s = s.trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

/// A space's colour theme: a 1–3 stop gradient, like Arc's colour picker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub colors: Vec<Rgb>,
    #[serde(default = "default_angle")]
    pub angle_deg: f32,
    /// 0 = washed out, 1 = fully saturated.
    #[serde(default = "default_intensity")]
    pub intensity: f32,
    #[serde(default)]
    pub mode: ThemeMode,
    /// Texture overlay strength, 0–1 (0 = none).
    #[serde(default)]
    pub grain: f32,
    /// Which texture the overlay uses.
    #[serde(default)]
    pub texture: Texture,
}

/// Surface textures for a space background, like Arc's noise styles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Texture {
    #[default]
    Grain,
    Sand,
    Denim,
    Tweed,
}

impl Texture {
    pub const ALL: [Texture; 4] = [Texture::Grain, Texture::Sand, Texture::Denim, Texture::Tweed];

    pub fn label(self) -> &'static str {
        match self {
            Texture::Grain => "Grain",
            Texture::Sand => "Sand",
            Texture::Denim => "Denim",
            Texture::Tweed => "Tweed",
        }
    }
}

fn default_angle() -> f32 {
    135.0
}

fn default_intensity() -> f32 {
    0.65
}

impl Theme {
    pub fn preset(index: usize) -> Theme {
        let presets: [&[Rgb]; 8] = [
            &[Rgb(0x9b, 0x8c, 0xff), Rgb(0xff, 0x9a, 0xc8)],
            &[Rgb(0x55, 0xc2, 0xff), Rgb(0x6e, 0xf0, 0xc0)],
            &[Rgb(0xff, 0xb3, 0x47), Rgb(0xff, 0x6a, 0x6a)],
            &[Rgb(0x3d, 0xd6, 0x8c), Rgb(0x2a, 0x9d, 0xf4)],
            &[Rgb(0xf7, 0x7b, 0xd8), Rgb(0x8f, 0x6b, 0xff), Rgb(0x52, 0xc8, 0xff)],
            &[Rgb(0xe8, 0xe4, 0xdc), Rgb(0xc9, 0xc2, 0xb6)],
            &[Rgb(0x2b, 0x2d, 0x42), Rgb(0x5c, 0x4d, 0x7d)],
            &[Rgb(0xff, 0xd8, 0x6b), Rgb(0xa3, 0xe6, 0x35)],
        ];
        Theme {
            colors: presets[index % presets.len()].to_vec(),
            angle_deg: default_angle(),
            intensity: default_intensity(),
            mode: ThemeMode::System,
            grain: 0.15,
            texture: Texture::Grain,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Space {
    pub name: String,
    /// An emoji, up to two letters, or `lucide:<name>`; see app/src/ui/space_icon.rs.
    pub icon: String,
    pub theme: Theme,
    pub profile_id: String,
    pub order: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub space_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub order: String,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Shown as an icon tile at the top of the sidebar; shared by every space
    /// of the same profile.
    Favorite,
    /// Pinned to one space, above the divider.
    Pinned,
}

/// A saved sidebar entry (favorite or pinned tab).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub profile_id: String,
    /// Set for pinned items.
    pub space_id: Option<String>,
    pub folder_id: Option<String>,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    pub order: String,
    /// For pinned items, the clock of the version of their space whose
    /// profile they took. Profile repair never goes back to an older one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_version: Option<String>,
}

impl Item {
    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().filter(|t| !t.is_empty()).unwrap_or(&self.title)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchEngine {
    #[default]
    Google,
    DuckDuckGo,
    Bing,
    Kagi,
    Ecosia,
    Perplexity,
}

impl SearchEngine {
    pub const ALL: [SearchEngine; 6] = [
        SearchEngine::Google,
        SearchEngine::DuckDuckGo,
        SearchEngine::Bing,
        SearchEngine::Kagi,
        SearchEngine::Ecosia,
        SearchEngine::Perplexity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SearchEngine::Google => "Google",
            SearchEngine::DuckDuckGo => "DuckDuckGo",
            SearchEngine::Bing => "Bing",
            SearchEngine::Kagi => "Kagi",
            SearchEngine::Ecosia => "Ecosia",
            SearchEngine::Perplexity => "Perplexity",
        }
    }

    pub fn search_url(self, query: &str) -> String {
        let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        match self {
            SearchEngine::Google => format!("https://www.google.com/search?q={q}"),
            SearchEngine::DuckDuckGo => format!("https://duckduckgo.com/?q={q}"),
            SearchEngine::Bing => format!("https://www.bing.com/search?q={q}"),
            SearchEngine::Kagi => format!("https://kagi.com/search?q={q}"),
            SearchEngine::Ecosia => format!("https://www.ecosia.org/search?q={q}"),
            SearchEngine::Perplexity => format!("https://www.perplexity.ai/search?q={q}"),
        }
    }

    /// The suggestion endpoint each engine's OpenSearch description names,
    /// for `query`. Perplexity publishes none.
    pub fn suggest_url(self, query: &str) -> Option<String> {
        let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        match self {
            SearchEngine::Google => Some(format!("https://www.google.com/complete/search?client=firefox&q={q}")),
            SearchEngine::DuckDuckGo => Some(format!("https://duckduckgo.com/ac/?q={q}&type=list")),
            SearchEngine::Bing => Some(format!("https://www.bing.com/osjson.aspx?query={q}")),
            SearchEngine::Kagi => Some(format!("https://kagi.com/api/autosuggest?q={q}")),
            SearchEngine::Ecosia => Some(format!("https://ac.ecosia.org/autocomplete?q={q}&type=list")),
            SearchEngine::Perplexity => None,
        }
    }
}

/// Account-wide preferences (one `settings` entity with id `global`). Search
/// and archive values remain the legacy defaults for profiles without overrides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Unpinned tabs untouched this long move to the archive. 0 = never.
    #[serde(default = "default_archive_hours")]
    pub auto_archive_hours: u32,
    #[serde(default)]
    pub search_engine: SearchEngine,
    #[serde(default)]
    pub sidebar_on_right: bool,
    #[serde(default = "yes")]
    pub show_url_in_sidebar: bool,
    #[serde(default = "yes")]
    pub smooth_animations: bool,
    /// Open links clicked in pinned tabs in Peek instead of a new tab.
    #[serde(default = "yes")]
    pub peek_external_links: bool,
    #[serde(default)]
    pub ai_features: bool,
}

fn default_archive_hours() -> u32 {
    12
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_archive_hours: default_archive_hours(),
            search_engine: SearchEngine::default(),
            sidebar_on_right: false,
            show_url_in_sidebar: true,
            smooth_animations: true,
            peek_external_links: true,
            ai_features: false,
        }
    }
}

/// A site customisation ("Boost"): CSS and/or JS injected on matching hosts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Boost {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub css: String,
    #[serde(default)]
    pub js: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Archive windows offered in Settings, in hours (0 = never).
pub const ARCHIVE_CHOICES: [u32; 7] = [1, 6, 12, 24, 24 * 7, 24 * 30, 0];

pub fn archive_label(hours: u32) -> &'static str {
    match hours {
        0 => "Never",
        1 => "1 hour",
        6 => "6 hours",
        12 => "12 hours",
        24 => "24 hours",
        168 => "7 days",
        720 => "30 days",
        _ => "Custom",
    }
}
