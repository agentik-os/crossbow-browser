//! Space icons: an emoji (or up to two letters) or a Lucide glyph stored as
//! `lucide:<name>`. Both kinds live in the existing `Space.icon` string, so the
//! library schema and sync payload are unchanged.

use super::lucide_catalog::ICONS;
use super::style::Ink;
use super::theme::Palette;
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, FontFamily, FontId, Id, Painter, Rect, RichText, Sense, Stroke, Ui, vec2};

pub const LUCIDE_PREFIX: &str = "lucide:";

/// Longest typed or pasted icon kept, in bytes; a flag or keycap sequence fits.
pub const MAX_CUSTOM_BYTES: usize = 32;

/// Shown first on the Icons tab and in the Space menu.
pub const SUGGESTED_ICONS: &[&str] = &[
    "house", "briefcase", "palette", "book-open", "headphones", "flask-conical", "leaf", "rocket", "star", "flame",
    "lightbulb", "gamepad-2", "code", "globe", "heart", "music", "plane", "graduation-cap", "dumbbell", "wallet",
    "camera", "users", "shopping-cart", "coffee", "moon", "sun", "compass", "film", "newspaper", "pen-tool",
];

/// Emoji with search words. Single code points only: the system emoji font draws
/// them in color, and no joined sequence depends on shaping support.
pub const EMOJI: &[(&str, &str)] = &[
    ("🏠", "house home"), ("💼", "briefcase work job"), ("🎨", "palette art design paint"), ("📚", "books study library read"),
    ("🎧", "headphones music audio"), ("🧪", "test tube lab science experiment"), ("🌿", "herb leaf plant nature"), ("🚀", "rocket launch startup"),
    ("⭐", "star favorite"), ("🔥", "fire hot flame"), ("💡", "light bulb idea"), ("🎮", "game controller gaming play"),
    ("🍿", "popcorn movies watch"), ("🛒", "cart shopping buy"), ("📈", "chart growth stocks finance"), ("🌙", "moon night"),
    ("☕", "coffee cafe drink"), ("🐾", "paw pets animals"), ("🎬", "clapper film video movie"), ("🧭", "compass travel explore"),
    ("😀", "grinning smile happy face"), ("😎", "cool sunglasses face"), ("🤓", "nerd glasses face"), ("🤔", "thinking face"),
    ("😴", "sleeping face rest"), ("🥳", "party face celebrate"), ("🤖", "robot ai bot"), ("👻", "ghost"),
    ("👽", "alien"), ("💀", "skull"), ("👋", "wave hello hand"), ("👍", "thumbs up like"),
    ("🙌", "raised hands celebrate"), ("💪", "muscle strong fitness"), ("🧠", "brain think"), ("👀", "eyes look watch"),
    ("❤", "heart love red"), ("💙", "blue heart"), ("💚", "green heart"), ("💜", "purple heart"),
    ("🖤", "black heart"), ("✨", "sparkles magic new"), ("⚡", "lightning zap energy"), ("🌈", "rainbow"),
    ("☀", "sun sunny weather"), ("⛅", "cloud sun weather"), ("❄", "snowflake winter cold"), ("🌊", "wave ocean sea water"),
    ("🌍", "globe earth world europe africa"), ("🌎", "globe earth world americas"), ("🌏", "globe earth world asia"), ("🗺", "map travel"),
    ("🏔", "mountain snow"), ("🏖", "beach vacation holiday"), ("🏕", "camping tent outdoors"), ("🌲", "tree evergreen forest"),
    ("🌵", "cactus desert"), ("🌸", "cherry blossom flower spring"), ("🌻", "sunflower flower"), ("🍀", "clover luck"),
    ("🍁", "maple leaf autumn"), ("🍄", "mushroom"), ("🐶", "dog puppy pet"), ("🐱", "cat kitten pet"),
    ("🦊", "fox"), ("🐻", "bear"), ("🐼", "panda"), ("🐸", "frog"),
    ("🦁", "lion"), ("🐝", "bee honey"), ("🦋", "butterfly"), ("🐙", "octopus"),
    ("🐳", "whale"), ("🦄", "unicorn"), ("🐉", "dragon"), ("🍎", "apple fruit red"),
    ("🍋", "lemon fruit"), ("🍕", "pizza food"), ("🍔", "burger food"), ("🍣", "sushi food"),
    ("🍜", "noodles ramen food"), ("🥗", "salad healthy food"), ("🍰", "cake dessert"), ("🍩", "doughnut donut dessert"),
    ("🍷", "wine glass drink"), ("🍺", "beer drink"), ("🍵", "tea drink"), ("🧁", "cupcake dessert"),
    ("⚽", "soccer football sport"), ("🏀", "basketball sport"), ("🎾", "tennis sport"), ("🏈", "american football sport"),
    ("🏆", "trophy win award"), ("🎯", "target goal focus"), ("🎲", "dice game"), ("🧩", "puzzle piece"),
    ("🎸", "guitar music"), ("🎹", "piano keyboard music"), ("🎤", "microphone sing podcast"), ("🎵", "music note song"),
    ("📷", "camera photo"), ("🎥", "movie camera video"), ("📺", "television tv"), ("📻", "radio"),
    ("💻", "laptop computer code"), ("🖥", "desktop computer"), ("📱", "phone mobile"), ("⌨", "keyboard typing"),
    ("🖱", "mouse computer"), ("💾", "floppy save"), ("🔧", "wrench tool fix"), ("🔨", "hammer tool build"),
    ("⚙", "gear settings"), ("🔒", "lock secure private"), ("🔑", "key password access"), ("🛡", "shield security"),
    ("📦", "package box shipping"), ("📁", "folder files"), ("📝", "memo notes write"), ("📅", "calendar date schedule"),
    ("📌", "pushpin pin"), ("📎", "paperclip attach"), ("✏", "pencil write edit"), ("🖊", "pen write"),
    ("📖", "book open read"), ("📰", "newspaper news"), ("✉", "envelope mail email"), ("📮", "postbox mail"),
    ("💬", "speech chat message"), ("📣", "megaphone announce marketing"), ("🔔", "bell notification"), ("💰", "money bag finance"),
    ("💳", "credit card payment"), ("💎", "gem diamond"), ("🎁", "gift present"), ("🎉", "party popper celebrate"),
    ("🎓", "graduation cap school education"), ("🏫", "school education"), ("🏥", "hospital health"), ("💊", "pill medicine health"),
    ("🏋", "weight lifting gym fitness"), ("🧘", "yoga meditation calm"), ("🚴", "cycling bike sport"), ("🏃", "running sport"),
    ("✈", "airplane travel flight"), ("🚗", "car drive"), ("🚲", "bicycle bike"), ("🚂", "train travel"),
    ("⛵", "sailboat boat"), ("🏙", "city skyline urban"), ("🏛", "building classical museum"), ("⛪", "church"),
    ("🎪", "circus tent"), ("🧳", "luggage travel"), ("🔬", "microscope science research"), ("🔭", "telescope space astronomy"),
    ("🧬", "dna biology science"), ("⚗", "alembic chemistry"), ("🪐", "planet saturn space"), ("🌌", "milky way galaxy space"),
    ("♻", "recycle environment"), ("☮", "peace"), ("☯", "yin yang balance"), ("♟", "chess pawn strategy"),
    ("✅", "check done complete"), ("❌", "cross no cancel"), ("❓", "question help"), ("❗", "exclamation important"),
    ("🔴", "red circle"), ("🟠", "orange circle"), ("🟡", "yellow circle"), ("🟢", "green circle"),
    ("🔵", "blue circle"), ("🟣", "purple circle"), ("⚫", "black circle"), ("⚪", "white circle"),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph<'a> {
    Lucide(char),
    Text(&'a str),
    /// Empty, or a Lucide name this build doesn't have (for example from a newer
    /// device); drawn as a dot rather than its stored text.
    Missing,
}

pub fn parse(stored: &str) -> Glyph<'_> {
    if let Some(name) = stored.strip_prefix(LUCIDE_PREFIX) {
        return lucide_glyph(name).map_or(Glyph::Missing, Glyph::Lucide);
    }
    if stored.trim().is_empty() { Glyph::Missing } else { Glyph::Text(stored) }
}

