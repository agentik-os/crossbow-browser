//! Settings → Passwords driven by simulated pointer and keyboard input inside
//! the real Settings sheet geometry, at 640×420 and 1360×860. Disposable
//! vaults, the memory key store and a fake pasteboard only; every secret is
//! a synthetic fixture.

use super::*;
use crate::passwords::tests::Harness;
use crate::passwords::{COPIED, COPY_UNAVAILABLE};
use crate::ui::settings::{Page, compact_navigation_needed, set_visuals, sheet_rect, show_backdrop, show_sheet};
use crossbow_core::vault::{EntryInput, STORAGE_ERROR, VaultError};
use egui::{Id, Rect, RichText, Vec2, pos2, vec2};

const BANNER: &str = "Preview — not security reviewed. Use test passwords only.";

fn sizes() -> [Vec2; 2] {
    [vec2(640.0, 420.0), vec2(1360.0, 860.0)]
}

struct Shown {
    text: Vec<(String, Rect, Rect)>,
}

impl Shown {
    fn has(&self, label: &str) -> bool {
        self.text.iter().any(|(text, _, _)| text == label)
    }

    fn mentions(&self, needle: &str) -> bool {
        self.text.iter().any(|(text, _, _)| text.contains(needle))
    }

    fn find(&self, label: &str, last: bool) -> Option<&(String, Rect, Rect)> {
        let mut matches = self.text.iter().filter(|(text, _, _)| text == label);
        if last { matches.next_back() } else { matches.next() }
    }
}

struct Sheet {
    h: Harness,
    ctx: egui::Context,
    size: Vec2,
    frame: u32,
    account: Option<String>,
    textures: std::collections::HashMap<egui::TextureId, egui::ColorImage>,
    shapes: Vec<egui::epaint::ClippedShape>,
    /// Every text egui put on its in-app clipboard.
    clipboard: Vec<String>,
    /// Whether the native window has keyboard focus.
    window_focused: bool,
}

impl Sheet {
    fn new(size: Vec2) -> Sheet {
        Sheet::with(Harness::new(), size)
    }

    fn with(h: Harness, size: Vec2) -> Sheet {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Sheet { h, ctx, size, frame: 0, account: None, textures: Default::default(), shapes: Vec::new(), clipboard: Vec::new(), window_focused: true }
    }

    /// One app frame: the logic pass's tick, then Settings with this page.
    fn frame(&mut self, events: Vec<egui::Event>) -> Shown {
        self.h.passwords.tick_at(self.account.as_deref(), true);
        self.draw(events)
    }

    /// A frame where Settings is closed or shows another page.
    fn frame_without_page(&mut self) {
        self.h.passwords.tick_at(self.account.as_deref(), false);
    }

    /// A frame while the window is hidden or minimised with Passwords open:
    /// eframe runs only the logic pass, so the page is not drawn.
    fn hidden_frame(&mut self) {
        self.h.passwords.tick_at(self.account.as_deref(), true);
    }

    fn draw(&mut self, events: Vec<egui::Event>) -> Shown {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), self.size);
        let rect = sheet_rect(screen, 1.0);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let mut input = egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(self.frame) / 60.0), events, focused: self.window_focused, ..Default::default() };
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(1.0);
        let passwords = &mut self.h.passwords;
        let mut output = self.ctx.run_ui(input, |ui| {
            show_backdrop(ui.ctx(), screen, 1.0);
            show_sheet(ui.ctx(), rect, |ui| {
                set_visuals(ui, &pal);
                // The sheet's panel and navigation column, as Settings paints them.
                ui.painter().rect_filled(rect, 14, pal.panel);
                ui.painter().rect_stroke(rect, 14, egui::Stroke::new(1.0, pal.panel_border), egui::StrokeKind::Inside);
                let body = if compact_navigation_needed(rect.size()) {
                    Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0))
                } else {
                    let nav = Rect::from_min_size(rect.min, vec2(196.0, rect.height()));
                    ui.painter().rect_filled(nav, egui::CornerRadius { nw: 14, sw: 14, ne: 0, se: 0 }, pal.panel_hover.gamma_multiply(0.8));
                    Rect::from_min_max(rect.min + vec2(224.0, 18.0), rect.max - vec2(28.0, 16.0))
                };
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                child.label(RichText::new("Passwords").size(20.0).strong().color(pal.panel_text));
                child.add_space(10.0);
                egui::ScrollArea::vertical().id_salt(("settings-scroll", Page::Passwords as u8)).auto_shrink([false, false]).show(&mut child, |ui| {
                    ui.set_width(body.width() - 8.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    show(passwords, ui, &pal);
                    ui.add_space(16.0);
                });
            });
        });
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command { self.clipboard.push(text.clone()); }
        }
        for (id, deltas) in std::mem::take(&mut output.textures_delta.set) {
            for delta in &deltas { self.update_texture(id, delta); }
        }
        self.frame += 1;
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        self.shapes = output.shapes;
        Shown { text }
    }

    fn settle(&mut self) -> Shown {
        for _ in 0..5 { self.frame(vec![]); }
        self.frame(vec![])
    }

    /// Scroll until `label` is fully inside the sheet, or fail.
    fn visible(&mut self, label: &str) -> Shown {
        self.scroll_to(label, false)
    }

    /// Scroll until `label`, or its last copy when `last`, is fully inside the sheet.
    fn scroll_to(&mut self, label: &str, last: bool) -> Shown {
        for attempt in 0..30 {
            let shown = self.settle();
            let delta = match shown.find(label, last) {
                Some((_, bounds, clip)) if clip.expand(0.5).contains_rect(*bounds) => return shown,
                Some((_, bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                None => if attempt < 15 { 100.0 } else { -100.0 },
            };
            self.wheel(delta);
        }
        let shown = self.settle();
        panic!("not visible: {label}; size={:?}; text={:?}", self.size, shown.text.iter().map(|(t, _, _)| t).collect::<Vec<_>>());
    }

    /// Whether `label` shows anywhere on the page, looking from top to bottom.
    /// egui paints only what is in view, so this scrolls.
    fn appears(&mut self, label: &str) -> bool {
        for _ in 0..10 { self.wheel(1000.0); }
        for _ in 0..30 {
            if self.settle().has(label) { return true; }
            self.wheel(-100.0);
        }
        false
    }

    fn wheel(&mut self, delta: f32) {
        let point = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0).right_bottom() - vec2(60.0, 40.0);
        self.frame(vec![
            egui::Event::PointerMoved(point),
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
        ]);
    }

    fn click(&mut self, label: &str) {
        self.click_at(label, false);
    }

    /// Click the last control showing `label`, such as the last entry's action.
    fn click_last(&mut self, label: &str) {
        self.click_at(label, true);
    }

    fn click_at(&mut self, label: &str, last: bool) {
        let shown = self.scroll_to(label, last);
        let pos = shown.find(label, last).unwrap().1.center();
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
        }
        self.settle();
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
        }
    }

    fn focused(&self) -> Option<Id> {
        self.ctx.memory(|memory| memory.focused())
    }

    fn focus_field(&mut self, name: &str) {
        let id = field_id(name);
        self.ctx.memory_mut(|memory| memory.request_focus(id));
        self.settle();
        assert_eq!(self.focused(), Some(id), "{name} can take focus at {:?}", self.size);
    }

    /// Replace the focused field's text, as a user selecting all and typing.
    fn type_text(&mut self, text: &str) {
        self.key(egui::Key::A, egui::Modifiers::COMMAND);
        self.frame(vec![egui::Event::Text(text.into())]);
        self.settle();
    }

    /// Tab, as a keyboard user would, until the control showing `label` has focus.
    fn tab_to(&mut self, label: &str) {
        for _ in 0..40 {
            self.key(egui::Key::Tab, egui::Modifiers::NONE);
            let shown = self.settle();
            let focused = self.focused().and_then(|id| self.ctx.read_response(id));
            if let Some(response) = focused {
                if shown.text.iter().any(|(text, bounds, _)| text == label && response.rect.contains(bounds.center())) { return; }
            }
        }
        panic!("not keyboard reachable: {label} at {:?}", self.size);
    }

    /// Every label stays inside the sheet's width.
    fn fits_width(&mut self) {
        let shown = self.settle();
        for (text, bounds, clip) in &shown.text {
            assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5, "{text:?} overflows at {:?}: {bounds:?} outside {clip:?}", self.size);
        }
    }

    /// Drag across a label, as a user selecting its text.
    fn drag_across(&mut self, label: &str) {
        let shown = self.visible(label);
        let bounds = shown.find(label, false).unwrap().1;
        let (from, to) = (bounds.left_center() + vec2(1.0, 0.0), bounds.right_center() - vec2(1.0, 0.0));
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![egui::Event::PointerMoved(from), button(from, true)]);
        self.frame(vec![egui::Event::PointerMoved(to)]);
        self.frame(vec![button(to, false)]);
    }

    /// Rewrite the vault file from outside, as damage or another app might.
    fn vault_file(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.h.dir.path().join(crossbow_core::vault::FILE_NAME)).unwrap()
    }

    fn add(&mut self, origin: &str, username: &str, password: &str) -> String {
        let vault = self.h.passwords.vault_mut().unwrap();
        vault.add(&EntryInput { origin, username, password, notes: "", profile: None }).unwrap()
    }

    fn update_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta) {
        let egui::ImageData::Color(patch) = &delta.image;
        let Some([x, y]) = delta.pos else {
            self.textures.insert(id, (**patch).clone());
            return;
        };
        let Some(texture) = self.textures.get_mut(&id) else { return };
        for row in 0..patch.size[1] {
            let to = (y + row) * texture.size[0] + x;
            texture.pixels[to..to + patch.size[0]].copy_from_slice(&patch.pixels[row * patch.size[0]..(row + 1) * patch.size[0]]);
        }
    }

    /// Rasterize the last frame to `target/lane-evidence/screens` for review:
    /// egui's own tessellation, filled in software, over a grey window.
    fn snapshot(&mut self, name: &str) {
        let (width, height) = (self.size.x as usize, self.size.y as usize);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), self.size);
        let mut pixels = vec![[0.55, 0.55, 0.57, 1.0]; width * height];
        for clipped in self.ctx.tessellate(std::mem::take(&mut self.shapes), 1.0) {
            let egui::epaint::Primitive::Mesh(mesh) = clipped.primitive else { continue };
            let Some(texture) = self.textures.get(&mesh.texture_id) else { continue };
            let clip = clipped.clip_rect.intersect(screen);
            for triangle in mesh.indices.chunks_exact(3) {
                fill_triangle(&mut pixels, width, clip, texture, [0, 1, 2].map(|i| mesh.vertices[triangle[i] as usize]));
            }
        }
        let bytes = pixels.iter().flat_map(|pixel| pixel.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/lane-evidence/screens");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_raw(width as u32, height as u32, bytes).unwrap().save(dir.join(format!("passwords-{name}-{width}x{height}.png"))).unwrap();
    }
}

/// Premultiplied, gamma-space blending, as egui's renderers do.
pub(crate) fn fill_triangle(pixels: &mut [[f32; 4]], width: usize, clip: Rect, texture: &egui::ColorImage, vertices: [egui::epaint::Vertex; 3]) {
    let [a, b, c] = vertices.map(|vertex| vertex.pos);
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let bounds = Rect::from_points(&[a, b, c]).intersect(clip);
    if area.abs() < 1e-6 || !bounds.is_positive() { return; }
    let channels = |color: egui::Color32| color.to_array().map(|value| f32::from(value) / 255.0);
    let colors = vertices.map(|vertex| channels(vertex.color));
    for y in bounds.min.y.floor() as usize..bounds.max.y.ceil() as usize {
        for x in bounds.min.x.floor() as usize..bounds.max.x.ceil() as usize {
            let p = pos2(x as f32 + 0.5, y as f32 + 0.5);
            let w0 = ((b.x - p.x) * (c.y - p.y) - (b.y - p.y) * (c.x - p.x)) / area;
            let w1 = ((c.x - p.x) * (a.y - p.y) - (c.y - p.y) * (a.x - p.x)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 || !clip.contains(p) { continue; }
            let uv = vertices[0].uv.to_vec2() * w0 + vertices[1].uv.to_vec2() * w1 + vertices[2].uv.to_vec2() * w2;
            let tx = ((uv.x * texture.size[0] as f32) as usize).min(texture.size[0] - 1);
            let ty = ((uv.y * texture.size[1] as f32) as usize).min(texture.size[1] - 1);
            let texel = channels(texture.pixels[ty * texture.size[0] + tx]);
            let dst = &mut pixels[y * width + x];
            let src: [f32; 4] = std::array::from_fn(|i| (colors[0][i] * w0 + colors[1][i] * w1 + colors[2][i] * w2) * texel[i]);
            *dst = std::array::from_fn(|i| src[i] + dst[i] * (1.0 - src[3]));
        }
    }
}

#[test]
fn unlock_add_generate_reveal_copy_edit_delete_and_lock_with_the_pointer() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.visible(BANNER);
        page.fits_width();
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "{:?}", page.h.passwords.page.error);
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        assert!(page.h.keys.contains(&id), "the first unlock created the vault key");
        page.visible("Lock Now");
        page.visible("No saved passwords yet.");

        page.click("Add Password");
        assert_eq!(page.focused(), Some(field_id("passwords-origin")), "Add focuses the website field");
        page.type_text("fixture.example/login");
        page.focus_field("passwords-username");
        page.type_text("fixture-user");
        page.click("Generate");
        let generated = page.h.passwords.page.editor.as_ref().unwrap().password.to_string();
        assert_eq!(generated.chars().count(), 20);
        assert!(!page.settle().mentions(&generated), "a generated password stays masked");
        page.click("Show Password");
        page.visible(&generated);
        page.click("Hide Password");
        assert!(!page.settle().mentions(&generated));
        page.focus_field("passwords-notes");
        page.type_text("fixture note");
        page.fits_width();
        page.click("Save");
        assert!(page.h.passwords.page.editor.is_none());
        page.visible("Password added.");
        page.visible("https://fixture.example");
        page.visible("fixture-user");
        assert!(!page.settle().mentions(&generated), "the list never shows a password unasked");

        page.click("Reveal");
        page.visible(&generated);
        page.fits_width();
        page.click("Hide");
        assert!(!page.settle().mentions(&generated));

        page.click("Copy");
        assert_eq!(page.h.board.text().as_deref(), Some(generated.as_str()));
        page.visible(COPIED);

        page.click("Edit");
        assert_eq!(page.h.passwords.page.editor.as_ref().unwrap().password.as_str(), generated, "Edit opens the saved password");
        assert_eq!(page.h.passwords.page.editor.as_ref().unwrap().notes.as_str(), "fixture note");
        page.focus_field("passwords-username");
        page.type_text("fixture-user-2");
        page.click("Save");
        page.visible("Password saved.");
        page.visible("fixture-user-2");

        page.click("Delete…");
        page.visible("Delete the password for fixture-user-2 on https://fixture.example? This can't be undone.");
        page.fits_width();
        page.click("Cancel");
        assert_eq!(page.h.passwords.vault().unwrap().entries().len(), 1, "Cancel deletes nothing");
        page.click("Delete…");
        page.click("Delete");
        page.visible("Password deleted.");
        page.visible("No saved passwords yet.");

        page.click("Lock Now");
        assert!(!page.h.passwords.unlocked());
        let shown = page.visible("Unlock");
        assert!(!shown.mentions("fixture"), "nothing opened stays on screen after locking");
        page.fits_width();
    }
}

