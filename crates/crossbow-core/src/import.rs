//! Importers: Arc's `StorableSidebar.json` (spaces, folders, pinned tabs,
//! favorites, themes) and Chromium `Bookmarks` files.
//!
//! Arc doesn't publish its format; the invented fixture in this file's tests
//! shows the parts the importer reads.
//! Parsing is separate from applying so both can be tested with fixtures.

use crate::library::{Library, Result as LibraryResult};
use crate::model::{Rgb, Theme};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Tab { url: String, title: String },
    Folder { name: String, expanded: bool, children: Vec<Node> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArcSpace {
    pub arc_id: String,
    pub title: String,
    pub icon: String,
    pub theme: Option<Theme>,
    /// Chromium profile directory (`Default`, `Profile 1`, ...).
    pub profile_dir: String,
    pub pinned: Vec<Node>,
    pub today: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArcData {
    pub spaces: Vec<ArcSpace>,
    /// Favorites per profile directory, in order.
    pub favorites: Vec<(String, Vec<(String, String)>)>,
    pub skipped: HashMap<String, usize>,
    pub split_views_flattened: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub source: &'static str,
    pub spaces: usize,
    pub folders: usize,
    pub pinned: usize,
    pub favorites: usize,
    pub archived: usize,
    pub already_imported: usize,
    pub notes: Vec<String>,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let n = |v: usize, one: &str, many: &str| format!("{v} {}", if v == 1 { one } else { many });
        if self.spaces > 0 {
            parts.push(n(self.spaces, "space", "spaces"));
        }
        if self.folders > 0 {
            parts.push(n(self.folders, "folder", "folders"));
        }
        if self.pinned > 0 {
            parts.push(n(self.pinned, "pinned tab", "pinned tabs"));
        }
        if self.favorites > 0 {
            parts.push(n(self.favorites, "favorite", "favorites"));
        }
        let mut s = if parts.is_empty() { format!("Nothing new to import from {}.", self.source) } else { format!("Imported {} from {}.", parts.join(", "), self.source) };
        if self.archived > 0 {
            s.push_str(&format!(" {} unpinned tab{} went to the Archive.", self.archived, if self.archived == 1 { "" } else { "s" }));
        }
        if self.already_imported > 0 {
            s.push_str(&format!(" {} space{} had already been imported.", self.already_imported, if self.already_imported == 1 { "" } else { "s" }));
        }
        for note in &self.notes {
            s.push(' ');
            s.push_str(note);
        }
        s
    }
}

// -- Arc --------------------------------------------------------------------------

fn pairs(v: &Value) -> Vec<(&Value, &Value)> {
    v.as_array().map(|a| a.chunks(2).filter(|c| c.len() == 2).map(|c| (&c[0], &c[1])).collect()).unwrap_or_default()
}

fn enum_case(v: &Value) -> Option<&str> {
    v.as_object().and_then(|o| o.keys().next()).map(String::as_str)
}

fn payload<'a>(v: &'a Value, case: &str) -> Option<&'a Value> {
    v.get(case).and_then(|p| p.get("_0"))
}

/// Profile directory of a ProfileRef, or None when it belongs to another Mac.
fn profile_dir(r: &Value, machine_id: Option<&str>) -> Option<String> {
    if r.get("default").is_some() {
        return Some("Default".into());
    }
    let c = payload(r, "custom")?;
    let dir = c.get("directoryBasename")?.as_str()?.to_string();
    match (machine_id, c.get("machineID").and_then(Value::as_str)) {
        (Some(local), Some(m)) if local != m => None,
        _ => Some(dir),
    }
}

fn color(v: &Value) -> Option<Rgb> {
    let f = |k: &str| v.get(k).and_then(Value::as_f64).map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8);
    Some(Rgb(f("red")?, f("green")?, f("blue")?))
}

fn arc_theme(custom: &Value) -> Option<Theme> {
    let style = custom.get("windowTheme")?.get("background")?;
    let single = payload(style, "single")?;
    let color_style = payload(single.get("style")?, "color")?;
    let (colors, modifiers) = if let Some(s) = payload(color_style, "blendedSingleColor") {
        (vec![color(s.get("color")?)?], s.get("modifiers"))
    } else if let Some(g) = payload(color_style, "blendedGradient") {
        let cs: Vec<Rgb> = g.get("baseColors")?.as_array()?.iter().filter_map(color).take(3).collect();
        (cs, g.get("modifiers"))
    } else {
        return None;
    };
    if colors.is_empty() {
        return None;
    }
    let mut t = Theme::preset(0);
    t.colors = colors;
    if let Some(m) = modifiers {
        if let Some(i) = m.get("intensityFactor").and_then(Value::as_f64) {
            t.intensity = i.clamp(0.0, 1.0) as f32;
        }
        if let Some(n) = m.get("noiseFactor").and_then(Value::as_f64) {
            t.grain = n.clamp(0.0, 1.0) as f32;
        }
    }
    Some(t)
}