pub fn lucide_glyph(name: &str) -> Option<char> {
    ICONS.binary_search_by(|(n, _)| n.cmp(&name)).ok().map(|i| ICONS[i].1)
}

pub fn lucide(name: &str) -> String {
    format!("{LUCIDE_PREFIX}{name}")
}

/// The words a screen reader and hover text use for an icon.
pub fn describe(stored: &str) -> String {
    if let Some(name) = stored.strip_prefix(LUCIDE_PREFIX) {
        return format!("{} icon", name.replace('-', " "));
    }
    EMOJI.iter().find(|(e, _)| *e == stored).map_or_else(|| stored.to_string(), |(_, words)| words.split(' ').next().unwrap_or(stored).to_string())
}

fn font(glyph: Glyph<'_>, size: f32) -> FontId {
    match glyph {
        Glyph::Lucide(_) => FontId::new(size, FontFamily::Name(super::icons::FONT.into())),
        _ => FontId::proportional(size),
    }
}

fn text(glyph: Glyph<'_>) -> String {
    match glyph {
        Glyph::Lucide(c) => c.to_string(),
        Glyph::Text(t) => t.to_string(),
        Glyph::Missing => "•".to_string(),
    }
}

/// Draws a Space's icon centered in `rect`. Lucide glyphs are centered on their
/// ink, like the chrome icons; text uses its line box like the other labels.
pub fn paint(p: &Painter, rect: Rect, stored: &str, size: f32, color: Color32) {
    let glyph = parse(stored);
    match glyph {
        Glyph::Lucide(c) => {
            let galley = p.layout_no_wrap(c.to_string(), font(glyph, size), color);
            p.galley(rect.center() - galley.mesh_bounds.center().to_vec2(), galley, color);
        }
        _ => {
            p.text(rect.center(), Align2::CENTER_CENTER, text(glyph), font(glyph, size), color);
        }
    }
}