#[test]
fn saved_entries_survive_a_restart_and_need_unlocking_again() {
    let mut page = Sheet::new(vec2(1360.0, 860.0));
    page.click("Unlock");
    page.click("Add Password");
    page.type_text("restart.example");
    page.focus_field("passwords-password");
    page.type_text("fixture-password-restart");
    page.click("Save");
    page.visible("https://restart.example");
    page.h.reopen();
    let shown = page.visible("Unlock");
    assert!(!shown.mentions("restart.example"), "a relaunched vault starts locked");
    page.click("Unlock");
    page.visible("https://restart.example");
    page.click("Reveal");
    page.visible("fixture-password-restart");
}

#[test]
fn key_store_failures_and_a_missing_key_are_explained_and_reset_needs_confirmation() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.h.keys.fail_reads(true);
        page.click("Unlock");
        page.visible(STORAGE_ERROR);
        assert!(!page.h.passwords.unlocked());
        page.fits_width();
        page.h.keys.fail_reads(false);
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "Unlock retries after the store recovers");
        page.add("example.com", "fixture", "fixture-password-reset");
        page.click("Lock Now");

        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.h.keys.forget(&id);
        page.click("Unlock");
        page.visible(&VaultError::MissingKey.to_string());
        page.fits_width();
        page.click("Reset Vault…");
        page.visible("Delete 1 saved password from this vault file and start a new, empty vault with a new key? No existing key is deleted, since a copy of this vault, such as a backup, may still need it. This can't be undone.");
        page.fits_width();
        page.click("Cancel");
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 1, "Cancel deletes nothing");
        page.click("Reset Vault…");
        page.click("Delete All and Reset");
        page.visible("The vault was reset. Unlock to start a new one.");
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 0);
        assert_ne!(page.h.passwords.vault().unwrap().id(), id, "a new vault id, so a copy's key is never replaced");
        page.click("Unlock");
        page.visible("No saved passwords yet.");
    }
}

#[test]
fn search_filters_entries_and_the_keyboard_reaches_every_action() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.settle();
        page.tab_to("Unlock");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.unlocked(), "Unlock works from the keyboard");
        page.add("bank.example", "fixture-a", "fixture-password-1");
        page.add("mail.example", "fixture-b", "fixture-password-2");
        page.add("shop.example", "bank-user", "fixture-password-3");
        page.settle();

        page.focus_field("passwords-search");
        page.type_text("BANK");
        assert!(page.appears("https://bank.example") && page.appears("bank-user"), "search matches websites and usernames");
        assert!(!page.appears("https://mail.example"), "search hides entries that don't match");
        page.type_text("nothing-matches");
        page.visible("No passwords match this search.");
        page.type_text("");
        page.key(egui::Key::Backspace, egui::Modifiers::NONE);
        page.visible("https://mail.example");

        page.tab_to("Reveal");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        assert_eq!(page.h.passwords.page.revealed.len(), 1, "Reveal works from the keyboard");
        page.tab_to("Add Password");
        page.key(egui::Key::Space, egui::Modifiers::NONE);
        assert!(page.h.passwords.page.editor.is_some(), "Add Password works from the keyboard");
        page.type_text("keyboard.example");
        page.tab_to("Cancel");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.editor.is_none(), "Cancel discards the draft");
        assert_eq!(page.h.passwords.vault().unwrap().entries().len(), 3);
    }
}

#[test]
fn invalid_input_is_explained_and_the_draft_is_kept_for_retry() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        page.type_text("ftp://fixture.example");
        page.click("Save");
        page.visible("Enter a website address that starts with https:// or http://.");
        page.fits_width();
        page.visible("Enter a website address that starts with https:// or http://.");
        page.focus_field("passwords-origin");
        page.type_text("fixture.example");
        page.click("Save");
        page.visible("Enter a password, or generate one.");
        assert_eq!(page.h.passwords.page.editor.as_ref().unwrap().origin.as_str(), "fixture.example", "the draft is kept");
        page.click("Generate");
        page.click("Save");
        page.visible("Password added.");
        // The generator explains impossible rules instead of producing nothing.
        page.click("Add Password");
        for class in ["a–z", "A–Z", "0–9", "Symbols"] { page.click(class); }
        page.click("Generate");
        page.visible("Choose at least one kind of character.");
        assert!(page.h.passwords.page.editor.as_ref().unwrap().password.is_empty());
    }
}

#[test]
fn damaged_entries_are_explained_and_can_only_be_deleted() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        let good = page.add("good.example", "fixture-good", "fixture-password-good");
        let bad = page.add("bad.example", "fixture-bad", "fixture-password-bad");
        page.click("Lock Now");
        let conn = rusqlite::Connection::open(page.h.dir.path().join(crossbow_core::vault::FILE_NAME)).unwrap();
        conn.execute("UPDATE entries SET username = x'01' WHERE id = ?1", [&bad]).unwrap();
        drop(conn);
        page.click("Unlock");
        let shown = page.visible("Damaged entry");
        assert!(!shown.mentions("bad.example") && !shown.mentions("fixture-bad"));
        page.visible(&VaultError::Unreadable.to_string());
        page.fits_width();
        page.visible("https://good.example");
        // Damaged entries sort last, and offer only Delete.
        assert_eq!(page.h.passwords.vault().unwrap().entries().last().map(|entry| entry.id.as_str()), Some(bad.as_str()));
        page.click_last("Delete…");
        assert_eq!(page.h.passwords.page.confirm_delete.as_deref(), Some(bad.as_str()));
        page.visible("Delete this damaged entry? This can't be undone.");
        page.click("Delete");
        let shown = page.visible("Password deleted.");
        assert!(!shown.has("Damaged entry"));
        let left: Vec<&str> = page.h.passwords.vault().unwrap().entries().iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(left, [good.as_str()], "only the damaged entry was deleted");
    }
}

#[test]
fn leaving_the_page_or_locking_hides_every_secret() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.account = Some("fixture-account".into());
        page.click("Unlock");
        page.add("leave.example", "fixture", "fixture-password-leave");
        page.click("Reveal");
        page.visible("fixture-password-leave");
        page.click("Edit");
        // Settings closes (Escape, the close button or another page).
        page.frame_without_page();
        page.frame_without_page();
        let shown = page.settle();
        assert!(!shown.mentions("fixture-password-leave"), "revealed passwords are hidden after leaving the page");
        assert!(page.h.passwords.page.editor.is_none(), "drafts are discarded");
        for name in crate::passwords::FIELD_IDS[1..].iter() {
            assert!(egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).is_none(), "{name} keeps no egui state");
        }
        assert!(page.h.passwords.unlocked(), "leaving the page doesn't lock");
        page.click("Reveal");
        page.visible("fixture-password-leave");
        // Signing out locks and hides it.
        page.account = None;
        let shown = page.visible("Unlock");
        assert!(!shown.mentions("fixture-password-leave"));
        page.visible("Locked because you signed out.");
    }
}

#[test]
fn copy_is_disabled_and_explained_without_a_supported_clipboard() {
    for size in sizes() {
        let mut page = Sheet::with(Harness::with_copy(false), size);
        page.click("Unlock");
        page.add("copy.example", "fixture", "fixture-password-copy");
        page.visible(COPY_UNAVAILABLE);
        page.click("Copy");
        assert_eq!(page.h.board.text(), None);
        page.fits_width();
    }
}

#[test]
fn secret_fields_keep_no_undo_history() {
    let mut page = Sheet::new(vec2(1360.0, 860.0));
    page.click("Unlock");
    page.click("Add Password");
    page.focus_field("passwords-password");
    page.type_text("fixture-password-undo");
    page.focus_field("passwords-origin");
    page.type_text("undo.example");
    for name in &crate::passwords::FIELD_IDS[1..] {
        let undo = egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).map(|state| state.undoer());
        assert!(undo.is_none_or(|undo| !undo.has_undo(&Default::default()) && !undo.is_in_flux()), "{name} keeps undo history");
    }
    // Undo can't bring a typed password back after it was cleared.
    page.focus_field("passwords-password");
    page.type_text("");
    page.key(egui::Key::Backspace, egui::Modifiers::NONE);
    page.key(egui::Key::Z, egui::Modifiers::COMMAND);
    assert!(page.h.passwords.page.editor.as_ref().unwrap().password.is_empty());
}

const SHARED: &str = "Every Crossbow account and profile for this macOS user shares it.";
const QA_BANNER: &str = "QA key file mode (CROSSBOW_VAULT_KEY_FILE=1): the vault key is a plain file beside the vault, not in the Keychain. Anyone with a copy of the data folder can open these passwords.";

#[test]
fn a_vault_file_damaged_at_open_offers_try_again_and_a_confirmed_reset() {
    for size in sizes() {
        let h = Harness::new();
        let path = h.dir.path().join(crossbow_core::vault::FILE_NAME);
        std::fs::write(&path, vec![0x5a; 8192]).unwrap();
        let mut page = Sheet::with(h, size);
        page.visible(&VaultError::Damaged.to_string());
        page.fits_width();
        page.click("Try Again");
        page.visible(&VaultError::Damaged.to_string());
        page.click("Reset Vault…");
        page.visible(MOVE_PROMPT);
        page.fits_width();
        page.click("Cancel");
        assert_eq!(std::fs::read(&path).unwrap(), vec![0x5a; 8192], "Cancel changes nothing");
        page.click("Reset Vault…");
        page.click("Move Aside and Reset");
        let (moved, notice) = moved_notice(&page);
        page.visible(&notice);
        page.fits_width();
        assert_eq!(std::fs::read(&moved).unwrap(), vec![0x5a; 8192], "the damaged file is kept, moved aside");
        page.click("Unlock");
        page.visible("No saved passwords yet.");
    }
}

#[test]
fn a_damaged_check_is_repaired_quietly_and_reset_is_never_the_default() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("example.com", "fixture", "fixture-password-check");
        page.click("Lock Now");
        let flip_check = |page: &Sheet| {
            let conn = page.vault_file();
            let mut check: Vec<u8> = conn.query_row("SELECT v FROM meta WHERE k = 'check'", [], |r| r.get(0)).unwrap();
            let last = check.len() - 1;
            check[last] ^= 0x01;
            conn.execute("UPDATE meta SET v = ?1 WHERE k = 'check'", [check]).unwrap();
        };
        flip_check(&page);
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "the stored key opens the entries, so the vault opens");
        page.visible(crate::passwords::CHECK_REPAIRED);
        page.visible("https://example.com");
        page.click("Lock Now");

        // A key that opens nothing: Try Again comes first, Reset only after confirming.
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.h.keys.replace(&id, crossbow_core::vault::VaultKey::from_slice(&[9; 32]).unwrap());
        page.click("Unlock");
        page.visible(&VaultError::KeyMismatch.to_string());
        let shown = page.visible("Reset Vault…");
        let (retry, reset) = (shown.find("Try Again", false).expect("Try Again is offered").1, shown.find("Reset Vault…", false).unwrap().1);
        assert!(retry.left() < reset.left() || retry.top() < reset.top(), "Try Again comes before Reset");
        page.fits_width();
        page.click("Try Again");
        page.visible(&VaultError::KeyMismatch.to_string());
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 1, "Try Again deletes nothing");

        // Only a newer format row means a newer Crossbow: update, no reset.
        // 11 is the first format past 8 that no flipped bit of a known one reaches.
        page.vault_file().execute("UPDATE meta SET v = 11 WHERE k = 'format'", []).unwrap();
        page.h.passwords.retry_open();
        page.visible(&VaultError::Newer.to_string());
        page.visible("Try Again");
        assert!(!page.appears("Reset Vault…"), "an update, not a reset, fixes a newer vault");
    }
}

/// The second recheck's dead end: one flipped bit took the check's epoch from
/// 1 to 257 and the page said "update Crossbow" with nothing to press.
#[test]
fn a_flipped_bit_in_the_check_header_is_repaired_quietly_on_the_page() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("example.com", "fixture", "fixture-password-flip");
        page.click("Lock Now");
        let conn = page.vault_file();
        let mut check: Vec<u8> = conn.query_row("SELECT v FROM meta WHERE k = 'check'", [], |r| r.get(0)).unwrap();
        check[3] ^= 1;
        conn.execute("UPDATE meta SET v = ?1 WHERE k = 'check'", [check]).unwrap();
        page.click("Unlock");
        assert!(page.h.passwords.unlocked());
        page.visible(crate::passwords::CHECK_REPAIRED);
        page.visible("https://example.com");
        assert!(!page.appears(&VaultError::Newer.to_string()));
        page.fits_width();
    }
}

#[test]
fn revealed_passwords_and_notes_never_reach_the_in_app_clipboard() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("copy.example", "fixture-user-copy", "fixture-password-select");
        page.click("Reveal");
        page.drag_across("fixture-password-select");
        page.frame(vec![egui::Event::Copy]);
        page.frame(vec![egui::Event::Cut]);
        page.settle();

        page.click("Edit");
        page.click("Show Password");
        page.focus_field("passwords-password");
        page.key(egui::Key::A, egui::Modifiers::COMMAND);
        page.frame(vec![egui::Event::Copy]);
        page.frame(vec![egui::Event::Cut]);
        page.settle();
        assert_eq!(page.h.passwords.page.editor.as_ref().unwrap().password.as_str(), "fixture-password-select", "Cut removes nothing");
        page.focus_field("passwords-notes");
        page.type_text("fixture-note-secret");
        page.key(egui::Key::A, egui::Modifiers::COMMAND);
        page.frame(vec![egui::Event::Copy]);
        page.settle();
        // A username is not secret: copying it still works, so the probe is live.
        page.focus_field("passwords-username");
        page.key(egui::Key::A, egui::Modifiers::COMMAND);
        page.frame(vec![egui::Event::Copy]);
        page.settle();
        assert!(page.clipboard.iter().any(|text| text == "fixture-user-copy"), "{:?}", page.clipboard);
        for secret in ["fixture-password-select", "fixture-note-secret"] {
            assert!(page.clipboard.iter().all(|text| !text.contains(secret)), "{secret} reached egui's clipboard at {size:?}");
        }
    }
}

