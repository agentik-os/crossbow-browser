//! Provider setup uses the existing Settings row and control vocabulary.

use crate::ai::SettingsDraft;
use crate::ui::settings::{keep_popup_above_sheet, row, section, separator};
use crate::ui::style::{self, Button, Ink, Kind};
use crate::ui::theme::Palette;
use crossbow_ai::local::{Diagnostic, LocalProvider, TEST_PROMPT};
use crossbow_ai::providers::{DiscoveryRoots, ProviderKind};
use crossbow_core::library::Library;
use egui::{RichText, Ui};

pub fn show(slot: &mut Option<SettingsDraft>, lib: &mut Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let Some(profile) = crate::downloads::active_profile(lib, space).map(str::to_owned) else {
        *slot = None;
        ui.label("This space has no available profile. Select another space to configure AI providers.");
        return;
    };
    if slot.as_ref().is_none_or(|state| state.profile != profile) { *slot = Some(SettingsDraft::load(lib, &profile)); }
    let state = slot.as_mut().unwrap();
    if state.discovery.is_none() && state.discovery_error.is_none() && !state.discovering() {
        scan(state, ui.ctx());
    }
    state.poll();
    content(state, lib, &profile, ui, pal, scan);
}

fn scan(state: &mut SettingsDraft, ctx: &egui::Context) {
    let home = directories::BaseDirs::new();
    state.scan(DiscoveryRoots::for_current_process(home.as_ref().map(|dirs| dirs.home_dir())), ctx);
}