/// Arc's built-in icon names → a glyph Crossbow can draw.
fn arc_icon(name: &str) -> Option<&'static str> {
    Some(match name {
        "star" => "⭐",
        "bookmark" => "🔖",
        "heart" => "💜",
        "flag" => "🚩",
        "folder" => "📁",
        "code" | "terminal" => "💻",
        "github" => "🐙",
        "home" | "house" => "🏠",
        "briefcase" | "work" => "💼",
        "book" | "books" => "📚",
        "music" | "headphones" => "🎧",
        "camera" | "photo" => "📷",
        "film" | "video" | "tv" => "🎬",
        "game" | "gamecontroller" => "🎮",
        "cart" | "shopping" | "bag" => "🛒",
        "chart" | "graph" | "stocks" => "📈",
        "moon" => "🌙",
        "sun" => "☀",
        "leaf" | "plant" => "🌿",
        "rocket" => "🚀",
        "lightbulb" | "bulb" => "💡",
        "coffee" | "cup" => "☕",
        "paw" | "pet" => "🐾",
        "fire" | "flame" => "🔥",
        "globe" | "world" => "🌍",
        "mail" | "envelope" => "✉",
        "calendar" => "📅",
        "pencil" | "edit" => "✏",
        "palette" | "paint" => "🎨",
        "school" | "graduation" => "🎓",
        "money" | "dollar" => "💰",
        "airplane" | "plane" | "travel" => "🛫",
        _ => return None,
    })
}

fn icon_of(custom: Option<&Value>) -> Option<String> {
    let it = custom?.get("iconType")?;
    if let Some(e) = it.get("emoji").and_then(|e| e.as_str().or_else(|| e.get("_0").and_then(Value::as_str))) {
        return Some(e.to_string());
    }
    let name = it.get("icon").and_then(|e| e.as_str().or_else(|| e.get("_0").and_then(Value::as_str)))?;
    arc_icon(name).map(String::from)
}

/// Parse `StorableSidebar.json`. `machine_id` filters out other Macs' profiles;
/// `expanded` lists folder item ids that are open (from `StorableWindows.json`).
pub fn parse_arc(sidebar_json: &str, machine_id: Option<&str>, expanded: &HashSet<String>) -> Result<ArcData, String> {
    let root: Value = serde_json::from_str(sidebar_json).map_err(|e| format!("Arc's sidebar file couldn't be read ({e})."))?;
    if root.get("version").and_then(Value::as_i64) != Some(1) {
        return Err("This version of Arc's sidebar file isn't supported yet.".into());
    }
    let container = root
        .pointer("/sidebar/containers")
        .and_then(Value::as_array)
        .and_then(|cs| cs.iter().find(|c| c.get("spaces").is_some()))
        .ok_or("Arc's sidebar file has no spaces.")?;
    let items: HashMap<&str, &Value> = pairs(&container["items"]).into_iter().filter_map(|(k, v)| Some((k.as_str()?, v))).collect();
    let mut out = ArcData::default();

    fn walk(id: &str, items: &HashMap<&str, &Value>, expanded: &HashSet<String>, out: &mut ArcData, depth: usize) -> Vec<Node> {
        let mut nodes = Vec::new();
        let Some(item) = items.get(id) else { return nodes };
        let children: Vec<&str> = item["childrenIds"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        for cid in children {
            let Some(child) = items.get(cid) else { continue };
            let data = &child["data"];
            match enum_case(data) {
                Some("tab") => {
                    let t = &data["tab"];
                    let Some(url) = t.get("savedURL").and_then(Value::as_str).filter(|u| u.starts_with("http") || u.starts_with("file:")) else {
                        *out.skipped.entry("unsupported link".into()).or_default() += 1;
                        continue;
                    };
                    let title = child.get("title").and_then(Value::as_str).or_else(|| t.get("savedTitle").and_then(Value::as_str)).unwrap_or("");
                    nodes.push(Node::Tab { url: url.into(), title: title.into() });
                }
                Some("list") if depth < 8 => {
                    let name = child.get("title").and_then(Value::as_str).unwrap_or("Folder").to_string();
                    let kids = walk(cid, items, expanded, out, depth + 1);
                    nodes.push(Node::Folder { name, expanded: expanded.is_empty() || expanded.contains(cid), children: kids });
                }
                Some("splitView") => {
                    out.split_views_flattened += 1;
                    nodes.extend(walk(cid, items, expanded, out, depth + 1));
                }
                Some(other) => *out.skipped.entry(other.to_string()).or_default() += 1,
                None => {}
            }
        }
        nodes
    }

    fn flat_tabs(nodes: Vec<Node>) -> Vec<(String, String)> {
        let mut v = Vec::new();
        for n in nodes {
            match n {
                Node::Tab { url, title } => v.push((url, title)),
                Node::Folder { children, .. } => v.extend(flat_tabs(children)),
            }
        }
        v
    }

    for (sid, space) in pairs(&container["spaces"]) {
        let Some(sid) = sid.as_str() else { continue };
        let mut pinned_id = None;
        let mut today_id = None;
        let new_ids = pairs(&space["newContainerIDs"]);
        let ids = if new_ids.is_empty() { pairs(&space["containerIDs"]) } else { new_ids };
        for (k, v) in ids {
            let key = k.as_str().or_else(|| enum_case(k));
            match key {
                Some("pinned") => pinned_id = v.as_str(),
                Some("unpinned") => today_id = v.as_str(),
                _ => {}
            }
        }
        let profile = profile_dir(&space["profile"], machine_id).unwrap_or_else(|| "Default".into());
        let n = out.spaces.len() + 1;
        let title = space.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| format!("Space {n}"));
        let icon = icon_of(space.get("customInfo")).unwrap_or_else(|| title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "•".into()));
        let theme = space.get("customInfo").and_then(arc_theme);
        let pinned = pinned_id.map(|p| walk(p, &items, expanded, &mut out, 0)).unwrap_or_default();
        let today = today_id.map(|t| flat_tabs(walk(t, &items, expanded, &mut out, 0))).unwrap_or_default();
        out.spaces.push(ArcSpace { arc_id: sid.into(), title, icon, theme, profile_dir: profile, pinned, today });
    }

    // Without the Mac's id, keep only favorites of profiles some space uses.
    let used: HashSet<String> = out.spaces.iter().map(|s| s.profile_dir.clone()).chain(["Default".to_string()]).collect();
    for (pref, cid) in pairs(&container["topAppsContainerIDs"]) {
        let (Some(dir), Some(cid)) = (profile_dir(pref, machine_id), cid.as_str()) else { continue };
        if machine_id.is_none() && !used.contains(&dir) {
            continue;
        }
        let tabs = flat_tabs(walk(cid, &items, expanded, &mut out, 0));
        if tabs.is_empty() {
            continue;
        }
        match out.favorites.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, v)) => v.extend(tabs),
            None => out.favorites.push((dir, tabs)),
        }
    }
    Ok(out)
}