#[test]
fn vault_fields_are_never_written_to_egui_storage() {
    #[derive(Default)]
    struct Storage(std::collections::HashMap<String, String>);
    impl eframe::Storage for Storage {
        fn get_string(&self, key: &str) -> Option<String> { self.0.get(key).cloned() }
        fn set_string(&mut self, key: &str, value: String) { self.0.insert(key.into(), value); }
        fn remove_string(&mut self, key: &str) { self.0.remove(key); }
        fn flush(&mut self) {}
    }
    let mut page = Sheet::new(vec2(1360.0, 860.0));
    page.click("Unlock");
    page.focus_field("passwords-search");
    page.type_text("fixture-search");
    page.click("Add Password");
    page.type_text("storage.example");
    for (name, text) in [("passwords-username", "fixture-user"), ("passwords-password", "fixture-password-of-41-characters-length"), ("passwords-notes", "fixture notes")] {
        page.focus_field(name);
        page.type_text(text);
    }
    assert!(egui::text_edit::TextEditState::load(&page.ctx, field_id("passwords-password")).is_some(), "the field keeps its state in memory");
    // Save egui's memory as eframe's autosave does, then load it as a relaunch would.
    let mut storage = Storage::default();
    eframe::set_value(&mut storage, "egui", &page.ctx.memory(|memory| memory.clone()));
    let restored: egui::Memory = eframe::get_value(&storage, "egui").unwrap();
    let relaunched = egui::Context::default();
    relaunched.memory_mut(|memory| *memory = restored);
    for name in crate::passwords::FIELD_IDS {
        assert!(egui::text_edit::TextEditState::load(&relaunched, field_id(name)).is_none(), "{name}'s cursor and length reached egui storage");
    }
}

#[test]
fn the_page_says_every_account_and_profile_on_this_mac_shares_the_vault() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        assert!(page.settle().mentions(SHARED), "the locked page says who shares the vault at {size:?}");
        page.click("Unlock");
        assert!(page.settle().mentions("Signing out doesn't hide it from other Crossbow accounts for this macOS user."), "{size:?}");
        page.fits_width();
    }
}

#[test]
fn qa_key_file_mode_is_stated_plainly_and_never_called_the_keychain() {
    for size in sizes() {
        let h = Harness::new();
        h.keys.report_qa_file(true);
        let says_keychain = |shown: &Shown| shown.text.iter().any(|(text, _, _)| text != QA_BANNER && text.contains("Keychain"));
        let mut page = Sheet::with(h, size);
        let shown = page.visible(QA_BANNER);
        page.fits_width();
        assert!(shown.mentions("a plain QA key file beside the vault"), "{size:?}");
        assert!(!says_keychain(&shown), "QA mode never says the key is in the Keychain");
        page.click("Unlock");
        assert!(!says_keychain(&page.visible(QA_BANNER)));
        page.add("example.com", "fixture", "fixture-password-qa");
        page.click("Lock Now");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.h.keys.forget(&id);
        page.click("Unlock");
        page.click("Reset Vault…");
        assert!(!says_keychain(&page.visible("Delete All and Reset")));
    }
}

#[test]
fn an_entry_with_another_envelope_epoch_is_damaged_and_can_be_deleted() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("current.example", "fixture-current", "fixture-password-current");
        let other = page.add("other.example", "fixture-other", "fixture-password-other");
        page.click("Lock Now");
        let conn = page.vault_file();
        let mut origin: Vec<u8> = conn.query_row("SELECT origin FROM entries WHERE id = ?1", [&other], |r| r.get(0)).unwrap();
        origin[1..5].copy_from_slice(&513u32.to_be_bytes());
        conn.execute("UPDATE entries SET origin = ?2 WHERE id = ?1", rusqlite::params![other, origin]).unwrap();
        page.click("Unlock");
        page.visible("Damaged entry");
        assert!(!page.appears("Saved by a newer Crossbow"), "inside a current file, nothing is newer");
        page.fits_width();
        page.click_last("Delete…");
        page.visible("Delete this damaged entry? This can't be undone.");
        page.click("Delete");
        page.visible("Password deleted.");
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 1);
    }
}

#[test]
fn focusing_the_password_field_holds_secure_input_until_blur_lock_or_leaving() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        assert!(!page.h.secure.held());
        page.focus_field("passwords-password");
        assert!(page.h.secure.held(), "typing a password holds Secure Event Input");
        page.focus_field("passwords-username");
        assert!(!page.h.secure.held(), "leaving the field releases it");
        page.focus_field("passwords-password");
        page.click("Lock Now");
        assert!(!page.h.secure.held(), "locking releases it");
        page.click("Unlock");
        page.click("Add Password");
        page.focus_field("passwords-password");
        assert!(page.h.secure.held());
        page.frame_without_page();
        page.frame_without_page();
        assert!(!page.h.secure.held(), "closing Settings releases it");
        let calls = page.h.secure.calls();
        assert!(calls.chunks(2).all(|pair| pair == [true, false]), "every enable is matched by one disable: {calls:?}");
    }
}

#[test]
fn secure_input_is_released_while_another_app_is_active() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        page.focus_field("passwords-password");
        assert!(page.h.secure.held());
        page.window_focused = false;
        page.settle();
        assert!(!page.h.secure.held(), "another app is typing: secure input would block its keystroke features");
        assert_eq!(page.focused(), Some(field_id("passwords-password")), "the field keeps egui focus");
        page.window_focused = true;
        page.settle();
        assert!(page.h.secure.held(), "coming back to the field holds it again");
        let calls = page.h.secure.calls();
        assert!(calls.chunks(2).all(|pair| pair == [true, false] || pair == [true]), "balanced: {calls:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_reset_that_cannot_move_the_damaged_file_aside_says_why() {
    use std::os::unix::fs::PermissionsExt;
    for size in sizes() {
        let h = Harness::new();
        std::fs::write(h.dir.path().join(crossbow_core::vault::FILE_NAME), vec![0x5a; 8192]).unwrap();
        let dir = h.dir.path().to_owned();
        let mut page = Sheet::with(h, size);
        page.visible(&VaultError::Damaged.to_string());
        page.click("Reset Vault…");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        page.click("Move Aside and Reset");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        page.visible(&VaultError::Damaged.to_string());
        page.visible(&VaultError::Storage.to_string());
        page.fits_width();
        page.click("Reset Vault…");
        page.click("Move Aside and Reset");
        page.visible(&moved_notice(&page).1);
        assert!(!page.appears(&VaultError::Storage.to_string()), "a reset that worked leaves no error behind");
    }
}

/// Rewrite the version and epoch of a sealed value in the vault file.
fn set_header(page: &Sheet, sql_select: &str, sql_update: &str, key: &str, version: u8, epoch: u32) {
    let conn = page.vault_file();
    let mut sealed: Vec<u8> = conn.query_row(sql_select, [key], |r| r.get(0)).unwrap();
    sealed[0] = version;
    sealed[1..5].copy_from_slice(&epoch.to_be_bytes());
    conn.execute(sql_update, rusqlite::params![key, sealed]).unwrap();
}

#[test]
fn a_damaged_check_header_opens_quietly_and_a_damaged_entry_header_can_be_deleted() {
    const CHECK: (&str, &str) = ("SELECT v FROM meta WHERE k = ?1", "UPDATE meta SET v = ?2 WHERE k = ?1");
    const ORIGIN: (&str, &str) = ("SELECT origin FROM entries WHERE id = ?1", "UPDATE entries SET origin = ?2 WHERE id = ?1");
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("kept.example", "fixture-kept", "fixture-password-kept");
        let damaged = page.add("damaged.example", "fixture-damaged", "fixture-password-damaged");
        page.click("Lock Now");
        set_header(&page, CHECK.0, CHECK.1, "check", 0, 1);
        set_header(&page, ORIGIN.0, ORIGIN.1, &damaged, 0, 1);
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "a zeroed version byte is damage, and the key still opens the entries");
        page.visible(crate::passwords::CHECK_REPAIRED);
        page.visible("https://kept.example");
        assert!(!page.appears("Saved by a newer Crossbow"), "zero is never a newer Crossbow");
        page.visible("Damaged entry");
        page.fits_width();
        page.click_last("Delete…");
        page.visible("Delete this damaged entry? This can't be undone.");
        page.click("Delete");
        page.visible("Password deleted.");
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 1);

        // With a key that opens nothing, the same damage offers Try Again and Reset.
        page.click("Lock Now");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.h.keys.replace(&id, crossbow_core::vault::VaultKey::from_slice(&[9; 32]).unwrap());
        set_header(&page, CHECK.0, CHECK.1, "check", 0, 1);
        page.click("Unlock");
        page.visible(&VaultError::KeyMismatch.to_string());
        page.visible("Try Again");
        page.visible("Reset Vault…");
    }
}

#[test]
fn hiding_the_window_keeps_the_typed_draft_and_releases_secure_input() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        page.type_text("draft.example");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-draft");
        assert!(page.h.secure.held());
        // ⌘H: the window loses focus and becomes invisible; eframe runs logic passes only.
        page.window_focused = false;
        page.h.passwords.set_secure_input(false); // what the AppKit observer does at once
        for _ in 0..3 { page.hidden_frame(); }
        assert!(!page.h.secure.held());
        // Back again: the draft is where the user left it.
        page.window_focused = true;
        page.settle();
        let editor = page.h.passwords.page.editor.as_ref().expect("the draft survives hiding");
        assert_eq!((editor.origin.as_str(), editor.password.as_str()), ("draft.example", "fixture-password-draft"));
        assert!(page.h.secure.held(), "the password field still has focus");
        page.click("Save");
        page.visible("Password added.");
    }
}

#[test]
fn a_save_after_a_reset_in_another_window_locks_and_says_so() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("before.example", "fixture", "fixture-password-before");
        page.click("Add Password");
        page.type_text("after.example");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-after");
        // Another window, or another process on this data folder, resets the vault.
        let mut other = crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap();
        other.reset().unwrap();
        drop(other);
        page.click("Save");
        assert!(!page.h.passwords.unlocked(), "the stale session locks");
        page.visible(KEPT);
        let editor = page.h.passwords.page.editor.as_ref().expect("the draft is kept, not wiped");
        assert_eq!((editor.origin.as_str(), editor.password.as_str()), ("after.example", "fixture-password-after"));
        page.visible("Discard Entry");
        page.fits_width();
        // After unlocking the new vault, the same draft saves into it.
        page.click("Unlock");
        page.visible("No saved passwords yet.");
        assert_eq!(page.h.passwords.page.editor.as_ref().map(|editor| editor.origin.as_str()), Some("after.example"));
        page.click("Save");
        page.visible("Password added.");
        page.visible("https://after.example");
        let id = page.h.passwords.vault().unwrap().entries()[0].id.clone();
        assert_eq!(page.h.passwords.vault().unwrap().password(&id).unwrap().as_str(), "fixture-password-after");
    }
}

const KEPT: &str = "The vault was reset in another window. Unlock again to save this entry.";

/// The second recheck's ⌘M probe: Crossbow stays active, eframe runs logic
/// passes only, and no app-level notification is posted.
#[cfg(target_os = "macos")]
#[test]
fn minimising_releases_secure_input_at_once_and_restoring_holds_it_again_only_with_focus() {
    use objc2_app_kit::NSWindowDidMiniaturizeNotification;
    let mut page = Sheet::new(vec2(1360.0, 860.0));
    page.h.passwords.observe_app_deactivation();
    page.click("Unlock");
    page.click("Add Password");
    page.type_text("draft.example");
    page.focus_field("passwords-password");
    page.type_text("fixture-password-draft");
    assert!(page.h.secure.held());
    unsafe { objc2_foundation::NSNotificationCenter::defaultCenter().postNotificationName_object(NSWindowDidMiniaturizeNotification, None) };
    assert!(!page.h.secure.held(), "released by the notification, before any frame");
    page.window_focused = false;
    for _ in 0..5 { page.hidden_frame(); }
    page.h.advance(std::time::Duration::from_secs(60));
    page.hidden_frame();
    assert!(!page.h.secure.held(), "still released a minute later");
    assert!(page.h.passwords.page.editor.is_some(), "the draft is kept");
    // Restored, but another app is active: frames are drawn without focus.
    page.settle();
    assert!(!page.h.secure.held(), "not held again without keyboard focus");
    page.window_focused = true;
    page.settle();
    assert!(page.h.secure.held(), "held again once the window has focus and the field is still focused");
    assert_eq!(page.h.secure.calls(), [true, false, true], "balanced");
}

#[test]
fn a_lost_vault_id_is_repaired_by_one_reset_on_the_page() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.vault_file().execute("DELETE FROM meta WHERE k = 'vault_id'", []).unwrap();
        page.click("Unlock");
        page.visible(&VaultError::Damaged.to_string());
        page.click("Reset Vault…");
        page.click("Delete All and Reset");
        page.visible("The vault was reset. Unlock to start a new one.");
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "no Reset loop and no relaunch");
        page.visible("No saved passwords yet.");
        page.add("two.example", "fixture", "fixture-password-two");
        page.settle();
        page.visible("https://two.example");
    }
}

/// An Edit draft holds the entry's decrypted password and notes, which a reset
/// in another window deleted. Changed or not, found by Save or by Key Details,
/// it is discarded, and nothing opened from the old vault stays in the page,
/// the vault, egui's field state or the clipboard.
#[test]
fn a_reset_elsewhere_discards_an_edit_draft_and_keeps_nothing_from_the_old_vault() {
    const OLD_PASSWORD: &str = "fixture-password-reset-away";
    const OLD_NOTES: &str = "fixture-note-reset-away";
    for size in sizes() {
        for (changed, via_key_details) in [(false, false), (false, true), (true, false), (true, true)] {
            let case = format!("changed={changed} via_key_details={via_key_details} at {size:?}");
            let mut page = Sheet::new(size);
            page.click("Unlock");
            page.h.passwords.vault_mut().unwrap().add(&EntryInput { origin: "bank.example", username: "fixture-reset", password: OLD_PASSWORD, notes: OLD_NOTES, profile: None }).unwrap();
            page.settle();
            page.click("Reveal");
            page.click("Copy");
            assert_eq!(page.h.board.text().as_deref(), Some(OLD_PASSWORD));
            if via_key_details { page.click("Key Details"); }
            page.click("Edit");
            if changed {
                page.focus_field("passwords-password");
                page.type_text("fixture-password-edited");
            }
            let mut other = crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap();
            other.reset().unwrap();
            drop(other);
            if via_key_details { page.settle(); } else { page.click("Save"); }
            assert!(!page.h.passwords.unlocked(), "{case}");
            page.visible(crate::passwords::RESET_ELSEWHERE_EDIT_DISCARDED);
            page.fits_width();
            let state = &page.h.passwords;
            assert!(state.page.editor.is_none() && !state.kept_draft(), "{case}: the draft is gone");
            assert!(state.page.revealed.is_empty(), "{case}");
            assert!(state.vault().unwrap().entries().is_empty(), "{case}");
            assert_eq!(page.h.board.text(), None, "{case}: the copied password was cleared");
            for name in FIELD_IDS {
                assert!(egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).is_none(), "{case}: {name} keeps no egui state");
            }
            assert!(!page.appears("Discard Entry"), "{case}");
            // Unlock, which needs no credential and makes the new vault's key, brings nothing back.
            page.click("Unlock");
            let shown = page.visible("No saved passwords yet.");
            assert!(page.h.passwords.page.editor.is_none(), "{case}");
            assert!(!shown.mentions("bank.example") && !shown.mentions("fixture-reset") && !shown.mentions(OLD_PASSWORD) && !shown.mentions(OLD_NOTES), "{case}");
        }
    }
}

