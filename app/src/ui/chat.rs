//! Prompt-only AI uses existing Settings controls, not an executable web view.

use crate::chat::{AccountScope, Session};
use crate::ui::settings::{section, separator};
use crate::ui::style::{self, Button, Ink, Kind};
use crate::ui::theme::Palette;
use crossbow_ai::Role;
use crossbow_core::library::Library;
use egui::{RichText, Ui};

/// Returns true only for an explicit Configure Providers action.
pub fn show(slot: &mut Option<Session>, lib: &mut Library, space: &str, account: &AccountScope, ui: &mut Ui, pal: &Palette) -> bool {
    let Some(profile) = crate::chat::target_profile(lib, space).map(str::to_owned) else {
        *slot = None;
        ui.label("This space has no profile. Select another space to use AI.");
        return false;
    };
    if slot.is_none() { *slot = Some(Session::load(lib, &profile, account)); }
    let state = slot.as_mut().unwrap();
    state.poll(lib, space, account);
    content(state, lib, space, account, ui, pal)
}

pub(super) fn content(state: &mut Session, lib: &mut Library, space: &str, account: &AccountScope, ui: &mut Ui, pal: &Palette) -> bool {
    let ink = Ink::new(pal.panel, pal.accent);
    let name = lib.profiles.get(&state.profile).map(|profile| profile.name.as_str()).unwrap_or("Unavailable profile");
    ui.label(style::text(format!("For {name}"), 12.0, style::weight::REGULAR).color(ink.secondary));
    if let Some(selection) = &state.selection {
        ui.label(RichText::new(format!("{} at {}", selection.endpoint.provider.kind().name(), selection.endpoint.address())).strong());
        ui.add(egui::Label::new(format!("Model: {}", selection.model.as_deref().unwrap_or("No model"))).wrap());
    }
    let configure = ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-configure")), |ui| {
        Button::new("Configure Providers").kind(Kind::Plain).show(ui, &ink).clicked()
    }).inner;
    ui.label(style::text("Send includes your message and the conversation below. No pages, tabs, files, browsing history or credentials are attached. The server may forward to cloud.", 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-stream")), |ui| {
        ui.add_enabled_ui(!state.busy() && state.selection.is_some(), |ui| {
            ui.horizontal(|ui| {
                let label = ui.label(style::text("Stream answer", 13.5, style::weight::MEDIUM).color(ink.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = crate::ui::settings::toggle(ui, &mut state.stream, ink.accent).labelled_by(label.id);
                    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), state.stream, "Stream answer"));
                    if response.has_focus() {
                        ui.painter().rect_stroke(response.rect.expand(2.0), 13.0, egui::Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
                    }
                    if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
                });
            });
        });
        ui.label(style::text("Off by default. Streaming shows incomplete text as it arrives. Unsupported servers return an error; no automatic fallback.", 12.0, style::weight::REGULAR).color(ink.secondary));
    });

    ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-composer")), |ui| {
        style::field_label(ui, &ink, "Message");
        let id = state.editor_id(ui.ctx());
        let focused = ui.memory(|memory| memory.has_focus(id));
        let frame = egui::Frame::new().fill(ink.fill).corner_radius(style::radius::MD)
            .inner_margin(12.0).stroke(if focused { egui::Stroke::new(1.5, ink.accent) } else { egui::Stroke::NONE });
        // A nested viewport must not shrink with the remaining outer height:
        // that makes even an empty three-line draft scroll and clips its hint.
        // Keep the composer viewport stable when completion clears a long draft;
        // otherwise every answer below it shifts even without a scroll request.
        let composer = egui::ScrollArea::vertical().id_salt(("chat-input-scroll", id)).min_scrolled_height(120.0).max_height(120.0).auto_shrink([false, false]).animated(false).show(ui, |ui| {
            ui.add_enabled(!state.busy() && state.selection.is_some(), egui::TextEdit::multiline(&mut state.draft)
                .id(id).frame(frame).desired_width(f32::INFINITY).desired_rows(3).min_size(egui::vec2(0.0, 120.0))
                .font(egui::FontId::proportional(15.0)).text_color(ink.text)
                .hint_text(style::text("Ask a question…", 15.0, style::weight::REGULAR).color(ink.secondary)))
        });
        state.track_composer(composer.id);
        if composer.inner.gained_focus() { ui.scroll_to_rect_animation(composer.inner_rect, None, egui::style::ScrollAnimation::none()); }
    });
    // The changing count stays on its own line, so clearing the draft cannot
    // rewrap the fixed explanation and move the conversation below.
    ui.label(style::text(format!("{} / 16,384 bytes", state.draft.len()), 12.0, style::weight::REGULAR).color(ink.secondary));
    ui.label(style::text("Up to 17 sent messages; 65,536 bytes of conversation. No automatic truncation.", 12.0, style::weight::REGULAR).color(ink.secondary));
    let actions = ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-actions")), |ui| ui.horizontal_wrapped(|ui| {
        ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-send")), |ui| {
            if Button::new("Send").kind(Kind::Primary).enabled(!state.busy() && state.selection.is_some() && !state.draft.trim().is_empty()).show(ui, &ink).clicked() {
                state.send(lib, space, account, ui.ctx());
            }
        });
        // An empty wrapped scope still changes the row height; completion must
        // not move the conversation below by removing only this button.
        if state.busy() {
            ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-cancel")), |ui| {
                if Button::new("Cancel Request").kind(Kind::Plain).show(ui, &ink).clicked() { state.cancel(); }
            });
        }
        ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-reset")), |ui| {
            if Button::new("New Chat").kind(Kind::Plain).show(ui, &ink).clicked() { *state = Session::load(lib, &state.profile, account); }
        });
    })).response;
    let reveal = state.reveal_status;
    let status = ui.scope_builder(egui::UiBuilder::new().scope_id(ui.make_persistent_id("chat-status")).accessibility_role(egui::Role::Status), |ui| {
        // Announce phase/error changes, not each token or the untrusted answer.
        // A stable atomic region does not move keyboard/accessibility focus.
        ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
            node.set_live(egui::accesskit::Live::Polite);
            node.set_live_atomic();
        });
        if state.busy() {
            ui.label(if state.partial.is_empty() { "Waiting for an answer… (up to 45 seconds)" } else { "Receiving an answer… (up to 45 seconds)" });
        }
        if let Some(error) = &state.error { ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger)); }
        if !state.busy() && state.error.is_none() && !state.messages.is_empty() { ui.label("Answer received. Review it below."); }
    }).response;
    ui.label(style::text("Session only. Leaving this page or starting a new chat clears it and cancels pending requests; the server may retain what it received. Retry with Send is a new request.", 12.0, style::weight::REGULAR).color(ink.secondary));
    if !state.messages.is_empty() || state.streaming_prompt().is_some() {
        section(ui, pal, "Conversation");
        for item in &state.messages {
            message(ui, pal, if item.role == Role::User { "You" } else { "Assistant" }, &item.content);
        }
        if let Some(prompt) = state.streaming_prompt() {
            // Render the same prompt/answer rows before and after completion.
            // They remain display-only until Session accepts the final result.
            message(ui, pal, "You", prompt);
            if !state.partial.is_empty() {
                // Keep the incomplete marker in the author row: removing a
                // trailing note would shrink content and clamp a bottom reader.
                message(ui, pal, "Assistant (incomplete)", &state.partial);
            }
        }
    }
    if !state.partial.is_empty() && state.streaming_prompt().is_none() {
        section(ui, pal, "Partial answer");
        ui.label(style::text("Incomplete — not in the conversation and never included in a retry.", 12.0, style::weight::REGULAR).color(ink.secondary));
        ui.add(egui::Label::new(&state.partial).wrap().selectable(true));
        separator(ui, pal);
    }
    if reveal && !ui.is_sizing_pass() && ui.is_visible() {
        let recovery = actions.rect.union(status.rect);
        if ui.clip_rect().contains_rect(recovery) { state.reveal_status = false; }
        else { ui.scroll_to_rect_animation(recovery, None, egui::style::ScrollAnimation::none()); }
    }
    configure
}