/// Folder ids that are open in any Arc window (`StorableWindows.json`).
pub fn arc_expanded_folders(windows_json: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Ok(v) = serde_json::from_str::<Value>(windows_json) {
        for w in v["windows"].as_array().into_iter().flatten() {
            for (k, state) in pairs(&w["expandedItems"]) {
                if let (Some(k), Some("expanded")) = (k.as_str(), enum_case(state)) {
                    out.insert(k.to_string());
                }
            }
        }
    }
    out
}

/// Chromium `Local State` → profile directory → display name.
pub fn chromium_profile_names(local_state_json: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(v) = serde_json::from_str::<Value>(local_state_json) {
        if let Some(cache) = v.pointer("/profile/info_cache").and_then(Value::as_object) {
            for (dir, info) in cache {
                if let Some(name) = info.get("name").and_then(Value::as_str) {
                    out.insert(dir.clone(), name.to_string());
                }
            }
        }
    }
    out
}

const KV_ARC_SPACE: &str = "import.arc.space.";
const KV_ARC_PROFILE: &str = "import.arc.profile.";
const KV_ARC_FAVORITE: &str = "import.arc.favorite.";
const KV_BOOKMARK_SPACE: &str = "import.bookmarks.space.";

fn add_nodes(lib: &mut Library, space: &str, folder: Option<&str>, nodes: &[Node], r: &mut ImportReport) -> LibraryResult<()> {
    for n in nodes {
        match n {
            Node::Tab { url, title } => {
                lib.add_pinned(space, folder, url, title)?;
                r.pinned += 1;
            }
            Node::Folder { name, expanded, children } => {
                let id = lib.create_folder(space, folder, name)?;
                if !expanded {
                    lib.update_folder(&id, |f| f.collapsed = true)?;
                }
                r.folders += 1;
                add_nodes(lib, space, Some(&id), children, r)?;
            }
        }
    }
    Ok(())
}

fn arc_profile(lib: &mut Library, dir: &str, names: &HashMap<String, String>) -> LibraryResult<String> {
    let key = format!("{KV_ARC_PROFILE}{dir}");
    if let Some(id) = lib.store.kv_get(&key)? {
        if lib.profiles.contains_key(&id) {
            return Ok(id);
        }
    }
    let name = names.get(dir).map(String::as_str).unwrap_or(dir);
    let existing = if dir == "Default" {
        lib.profiles.get_key_value(&lib.default_profile_id()).map(|(id, _)| id.clone())
    } else {
        lib.profiles.iter().find(|(_, p)| p.name == name).map(|(id, _)| id.clone())
    };
    let id = match existing {
        Some(id) => id,
        None => lib.create_profile(if dir == "Default" { "Personal" } else { name })?,
    };
    lib.store.kv_set(&key, &id)?;
    Ok(id)
}

/// Import Arc alongside existing spaces, without changing their customization.
/// A retry skips existing imported spaces and favorites, including renamed ones.
/// Entities, archive entries and retry markers commit together or not at all.
pub fn apply_arc(lib: &mut Library, data: &ArcData, profile_names: &HashMap<String, String>) -> Result<ImportReport, String> {
    lib.transaction(|lib| apply_arc_transaction(lib, data, profile_names)).map_err(|e| e.to_string())
}