#[test]
fn a_kept_draft_is_discarded_only_with_a_reason() {
    let mut page = Sheet::new(vec2(1360.0, 860.0));
    page.account = Some("fixture-account".into());
    let keep_one = |page: &mut Sheet| {
        page.click("Unlock");
        page.click("Add Password");
        page.type_text("kept.example");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-kept");
        crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap().reset().unwrap();
        page.click("Save");
        page.visible(KEPT);
        assert!(!page.appears("kept.example"), "locked, the draft is never shown");
    };
    // The user can drop it without unlocking.
    keep_one(&mut page);
    page.click("Discard Entry");
    assert!(page.h.passwords.page.editor.is_none());
    page.visible("The unsaved entry was discarded.");
    // Five minutes without activity discard it, as the idle lock wipes any draft.
    keep_one(&mut page);
    page.h.advance(crossbow_core::vault::IDLE_TIMEOUT);
    page.settle();
    assert!(page.h.passwords.page.editor.is_none());
    page.visible("The unsaved entry was discarded after 5 minutes without activity.");
    // Signing out discards it and says so.
    keep_one(&mut page);
    page.account = None;
    page.settle();
    assert!(page.h.passwords.page.editor.is_none(), "a kept draft never outlives the signed-in account");
    page.visible("You signed out, so the unsaved entry was discarded.");
    // Leaving the page discards it, as it does any draft.
    page.account = Some("fixture-account".into());
    keep_one(&mut page);
    page.frame_without_page();
    assert!(page.h.passwords.page.editor.is_none());
}

#[test]
fn key_details_name_the_live_keychain_item() {
    let item = |id: &str, made: &str| format!("Keychain item “app.crossbow.vault”, account {id}{made}. This vault uses only this item. Items under other accounts belong to earlier resets or other data folders.");
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.click("Key Details");
        page.visible(&item(&id, ""));
        page.fits_width();
        page.click("Hide Key Details");
        assert!(!page.appears(&item(&id, "")));
        // A reset in another window while the details are open: they follow
        // it at once, and the unlocked session locks rather than show the old id.
        page.click("Key Details");
        let path = page.h.dir.path().to_owned();
        let mut other = crossbow_core::vault::Vault::open(&path, Box::new(page.h.keys.clone())).unwrap();
        other.reset().unwrap();
        let new_id = other.id().to_owned();
        drop(other);
        assert_ne!(new_id, id);
        page.visible(&item(&new_id, ", which the first unlock creates"));
        assert!(!page.h.passwords.unlocked());
        page.visible(&VaultError::ResetElsewhere.to_string());
        assert!(!page.appears(&item(&id, "")), "the old id is gone");
        page.click("Unlock");
        page.visible(&item(&new_id, ""));
        page.fits_width();
    }
}

#[test]
fn qa_key_file_mode_shows_a_badge_and_names_the_file() {
    for size in sizes() {
        let h = Harness::new();
        h.keys.report_qa_file(true);
        let mut page = Sheet::with(h, size);
        page.visible("QA key file");
        page.click("Key Details");
        let shown = page.settle();
        assert!(shown.text.iter().any(|(text, _, _)| text.starts_with("QA key file vault-") && text.ends_with(".key, in the data folder.")), "{:?}", shown.text.iter().map(|(t, _, _)| t).collect::<Vec<_>>());
        page.fits_width();
    }
    let mut page = Sheet::new(vec2(1360.0, 860.0));
    assert!(!page.settle().has("QA key file"), "no badge with the Keychain");
}

#[test]
fn a_scheme_typed_without_its_slashes_is_refused_in_the_editor() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        page.type_text("https//bank.example");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-typo");
        page.click("Save");
        page.visible("Check the Website field. Enter an address like https://example.com.");
        assert_eq!(page.h.passwords.page.editor.as_ref().unwrap().origin.as_str(), "https//bank.example", "the draft stays for a correction");
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 0);
    }
}

#[test]
fn entries_name_their_profile_and_last_use_and_never_saved_sites_can_be_removed() {
    const A: &str = "fixture-profile-a";
    const B: &str = "fixture-profile-b";
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
        page.settle();
        let vault = page.h.passwords.vault().unwrap();
        vault.never_save(A, "https://login.example").unwrap();
        vault.never_save(B, "https://work.example").unwrap();
        // Listed while locked, and only for the active profile.
        assert!(page.settle().mentions(PROFILES), "the locked page says passwords follow profiles");
        page.visible("NEVER SAVED");
        page.visible("https://login.example");
        assert!(!page.appears("https://work.example"), "another profile's site isn't listed at {size:?}");
        page.fits_width();
        page.click("Remove");
        assert!(!page.appears("NEVER SAVED"), "the section goes with its last site");
        assert!(page.h.passwords.never_saved(A).is_empty());
        assert_eq!(page.h.passwords.never_saved(B), ["https://work.example"], "Remove touches only this profile");

        page.click("Unlock");
        let mut add = |origin: &str, username: &str, profile: Option<&str>| {
            page.h.passwords.vault_mut().unwrap().add(&EntryInput { origin, username, password: "fixture-password", notes: "", profile }).unwrap()
        };
        let used = add("https://login.example", "fixture-a", Some(A));
        add("https://work.example", "fixture-b", Some(B));
        let legacy = add("https://old.example", "fixture-c", None);
        page.h.passwords.vault_mut().unwrap().mark_used(&used, crossbow_core::now_ms() - 3 * 86_400_000).unwrap();
        page.visible("Profile: Personal · Last used 3 days ago");
        page.visible("Profile: Work · Never used on a page");
        page.visible("In no profile, so never offered on pages · Never used on a page");
        page.fits_width();
        page.click("Use in Current Profile");
        assert!(!page.appears("Use in Current Profile"), "one click, and only untagged entries have it");
        assert_eq!(page.h.passwords.vault().unwrap().entry(&legacy).unwrap().profile.as_deref(), Some(A));
        assert!(!page.appears("In no profile, so never offered on pages · Never used on a page"));
        assert_eq!(page.h.passwords.logins_for("https://old.example", A).len(), 1, "now filled in this profile");
    }
}

#[test]
fn last_use_is_counted_in_whole_days() {
    let day = 86_400_000;
    let now = 100 * day + 5;
    assert_eq!(last_used(None, now), "Never used on a page");
    assert_eq!(last_used(Some(now - 1), now), "Last used today");
    assert_eq!(last_used(Some(now - day), now), "Last used yesterday");
    assert_eq!(last_used(Some(now - 9 * day - 1), now), "Last used 9 days ago");
    assert_eq!(last_used(Some(now + day), now), "Last used today", "a clock moved back isn't a future use");
}

/// Review images of each state, with fixture data and every password masked:
/// `cargo test -p crossbow review_images -- --ignored`.
#[test]
#[ignore = "writes PNGs to target/lane-evidence/screens for manual review"]
fn review_images() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.settle();
        page.snapshot("1-locked");
        page.h.keys.fail_reads(true);
        page.click("Unlock");
        page.snapshot("2-key-store-error");
        page.h.keys.fail_reads(false);
        page.click("Unlock");
        page.add("bank.example", "fixture-a", "fixture-password-1");
        page.add("mail.example:8443", "fixture-b", "fixture-password-2");
        page.add("shop.example", "", "fixture-password-3");
        page.settle();
        page.snapshot("3-list");
        page.click("Add Password");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-typed");
        page.snapshot("4-add");
        page.click("Cancel");
        page.click_last("Edit");
        page.snapshot("5-edit");
        page.click("Cancel");
        page.click_last("Delete…");
        page.snapshot("6-delete");
        page.click("Cancel");
        page.click("Copy");
        page.snapshot("7-copied");
        page.click("Key Details");
        page.snapshot("7b-key-details");
        page.click("Hide Key Details");

        page.click("Lock Now");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        let right = crossbow_core::vault::VaultKeyStore::load(&page.h.keys, &id).unwrap().unwrap();
        page.h.keys.replace(&id, crossbow_core::vault::VaultKey::from_slice(&[9; 32]).unwrap());
        page.click("Unlock");
        page.snapshot("8-key-mismatch");
        page.click("Reset Vault…");
        page.snapshot("9-reset-confirm");
        page.click("Cancel");
        page.h.keys.replace(&id, right);
        let conn = page.vault_file();
        let mut check: Vec<u8> = conn.query_row("SELECT v FROM meta WHERE k = 'check'", [], |r| r.get(0)).unwrap();
        let last = check.len() - 1;
        check[last] ^= 0x01;
        conn.execute("UPDATE meta SET v = ?1 WHERE k = 'check'", [check]).unwrap();
        let damaged: String = conn.query_row("SELECT id FROM entries ORDER BY created_ms LIMIT 1", [], |r| r.get(0)).unwrap();
        let mut origin: Vec<u8> = conn.query_row("SELECT origin FROM entries WHERE id = ?1", [&damaged], |r| r.get(0)).unwrap();
        origin[1..5].copy_from_slice(&2u32.to_be_bytes());
        conn.execute("UPDATE entries SET origin = ?2 WHERE id = ?1", rusqlite::params![damaged, origin]).unwrap();
        page.click("Try Again");
        page.snapshot("10-check-repaired-and-damaged-entry");

        let h = Harness::new();
        std::fs::write(h.dir.path().join(crossbow_core::vault::FILE_NAME), vec![0x5a; 8192]).unwrap();
        h.keys.report_qa_file(true);
        let mut page = Sheet::with(h, size);
        page.settle();
        page.snapshot("11-damaged-file-in-qa-mode");
        page.click("Reset Vault…");
        page.snapshot("12-damaged-file-reset-confirm");

        let h = Harness::new();
        h.keys.report_qa_file(true);
        let mut page = Sheet::with(h, size);
        page.click("Key Details");
        page.snapshot("13-qa-badge-and-key-details");

        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        page.type_text("draft.example");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-draft");
        crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap().reset().unwrap();
        page.click("Save");
        page.snapshot("14-reset-in-another-window");
        page.click("Unlock");
        page.snapshot("15-kept-draft-after-unlock");

        let mut page = Sheet::new(size);
        two_profiles(&mut page, "fixture-profile-a");
        page.visible(OFFER_TO_SAVE);
        page.snapshot("16-offer-to-save-off");
        let (track, _) = page.switch(OFFER_TO_SAVE);
        page.click_point(track.center());
        page.visible(OFFER_TO_SAVE);
        page.snapshot("17-offer-to-save-on");
    }
}

// -- round 4 ---------------------------------------------------------------------------------

/// The third recheck's wrong-typed values, on the page after a relaunch. None
/// says to check permissions and disk space, and each leaves a recovery.
#[test]
fn a_wrong_typed_value_is_never_shown_as_a_storage_error() {
    let storage = VaultError::Storage.to_string();
    for size in sizes() {
        // A non-UTF-8 vault id: the damaged-file path, Try Again first and a confirmed Reset.
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.vault_file().execute("UPDATE meta SET v = CAST(x'3031ff32' AS TEXT) WHERE k = 'vault_id'", []).unwrap();
        page.h.reopen();
        page.visible(&VaultError::Damaged.to_string());
        page.fits_width();
        assert!(!page.appears(&storage), "at {size:?}");
        page.click("Try Again");
        page.visible(&VaultError::Damaged.to_string());
        page.click("Reset Vault…");
        page.visible(MOVE_PROMPT);
        page.click("Move Aside and Reset");
        page.visible(&moved_notice(&page).1);
        page.click("Unlock");
        page.visible("No saved passwords yet.");

        // One entry's origin retyped: that entry alone is damaged and deletable.
        let mut page = Sheet::new(size);
        page.click("Unlock");
        let bad = page.add("one.example", "fixture", "fixture-password-one");
        page.add("two.example", "fixture", "fixture-password-two");
        page.click("Lock Now");
        page.vault_file().execute("UPDATE entries SET origin = CAST(origin AS TEXT) WHERE id = ?1", [&bad]).unwrap();
        page.h.reopen();
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "at {size:?}: {:?}", page.h.passwords.page.error);
        page.visible("Damaged entry");
        page.visible("https://two.example");
        page.fits_width();
        assert!(!page.appears(&storage), "at {size:?}");
        page.click_last("Delete…");
        page.click("Delete");
        page.visible("Password deleted.");
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 1);

        // The check retyped: the stored key opens the entries, so the check is rewritten.
        page.click("Lock Now");
        page.vault_file().execute("UPDATE meta SET v = CAST(v AS TEXT) WHERE k = 'check'", []).unwrap();
        page.h.reopen();
        page.click("Unlock");
        assert!(page.h.passwords.unlocked(), "at {size:?}: {:?}", page.h.passwords.page.error);
        page.visible(crate::passwords::CHECK_REPAIRED);
        page.visible("https://two.example");
        assert!(!page.appears(&storage), "at {size:?}");
    }
}

/// A kept draft lasts 5 minutes from the last input, as any draft does before
/// the idle lock: Key Details finding the reset, and the frames drawn every
/// second after it, never restart the limit. The third recheck's probe kept
/// one until 598 s without input. Input after the reset, or Save pressed to
/// find it, does restart it.
#[test]
fn a_kept_draft_lasts_5_minutes_from_the_last_input_however_the_reset_is_found() {
    use std::time::Duration;
    let (mut held_until, mut expected) = (Vec::new(), Vec::new());
    // (how the reset is found, when, the last input after it, the last second the draft is held)
    for (found_by, reset_at, input_at, last_held) in [
        ("none", None, None, 299),
        ("key details", Some(1), None, 299),
        ("key details", Some(299), None, 299),
        ("key details", Some(100), Some(200), 499),
        ("save", Some(299), None, 598),
    ] {
        for size in sizes() {
            let case = format!("{found_by} at {reset_at:?}, input at {input_at:?}, {size:?}");
            let mut page = Sheet::new(size);
            page.click("Unlock");
            page.click("Add Password");
            page.type_text("kept.example");
            page.focus_field("passwords-password");
            page.type_text("fixture-password-kept");
            // Key Details checks for a reset on every frame it is drawn.
            if found_by != "save" { page.click("Key Details"); }
            // From here on, one frame a second, as the holding repaint draws, and no input.
            let mut held = None;
            for second in 1..=620 {
                page.h.advance(Duration::from_secs(1));
                if reset_at == Some(second) {
                    crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap().reset().unwrap();
                    if found_by == "save" { page.click("Save"); }
                }
                if input_at == Some(second) { page.key(egui::Key::Tab, egui::Modifiers::NONE); }
                page.frame(vec![]);
                if page.h.passwords.page.editor.is_some() { held = Some(second); }
            }
            held_until.push((case.clone(), held));
            expected.push((case.clone(), Some(last_held)));
            let notice = if reset_at.is_some() { "The unsaved entry was discarded after 5 minutes without activity." } else { "Locked after 5 minutes without activity." };
            assert_eq!(page.h.passwords.page.notice.as_deref(), Some(notice), "{case}");
        }
    }
    assert_eq!(held_until, expected, "the last second each draft was held, without input");
}