/// `icon  name` as one label, with each part in its own font.
pub fn label_job(stored: &str, name: &str, size: f32, color: Color32) -> LayoutJob {
    let glyph = parse(stored);
    let mut job = LayoutJob::default();
    job.append(&text(glyph), 0.0, TextFormat::simple(font(glyph, size), color));
    job.append(name, size * 0.6, TextFormat::simple(FontId::proportional(size), color));
    job
}

/// Accepts a typed or pasted emoji, or up to two letters or digits.
pub fn custom_icon(input: &str) -> Result<String, &'static str> {
    let icon = input.trim();
    if icon.is_empty() {
        return Err("Type or paste an emoji.");
    }
    if icon.starts_with(LUCIDE_PREFIX) {
        return Err("Choose Lucide icons on the Icons tab.");
    }
    let plain = icon.chars().all(char::is_alphanumeric);
    if icon.chars().any(|c| c.is_whitespace() || c.is_control()) || icon.len() > MAX_CUSTOM_BYTES || (plain && icon.chars().count() > 2) {
        return Err("Use one emoji, or up to two letters.");
    }
    Ok(icon.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Emoji,
    Icons,
}

#[derive(Clone, Default)]
struct PickerState {
    tab: Option<Tab>,
    query: String,
    custom: String,
    error: Option<&'static str>,
}

fn matches(query: &str, words: &str) -> bool {
    let words = words.replace('-', " ");
    query.split_whitespace().all(|q| words.split(' ').any(|w| w.starts_with(&q.to_lowercase())))
}