fn apply_arc_transaction(lib: &mut Library, data: &ArcData, profile_names: &HashMap<String, String>) -> LibraryResult<ImportReport> {
    let mut report = ImportReport { source: "Arc", ..Default::default() };
    for s in &data.spaces {
        let key = format!("{KV_ARC_SPACE}{}", s.arc_id);
        if let Some(existing) = lib.store.kv_get(&key)? {
            if let Some(space) = lib.spaces.get(&existing) {
                // Keep the profile association for imports made before profile markers existed.
                let profile_key = format!("{KV_ARC_PROFILE}{}", s.profile_dir);
                if lib.store.kv_get(&profile_key)?.is_none() {
                    lib.store.kv_set(&profile_key, &space.profile_id)?;
                }
                report.already_imported += 1;
                continue;
            }
        }
        let profile = arc_profile(lib, &s.profile_dir, profile_names)?;
        let theme = s.theme.clone().unwrap_or_else(|| Theme::preset(lib.spaces.len()));
        let id = lib.create_space(&s.title, &s.icon, theme, &profile)?;
        report.spaces += 1;
        add_nodes(lib, &id, None, &s.pinned, &mut report)?;
        let now = crate::now_ms();
        for (url, title) in &s.today {
            lib.store.archive_tab(&profile, url, title, &id, now)?;
            report.archived += 1;
        }
        lib.store.kv_set(&key, &id)?;
    }

    for (dir, tabs) in &data.favorites {
        if tabs.is_empty() {
            continue;
        }
        let profile = arc_profile(lib, dir, profile_names)?;
        let mut existing: HashMap<String, String> = lib.favorites(&profile).into_iter().map(|(id, i)| (i.url, id)).collect();
        for (url, title) in tabs {
            let key = format!("{KV_ARC_FAVORITE}{}", serde_json::to_string(&(dir, url))?);
            if let Some(id) = lib.store.kv_get(&key)? {
                if lib.items.contains_key(&id) {
                    continue;
                }
            }
            let id = match existing.get(url) {
                Some(id) => id.clone(),
                None => {
                    let id = lib.add_favorite(&profile, url, title)?;
                    existing.insert(url.clone(), id.clone());
                    report.favorites += 1;
                    id
                }
            };
            lib.store.kv_set(&key, &id)?;
        }
    }
    if data.split_views_flattened > 0 {
        report.notes.push(format!("{} split view{} became pinned tabs.", data.split_views_flattened, if data.split_views_flattened == 1 { "" } else { "s" }));
    }
    let skipped: usize = data.skipped.values().sum();
    if skipped > 0 {
        report.notes.push(format!("{skipped} item{} (notes, easels or unsupported links) were skipped.", if skipped == 1 { "" } else { "s" }));
    }
    Ok(report)
}

// -- Chromium bookmarks ----------------------------------------------------------------

/// Parse a Chromium `Bookmarks` file into folders and links.
pub fn parse_chromium_bookmarks(json: &str) -> Result<Vec<Node>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("The bookmarks file couldn't be read ({e})."))?;
    fn conv(n: &Value, depth: usize) -> Option<Node> {
        match n.get("type")?.as_str()? {
            "url" => {
                let url = n.get("url")?.as_str()?;
                if !(url.starts_with("http") || url.starts_with("file:")) {
                    return None;
                }
                Some(Node::Tab { url: url.into(), title: n.get("name").and_then(Value::as_str).unwrap_or("").into() })
            }
            "folder" if depth < 8 => {
                let children: Vec<Node> = n.get("children")?.as_array()?.iter().filter_map(|c| conv(c, depth + 1)).collect();
                Some(Node::Folder { name: n.get("name").and_then(Value::as_str).unwrap_or("Folder").into(), expanded: false, children })
            }
            _ => None,
        }
    }
    let mut out = Vec::new();
    for root in ["bookmark_bar", "other", "synced"] {
        if let Some(r) = v.pointer(&format!("/roots/{root}")) {
            let kids: Vec<Node> = r.get("children").and_then(Value::as_array).map(|a| a.iter().filter_map(|c| conv(c, 0)).collect()).unwrap_or_default();
            if kids.is_empty() {
                continue;
            }
            if root == "bookmark_bar" {
                out.extend(kids);
            } else {
                let name = r.get("name").and_then(Value::as_str).unwrap_or(root).to_string();
                out.push(Node::Folder { name, expanded: false, children: kids });
            }
        }
    }
    Ok(out)
}

