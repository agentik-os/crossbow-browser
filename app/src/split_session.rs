//! Device-local split layout checkpoints. Restore only references whose current
//! Space, profile and tab ownership still agree; no web views are opened here.

use std::collections::{BTreeMap, HashMap, HashSet};
use crossbow_core::library::Library;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::app::tab_profile;
use crate::tabs::{FAVORITES_SPACE, Split, Tab};

#[derive(Serialize, Deserialize)]
struct SavedSplit {
    profile_id: String,
    tabs: Vec<String>,
    #[serde(default)]
    ratios: Vec<f32>,
}

pub struct SessionLayout<'a> {
    pub space: &'a str,
    pub active_tabs: &'a HashMap<String, String>,
    pub splits: &'a HashMap<String, Split>,
    pub focused_pane: Option<&'a str>,
    pub sidebar: bool,
    pub sidebar_width: f32,
}

impl SessionLayout<'_> {
    pub fn encode(&self, lib: &Library, tabs: &[Tab]) -> String {
        serde_json::json!({
            "space": self.space,
            "active_tabs": self.active_tabs,
            "splits": encode(lib, tabs, self.splits),
            "focused_pane": focused_pane(lib, tabs, self.space, self.active_tabs.get(self.space).map(String::as_str), self.splits, self.focused_pane),
            "sidebar": self.sidebar,
            "sidebar_width": self.sidebar_width,
        }).to_string()
    }
}

fn encode(lib: &Library, tabs: &[Tab], splits: &HashMap<String, Split>) -> Value {
    let spaces: BTreeMap<_, _> = splits.iter().filter_map(|(space, split)| {
        let owner = lib.spaces.get(space)?;
        let split = validated_members(lib, tabs, space, &owner.profile_id, split)?;
        (split.tabs.len() >= 2).then(|| (space, SavedSplit { profile_id: owner.profile_id.clone(), tabs: split.tabs, ratios: split.ratios }))
    }).collect();
    serde_json::json!({ "version": 1, "spaces": spaces })
}

pub fn restore(lib: &Library, tabs: &[Tab], state: &Value, active: &mut HashMap<String, String>) -> HashMap<String, Split> {
    let saved = &state["splits"];
    if saved["version"].as_u64() != Some(1) { return HashMap::new(); }
    let Some(spaces) = saved["spaces"].as_object() else { return HashMap::new(); };
    spaces.iter().filter_map(|(space, value)| {
        let saved: SavedSplit = serde_json::from_value(value.clone()).ok()?;
        let selected = state["active_tabs"][space].as_str();
        let was_visible = selected.is_some_and(|id| saved.tabs.iter().any(|pane| pane == id));
        let split = Split { tabs: saved.tabs, ratios: saved.ratios };
        let split = validated_members(lib, tabs, space, &saved.profile_id, &split)?;
        // A close may commit before its new layout. Recover only a previously
        // visible split, including a final pane that now stands alone.
        if was_visible && !split.tabs.iter().any(|id| Some(id.as_str()) == selected) {
            let requested = state["focused_pane"].as_str().filter(|_| state["space"].as_str() == Some(space.as_str()));
            let survivor = split.tabs.iter().find(|id| Some(id.as_str()) == requested).unwrap_or(&split.tabs[0]);
            active.insert(space.clone(), survivor.clone());
        }
        (split.tabs.len() >= 2).then(|| (space.clone(), split))
    }).collect()
}

pub fn focused_pane<'a>(lib: &Library, tabs: &[Tab], space: &str, active: Option<&'a str>, splits: &HashMap<String, Split>, requested: Option<&'a str>) -> Option<&'a str> {
    let owner = lib.spaces.get(space)?;
    let valid = |id: &str| tabs.iter().any(|tab| tab.id == id && in_space(lib, tab, space, &owner.profile_id));
    let active = active.filter(|id| valid(id))?;
    let split = splits.get(space).filter(|split| split.tabs.iter().any(|id| id == active));
    requested.filter(|id| valid(id) && match split {
        Some(split) => split.tabs.iter().any(|pane| pane == id),
        None => *id == active,
    }).or(Some(active))
}

fn in_space(lib: &Library, tab: &Tab, space: &str, profile: &str) -> bool {
    (tab.space_id == space || tab.space_id == FAVORITES_SPACE) && tab_profile(lib, tab) == Some(profile)
}

fn validated_members(lib: &Library, tabs: &[Tab], space: &str, profile: &str, split: &Split) -> Option<Split> {
    if lib.spaces.get(space)?.profile_id != profile || !lib.profiles.contains_key(profile) || !(2..=4).contains(&split.tabs.len()) {
        return None;
    }
    let mut seen = HashSet::new();
    let retained: Vec<_> = split.tabs.iter().enumerate().filter(|(_, id)| {
        seen.insert(id.as_str()) && tabs.iter().any(|tab| tab.id == **id && in_space(lib, tab, space, profile))
    }).collect();
    if retained.is_empty() { return None; }
    let mut ratios: Vec<_> = retained.iter().map(|(i, _)| split.ratios.get(*i).copied().unwrap_or(f32::NAN)).collect();
    let sum: f64 = ratios.iter().map(|r| f64::from(*r)).sum();
    if split.ratios.len() != split.tabs.len() || !sum.is_finite() || sum <= 0.0
        || ratios.iter().any(|r| !r.is_finite() || f64::from(*r) / sum < 0.15) {
        ratios.fill(1.0 / retained.len() as f32);
    } else if (sum - 1.0).abs() > f64::from(f32::EPSILON) {
        for ratio in &mut ratios { *ratio = (f64::from(*ratio) / sum) as f32; }
    }
    Some(Split { tabs: retained.into_iter().map(|(_, id)| id.clone()).collect(), ratios })
}