/// The third recheck's V9 gap, in the editor: the message names the fix, and
/// the corrected address saves.
#[test]
fn a_dotless_website_without_a_scheme_is_refused_in_the_editor() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Add Password");
        page.type_text("htps/bank.example");
        page.focus_field("passwords-password");
        page.type_text("fixture-password-typo");
        page.click("Save");
        page.visible("Enter the full site address, like bank.example.");
        page.fits_width();
        assert_eq!(page.h.passwords.vault().unwrap().count().unwrap(), 0, "at {size:?}");
        page.focus_field("passwords-origin");
        page.type_text("http://router");
        page.click("Save");
        page.visible("Password added.");
        page.visible("http://router");
    }
}

const OFFER_TO_SAVE: &str = "Offer to save passwords from websites";
const OFFER_PREVIEW: &str = "Preview: use test sites and test passwords only until the vault passes its security review.";
const OFFER_APPLIES: &str = "For this profile on this device. Changes apply to pages loaded afterwards.";

#[test]
fn offer_to_save_is_shown_off_by_default_with_the_preview_line_locked_or_unlocked() {
    for size in sizes() {
        for unlock in [false, true] {
            let mut page = Sheet::new(size);
            page.h.passwords.set_profiles("fixture-profile-a", [("fixture-profile-a".to_owned(), "Personal".to_owned())]);
            if unlock { page.click("Unlock"); }
            // The Passwords | Addresses switch above can push the row's lines
            // apart across the fold at 640×420: each is scrolled to.
            for line in [OFFER_PREVIEW, OFFER_TO_SAVE, OFFER_APPLIES] { page.visible(line); }
            page.fits_width();
        }
    }
}

impl Sheet {
    /// The switch in the row titled `title`, as painted: its track and whether
    /// its knob sits on the right, which is on.
    fn switch(&mut self, title: &str) -> (Rect, bool) {
        // The switch sits beside the row's title and description, so it can
        // still be below the fold once the title is in view: scroll on.
        for _ in 0..10 {
            let shown = self.visible(title);
            let clip = shown.find(title, false).unwrap().2;
            let found = self.switch_now(&shown, title).expect("a switch in the row");
            if clip.expand(0.5).contains_rect(found.0) { return found; }
            self.wheel(-40.0);
        }
        panic!("the switch beside {title:?} never comes into view at {:?}", self.size);
    }

    /// The switch as the last frame painted it, without scrolling.
    fn switch_now(&self, shown: &Shown, title: &str) -> Option<(Rect, bool)> {
        let row = shown.find(title, false)?.1;
        let (top, bottom) = (row.top() - 4.0, row.bottom() + 40.0);
        let mut track = None;
        let mut knob = None;
        for clipped in &self.shapes {
            match &clipped.shape {
                egui::Shape::Rect(rect) if (rect.rect.size() - vec2(38.0, 22.0)).length() < 0.5 && rect.rect.top() >= top && rect.rect.bottom() <= bottom => track = Some(rect.rect),
                egui::Shape::Circle(circle) if (circle.radius - 9.0).abs() < 0.01 && circle.center.y >= top && circle.center.y <= bottom => knob = Some(circle.center),
                _ => {}
            }
        }
        let (track, knob) = (track?, knob?);
        assert!(track.contains(knob));
        Some((track, knob.x > track.center().x))
    }

    fn click_point(&mut self, pos: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
        }
        self.settle();
    }
}

fn two_profiles(page: &mut Sheet, current: &str) {
    page.h.passwords.set_profiles(current, [("fixture-profile-a".to_owned(), "Personal".to_owned()), ("fixture-profile-b".to_owned(), "Work".to_owned())]);
}

#[test]
fn the_offer_switch_turns_on_by_click_or_keyboard_for_the_active_profile_only() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        two_profiles(&mut page, "fixture-profile-a");
        page.click("Unlock");
        let (track, on) = page.switch(OFFER_TO_SAVE);
        assert!(!on && !page.h.passwords.offer_to_save("fixture-profile-a"), "off by default at {size:?}");
        assert!(page.shapes.iter().any(|clipped| clipped.clip_rect.expand(0.5).contains_rect(track)), "the whole switch is in view");
        page.click_point(track.center());
        assert!(page.switch(OFFER_TO_SAVE).1 && page.h.passwords.offer_to_save("fixture-profile-a"), "on after a click at {size:?}");
        assert!(!page.h.passwords.offer_to_save("fixture-profile-b"));
        assert_eq!(page.h.passwords.offer_to_save_settings(), [("fixture-profile-a".to_owned(), true)], "one change for the app to save");
        // Another profile has its own switch.
        two_profiles(&mut page, "fixture-profile-b");
        assert!(!page.switch(OFFER_TO_SAVE).1);
        two_profiles(&mut page, "fixture-profile-a");
        assert!(page.switch(OFFER_TO_SAVE).1);
        // Keyboard: Tab reaches the switch and Space turns it off.
        let mut reached = false;
        for _ in 0..40 {
            page.key(egui::Key::Tab, egui::Modifiers::NONE);
            let shown = page.settle();
            let track = page.switch_now(&shown, OFFER_TO_SAVE).map(|(track, _)| track);
            if track.is_some_and(|track| page.focused().and_then(|id| page.ctx.read_response(id)).is_some_and(|response| response.rect.contains(track.center()))) { reached = true; break; }
        }
        assert!(reached, "the switch is keyboard reachable at {size:?}");
        page.key(egui::Key::Space, egui::Modifiers::NONE);
        page.settle();
        assert!(!page.switch(OFFER_TO_SAVE).1 && !page.h.passwords.offer_to_save("fixture-profile-a"), "Space turns it off at {size:?}");
        page.fits_width();
    }
}

#[test]
fn a_switch_change_that_cannot_be_saved_is_off_and_the_page_says_why() {
    for size in sizes() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("library.sqlite");
        let mut lib = crossbow_core::library::Library::open(crossbow_core::store::Store::open(&db).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TRIGGER reject_switch BEFORE INSERT ON kv WHEN NEW.k LIKE 'passwords.offer_to_save.v1.%' BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
        let mut page = Sheet::new(size);
        page.h.passwords.set_profiles(&profile, [(profile.clone(), "Personal".to_owned())]);
        // The app's logic pass, then the page.
        crate::passwords::sync_offer_to_save(&mut page.h.passwords, &lib);
        let (track, _) = page.switch(OFFER_TO_SAVE);
        page.click_point(track.center());
        crate::passwords::sync_offer_to_save(&mut page.h.passwords, &lib);
        let shown = page.settle();
        assert!(shown.mentions("Could not save this setting: ") && shown.mentions("It is still off."), "{size:?}");
        assert!(!page.switch(OFFER_TO_SAVE).1);
        page.fits_width();
    }
}

// -- round 5: one reset handler for every write, and a Reset that is checked --------------------

const MOVE_PROMPT: &str = "Move the damaged vault file aside and start a new, empty vault with a new key? The old file stays in Crossbow's data folder, and no existing key is deleted.";

/// The one file a replacement moved aside, and the notice that names it.
fn moved_notice(page: &Sheet) -> (std::path::PathBuf, String) {
    let moved: Vec<String> = std::fs::read_dir(page.h.dir.path()).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("vault-damaged-") && name.ends_with(".sqlite")).collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    (page.h.dir.path().join(&moved[0]), crate::passwords::moved_aside(&moved[0]))
}

/// Q4, ported from the fourth recheck's probe: a reset in another window
/// found by Use in Current Profile, a page offer's Save or Update, or a fill.
/// Each goes through the page's one reset handling: it locks, forgets every
/// revealed password and the copy, drops an Edit draft with its decrypted
/// values, keeps a New draft hidden for at most 5 minutes after the last
/// input, and says why.
#[test]
fn a_reset_elsewhere_found_by_any_page_or_offer_write_is_handled_as_one() {
    use std::time::Duration;
    const A: &str = "fixture-profile-a";
    const OLD_PASSWORD: &str = "fixture-r5-old-password";
    const DRAFT_PASSWORD: &str = "fixture-r5-draft-password";
    for via in ["Use in Current Profile", "an offer's Save", "an offer's Update", "a fill"] {
        for draft in ["edit", "new"] {
            let case = format!("{via}, {draft} draft");
            let mut page = Sheet::new(vec2(1360.0, 860.0));
            two_profiles(&mut page, A);
            page.click("Unlock");
            let old = page.h.passwords.vault_mut().unwrap().add(&EntryInput { origin: "bank.example", username: "fixture-r5", password: OLD_PASSWORD, notes: "fixture-r5-notes", profile: Some(A) }).unwrap();
            page.h.passwords.vault_mut().unwrap().add(&EntryInput { origin: "legacy.example", username: "fixture-r5-legacy", password: "fixture-r5-legacy-password", notes: "", profile: None }).unwrap();
            page.settle();
            page.click("Reveal");
            page.click("Copy");
            assert_eq!(page.h.board.text().as_deref(), Some(OLD_PASSWORD), "{case}");
            page.h.passwords.set_offer_to_save(A, true);
            match via {
                "an offer's Save" => page.h.passwords.offer_login("tab-1", A, "https://site.example", "fixture-r5-user", "fixture-r5-page-password"),
                "an offer's Update" => page.h.passwords.offer_login("tab-1", A, "https://bank.example", "fixture-r5", "fixture-r5-page-password"),
                _ => {}
            }
            if draft == "edit" {
                page.click("Edit");
            } else {
                page.click("Add Password");
                page.type_text("draft.example");
                page.focus_field("passwords-password");
                page.type_text(DRAFT_PASSWORD);
            }
            // Another window resets the vault.
            crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap().reset().unwrap();
            match via {
                "Use in Current Profile" => page.click("Use in Current Profile"),
                "a fill" => assert_eq!(page.h.passwords.login_to_fill(&old, "https://bank.example", A).map(|_| ()), Err(crate::passwords::FILL_GONE), "{case}"),
                _ => assert_eq!(page.h.passwords.save_offer("tab-1"), None, "{case}"),
            }
            page.settle();
            let state = &page.h.passwords;
            assert!(!state.unlocked(), "{case}");
            assert!(state.page.revealed.is_empty(), "{case}: no revealed password is kept");
            assert_ne!(page.h.board.text().as_deref(), Some(OLD_PASSWORD), "{case}: the copy is cleared");
            if via.starts_with("an offer") { assert_eq!(state.offer("tab-1").and_then(|offer| offer.error.clone()), Some(VaultError::ResetElsewhere), "{case}"); }
            if draft == "edit" {
                assert!(state.page.editor.is_none(), "{case}: the Edit draft and its decrypted values are gone");
                page.visible(crate::passwords::RESET_ELSEWHERE_EDIT_DISCARDED);
            } else {
                assert!(state.kept_draft() && state.page.editor.as_ref().is_some_and(|editor| editor.password.as_str() == DRAFT_PASSWORD), "{case}: the New draft is kept");
                page.visible(crate::passwords::RESET_ELSEWHERE_KEPT);
            }
            // Ten minutes of frames without input: a kept draft goes 5 minutes after the last input.
            let mut last_present = None;
            for second in 1..=600 {
                page.h.advance(Duration::from_secs(1));
                page.frame(vec![]);
                if page.h.passwords.page.editor.is_some() { last_present = Some(second); }
            }
            match draft {
                "edit" => assert_eq!(last_present, None, "{case}"),
                _ => assert!(last_present.is_some_and(|second| second <= 300), "{case}: kept until {last_present:?} s"),
            }
            page.click("Unlock");
            let shown = page.settle();
            assert!(page.h.passwords.unlocked(), "{case}");
            assert!(page.h.passwords.page.editor.is_none() && !shown.mentions("bank.example") && !shown.mentions(OLD_PASSWORD), "{case}: nothing of the old vault comes back");
        }
    }
}

/// Q1 and Q3 on the page, ported from the reviewer's probe: a renamed entries
/// column, the entries table on its own index's page, a lost table and lost
/// meta rows. Each is the damaged-file state at both sizes; one Reset moves
/// the file aside, says where, and the new vault unlocks now and after a
/// relaunch. Reset is never offered again.
#[test]
fn schema_damage_on_the_page_recovers_with_one_reset_that_moves_the_file_aside() {
    let edits = [
        ("renamed column", "PRAGMA writable_schema = ON; UPDATE sqlite_master SET sql = replace(sql, 'origin BLOB', 'nrigin BLOB') WHERE name = 'entries'; PRAGMA writable_schema = OFF;"),
        ("root page on its index", "PRAGMA writable_schema = ON; UPDATE sqlite_master SET rootpage = (SELECT rootpage FROM sqlite_master WHERE name = 'sqlite_autoindex_entries_1') WHERE name = 'entries'; PRAGMA writable_schema = OFF;"),
        ("lost tables", "DROP TABLE entries; DROP TABLE never_save;"),
        ("lost format and id rows", "DELETE FROM meta WHERE k IN ('format', 'vault_id');"),
    ];
    for (case, edit) in edits {
        for size in sizes() {
            let mut page = Sheet::new(size);
            page.click("Unlock");
            page.add("one.example", "fixture", "fixture-password-one");
            page.add("two.example", "fixture", "fixture-password-two");
            page.click("Lock Now");
            page.h.reopen();
            {
                let conn = page.vault_file();
                conn.execute_batch(edit).unwrap();
                let version: i64 = conn.query_row("PRAGMA schema_version", [], |r| r.get(0)).unwrap();
                conn.execute_batch(&format!("PRAGMA schema_version = {}", version + 1)).unwrap();
            }
            page.h.reopen();
            page.visible(&VaultError::Damaged.to_string());
            page.fits_width();
            assert!(!page.appears(&VaultError::Storage.to_string()), "{case} at {size:?}");
            page.click("Reset Vault…");
            page.visible(MOVE_PROMPT);
            page.fits_width();
            page.click("Move Aside and Reset");
            let (moved, notice) = moved_notice(&page);
            page.visible(&notice);
            page.fits_width();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&moved).unwrap().permissions().mode() & 0o777, 0o600, "{case}");
            }
            page.click("Unlock");
            page.visible("No saved passwords yet.");
            assert!(!page.appears("Reset Vault…"), "{case} at {size:?}: no second Reset");
            page.h.reopen();
            page.click("Unlock");
            page.visible("No saved passwords yet.");
        }
    }
}