fn message(ui: &mut Ui, pal: &Palette, author: &str, text: &str) {
    ui.label(RichText::new(author).strong());
    // Untrusted content stays selectable plain text, never links or executable output.
    ui.add(egui::Label::new(text).wrap().selectable(true));
    separator(ui, pal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_ai::{Message, local::LocalEndpoint};
    use crossbow_core::model::Theme;

    #[test]
    fn review_stream_status_is_polite_stable_and_excludes_answer_tokens() {
        let (mut lib, space, mut chat) = crate::chat::tests::configured(LocalEndpoint::default());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let pal = crate::ui::theme::palette(&Theme::preset(0), false);
        let mut status_id = None;
        let mut previous = None;
        for partial in ["first incomplete token", "first incomplete token and more", ""] {
            chat.partial = partial.into();
            chat.error = (!partial.is_empty()).then(|| "Synthetic request cancelled.".into());
            if partial.is_empty() {
                chat.messages = vec![Message::user("Synthetic question"), Message { role: Role::Assistant, content: "Synthetic completed answer".into() }];
            }
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| { content(&mut chat, &mut lib, &space, &None, ui, &pal); });
            let update = output.platform_output.accesskit_update.take().unwrap();
            output.textures_delta.clear();
            let regions: Vec<_> = update.nodes.iter().filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite)).collect();
            assert_eq!(regions.len(), 1, "chat needs one polite status region, not per-token announcements");
            let (id, node) = regions[0];
            assert_eq!(node.role(), egui::Role::Status);
            assert!(node.is_live_atomic());
            if let Some(previous) = status_id { assert_eq!(*id, previous); }
            status_id = Some(*id);
            let mut pending = node.children().to_vec();
            let mut labels = Vec::new();
            while let Some(id) = pending.pop() {
                let node = &update.nodes.iter().find(|(other, _)| *other == id).unwrap().1;
                if let Some(label) = node.label().or_else(|| node.value()) { labels.push(label.to_owned()); }
                pending.extend_from_slice(node.children());
            }
            assert!(labels.iter().all(|label| !label.contains("token") && !label.contains("Synthetic completed answer")));
            let expected = if partial.is_empty() { "Answer received. Review it below." } else { "Synthetic request cancelled." };
            assert!(labels.iter().any(|label| label == expected), "status not exposed: {labels:?}");
            if !partial.is_empty() {
                if let Some(previous) = previous { assert_eq!(labels, previous); }
                previous = Some(labels);
            }
        }
    }

    #[test]
    fn review_stream_short_composer_fills_the_reserved_viewport() {
        for (width, dark) in [(560.0, false), (388.0, false), (560.0, true), (388.0, true)] {
            for draft in ["", "Synthetic short prompt"] {
                let (mut lib, space, mut chat) = crate::chat::tests::configured(LocalEndpoint::default());
                chat.draft = draft.into();
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let pal = crate::ui::theme::palette(&Theme::preset(0), dark);
                let editor = chat.editor_id(&ctx);
                for _ in 0..2 {
                    ctx.run_ui(egui::RawInput::default(), |ui| {
                        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 3000.0));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                        child.set_width(width);
                        crate::ui::settings::set_visuals(&mut child, &pal);
                        content(&mut chat, &mut lib, &space, &None, &mut child, &pal);
                    }).textures_delta.clear();
                }
                let response = ctx.read_response(editor).unwrap();
                assert!((response.rect.height() - 120.0).abs() < 0.5,
                    "short composer must fill its 120pt viewport: width={width}, dark={dark}, draft={draft:?}, rect={:?}", response.rect);
            }
        }
    }

    #[test]
    fn review_chat_disposal_retires_composer_scroll_state() {
        let (mut lib, space, chat) = crate::chat::tests::configured(LocalEndpoint::default());
        let profile = chat.profile.clone();
        drop(chat);
        let ctx = egui::Context::default();
        let pal = crate::ui::theme::palette(&Theme::preset(0), false);
        let baseline = ctx.data_mut(|data| data.count::<egui::scroll_area::State>());
        for _ in 0..8 {
            let mut chat = Session::load(&mut lib, &profile, &None);
            for parent in ["first", "second"] {
                ctx.run_ui(egui::RawInput::default(), |ui| {
                    ui.push_id(parent, |ui| { content(&mut chat, &mut lib, &space, &None, ui, &pal); });
                }).textures_delta.clear();
                assert_eq!(ctx.data_mut(|data| data.count::<egui::scroll_area::State>()), baseline + 1);
            }
            drop(chat);
            assert_eq!(ctx.data_mut(|data| data.count::<egui::scroll_area::State>()), baseline, "disposed chat retained its composer scroll state");
        }
    }

    #[test]
    fn prompt_and_untrusted_answers_stay_in_normal_compact_light_dark_columns() {
        for (width, dark) in [(560.0, false), (544.0, false), (560.0, true), (544.0, true)] {
            let (mut lib, space, mut chat) = crate::chat::tests::configured(LocalEndpoint::default());
            chat.draft = "long-question/".repeat(180);
            chat.messages = vec![Message::user("Synthetic question"), Message { role: Role::Assistant, content: "<script>not executable</script> ".repeat(300) }];
            chat.stream = true;
            chat.partial = "https://not-a-link.example.test/<script>plain-partial</script>/".repeat(200);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let pal = crate::ui::theme::palette(&Theme::preset(0), dark);
            for _ in 0..2 {
                ctx.run_ui(egui::RawInput::default(), |ui| {
                    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 3000.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    child.set_width(width);
                    crate::ui::settings::set_visuals(&mut child, &pal);
                    content(&mut chat, &mut lib, &space, &None, &mut child, &pal);
                    assert!(child.min_rect().width() <= width + 0.5, "chat overflow: {:?}", child.min_rect());
                }).textures_delta.clear();
            }
        }
    }
}