/// Import bookmarks once per source/space name, alongside existing spaces.
/// This is a snapshot import, not synchronization: retries leave the imported
/// space untouched. Deleting that space permits a fresh import.
pub fn apply_bookmarks(lib: &mut Library, nodes: &[Node], space_name: &str, source: &'static str) -> Result<ImportReport, String> {
    lib.transaction(|lib| {
        let mut report = ImportReport { source, ..Default::default() };
        if nodes.is_empty() {
            return Ok(report);
        }
        let key = format!("{KV_BOOKMARK_SPACE}{}", serde_json::to_string(&(source, space_name))?);
        if let Some(id) = lib.store.kv_get(&key)? {
            if lib.spaces.contains_key(&id) {
                report.already_imported = 1;
                return Ok(report);
            }
        }
        let profile = if lib.profiles.is_empty() { lib.create_profile("Personal")? } else { lib.default_profile_id() };
        let space = lib.create_space(space_name, "🔖", Theme::preset(lib.spaces.len()), &profile)?;
        report.spaces = 1;
        add_nodes(lib, &space, None, nodes, &mut report)?;
        lib.store.kv_set(&key, &space)?;
        Ok(report)
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// An invented fixture of the parts of Arc's format the importer reads.
    const FIXTURE: &str = r#"{
      "version": 1,
      "sidebar": { "containers": [ { "global": {} }, {
        "spaces": [ "00000000-0000-0000-0000-00000000000A", {
            "id": "00000000-0000-0000-0000-00000000000A", "title": "Work", "profile": { "default": true },
            "customInfo": { "iconType": { "icon": "star" } },
            "containerIDs": ["pinned", "00000000-0000-0000-0000-000000000001", "unpinned", "00000000-0000-0000-0000-000000000002"],
            "newContainerIDs": [{ "pinned": {} }, "00000000-0000-0000-0000-000000000001", { "unpinned": { "_0": { "shared": {} } } }, "00000000-0000-0000-0000-000000000002"] } ],
        "items": [
          "00000000-0000-0000-0000-000000000001", { "id": "00000000-0000-0000-0000-000000000001", "parentID": null, "childrenIds": ["00000000-0000-0000-0000-000000000011"], "title": null,
            "data": { "itemContainer": { "containerType": { "spaceItems": { "_0": "00000000-0000-0000-0000-00000000000A" } } } } },
          "00000000-0000-0000-0000-000000000011", { "id": "00000000-0000-0000-0000-000000000011", "parentID": "00000000-0000-0000-0000-000000000001", "childrenIds": ["00000000-0000-0000-0000-000000000021"], "title": "Folder A",
            "data": { "list": { "customInfo": { "iconType": { "icon": "folder" } } } } },
          "00000000-0000-0000-0000-000000000021", { "id": "00000000-0000-0000-0000-000000000021", "parentID": "00000000-0000-0000-0000-000000000011", "childrenIds": [], "title": "Renamed tab",
            "data": { "tab": { "savedURL": "https://example.com/", "savedTitle": "Example", "savedMuteStatus": "allowAudio" } } },
          "00000000-0000-0000-0000-000000000002", { "id": "00000000-0000-0000-0000-000000000002", "parentID": null, "childrenIds": ["00000000-0000-0000-0000-000000000041"], "title": null,
            "data": { "itemContainer": { "containerType": { "spaceItems": { "_0": "00000000-0000-0000-0000-00000000000A" } } } } },
          "00000000-0000-0000-0000-000000000041", { "id": "00000000-0000-0000-0000-000000000041", "parentID": "00000000-0000-0000-0000-000000000002", "childrenIds": [], "title": null,
            "data": { "tab": { "savedURL": "https://news.example.net/", "savedTitle": "News" } } },
          "00000000-0000-0000-0000-000000000003", { "id": "00000000-0000-0000-0000-000000000003", "parentID": null, "childrenIds": ["00000000-0000-0000-0000-000000000031"], "title": null,
            "data": { "itemContainer": { "containerType": { "topApps": { "_0": { "default": true } } } } } },
          "00000000-0000-0000-0000-000000000031", { "id": "00000000-0000-0000-0000-000000000031", "parentID": "00000000-0000-0000-0000-000000000003", "childrenIds": [], "title": null,
            "data": { "tab": { "savedURL": "https://mail.example.org/", "savedTitle": "Mail" } } }
        ],
        "topAppsContainerIDs": [{ "default": true }, "00000000-0000-0000-0000-000000000003",
                                { "custom": { "_0": { "machineID": "OTHER-MAC", "directoryBasename": "Profile 9" } } }, "00000000-0000-0000-0000-000000000003"]
      } ] },
      "sidebarSyncState": {}, "firebaseSyncState": {}
    }"#;

    #[test]
    fn parses_fixture() {
        let d = parse_arc(FIXTURE, Some("THIS-MAC"), &HashSet::new()).unwrap();
        assert_eq!(d.spaces.len(), 1);
        let s = &d.spaces[0];
        assert_eq!(s.title, "Work");
        assert_eq!(s.icon, "⭐");
        assert_eq!(s.profile_dir, "Default");
        assert_eq!(
            s.pinned,
            vec![Node::Folder { name: "Folder A".into(), expanded: true, children: vec![Node::Tab { url: "https://example.com/".into(), title: "Renamed tab".into() }] }]
        );
        assert_eq!(s.today, vec![("https://news.example.net/".into(), "News".into())]);
        // The other Mac's favorites container is ignored.
        assert_eq!(d.favorites, vec![("Default".into(), vec![("https://mail.example.org/".into(), "Mail".into())])]);
    }

    #[test]
    fn applies_once() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let initial = lib.spaces_ordered()[0].clone();
        let d = parse_arc(FIXTURE, None, &HashSet::new()).unwrap();
        let r = apply_arc(&mut lib, &d, &HashMap::new()).unwrap();
        assert_eq!((r.spaces, r.folders, r.pinned, r.favorites, r.archived), (1, 1, 1, 1, 1));
        // The initial space belongs to the user, even when it is empty.
        assert_eq!(lib.spaces.len(), 2);
        assert_eq!(lib.spaces[&initial.0], initial.1);
        let again = apply_arc(&mut lib, &d, &HashMap::new()).unwrap();
        assert_eq!((again.spaces, again.pinned, again.favorites, again.already_imported), (0, 0, 0, 1));
    }

    #[test]
    fn rejects_unknown_version() {
        assert!(parse_arc(r#"{"version": 2}"#, None, &HashSet::new()).is_err());
    }

    #[test]
    fn bookmarks() {
        let json = r#"{"roots":{"bookmark_bar":{"children":[{"type":"url","name":"A","url":"https://a.example/"},{"type":"folder","name":"F","children":[{"type":"url","name":"B","url":"https://b.example/"},{"type":"url","name":"JS","url":"javascript:void(0)"}]}]},"other":{"name":"Other","children":[]}}}"#;
        let nodes = parse_chromium_bookmarks(json).unwrap();
        assert_eq!(nodes.len(), 2);
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let r = apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap();
        assert_eq!((r.spaces, r.folders, r.pinned), (1, 1, 2));
    }

    const ARC_SIDEBAR: &str = include_str!("../../../app/tests/fixtures/import/Arc/StorableSidebar.json");
    const ARC_WINDOWS: &str = include_str!("../../../app/tests/fixtures/import/Arc/StorableWindows.json");
    const ARC_PROFILES: &str = include_str!("../../../app/tests/fixtures/import/Arc/User Data/Local State");
    const CHROME_BOOKMARKS: &str = include_str!("../../../app/tests/fixtures/import/Chrome/Bookmarks");

    fn fixture_arc() -> ArcData {
        parse_arc(ARC_SIDEBAR, None, &arc_expanded_folders(ARC_WINDOWS)).unwrap()
    }

    fn memory_snapshot(lib: &Library) -> Value {
        serde_json::json!({
            "profiles": lib.profiles, "spaces": lib.spaces, "folders": lib.folders,
            "items": lib.items, "boosts": lib.boosts, "settings": lib.settings,
        })
    }

    fn database_snapshot(conn: &rusqlite::Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        ["entities", "kv", "tabs", "archive", "history", "profile_history", "sqlite_sequence"].into_iter().map(|table| {
            let mut statement = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
            let columns = statement.column_count();
            statement.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
        }).collect()
    }

    fn customize_initial_space(lib: &mut Library) -> String {
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| {
            s.name = "My empty workspace".into();
            s.icon = "M".into();
            s.theme = Theme::preset(7);
            s.theme.angle_deg = 42.0;
        }).unwrap();
        space
    }

    #[test]
    fn source_fixtures_parse_with_expected_contents() {
        let data = fixture_arc();
        assert_eq!(data.spaces.len(), 2);
        assert_eq!(data.spaces[0].title, "Fixture Work");
        assert_eq!(data.spaces[1].profile_dir, "Profile 1");
        assert_eq!(data.split_views_flattened, 1);
        assert_eq!(data.favorites.len(), 2);
        assert_eq!(chromium_profile_names(ARC_PROFILES)["Profile 1"], "Fixture Research Profile");
        let nodes = parse_chromium_bookmarks(CHROME_BOOKMARKS).unwrap();
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let arc = apply_arc(&mut lib, &data, &chromium_profile_names(ARC_PROFILES)).unwrap();
        assert_eq!((arc.spaces, arc.folders, arc.pinned, arc.favorites, arc.archived), (2, 1, 4, 2, 1));
        let chrome = apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap();
        assert_eq!((chrome.spaces, chrome.folders, chrome.pinned), (1, 2, 3));
        assert!(lib.spaces.values().all(|s| lib.profiles.contains_key(&s.profile_id)));
    }

    #[test]
    fn imported_archives_keep_their_source_profile_after_reopen_and_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive-profiles.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let mut data = fixture_arc();
        data.spaces[0].today = vec![("https://shared.example/".into(), "Work private".into())];
        data.spaces[1].today = vec![("https://shared.example/".into(), "Research private".into())];
        let names = chromium_profile_names(ARC_PROFILES);
        assert_eq!(apply_arc(&mut lib, &data, &names).unwrap().archived, 2);
        let mut expected = Vec::new();
        for space in &data.spaces {
            let id = lib.store.kv_get(&format!("{KV_ARC_SPACE}{}", space.arc_id)).unwrap().unwrap();
            let profile = lib.spaces[&id].profile_id.clone();
            let rows = lib.store.archive(&profile, 300).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].title, space.today[0].1);
            assert_eq!(rows[0].profile_id, profile);
            assert_eq!(rows[0].space_id, id);
            expected.push((profile, rows));
        }
        assert_ne!(expected[0].0, expected[1].0);
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(apply_arc(&mut lib, &data, &names).unwrap().archived, 0);
        for (profile, rows) in expected { assert_eq!(lib.store.archive(&profile, 300).unwrap(), rows); }
    }

    #[test]
    fn arc_retry_preserves_customization_and_deduplicates_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let initial = customize_initial_space(&mut lib);
        let initial_space = lib.spaces[&initial].clone();
        let mut data = fixture_arc();
        let duplicate = data.favorites[1].1[0].clone();
        data.favorites[1].1.push(duplicate);
        let report = apply_arc(&mut lib, &data, &chromium_profile_names(ARC_PROFILES)).unwrap();
        assert_eq!((report.spaces, report.folders, report.pinned, report.favorites, report.archived), (2, 1, 4, 2, 1));
        assert_eq!(lib.spaces[&initial], initial_space);
        let imported = lib.spaces.iter().find(|(_, s)| s.name == "Fixture Work").unwrap().0.clone();
        lib.update_space(&imported, |s| { s.name = "My imported workspace".into(); s.theme = Theme::preset(3); }).unwrap();
        let favorite = lib.items.iter().find(|(_, i)| i.url == "https://library.example/").unwrap().0.clone();
        lib.update_item(&favorite, |i| { i.url = "https://edited.example/".into(); i.title = "My edited favorite".into(); }).unwrap();
        let folder = lib.folders.keys().next().unwrap().clone();
        lib.update_folder(&folder, |f| f.name = "My imported folder".into()).unwrap();
        let before = memory_snapshot(&lib);
        drop(lib);

        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(memory_snapshot(&lib), before);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let before_db = database_snapshot(&conn);
        let mut renamed_profiles = chromium_profile_names(ARC_PROFILES);
        renamed_profiles.insert("Profile 1".into(), "A different source display name".into());
        let again = apply_arc(&mut lib, &data, &renamed_profiles).unwrap();
        assert_eq!((again.spaces, again.folders, again.pinned, again.favorites, again.archived, again.already_imported), (0, 0, 0, 0, 0, 2));
        assert_eq!(memory_snapshot(&lib), before);
        assert_eq!(database_snapshot(&conn), before_db);
        drop(lib);
        let lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(memory_snapshot(&lib), before);
    }

    #[test]
    fn chrome_retry_preserves_customization_and_deduplicates_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let initial = customize_initial_space(&mut lib);
        let initial_space = lib.spaces[&initial].clone();
        let nodes = parse_chromium_bookmarks(CHROME_BOOKMARKS).unwrap();
        let report = apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap();
        assert_eq!((report.spaces, report.folders, report.pinned), (1, 2, 3));
        assert_eq!(lib.spaces[&initial], initial_space);
        let imported = lib.spaces.iter().find(|(_, s)| s.name == "Chrome").unwrap().0.clone();
        lib.update_space(&imported, |s| s.name = "My imported bookmarks".into()).unwrap();
        let item = lib.items.keys().next().unwrap().clone();
        lib.update_item(&item, |i| i.title = "My bookmark title".into()).unwrap();
        let before = memory_snapshot(&lib);
        drop(lib);

        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(memory_snapshot(&lib), before);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let before_db = database_snapshot(&conn);
        let again = apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap();
        assert_eq!((again.spaces, again.folders, again.pinned, again.already_imported), (0, 0, 0, 1));
        assert_eq!(memory_snapshot(&lib), before);
        assert_eq!(database_snapshot(&conn), before_db);
        drop(lib);
        let lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(memory_snapshot(&lib), before);
    }

    #[test]
    fn bookmark_retry_markers_do_not_match_unrelated_space_names() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let initial = customize_initial_space(&mut lib);
        lib.update_space(&initial, |s| s.name = "Chrome".into()).unwrap();
        let initial_space = lib.spaces[&initial].clone();
        let nodes = parse_chromium_bookmarks(CHROME_BOOKMARKS).unwrap();
        let first = apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap();
        assert_eq!(first.spaces, 1);
        assert_eq!(lib.spaces.len(), 2);
        assert_eq!(lib.spaces[&initial], initial_space);
        let imported = lib.spaces.keys().find(|id| **id != initial).unwrap().clone();
        // A different browser is a separate source even with the same space name.
        assert_eq!(apply_bookmarks(&mut lib, &nodes, "Chrome", "Chromium").unwrap().spaces, 1);
        assert_eq!(lib.spaces.len(), 3);
        lib.delete_space(&imported).unwrap();
        assert_eq!(apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap().spaces, 1);
        assert_eq!(lib.spaces.len(), 3);
        assert_eq!(lib.items.len(), 6);
        assert_eq!(lib.spaces[&initial], initial_space);
    }

    #[test]
    fn empty_bookmarks_do_not_mark_the_source_as_imported() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        assert_eq!(apply_bookmarks(&mut lib, &[], "Chrome", "Chrome").unwrap().spaces, 0);
        assert!(lib.profiles.is_empty());
        assert!(lib.spaces.is_empty());
        let nodes = parse_chromium_bookmarks(CHROME_BOOKMARKS).unwrap();
        assert_eq!(apply_bookmarks(&mut lib, &nodes, "Chrome", "Chrome").unwrap().spaces, 1);
        assert_eq!(lib.profiles.len(), 1);
        assert!(lib.spaces.values().all(|s| lib.profiles.contains_key(&s.profile_id)));
    }

    fn failed_import_rolls_back(arc: bool, trigger: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let initial = customize_initial_space(&mut lib);
        lib.store.archive_tab(&lib.spaces[&initial].profile_id, "https://old.example/", "Existing archive", &initial, 1).unwrap();
        lib.store.record_visit(&lib.spaces[&initial].profile_id, "https://old.example/", "Existing history", 1).unwrap();
        lib.store.put_tab(&crate::store::TabRow {
            id: "existing-tab".into(), space_id: initial, item_id: None,
            url: "https://old.example/".into(), title: "Existing tab".into(),
            order: "a".into(), created_ms: 1, last_active_ms: 1,
        }).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(trigger).unwrap();
        let before = memory_snapshot(&lib);
        let revision = lib.revision;
        let before_db = database_snapshot(&conn);
        let mut data = fixture_arc();
        // Create a profile before the failing writes, and archive once before failing.
        data.spaces[0].profile_dir = "Profile 1".into();
        data.spaces[0].today.push(("https://reject.example/".into(), "Rejected archive".into()));
        let nodes = parse_chromium_bookmarks(CHROME_BOOKMARKS).unwrap();
        let apply = |lib: &mut Library| {
            if arc { apply_arc(lib, &data, &chromium_profile_names(ARC_PROFILES)) }
            else { apply_bookmarks(lib, &nodes, "Chrome", "Chrome") }
        };
        let error = apply(&mut lib).unwrap_err();
        assert!(error.contains("fixture failure") || error.contains("FOREIGN KEY constraint failed"), "{error}");
        assert_eq!(memory_snapshot(&lib), before);
        assert_eq!(lib.revision, revision);
        assert_eq!(database_snapshot(&conn), before_db);
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(memory_snapshot(&lib), before);
        assert_eq!(database_snapshot(&conn), before_db);

        conn.execute_batch("DROP TRIGGER reject_import").unwrap();
        let report = apply(&mut lib).unwrap();
        assert!(report.spaces > 0);
        let imported = memory_snapshot(&lib);
        let imported_db = database_snapshot(&conn);
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(memory_snapshot(&lib), imported);
        assert_eq!(database_snapshot(&conn), imported_db);
        let retry = apply(&mut lib).unwrap();
        assert_eq!((retry.spaces, retry.folders, retry.pinned, retry.favorites, retry.archived), (0, 0, 0, 0, 0));
        assert_eq!(memory_snapshot(&lib), imported);
        assert_eq!(database_snapshot(&conn), imported_db);
    }

    #[test]
    fn arc_archive_failure_rolls_back_entities_clock_archive_and_markers() {
        failed_import_rolls_back(true, "CREATE TRIGGER reject_import BEFORE INSERT ON archive
            WHEN NEW.url = 'https://reject.example/' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;");
    }

    #[test]
    fn arc_marker_failure_rolls_back_all_imported_spaces_and_favorites() {
        failed_import_rolls_back(true, "CREATE TRIGGER reject_import BEFORE INSERT ON kv
            WHEN NEW.k LIKE 'import.arc.favorite.%' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;");
    }

    #[test]
    fn chrome_item_failure_rolls_back_space_folders_clock_and_markers() {
        failed_import_rolls_back(false, "CREATE TRIGGER reject_import BEFORE INSERT ON entities
            WHEN NEW.kind = 'item' AND NEW.payload LIKE '%docs.example%'
            BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;");
    }

    #[test]
    fn chrome_marker_failure_rolls_back_the_entire_import() {
        failed_import_rolls_back(false, "CREATE TRIGGER reject_import BEFORE INSERT ON kv
            WHEN NEW.k LIKE 'import.bookmarks.space.%' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;");
    }

    #[test]
    fn commit_failure_rolls_back_database_and_memory_before_retry() {
        failed_import_rolls_back(false, "CREATE TABLE import_commit_guard (
                tab_id TEXT REFERENCES tabs(id) DEFERRABLE INITIALLY DEFERRED
            );
            CREATE TRIGGER reject_import AFTER INSERT ON entities WHEN NEW.kind = 'space'
            BEGIN INSERT INTO import_commit_guard VALUES ('missing-tab'); END;");
    }

    #[test]
    fn malformed_source_fixtures_are_rejected_before_apply() {
        let arc = include_str!("../../../app/tests/fixtures/import/malformed/Arc/StorableSidebar.json");
        let chrome = include_str!("../../../app/tests/fixtures/import/malformed/Chrome/Bookmarks");
        assert!(parse_arc(arc, None, &HashSet::new()).is_err());
        assert!(parse_chromium_bookmarks(chrome).is_err());
    }
}