/// Every choice on a tab for `query`, as (stored value, accessible name).
pub fn choices(tab: Tab, query: &str) -> Vec<(String, String)> {
    match tab {
        Tab::Emoji => EMOJI.iter().filter(|(_, words)| matches(query, words)).map(|(e, _)| (e.to_string(), describe(e))).collect(),
        Tab::Icons => {
            let suggested = SUGGESTED_ICONS.iter().copied();
            let rest = ICONS.iter().map(|(n, _)| *n).filter(|n| !SUGGESTED_ICONS.contains(n));
            suggested.chain(rest).filter(|n| matches(query, n)).map(|n| (lucide(n), describe(&lucide(n)))).collect()
        }
    }
}

fn cell(ui: &mut Ui, pal: &Palette, id: Id, stored: &str, label: &str, selected: bool, side: f32) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    let resp = ui.interact(rect, id.with(stored), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 7, pal.accent.gamma_multiply(0.35));
    } else if resp.hovered() {
        p.rect_filled(rect, 7, pal.panel_hover);
    }
    if resp.has_focus() {
        p.rect_stroke(rect.expand(1.0), 8, Stroke::new(1.5, pal.accent), egui::StrokeKind::Outside);
    }
    paint(p, rect, stored, (side * 0.52).round(), pal.panel_text);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(label)
}

/// Quick picks for the Space menu: the first suggestions of each kind.
pub fn quick_picks(ui: &mut Ui, pal: &Palette, id: Id, current: &str, width: f32) -> Option<String> {
    let mut chosen = None;
    let emoji = EMOJI.iter().take(10).map(|(e, _)| e.to_string());
    let icons = SUGGESTED_ICONS.iter().take(10).map(|n| lucide(n));
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(width);
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        for stored in emoji.chain(icons) {
            if cell(ui, pal, id, &stored, &describe(&stored), stored == current, 26.0).clicked() && stored != current {
                chosen = Some(stored);
            }
        }
    });
    chosen
}