/// Q2 on the page: a Reset of an openable vault that leaves the file damaged
/// (here, a table the reset never touches) is never reported as reset. The
/// file is moved aside, the page says so, and the new vault works.
#[test]
fn a_reset_that_leaves_damage_moves_the_file_aside_instead_of_reporting_reset() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.vault_file().execute_batch("UPDATE meta SET v = x'00' WHERE k = 'vault_id'; CREATE TABLE extra (x);").unwrap();
        page.click("Unlock");
        page.visible(&VaultError::Damaged.to_string());
        page.click("Reset Vault…");
        page.visible("Delete 1 saved password from this vault file and start a new, empty vault with a new key? No existing key is deleted, since a copy of this vault, such as a backup, may still need it. This can't be undone.");
        page.click("Delete All and Reset");
        assert!(!page.appears("The vault was reset. Unlock to start a new one."), "at {size:?}");
        let (_, notice) = moved_notice(&page);
        page.visible(&notice);
        page.fits_width();
        page.click("Unlock");
        page.visible("No saved passwords yet.");
        page.h.reopen();
        page.click("Unlock");
        page.visible("No saved passwords yet.");
    }
}

/// Q4: a reset in another window found by Reveal, Copy or Edit, which read
/// the entry's row and find it gone, is the same reset, never "no longer
/// exists".
#[test]
fn a_reset_elsewhere_found_by_reveal_copy_or_edit_is_the_same_reset() {
    for via in ["Reveal", "Copy", "Edit"] {
        let mut page = Sheet::new(vec2(640.0, 420.0));
        page.click("Unlock");
        page.add("bank.example", "fixture", "fixture-r5-password");
        page.settle();
        crossbow_core::vault::Vault::open(page.h.dir.path(), Box::new(page.h.keys.clone())).unwrap().reset().unwrap();
        page.click(via);
        assert!(!page.h.passwords.unlocked(), "{via}");
        assert!(page.h.passwords.page.revealed.is_empty() && page.h.passwords.page.editor.is_none(), "{via}");
        assert_ne!(page.h.board.text().as_deref(), Some("fixture-r5-password"), "{via}");
        page.visible(&VaultError::ResetElsewhere.to_string());
        assert!(!page.appears(&VaultError::NotFound.to_string()), "{via}");
    }
}

/// Item 6: egui's TextEdit copies its whole text into Strings it never wipes,
/// every frame it is drawn. A masked password field the user isn't typing in
/// hands egui a stand-in of the same length instead, drawn the same, so the
/// password reaches those copies only while its field has focus or is shown.
/// Clicking it focuses it with the caret where clicked, and typing edits the
/// real password, at both sizes.
#[test]
fn a_masked_password_field_hands_egui_a_stand_in_until_it_has_focus() {
    let source = include_str!("passwords.rs");
    let field = &source[source.find("fn field(").unwrap()..];
    assert!(field.contains("let stand_in = \"*\".repeat(value.chars().count());") && field.contains("TextEdit::singleline(&mut stand_in.as_str())"), "the unfocused masked field edits a stand-in");
    assert!(field.contains("if masked && !focused"), "only while it has no focus");
    let bullets = "•".repeat("fixture-r5-mask".chars().count());
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        let id = page.add("bank.example", "fixture", "fixture-r5-mask");
        page.click("Edit");
        let shown = page.visible(&bullets);
        let idle = shown.find(&bullets, false).unwrap().1;
        let idle_offset = idle.min - shown.find("Generate", false).unwrap().1.min;
        assert_ne!(page.focused(), Some(field_id(crate::passwords::FIELD_IDS[3])), "at {size:?}");
        // Click just right of the last bullet: focus, with the caret at the end.
        page.click_point(pos2(idle.right() + 2.0, idle.center().y));
        assert_eq!(page.focused(), Some(field_id(crate::passwords::FIELD_IDS[3])), "a click focuses it at {size:?}");
        // Measured from the button below the field, since focus may scroll the page.
        let shown = page.settle();
        let typing = shown.find(&bullets, false).unwrap().1;
        let typing_offset = typing.min - shown.find("Generate", false).unwrap().1.min;
        assert!((typing_offset - idle_offset).length() < 0.5 && (typing.size() - idle.size()).length() < 0.5, "drawn in the same place focused {typing:?} and not {idle:?} at {size:?}");
        page.frame(vec![egui::Event::Text("Z".into())]);
        page.settle();
        assert_eq!(page.h.passwords.page.editor.as_ref().unwrap().password.as_str(), "fixture-r5-maskZ", "typing edits the password at {size:?}");
        page.click("Save");
        assert_eq!(page.h.passwords.vault().unwrap().password(&id).unwrap().as_str(), "fixture-r5-maskZ");
    }
}

const RECOVER_PROMPT: &str = "Copy the passwords, addresses, cards and never-save sites that can still be read into a new vault file under the same key? The damaged file is kept beside it in Crossbow's data folder.";

/// The notice after Recover Passwords, and the path of the file it moved aside.
fn recovered_notice(page: &Sheet, recovered: usize, found: usize) -> (std::path::PathBuf, String) {
    let (moved, _) = moved_notice(page);
    let name = moved.file_name().unwrap().to_string_lossy().into_owned();
    let plural = if found == 1 { "" } else { "s" };
    (moved, format!("Recovered {recovered} of {found} password{plural}. The damaged vault file is kept as {name} in Crossbow's data folder, readable only by your user account."))
}

/// A vault with two passwords whose file is damage at open with its
/// entries intact: a renamed column. The ids.
fn damaged_with_two(page: &mut Sheet) -> [String; 2] {
    page.click("Unlock");
    let ids = [page.add("one.example", "fixture", "fixture-password-one"), page.add("two.example", "fixture", "fixture-password-two")];
    page.click("Lock Now");
    page.vault_file().execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
    page.h.reopen();
    ids
}

/// R5-3 on the page: a file that is damage at open while its key is
/// available offers Recover Passwords… between Try Again and Reset Vault…,
/// behind a confirmation that Cancel leaves and Escape doesn't act on. It
/// says how many passwords it got back, and they open after Unlock and
/// after a relaunch. Move Aside and Reset stays the last resort.
#[test]
fn a_damaged_file_whose_key_is_available_offers_recover_passwords() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        let ids = damaged_with_two(&mut page);
        let path = page.h.dir.path().join(crossbow_core::vault::FILE_NAME);
        let image = std::fs::read(&path).unwrap();
        page.visible(&VaultError::Damaged.to_string());
        let shown = page.visible("Recover Passwords…");
        let x = |label: &str| shown.find(label, false).unwrap().1.center().x;
        assert!(x("Try Again") < x("Recover Passwords…") && x("Recover Passwords…") < x("Reset Vault…"), "Try Again first, Reset last, at {size:?}");
        page.fits_width();
        page.tab_to("Recover Passwords…");
        page.click("Recover Passwords…");
        page.visible(RECOVER_PROMPT);
        page.fits_width();
        page.key(egui::Key::Escape, egui::Modifiers::NONE);
        page.visible(RECOVER_PROMPT);
        page.click("Cancel");
        assert!(!page.appears(RECOVER_PROMPT), "Cancel closes it at {size:?}");
        assert_eq!(std::fs::read(&path).unwrap(), image, "Escape and Cancel change nothing");
        page.visible("Reset Vault…");
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        let (moved, notice) = recovered_notice(&page, 2, 2);
        page.visible(&notice);
        page.fits_width();
        assert_eq!(std::fs::read(&moved).unwrap(), image, "the damaged file is kept, moved aside");
        assert!(!page.appears(&VaultError::Damaged.to_string()), "at {size:?}");
        page.click("Unlock");
        page.visible("https://one.example");
        page.visible("https://two.example");
        let vault = page.h.passwords.vault().unwrap();
        assert_eq!((vault.password(&ids[0]).unwrap().as_str(), vault.password(&ids[1]).unwrap().as_str()), ("fixture-password-one", "fixture-password-two"));
        page.click("Lock Now");
        page.h.reopen();
        page.click("Unlock");
        page.visible("https://two.example");
        assert_eq!(page.h.passwords.vault().unwrap().password(&ids[1]).unwrap().as_str(), "fixture-password-two", "after a relaunch");
    }
}

/// Without the vault's key nothing can be recovered, so only Try Again and
/// Reset Vault… are offered.
#[test]
fn recover_passwords_is_offered_only_with_the_vaults_key() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.click("Lock Now");
        page.vault_file().execute_batch("DELETE FROM meta WHERE k = 'format'").unwrap();
        crossbow_core::vault::VaultKeyStore::clear(&page.h.keys, &id).unwrap();
        page.h.reopen();
        page.visible(&VaultError::Damaged.to_string());
        page.visible("Reset Vault…");
        assert!(!page.appears("Recover Passwords…"), "at {size:?}");
    }
}

/// A Recover that can't write its new file says why, leaves the damaged
/// file where it was, and works when tried again.
#[cfg(unix)]
#[test]
fn a_recover_that_cannot_write_says_why_and_can_be_tried_again() {
    use std::os::unix::fs::PermissionsExt;
    for size in sizes() {
        let mut page = Sheet::new(size);
        damaged_with_two(&mut page);
        let dir = page.h.dir.path().to_owned();
        let image = std::fs::read(dir.join(crossbow_core::vault::FILE_NAME)).unwrap();
        page.click("Recover Passwords…");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        page.click("Recover Passwords");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        page.visible(&VaultError::Damaged.to_string());
        page.visible(&VaultError::Storage.to_string());
        page.fits_width();
        assert_eq!(std::fs::read(dir.join(crossbow_core::vault::FILE_NAME)).unwrap(), image, "the damaged file is where it was");
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        page.visible(&recovered_notice(&page, 2, 2).1);
        assert!(!page.appears(&VaultError::Storage.to_string()), "no error is left behind at {size:?}");
    }
}

/// Recover Passwords as rendered, for review: the offer, its confirmation
/// and the result. Run with --ignored.
#[test]
#[ignore = "writes PNGs to target/lane-evidence/screens for manual review"]
fn recover_review_images() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        damaged_with_two(&mut page);
        page.visible("Recover Passwords…");
        page.snapshot("r6-1-recover-offered");
        page.click("Recover Passwords…");
        page.visible(RECOVER_PROMPT);
        page.snapshot("r6-2-recover-prompt");
        page.click("Recover Passwords");
        let notice = recovered_notice(&page, 2, 2).1;
        page.visible(&notice);
        page.snapshot("r6-3-recovered");
        // Round 7: a Recover that couldn't read one password, with a never-save site.
        let mut page = Sheet::new(size);
        let ids = damaged_with_two(&mut page);
        let vault = page.vault_file();
        vault.execute("UPDATE entries SET password = X'00' || substr(password, 2) WHERE id = ?1", [&ids[0]]).unwrap();
        vault.execute("INSERT INTO never_save (profile, origin, created_ms) VALUES ('fixture-profile', 'https://bank.example', 1)", []).unwrap();
        drop(vault);
        page.h.reopen();
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 1 of 2 passwords and 1 of 1 never-save site. 1 couldn't be read; the damaged file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.snapshot("r7-4-recovered-partly");
    }
}

/// The moved-aside file's name, once.
fn moved_name(page: &Sheet) -> String {
    moved_notice(page).0.file_name().unwrap().to_string_lossy().into_owned()
}

/// R6-1 on the page: a Recover that can't read every row it saw says how
/// many, and where the damaged file is kept; the never-save list is counted
/// with the passwords (R6-2).
#[test]
fn a_partial_recover_says_what_it_couldnt_read_and_where_the_file_is() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        let ids = damaged_with_two(&mut page);
        let vault = page.vault_file();
        vault.execute("UPDATE entries SET password = X'00' || substr(password, 2) WHERE id = ?1", [&ids[0]]).unwrap();
        vault.execute("INSERT INTO never_save (profile, origin, created_ms) VALUES ('fixture-profile', 'https://bank.example', 1)", []).unwrap();
        drop(vault);
        page.h.reopen();
        page.click("Recover Passwords…");
        page.visible(RECOVER_PROMPT);
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 1 of 2 passwords and 1 of 1 never-save site. 1 couldn't be read; the damaged file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.fits_width();
        page.click("Unlock");
        page.visible("https://two.example");
        assert!(!page.appears("https://one.example"), "at {size:?}");
        assert_eq!(page.h.passwords.vault().unwrap().never_saved("fixture-profile").unwrap(), ["https://bank.example"]);
    }
}

/// Flip `mask` at byte `at` of b-tree `name`'s root page in the vault file,
/// its WAL folded in first, as the seventh recheck's page probe did.
fn flip_root(page: &Sheet, name: &str, at: usize, mask: u8) {
    let conn = page.vault_file();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
    let root: i64 = conn.query_row("SELECT rootpage FROM sqlite_master WHERE name = ?1", [name], |r| r.get(0)).unwrap();
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    drop(conn);
    let dir = page.h.dir.path();
    for suffix in ["-wal", "-shm"] { let _ = std::fs::remove_file(dir.join(format!("{}{suffix}", crossbow_core::vault::FILE_NAME))); }
    let path = dir.join(crossbow_core::vault::FILE_NAME);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[(root as usize - 1) * size as usize + at] ^= mask;
    std::fs::write(&path, bytes).unwrap();
}