pub(super) fn content(state: &mut SettingsDraft, lib: &mut Library, profile: &str, ui: &mut Ui, pal: &Palette, scan: impl FnMut(&mut SettingsDraft, &egui::Context)) {
    let ink = Ink::new(pal.panel, pal.accent);
    let name = lib.profiles.get(profile).map(|profile| profile.name.as_str()).unwrap_or("Unavailable profile");
    ui.label(style::text(format!("For {name} · this device only"), 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.label("Save a selection for Ask AI and for the browser features below. Each feature stays off until you turn it on and confirm what it sends.");

    section(ui, pal, "Connection");
    row(ui, pal, "Diagnostic server", "Start the server yourself. Crossbow will not launch it or download models.", |ui| {
        let mut provider = state.draft.endpoint.provider;
        egui::ComboBox::from_id_salt("ai-provider").selected_text(provider.kind().name()).width(210.0).show_ui(ui, |ui| {
            keep_popup_above_sheet(ui);
            for candidate in LocalProvider::ALL { ui.selectable_value(&mut provider, candidate, candidate.kind().name()); }
        });
        state.set_provider(provider);
    });
    row(ui, pal, "Loopback port", "Only 127.0.0.1. No proxy, redirects or credentials.", |ui| {
        let mut endpoint = state.draft.endpoint;
        let label = ui.label("Port");
        ui.add(egui::DragValue::new(&mut endpoint.port).range(1..=65535)).labelled_by(label.id);
        state.set_endpoint(endpoint);
    });
    ui.label(style::text(state.draft.endpoint.address(), 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.label(style::text("A local server can still forward requests to the cloud. Check its model and configuration; Crossbow cannot certify offline inference.", 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("ai-refresh-models")), |ui| {
        if Button::new("Refresh Models").enabled(!state.busy()).show(ui, &ink).clicked() {
            state.start(lib, Some(profile), Diagnostic::Models, ui.ctx());
        }
    });
    ui.label(style::text("Refresh sends only GET /v1/models, with no prompt or browsing data.", 12.0, style::weight::REGULAR).color(ink.secondary));
    row(ui, pal, "Model", "No automatic selection or fallback.", |ui| {
        let mut model = state.draft.model.clone();
        let shown = model.clone().unwrap_or_else(|| "Select a model".into());
        ui.add_enabled_ui(state.catalogue_loaded() && !state.busy(), |ui| {
            egui::ComboBox::from_id_salt("ai-model").selected_text(shown).width(210.0).truncate().show_ui(ui, |ui| {
                keep_popup_above_sheet(ui);
                ui.selectable_value(&mut model, None, "No model selected");
                for candidate in &state.models {
                    let selected = model.as_ref() == Some(candidate);
                    if ui.add(egui::Button::selectable(selected, candidate).wrap_mode(egui::TextWrapMode::Truncate)).on_hover_text(candidate).clicked() {
                        model = Some(candidate.clone());
                    }
                }
            });
        });
        state.set_model(model);
    });
    if !state.catalogue_loaded() && !state.busy() {
        ui.label(style::text("Refresh models to check this server before testing a saved choice.", 12.0, style::weight::REGULAR).color(ink.secondary));
    } else if state.catalogue_loaded() && state.models.is_empty() {
        ui.label("This server reported no models. Configure a model in the provider, then refresh.");
    }
    ui.label(style::text("Test sends this phrase to the selected model:", 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.label(TEST_PROMPT);
    ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("ai-test-actions")), |ui| ui.horizontal_wrapped(|ui| {
        if Button::new("Send Test Phrase").enabled(state.can_test()).show(ui, &ink).clicked() {
            if let Some(model) = state.draft.model.clone() {
                state.start(lib, Some(profile), Diagnostic::ConnectionTest { model }, ui.ctx());
            }
        }
        if state.busy() && Button::new("Cancel Request").kind(Kind::Plain).show(ui, &ink).clicked() { state.cancel_request(); }
    }));
    if state.busy() {
        ui.label(if state.testing() { "Waiting for the test answer… (up to 45 seconds)" } else { "Reading models… (up to 8 seconds)" });
    }
    if let Some(answer) = &state.answer {
        ui.label(RichText::new("Server response").strong());
        // Plain label, not Markdown or HTML: provider output is untrusted data.
        ui.label(answer);
        ui.label(style::text("A response confirms this diagnostic only, not offline execution or browser AI readiness.", 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    separator(ui, pal);
    ui.label(style::text("Save stores only your selection, not consent to send browser data. Leaving this page cancels requests and discards unsaved changes.", 12.0, style::weight::REGULAR).color(ink.secondary));
    let reveal_error = state.error.is_some() && state.reveal_error;
    let recovery = ui.vertical(|ui| {
        if let Some(error) = &state.error { ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger)); }
        ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("ai-preference-actions")), |ui| ui.horizontal_wrapped(|ui| {
            if Button::new("Save Selection").kind(Kind::Primary).enabled(state.changed() && !state.busy()).show(ui, &ink).clicked() {
                state.save(lib, Some(profile));
            }
            if Button::new("Cancel / Reload").kind(Kind::Plain).show(ui, &ink).clicked() { state.reload(lib); }
        }));
    }).response;

    installations(state, ui, pal, scan);
    // A wheel's remaining motion can cancel the scroll request. Keep it pending
    // until recovery is visible, then let the user scroll away without snapping back.
    if reveal_error && !ui.is_sizing_pass() && ui.is_visible() {
        if ui.clip_rect().contains_rect(recovery.rect) {
            state.reveal_error = false;
        } else {
            recovery.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        }
    }
}

fn installations(state: &mut SettingsDraft, ui: &mut Ui, pal: &Palette, mut scan: impl FnMut(&mut SettingsDraft, &egui::Context)) {
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Detected installations");
    ui.label(style::text("Checks known executable and app names only. Does not launch clients, read credentials or check sign-in. Installed does not mean connected.", 12.0, style::weight::REGULAR).color(ink.secondary));
    if state.discovering() { ui.label("Checking installed providers… (up to 5 seconds)"); }
    let reveal_error = state.discovery_error.is_some() && state.reveal_discovery_error;
    let recovery = ui.vertical(|ui| {
        if let Some(error) = &state.discovery_error { ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger)); }
        ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("ai-scan-actions")), |ui| ui.horizontal_wrapped(|ui| {
            ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("ai-scan-again")), |ui| {
                if Button::new("Scan Again").enabled(!state.discovering()).show(ui, &ink).clicked() { scan(state, ui.ctx()); }
            });
            if state.discovering() {
                ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("ai-cancel-scan")), |ui| {
                    if Button::new("Cancel Scan").kind(Kind::Plain).show(ui, &ink).clicked() { state.cancel_scan(); }
                });
            }
        }));
    }).response;
    if let Some(discovery) = &state.discovery {
        if discovery.providers.is_empty() { ui.label("No supported installation found in the checked locations."); }
        for kind in ProviderKind::ALL {
            let found = discovery.providers.iter().find(|provider| provider.kind == kind);
            let status = if found.is_some() { "Installed" } else { "Not detected" };
            ui.label(RichText::new(format!("{} · {status}", kind.name())).strong());
            ui.label(style::text(kind.execution_description(), 12.0, style::weight::REGULAR).color(ink.secondary));
            if !LocalProvider::ALL.iter().any(|provider| provider.kind() == kind) {
                ui.label(style::text("Client adapter not available in this build.", 12.0, style::weight::REGULAR).color(ink.secondary));
            }
            if let Some(found) = found {
                for path in [found.executable.as_ref(), found.application.as_ref()].into_iter().flatten() {
                    let path = path.display().to_string();
                    ui.add(egui::Label::new(style::text(&path, 12.0, style::weight::REGULAR).color(ink.secondary)).truncate()).on_hover_text(&path);
                }
            }
            separator(ui, pal);
        }
        if discovery.path_limit_reached { ui.label("Scan limit reached. Some search locations were not checked."); }
    }
    if reveal_error && !ui.is_sizing_pass() && ui.is_visible() {
        if ui.clip_rect().contains_rect(recovery.rect) { state.reveal_discovery_error = false; }
        else { recovery.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_ai::providers::{DetectedProvider, Discovery};
    use crossbow_core::{model::Theme, store::Store};
    use egui::{Rect, pos2, vec2};

    #[test]
    fn provider_content_stays_inside_normal_and_compact_settings_columns() {
        for (width, dark) in [(560.0, false), (544.0, false), (560.0, true), (544.0, true)] {
            let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
            lib.ensure_defaults().unwrap();
            let profile = lib.default_profile_id();
            let mut state = SettingsDraft::load(&lib, &profile);
            state.set_model(Some("long-model-id/".repeat(18)));
            state.answer = Some("response ".repeat(400));
            state.error = Some("Could not save provider preferences. Your draft is unchanged. Check disk space and permissions, then retry Save or Cancel.".into());
            state.discovery_error = Some("Provider scan timed out. Filesystem access may still be pending; wait before trying Scan Again.".into());
            state.discovery = Some(Discovery {
                providers: vec![DetectedProvider { kind: ProviderKind::ClaudeCode, executable: Some(format!("/fixture/{}/claude", "long-folder/".repeat(30)).into()), application: None }],
                path_limit_reached: true,
            });
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let pal = crate::ui::theme::palette(&Theme::preset(0), dark);
            for _ in 0..2 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 3000.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    child.set_width(width);
                    crate::ui::settings::set_visuals(&mut child, &pal);
                    content(&mut state, &mut lib, &profile, &mut child, &pal, scan);
                    assert!(child.min_rect().width() <= width + 0.5, "provider content exceeds {width}px column: {:?}", child.min_rect());
                });
                output.textures_delta.clear();
            }
        }
    }
}