/// The full chooser: Emoji or Icons, a search field, a scrolling grid and, for
/// emoji, a field to type or paste any other one. Returns the new stored value.
pub fn picker(ui: &mut Ui, pal: &Palette, id: Id, current: &str, segmented: impl FnOnce(&mut Ui, &mut Tab) -> bool) -> Option<String> {
    let mut state: PickerState = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut tab = state.tab.unwrap_or(if current.starts_with(LUCIDE_PREFIX) { Tab::Icons } else { Tab::Emoji });
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        if segmented(ui, &mut tab) {
            state.query.clear();
            state.error = None;
        }
        let hint = if tab == Tab::Emoji { "Search emoji" } else { "Search icons" };
        ui.add(egui::TextEdit::singleline(&mut state.query).id(id.with("search")).hint_text(hint).desired_width(ui.available_width().clamp(120.0, 220.0)));
    });
    state.tab = Some(tab);

    let items = choices(tab, &state.query);
    let side = 32.0;
    let gap = 4.0;
    if items.is_empty() {
        ui.label(RichText::new(format!("No {} match “{}”.", if tab == Tab::Emoji { "emoji" } else { "icons" }, state.query.trim())).size(12.5).color(pal.panel_dim));
    } else {
        let columns = (((ui.available_width() + gap) / (side + gap)).floor() as usize).max(1);
        let rows = items.len().div_ceil(columns);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(gap, gap);
            egui::ScrollArea::vertical().id_salt(id.with(("grid", tab as u8))).max_height(5.0 * (side + gap) - gap).auto_shrink([false, true]).show_rows(ui, side, rows, |ui, range| {
                for row in range {
                    ui.horizontal(|ui| {
                        for (stored, label) in items.iter().skip(row * columns).take(columns) {
                            if cell(ui, pal, id, stored, label, *stored == current, side).clicked() && *stored != current {
                                chosen = Some(stored.clone());
                            }
                        }
                    });
                }
            });
        });
    }

    if tab == Tab::Emoji {
        let ink = Ink::new(pal.panel, pal.accent);
        let mut submit = false;
        ui.horizontal_wrapped(|ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut state.custom).id(id.with("custom")).hint_text("Or type an emoji").char_limit(16).desired_width(140.0));
            if field.changed() {
                state.error = None;
            }
            submit |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            submit |= ui.add_enabled(!state.custom.trim().is_empty(), egui::Button::new("Use")).clicked();
        });
        if submit {
            match custom_icon(&state.custom) {
                Ok(icon) => {
                    state.custom.clear();
                    if icon != current {
                        chosen = Some(icon);
                    }
                }
                Err(error) => state.error = Some(error),
            }
        }
        if let Some(error) = state.error {
            ui.label(RichText::new(error).size(12.0).color(ink.danger));
        }
    }
    ui.data_mut(|d| d.insert_temp(id, state));
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        ctx
    }

    #[test]
    fn every_catalog_icon_is_named_once_in_order_and_renders_from_the_lucide_font() {
        let ctx = context();
        assert!(ICONS.len() > 1500, "the whole bundled set: {}", ICONS.len());
        assert!(ICONS.windows(2).all(|w| w[0].0 < w[1].0), "sorted and unique for binary search");
        let font = FontId::new(16.0, FontFamily::Name(super::super::icons::FONT.into()));
        ctx.fonts_mut(|fonts| {
            for (name, glyph) in ICONS {
                assert!(fonts.has_glyph(&font, *glyph), "{name} has no glyph");
            }
        });
        // The catalog agrees with the chrome's hand-picked mapping.
        for (name, glyph) in [("arrow-left", '\u{e04c}'), ("folder", '\u{e0dc}'), ("settings", '\u{e157}'), ("sparkles", '\u{e416}')] {
            assert_eq!(lucide_glyph(name), Some(glyph));
        }
        for name in SUGGESTED_ICONS {
            assert!(lucide_glyph(name).is_some(), "suggested icon {name} is not in the font");
        }
    }

    /// The emoji glyphs themselves come from the system font at run time (checked in
    /// the native app); here each entry must be a single, unique emoji code point.
    #[test]
    fn every_listed_emoji_is_one_unique_emoji_code_point_with_search_words() {
        let mut seen = std::collections::HashSet::new();
        for (emoji, words) in EMOJI {
            assert!(seen.insert(*emoji), "{emoji} listed twice");
            let mut chars = emoji.chars();
            let c = chars.next().unwrap();
            assert!(chars.next().is_none(), "{emoji} must be one code point");
            assert!(matches!(u32::from(c), 0x2300..=0x2BFF | 0x1F300..=0x1FAFF), "{emoji} is outside the emoji blocks");
            assert!(!words.trim().is_empty() && words == &words.to_lowercase(), "{emoji}: {words}");
        }
    }

    #[test]
    fn stored_values_parse_to_lucide_text_or_missing() {
        assert_eq!(parse("lucide:house"), Glyph::Lucide('\u{e0f8}'));
        assert_eq!(parse("🏠"), Glyph::Text("🏠"));
        assert_eq!(parse("AB"), Glyph::Text("AB"));
        assert_eq!(parse(""), Glyph::Missing);
        assert_eq!(parse("lucide:not-a-real-icon"), Glyph::Missing, "an unknown name never shows its raw text");
        assert_eq!(describe("lucide:flask-conical"), "flask conical icon");
        assert_eq!(describe("🏠"), "house");
    }

    #[test]
    fn typed_icons_are_one_emoji_or_two_letters() {
        assert_eq!(custom_icon(" 🦉 "), Ok("🦉".into()));
        assert_eq!(custom_icon("AB"), Ok("AB".into()));
        assert_eq!(custom_icon("🇫🇷"), Ok("🇫🇷".into()));
        assert!(custom_icon("").is_err());
        assert!(custom_icon("ABC").is_err());
        assert!(custom_icon("a b").is_err());
        assert!(custom_icon("lucide:house").is_err());
        assert!(custom_icon(&"🦉".repeat(9)).is_err());
    }

    #[test]
    fn search_matches_word_starts_on_both_tabs() {
        let icons = choices(Tab::Icons, "");
        assert_eq!(icons.len(), ICONS.len());
        assert_eq!(icons[0].0, "lucide:house", "suggestions come first");
        let flask: Vec<_> = choices(Tab::Icons, "flask").into_iter().map(|(s, _)| s).collect();
        assert!(flask.contains(&"lucide:flask-conical".to_string()) && flask.contains(&"lucide:flask-round".to_string()));
        assert!(choices(Tab::Icons, "conical flask").iter().any(|(s, _)| s == "lucide:flask-conical"));
        assert!(choices(Tab::Icons, "zzzz").is_empty());
        let coffee: Vec<_> = choices(Tab::Emoji, "Cof").into_iter().map(|(s, _)| s).collect();
        assert_eq!(coffee, ["☕"]);
        assert_eq!(choices(Tab::Emoji, "").len(), EMOJI.len());
    }


    /// The picker in a column as wide as the Settings body, with the caller's
    /// Emoji/Icons switch, driven by real pointer and keyboard events.
    struct Harness {
        ctx: egui::Context,
        width: f32,
        current: String,
        frame: u32,
    }

    struct Frame {
        chosen: Option<String>,
        cells: Vec<(String, Rect)>,
        text: Vec<(String, Rect)>,
        clip: Rect,
    }

    impl Harness {
        fn new(width: f32, current: &str) -> Self {
            Self { ctx: context(), width, current: current.into(), frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Frame {
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(self.width + 40.0, 700.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
            self.frame += 1;
            let id = Id::unique("picker-test");
            let current = self.current.clone();
            let column = Rect::from_min_size(egui::pos2(20.0, 20.0), vec2(self.width, 660.0));
            let mut chosen = None;
            let mut output = self.ctx.run_ui(input, |ui| {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(column));
                crate::ui::settings::set_visuals(&mut child, &pal);
                chosen = picker(&mut child, &pal, id, &current, |ui, tab| {
                    let before = *tab;
                    for (value, label) in [(Tab::Emoji, "Emoji"), (Tab::Icons, "Icons")] {
                        if ui.selectable_label(*tab == value, label).clicked() {
                            *tab = value;
                        }
                    }
                    *tab != before
                });
            });
            output.textures_delta.clear();
            let mut cells = Vec::new();
            for (stored, _) in choices(Tab::Emoji, "").into_iter().chain(choices(Tab::Icons, "")) {
                if let Some(resp) = self.ctx.read_response(id.with(&stored)) {
                    if resp.rect.is_positive() && resp.interact_rect.is_positive() {
                        cells.push((stored, resp.rect));
                    }
                }
            }
            let text = output.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.job.text.clone(), Rect::from_min_size(t.pos, t.galley.size()))),
                _ => None,
            }).collect();
            Frame { chosen, cells, text, clip: column }
        }

        fn settle(&mut self) -> Frame {
            for _ in 0..3 {
                self.frame(vec![]);
            }
            self.frame(vec![])
        }

        fn click_at(&mut self, pos: egui::Pos2) -> Option<String> {
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            let down = self.frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]).chosen;
            let up = self.frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]).chosen;
            down.or(up)
        }

        fn click_cell(&mut self, stored: &str) -> Option<String> {
            let shown = self.settle();
            let rect = shown.cells.iter().find(|(s, _)| s == stored).unwrap_or_else(|| panic!("{stored} not drawn")).1;
            self.click_at(rect.center())
        }

        fn click_text(&mut self, label: &str) {
            let shown = self.settle();
            let rect = shown.text.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("{label} not drawn")).1;
            self.click_at(rect.center());
        }

        fn type_into(&mut self, field: &str, text: &str) {
            let id = Id::unique("picker-test").with(field);
            self.ctx.memory_mut(|m| m.request_focus(id));
            self.frame(vec![]);
            self.frame(vec![egui::Event::Text(text.into())]);
        }

        fn press(&mut self, key: egui::Key) -> Option<String> {
            let down = self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]).chosen;
            let up = self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Default::default() }]).chosen;
            down.or(up)
        }
    }

    // Settings body widths at 640×420 (compact) and 1360×860.
    const WIDTHS: [f32; 2] = [560.0, 1080.0];

    #[test]
    fn clicking_an_emoji_or_a_lucide_icon_chooses_it_and_the_grid_fits_the_column() {
        for width in WIDTHS {
            let mut h = Harness::new(width, "🏠");
            let shown = h.settle();
            assert!(shown.text.iter().any(|(t, _)| t == "Emoji") && shown.text.iter().any(|(t, _)| t == "Icons"));
            assert!(shown.text.iter().any(|(t, _)| t == "Or type an emoji"), "custom field on the Emoji tab");
            assert!(shown.cells.len() > 20, "a grid of emoji at {width}");
            for (stored, rect) in &shown.cells {
                assert!(shown.clip.expand(0.5).contains_rect(*rect), "{stored} outside the column at {width}: {rect:?}");
            }
            assert_eq!(h.click_cell("☕"), Some("☕".into()));
            assert_eq!(h.click_cell("🏠"), None, "the current icon is not chosen again");

            h.click_text("Icons");
            let shown = h.settle();
            assert!(shown.cells.iter().any(|(s, _)| s == "lucide:house"), "suggested Lucide icons first");
            assert!(!shown.text.iter().any(|(t, _)| t == "Or type an emoji"));
            assert_eq!(h.click_cell("lucide:rocket"), Some("lucide:rocket".into()));
        }
    }

    #[test]
    fn searching_icons_narrows_the_grid_and_says_when_nothing_matches() {
        for width in WIDTHS {
            let mut h = Harness::new(width, "lucide:house");
            let shown = h.settle();
            assert!(shown.cells.iter().any(|(s, _)| s == "lucide:house"), "a Lucide icon opens on the Icons tab");
            h.type_into("search", "flask");
            let shown = h.settle();
            let drawn: Vec<_> = shown.cells.iter().map(|(s, _)| s.as_str()).collect();
            assert!(drawn.contains(&"lucide:flask-conical") && drawn.iter().all(|s| s.contains("flask")), "{drawn:?}");
            assert_eq!(h.click_cell("lucide:flask-conical"), Some("lucide:flask-conical".into()));
            h.type_into("search", "zzzz");
            let shown = h.settle();
            assert!(shown.cells.is_empty());
            assert!(shown.text.iter().any(|(t, _)| t == "No icons match “flaskzzzz”."), "{:?}", shown.text);
        }
    }

    #[test]
    fn a_typed_emoji_is_used_on_enter_and_an_unusable_one_is_explained() {
        for width in WIDTHS {
            let mut h = Harness::new(width, "🏠");
            h.settle();
            h.type_into("custom", "🦉");
            assert_eq!(h.press(egui::Key::Enter), Some("🦉".into()));
            h.type_into("custom", "ABC");
            assert_eq!(h.press(egui::Key::Enter), None);
            let shown = h.settle();
            assert!(shown.text.iter().any(|(t, _)| t == "Use one emoji, or up to two letters."), "{:?}", shown.text);
        }
    }

    #[test]
    fn grid_cells_are_reachable_and_chosen_from_the_keyboard() {
        let mut h = Harness::new(WIDTHS[0], "🏠");
        h.settle();
        let first = h.settle().cells[0].0.clone();
        for _ in 0..12 {
            h.press(egui::Key::Tab);
            let focused = h.ctx.memory(|m| m.focused());
            if focused == Some(Id::unique("picker-test").with(&first)) {
                assert_eq!(h.press(egui::Key::Enter), if first == "🏠" { None } else { Some(first.clone()) });
                h.press(egui::Key::Tab);
                let next = h.ctx.memory(|m| m.focused()).unwrap();
                let second = h.settle().cells[1].0.clone();
                assert_eq!(next, Id::unique("picker-test").with(&second));
                assert_eq!(h.press(egui::Key::Space), Some(second));
                return;
            }
        }
        panic!("the first cell never took keyboard focus");
    }
}