/// R7-1 on the page, ported from the seventh recheck's probe: 40 entries,
/// and the last by id, X, gets one damaged bit in its sealed notes, so its
/// password still reveals. After Lock Now the entries index's cell count
/// loses a bit (0x28 to 0x20), so a read of the whole index ends short of
/// X. Round 7's notice said "Recovered 39 of 39 passwords" and named the
/// kept file as if nothing was left out. Now it says one couldn't be read;
/// X stays in the kept file and isn't in the new vault.
#[test]
fn a_recover_says_it_couldnt_read_an_entry_a_short_index_read_leaves_out() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        let ids: Vec<String> = (0..40).map(|i| page.add(&format!("site{i:03}.example"), "fixture-user", &format!("fixture-pw-{i:03}-K"))).collect();
        let (place, x) = ids.iter().enumerate().max_by_key(|(_, id)| *id).map(|(place, id)| (place, id.clone())).unwrap();
        let vault = page.vault_file();
        let mut notes: Vec<u8> = vault.query_row("SELECT notes FROM entries WHERE id = ?1", [&x], |r| r.get(0)).unwrap();
        let middle = notes.len() / 2;
        notes[middle] ^= 1;
        vault.execute("UPDATE entries SET notes = ?1 WHERE id = ?2", rusqlite::params![notes, x]).unwrap();
        drop(vault);
        assert_eq!(page.h.passwords.vault().unwrap().password(&x).unwrap().as_str(), format!("fixture-pw-{place:03}-K"), "X's password reveals, at {size:?}");
        page.click("Lock Now");
        flip_root(&page, "sqlite_autoindex_entries_1", 4, 0x08);
        page.h.reopen();
        page.visible(&VaultError::Damaged.to_string());
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 39 of 40 passwords. 1 couldn't be read; the damaged file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.fits_width();
        let kept = rusqlite::Connection::open_with_flags(page.h.dir.path().join(&name), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(kept.query_row("SELECT COUNT(*) FROM entries NOT INDEXED WHERE id = ?1", [&x], |r| r.get::<_, i64>(0)).unwrap(), 1, "X stays in the kept file, at {size:?}");
        drop(kept);
        page.click("Unlock");
        let vault = page.h.passwords.vault().unwrap();
        assert_eq!((vault.entries().len(), vault.entry(&x).is_none()), (39, true), "the other 39, at {size:?}");
    }
}

/// R7-1's count on the page: a row that every read returns with every cell
/// NULL, at its own rowid, is still a password the file held. Its record
/// header's size reads as 0, and the index's cell count (2 to 0) lists no
/// row. Only a "row" at rowid 0 or below with no cell is none, and this one
/// is at rowid 2, so the notice says one couldn't be read.
#[test]
fn a_recover_counts_a_row_whose_cells_all_read_empty() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        let ids = [page.add("one.example", "fixture", "fixture-password-one"), page.add("two.example", "fixture", "fixture-password-two")];
        page.click("Lock Now");
        let rowid: i64 = page.vault_file().query_row("SELECT rowid FROM entries WHERE id = ?1", [&ids[1]], |r| r.get(0)).unwrap();
        assert!(rowid > 0, "at {size:?}");
        empty_the_record(&page, rowid);
        flip_root(&page, "sqlite_autoindex_entries_1", 4, 0x02);
        page.h.reopen();
        page.visible(&VaultError::Damaged.to_string());
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 1 of 2 passwords. 1 couldn't be read; the damaged file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.fits_width();
        page.click("Unlock");
        let vault = page.h.passwords.vault().unwrap();
        assert_eq!((vault.entries().len(), vault.password(&ids[0]).unwrap().as_str()), (1, "fixture-password-one"), "at {size:?}");
    }
}

/// Set the size of the record header of the entries row at `rowid` to 0 in
/// the vault file, its WAL folded in first, so SQLite reads every cell of
/// that row as NULL. The table is one leaf.
fn empty_the_record(page: &Sheet, rowid: i64) {
    let conn = page.vault_file();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
    let root: i64 = conn.query_row("SELECT rootpage FROM sqlite_master WHERE name = 'entries'", [], |r| r.get(0)).unwrap();
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    drop(conn);
    let dir = page.h.dir.path();
    for suffix in ["-wal", "-shm"] { let _ = std::fs::remove_file(dir.join(format!("{}{suffix}", crossbow_core::vault::FILE_NAME))); }
    let path = dir.join(crossbow_core::vault::FILE_NAME);
    let mut bytes = std::fs::read(&path).unwrap();
    let start = (root as usize - 1) * size as usize;
    assert_eq!(bytes[start], 0x0d, "one table leaf");
    let varint = |at: usize| -> (i64, usize) {
        let mut value = 0i64;
        for (n, byte) in bytes[at..at + 9].iter().enumerate() {
            value = (value << 7) | i64::from(byte & 0x7f);
            if byte & 0x80 == 0 { return (value, n + 1); }
        }
        unreachable!("a varint of at most 8 bytes here")
    };
    let cells = u16::from_be_bytes([bytes[start + 3], bytes[start + 4]]) as usize;
    // Each cell: its payload size, its rowid, then its record, which starts with its header's size.
    let header = (0..cells).map(|n| start + u16::from_be_bytes([bytes[start + 8 + 2 * n], bytes[start + 9 + 2 * n]]) as usize)
        .find_map(|cell| { let size = varint(cell).1; let (id, len) = varint(cell + size); (id == rowid).then_some(cell + size + len) }).unwrap();
    assert!(bytes[header] > 0 && bytes[header] < 0x80, "a one-byte header size");
    bytes[header] = 0;
    std::fs::write(&path, bytes).unwrap();
}

/// R7-2 on the page: a Recover that stopped after it linked the damaged
/// file under its kept name, as a crash or power loss can leave it. The
/// next launch takes that name back, the page offers Recover again, and its
/// notice names the one file kept, the only moved-aside name in the folder.
#[test]
fn a_recover_that_stopped_part_way_leaves_one_kept_name_after_the_next() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        let ids = damaged_with_two(&mut page);
        let stopped = "vault-damaged-20261029T000000Z.sqlite";
        std::fs::hard_link(page.h.dir.path().join(crossbow_core::vault::FILE_NAME), page.h.dir.path().join(stopped)).unwrap();
        page.h.reopen();
        page.visible(&VaultError::Damaged.to_string());
        assert!(!page.h.dir.path().join(stopped).exists(), "the next launch takes the stopped Recover's name back, at {size:?}");
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        page.visible(&recovered_notice(&page, 2, 2).1);
        page.click("Unlock");
        let vault = page.h.passwords.vault().unwrap();
        assert_eq!((vault.password(&ids[0]).unwrap().as_str(), vault.password(&ids[1]).unwrap().as_str()), ("fixture-password-one", "fixture-password-two"), "at {size:?}");
    }
}

/// R5-5 and Q3 on the page: a format-2 file that lost its format row and
/// its never-save table recovers its passwords and says the list couldn't be
/// read in full, instead of emptying it without a word.
#[test]
fn a_recover_says_when_the_never_save_list_couldnt_be_read() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.vault_file().execute_batch("DELETE FROM meta WHERE k = 'format'; DROP TABLE never_save;").unwrap();
        page.h.reopen();
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 1 of 1 password. The never-save list couldn't be read in full. The damaged vault file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.fits_width();
    }
}

// -- round 9: an undo that can't finish keeps the vault closed (R8-2), and every kept file is named (R7-2) --

/// The name a stopped Recover kept the vault file under in these tests.
const STOPPED: &str = "vault-damaged-20261031T120000Z.sqlite";

/// Every file in the data folder but the folder's own, by name, with its bytes.
fn data_files(page: &Sheet) -> std::collections::BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(page.h.dir.path()).unwrap().map(|file| file.unwrap())
        .filter(|file| file.file_type().unwrap().is_file())
        .map(|file| (file.file_name().to_string_lossy().into_owned(), std::fs::read(file.path()).unwrap())).collect()
}

/// R8-2 on the page, ported from the eighth recheck's stopped-undo probe: a
/// Recover stopped after keeping the vault file under a second name and
/// moving its WAL there, and the WAL can't be put back beside it (here, its
/// record is a stand-in the undo can't read; a stop with no record at all is
/// R9-2's). The page names every kept file, says not to delete them, and
/// offers Try Again and, since round 12, Open Without Them…, which asks
/// first (see a_stop_nothing_can_place_opens_without_its_files_once_confirmed_by_keyboard);
/// nothing else opens, creates or writes vault.sqlite, whatever is clicked
/// or called. Moving the WAL out by hand is no longer shown here: the vault
/// would open without its newest entries (R9-2).
#[cfg(unix)]
#[test]
fn ported_r8_2_an_undo_that_cant_finish_keeps_the_vault_closed_and_names_every_kept_file() {
    let stuck = stuck_line(&format!("{STOPPED}, {STOPPED}-wal"));
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.h.reopen();
        let dir = page.h.dir.path().to_owned();
        std::fs::hard_link(dir.join(crossbow_core::vault::FILE_NAME), dir.join(STOPPED)).unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-wal")), b"fixture-kept-wal").unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-swap")), b"{\"stand-in\":true}").unwrap();
        let before = data_files(&page);
        page.visible(&stuck);
        page.fits_width();
        page.visible("Try Again");
        for absent in ["Unlock", "Reset Vault…", "Recover Passwords…"] {
            assert!(!page.appears(absent), "{absent} at {size:?}");
        }
        page.click("Try Again");
        page.visible(&stuck);
        page.h.passwords.unlock();
        page.h.passwords.reset();
        page.h.passwords.recover();
        page.h.passwords.open_for_pages();
        assert!(page.h.passwords.status().is_err());
        page.visible(&stuck);
        assert_eq!(data_files(&page), before, "nothing was created, written or moved at {size:?}");
    }
}

/// The line that names what a finished Recover that stopped left, for `files`.
fn left_line(files: &str, many: bool) -> String {
    let hold = if many { "They hold" } else { "It holds" };
    format!("A Recover Passwords that stopped before saying where it kept the damaged vault file left {files} in Crossbow's data folder. {hold} that file as it was, with any passwords Recover couldn't read.")
}

/// R7-2 on the page: the files a Recover left when it stopped after its
/// swap, before it could say where it kept them (its record marked
/// finished), are named on the vault page while locked, unlocked and
/// damaged, and by the next Recover's notice beside its own copy, while
/// they exist.
#[test]
fn ported_r7_2_every_file_a_stopped_recover_left_is_named_until_it_is_dealt_with() {
    let line = left_line(&format!("{STOPPED} and {STOPPED}-wal"), true);
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.add("two.example", "fixture", "fixture-password-two");
        page.click("Lock Now");
        let dir = page.h.dir.path().to_owned();
        std::fs::write(dir.join(STOPPED), b"fixture-kept-vault").unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-wal")), b"fixture-kept-wal").unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-swap")), FINISHED_RECORD).unwrap();
        page.h.reopen();
        page.visible("Passwords are locked");
        page.visible(&line);
        page.fits_width();
        page.click("Unlock");
        page.visible("https://one.example");
        page.visible(&line);
        page.click("Lock Now");
        page.vault_file().execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
        page.h.reopen();
        page.visible(&VaultError::Damaged.to_string());
        page.visible(&line);
        page.click("Recover Passwords…");
        page.click("Recover Passwords");
        let own: Vec<String> = data_files(&page).into_keys().filter(|name| name.starts_with("vault-damaged-") && name.ends_with(".sqlite") && name != STOPPED).collect();
        assert_eq!(own.len(), 1, "{own:?}");
        page.visible(&format!("Recovered 2 of 2 passwords. The damaged vault file is kept as {} in Crossbow's data folder, readable only by your user account. A Recover Passwords that stopped earlier also left {STOPPED} and {STOPPED}-wal there.", own[0]));
        page.fits_width();
        page.visible(&line);
        // Dealt with: the line goes the next time the page opens.
        std::fs::remove_file(dir.join(STOPPED)).unwrap();
        std::fs::remove_file(dir.join(format!("{STOPPED}-wal"))).unwrap();
        page.frame_without_page();
        page.settle();
        assert!(!page.appears(&line), "at {size:?}");
    }
}

/// A finished Recover's record, as the open after a stop past its last rename
/// marks it: whatever the vault file is by then, its kept files are named.
const FINISHED_RECORD: &[u8] = br#"{"file":{"dev":0,"ino":0,"len":0,"sha256":""},"wal":null,"journal":null,"recovered":null,"finished":true}"#;

// -- round 10: a stopped Recover is judged by the vault file's identity (R9-1) --

/// A vault of one.example and two.example whose second entry is only in its
/// WAL: the file's and the WAL's bytes, taken while another connection kept
/// the WAL from being folded in. Ported from the ninth recheck's page
/// probes. The page is left closed, and the folder holds no vault file.
fn wal_snapshot(page: &mut Sheet) -> (Vec<u8>, Vec<u8>) {
    let file = crossbow_core::vault::FILE_NAME;
    page.click("Unlock");
    page.add("one.example", "fixture", "fixture-password-one");
    let holder = page.vault_file();
    holder.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
    page.add("two.example", "fixture", "fixture-password-two");
    let dir = page.h.dir.path().to_owned();
    let main = std::fs::read(dir.join(file)).unwrap();
    let wal = std::fs::read(dir.join(format!("{file}-wal"))).unwrap();
    page.click("Lock Now");
    page.h.reopen();
    drop(holder);
    for suffix in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(dir.join(format!("{file}{suffix}"))); }
    (main, wal)
}

/// The closed page's line for an undo that can't finish, naming `files`.
fn stuck_line(files: &str) -> String {
    format!("Recover Passwords stopped before it finished, and the vault file couldn't be put back as it was. The vault stays closed so nothing is written over these files in Crossbow's data folder: {files}. They may hold passwords the vault file doesn't, so don't delete them. Try Again puts them back if it can. If it can't, Open Without Them opens the vault file as it is.")
}

/// R9-1 on the page, ported from the ninth recheck's page probe: a stopped
/// Recover's record (here a stand-in the undo can't read), the vault file's
/// WAL, which holds two.example, moved under the kept name, and the kept
/// name itself removed by hand. Nothing shows the vault file is the one the
/// Recover kept, so the vault stays closed, Unlock isn't offered, and the
/// page names the WAL and says not to delete it; nothing on the page says
/// it may be. Try Again changes nothing.
#[cfg(unix)]
#[test]
fn a_kept_name_removed_by_hand_keeps_the_page_closed_and_nothing_says_its_wal_may_go() {
    let file = crossbow_core::vault::FILE_NAME;
    for size in sizes() {
        let mut page = Sheet::new(size);
        let (main, wal) = wal_snapshot(&mut page);
        let dir = page.h.dir.path().to_owned();
        std::fs::write(dir.join(file), &main).unwrap();
        std::fs::hard_link(dir.join(file), dir.join(STOPPED)).unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-wal")), &wal).unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-swap")), b"{\"stand-in\":true}").unwrap();
        std::fs::remove_file(dir.join(STOPPED)).unwrap();
        let before = data_files(&page);
        let line = stuck_line(&format!("{STOPPED}-wal"));
        page.visible(&line);
        page.fits_width();
        for _ in 0..2 {
            assert!(!page.appears("Unlock"), "at {size:?}");
            assert!(!page.settle().mentions("until you move or delete"), "at {size:?}");
            page.click("Try Again");
            page.visible(&line);
        }
        assert_eq!(data_files(&page), before, "nothing was created, written or moved at {size:?}");
    }
}

/// A finished Recover's record, as the open after a stop past its last
/// rename marks it: its kept files are named, as holding the damaged file
/// with anything Recover couldn't read, and never as ones to move or delete.
#[test]
fn a_finished_recovers_kept_files_are_named_without_saying_they_may_go() {
    let line = format!("A Recover Passwords that stopped before saying where it kept the damaged vault file left {STOPPED} and {STOPPED}-wal in Crossbow's data folder. They hold that file as it was, with any passwords Recover couldn't read.");
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        let dir = page.h.dir.path().to_owned();
        std::fs::write(dir.join(STOPPED), b"fixture-kept-vault").unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-wal")), b"fixture-kept-wal").unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-swap")), FINISHED_RECORD).unwrap();
        page.h.reopen();
        page.visible("Passwords are locked");
        page.visible(&line);
        page.fits_width();
        assert!(!page.settle().mentions("move or delete"), "at {size:?}");
    }
}

#[path = "address_settings_tests.rs"]
mod address_settings_tests;

#[path = "card_settings_tests.rs"]
mod card_settings_tests;

/// The line that says what a stop with no record left, for `files`.
fn unrecorded_line(files: &str) -> String {
    format!("A Recover Passwords from an older version of Crossbow stopped before it finished and left no record of the vault file as it was. Crossbow can't tell whether these files in its data folder still belong with the vault file, so the vault stays closed and nothing is written over them: {files}. They hold passwords the vault file may not have. Don't move or delete them, and copy the whole data folder somewhere safe before changing anything in it. Open Without Them opens the vault file as it is.")
}

/// R9-2 on the page, ported from the ninth recheck's README-step probe: a
/// Recover stopped by a build before round 9 left the vault file under a
/// kept name and its WAL there, with no record. The page says what happened
/// and that the named files hold passwords, fits its width, and offers no
/// Try Again, which can't change this, nor Unlock, Reset or Recover; Open
/// Without Them… is a_stop_with_no_record_opens_without_its_files_once_confirmed's.
/// Nothing else creates, writes or moves a file whatever is called, and a
/// restart finds the same.
#[cfg(unix)]
#[test]
fn ported_r9_2_a_stop_with_no_record_says_what_happened_and_offers_nothing_that_cant_work() {
    let line = unrecorded_line(&format!("{STOPPED}, {STOPPED}-wal"));
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.h.reopen();
        let dir = page.h.dir.path().to_owned();
        std::fs::hard_link(dir.join(crossbow_core::vault::FILE_NAME), dir.join(STOPPED)).unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-wal")), b"fixture-kept-wal").unwrap();
        let before = data_files(&page);
        page.visible(&line);
        page.fits_width();
        for absent in ["Try Again", "Unlock", "Reset Vault…", "Recover Passwords…"] {
            assert!(!page.appears(absent), "{absent} at {size:?}");
        }
        page.h.passwords.retry_open();
        page.h.passwords.unlock();
        page.h.passwords.reset();
        page.h.passwords.recover();
        page.h.passwords.open_for_pages();
        assert!(page.h.passwords.status().is_err());
        page.visible(&line);
        assert_eq!(data_files(&page), before, "nothing was created, written or moved at {size:?}");
        page.h.reopen();
        page.visible(&line);
        assert!(!page.appears("Try Again"), "after a restart at {size:?}");
        assert_eq!(data_files(&page), before, "after a restart at {size:?}");
    }
}

// -- round 12: a way out of a stopped Recover, and kept files gone (R10-1, R10-2) --

const OPEN_WITHOUT_ASK: &str = "Open Without Them…";
const OPEN_WITHOUT: &str = "Open Without Them";

/// A file's identity as a stopped Recover's record holds it: device, inode,
/// length and SHA-256.
#[cfg(unix)]
fn identity(path: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).unwrap();
    let sha256 = crossbow_core::content_blocking::matcher::sha256_hex(&std::fs::read(path).unwrap());
    format!(r#"{{"dev":{},"ino":{},"len":{},"sha256":"{sha256}"}}"#, meta.dev(), meta.ino(), meta.len())
}

/// The vault file `main` with `wal` folded in, as a new file at `to`: what
/// Recover's working file holds for this vault. Ported from the tenth
/// recheck's page probes.
fn folded(main: &[u8], wal: &[u8], to: &std::path::Path) {
    let probe = tempfile::tempdir().unwrap();
    let file = crossbow_core::vault::FILE_NAME;
    std::fs::write(probe.path().join(file), main).unwrap();
    std::fs::write(probe.path().join(format!("{file}-wal")), wal).unwrap();
    rusqlite::Connection::open(probe.path().join(file)).unwrap().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    std::fs::copy(probe.path().join(file), to).unwrap();
}

/// A Recover stopped after moving the vault file's WAL under its kept name
/// (step 3), with its record, as the tenth recheck's page probe made it,
/// and Recover's working file if `working`. The WAL holds two.example.
#[cfg(unix)]
fn round_10_stop_at_step_3(page: &mut Sheet, working: bool) -> Vec<u8> {
    let file = crossbow_core::vault::FILE_NAME;
    let (main, wal) = wal_snapshot(page);
    let dir = page.h.dir.path().to_owned();
    std::fs::write(dir.join(file), &main).unwrap();
    std::fs::write(dir.join(format!("{file}-wal")), &wal).unwrap();
    folded(&main, &wal, &dir.join("vault-recovery.sqlite"));
    let record = format!(r#"{{"file":{},"wal":{},"journal":null,"recovered":{},"finished":false}}"#,
        identity(&dir.join(file)), identity(&dir.join(format!("{file}-wal"))), identity(&dir.join("vault-recovery.sqlite")));
    std::fs::write(dir.join(format!("{STOPPED}-swap")), record).unwrap();
    std::fs::hard_link(dir.join(file), dir.join(STOPPED)).unwrap();
    std::fs::rename(dir.join(format!("{file}-wal")), dir.join(format!("{STOPPED}-wal"))).unwrap();
    if !working { std::fs::remove_file(dir.join("vault-recovery.sqlite")).unwrap(); }
    wal
}

/// The text of the control that has keyboard focus, if any.
fn focused_label(page: &mut Sheet) -> Option<String> {
    let shown = page.settle();
    let response = page.focused().and_then(|id| page.ctx.read_response(id))?;
    shown.text.iter().find(|(_, bounds, _)| response.rect.contains(bounds.center())).map(|(text, ..)| text.clone())
}

/// The line that names what Open Without Them set aside, for `files`.
fn set_aside_line(files: &str, many: bool) -> String {
    let hold = if many { "They may hold" } else { "It may hold" };
    format!("A Recover Passwords that stopped before it finished left {files} in Crossbow's data folder. {hold} passwords the vault doesn't.")
}

/// What Open Without Them asks for files that are still there, `files`.
fn open_without_question(files: &str) -> String {
    format!("Open the vault file as it is now? Passwords, addresses and cards that are only in {files} won't be in the vault. The files stay in Crossbow's data folder, and Crossbow won't put them back or write over them.")
}

/// Open Without Them…'s question, by keyboard: Tab to it and press Return.
fn ask_by_keyboard(page: &mut Sheet, question: &str) {
    page.tab_to(OPEN_WITHOUT_ASK);
    page.key(egui::Key::Enter, egui::Modifiers::NONE);
    page.visible(question);
}

/// R10-1 on the page, from the tenth recheck's round-9-record probe: a
/// Recover stopped after its final rename under a round 9 build, whose
/// record names no new file, so nothing can place it. The page stays
/// closed, names the kept files, and offers Try Again, then Open Without
/// Them…, which asks first. Cancel is the default: it has focus when the
/// question appears, so Return cancels, as do Escape and leaving the page,
/// Return with nothing focused does nothing, and nothing changes. Tab to
/// Open Without Them and Return opens the vault
/// as its file is, here the recovered one with both entries, and names the
/// kept files as set aside, before and after a relaunch, unchanged. At both
/// sizes, by keyboard only.
#[cfg(unix)]
#[test]
fn a_stop_nothing_can_place_opens_without_its_files_once_confirmed_by_keyboard() {
    let file = crossbow_core::vault::FILE_NAME;
    let stuck = stuck_line(&format!("{STOPPED}, {STOPPED}-wal"));
    let question = open_without_question(&format!("{STOPPED} and {STOPPED}-wal"));
    let line = set_aside_line(&format!("{STOPPED} and {STOPPED}-wal"), true);
    for size in sizes() {
        let mut page = Sheet::new(size);
        let (main, wal) = wal_snapshot(&mut page);
        let dir = page.h.dir.path().to_owned();
        std::fs::write(dir.join(file), &main).unwrap();
        std::fs::write(dir.join(format!("{file}-wal")), &wal).unwrap();
        let record = format!(r#"{{"file":{},"wal":{},"journal":null}}"#, identity(&dir.join(file)), identity(&dir.join(format!("{file}-wal"))));
        std::fs::write(dir.join(format!("{STOPPED}-swap")), record).unwrap();
        std::fs::hard_link(dir.join(file), dir.join(STOPPED)).unwrap();
        std::fs::rename(dir.join(format!("{file}-wal")), dir.join(format!("{STOPPED}-wal"))).unwrap();
        folded(&main, &wal, &dir.join("vault-recovery.sqlite"));
        std::fs::rename(dir.join("vault-recovery.sqlite"), dir.join(file)).unwrap();
        let before = data_files(&page);
        page.visible(&stuck);
        page.fits_width();
        let shown = page.visible(OPEN_WITHOUT_ASK);
        let at = |label: &str| shown.find(label, false).unwrap().1;
        assert!(at("Try Again").right() < at(OPEN_WITHOUT_ASK).left() && (at("Try Again").center().y - at(OPEN_WITHOUT_ASK).center().y).abs() < 1.0, "Try Again first, on one row, at {size:?}");
        ask_by_keyboard(&mut page, &question);
        page.fits_width();
        assert_eq!(focused_label(&mut page).as_deref(), Some("Cancel"), "Cancel is the default at {size:?}");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        assert!(!page.appears(&question), "Return on Cancel cancels at {size:?}");
        ask_by_keyboard(&mut page, &question);
        page.ctx.memory_mut(|memory| if let Some(id) = memory.focused() { memory.surrender_focus(id) });
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(&question);
        assert_eq!(data_files(&page), before, "Return with nothing focused does nothing at {size:?}");
        page.key(egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!page.appears(&question), "Escape cancels at {size:?}");
        ask_by_keyboard(&mut page, &question);
        page.frame_without_page();
        assert!(!page.appears(&question), "leaving the page cancels at {size:?}");
        page.visible(&stuck);
        assert_eq!(data_files(&page), before, "nothing changed at {size:?}");
        ask_by_keyboard(&mut page, &question);
        page.tab_to(OPEN_WITHOUT);
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible("Passwords are locked");
        page.visible(&line);
        page.fits_width();
        assert!(!page.appears(&stuck), "at {size:?}");
        page.tab_to("Unlock");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible("https://one.example");
        page.visible("https://two.example");
        page.h.reopen();
        page.visible("Passwords are locked");
        page.visible(&line);
        for kept in [STOPPED.to_owned(), format!("{STOPPED}-wal")] {
            assert_eq!(std::fs::read(dir.join(&kept)).unwrap(), before[&kept], "{kept} unchanged at {size:?}");
        }
    }
}

/// R10-2 on the page, from the tenth recheck's deleted-files probe: a
/// stopped Recover's kept WAL, which holds two.example, deleted in Finder,
/// or the WAL and the kept name. With Recover's working file still there
/// as recorded, the next open finishes that Recover: the page opens locked,
/// names the kept vault file as any finished stop's, and Unlock lists both
/// entries. Without the working file, the page stays closed and says the
/// WAL is gone, naming it, with Try Again and Open Without Them…; put back,
/// Try Again opens both entries, and Open Without Them, confirmed, opens the
/// vault file as it is, without them, and names nothing.
#[cfg(unix)]
#[test]
fn a_stopped_recovers_kept_wal_gone_finishes_or_says_so_and_waits() {
    let gone = VaultError::KeptFilesGone(vec![format!("{STOPPED}-wal")]).to_string();
    for size in sizes() {
        for deleted in ["the WAL", "both"] {
            let mut page = Sheet::new(size);
            round_10_stop_at_step_3(&mut page, true);
            let dir = page.h.dir.path().to_owned();
            std::fs::remove_file(dir.join(format!("{STOPPED}-wal"))).unwrap();
            if deleted == "both" { std::fs::remove_file(dir.join(STOPPED)).unwrap(); }
            page.visible("Passwords are locked");
            page.visible(&left_line(STOPPED, false));
            page.click("Unlock");
            page.visible("https://one.example");
            page.visible("https://two.example");
        }
        for then in ["put back", "open without"] {
            let mut page = Sheet::new(size);
            let wal = round_10_stop_at_step_3(&mut page, false);
            let dir = page.h.dir.path().to_owned();
            std::fs::remove_file(dir.join(format!("{STOPPED}-wal"))).unwrap();
            let before = data_files(&page);
            page.visible(&gone);
            page.fits_width();
            page.visible("Try Again");
            page.click("Try Again");
            page.visible(&gone);
            assert_eq!(data_files(&page), before, "{then}: nothing changed at {size:?}");
            if then == "put back" {
                std::fs::write(dir.join(format!("{STOPPED}-wal")), &wal).unwrap();
                page.click("Try Again");
            } else {
                page.click(OPEN_WITHOUT_ASK);
                page.visible(&format!("Open the vault file without {STOPPED}-wal? Anything saved only in them won't be in the vault."));
                page.fits_width();
                page.click(OPEN_WITHOUT);
            }
            page.visible("Passwords are locked");
            page.click("Unlock");
            assert_eq!(page.appears("https://two.example"), then == "put back", "{then} at {size:?}");
            assert!(!page.settle().mentions("vault-damaged-"), "{then}: nothing named at {size:?}");
        }
    }
}

/// R9-2's stop with no record, from a build before round 9: no Try Again,
/// which can't change it, but Open Without Them…, which opens the vault
/// file as it is once confirmed and names the kept files as set aside.
#[cfg(unix)]
#[test]
fn a_stop_with_no_record_opens_without_its_files_once_confirmed() {
    let line = unrecorded_line(&format!("{STOPPED}, {STOPPED}-wal"));
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.click("Lock Now");
        page.h.reopen();
        let dir = page.h.dir.path().to_owned();
        std::fs::hard_link(dir.join(crossbow_core::vault::FILE_NAME), dir.join(STOPPED)).unwrap();
        std::fs::write(dir.join(format!("{STOPPED}-wal")), b"fixture-kept-wal").unwrap();
        page.visible(&line);
        assert!(!page.appears("Try Again"), "at {size:?}");
        page.click(OPEN_WITHOUT_ASK);
        page.visible(&open_without_question(&format!("{STOPPED} and {STOPPED}-wal")));
        page.fits_width();
        page.click(OPEN_WITHOUT);
        page.visible("Passwords are locked");
        page.visible(&set_aside_line(&format!("{STOPPED} and {STOPPED}-wal"), true));
        page.click("Unlock");
        page.visible("https://one.example");
        assert_eq!(std::fs::read(dir.join(format!("{STOPPED}-wal"))).unwrap(), b"fixture-kept-wal", "at {size:?}");
    }
}
