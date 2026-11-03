//! Simulated frames of unread badges on pinned rows, favorite tiles and
//! collapsed folders, at a compact and a normal window and the sidebar's
//! narrowest, default and widest widths (rows of 180, 232 and 400 points).

use super::*;
use crate::unread::Watched;
use crossbow_core::model::Theme;
use crossbow_core::store::{Store, TabRow};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const WIDTHS: [f32; 3] = [200.0, 252.0, 420.0];
const ON: Marks = Marks { unread: true, previews: true };
const OFF: Marks = Marks { unread: false, previews: true };

struct Painted {
    texts: Vec<(String, Rect, Rect)>,
    fills: Vec<(Rect, Rect)>,
    dots: Vec<(Pos2, Rect)>,
}

impl Painted {
    fn text(&self, label: &str) -> (Rect, Rect) {
        self.texts.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)).unwrap_or_else(|| panic!("{label:?} not drawn"))
    }
}

fn frame(ctx: &egui::Context, size: Vec2, content: impl FnMut(&mut Ui)) -> Painted {
    let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), ..Default::default() }, content);
    output.textures_delta.clear();
    let accent = Ink::new(pal().panel, pal().accent).accent;
    let mut painted = Painted { texts: Vec::new(), fills: Vec::new(), dots: Vec::new() };
    for shape in &output.shapes {
        match &shape.shape {
            egui::Shape::Text(text) => painted.texts.push((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            egui::Shape::Rect(rect) if rect.fill == accent => painted.fills.push((rect.rect, shape.clip_rect)),
            egui::Shape::Circle(circle) if circle.fill == accent => painted.dots.push((circle.center, shape.clip_rect)),
            _ => {}
        }
    }
    painted
}

fn pal() -> Palette {
    theme::palette(&Theme::preset(0), false)
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    window::install_fonts(&ctx);
    ctx
}

const LONG: &str = "A pinned tab with a title far too long for any sidebar width";

#[test]
fn row_badges_sit_before_the_controls_and_the_title_stops_short_of_them() {
    let ctx = context();
    for size in SIZES {
        for width in WIDTHS {
            for (badge, text) in [(Badge::Count(3), Some("3")), (Badge::Count(1_234), Some("99+")), (Badge::Dot, None)] {
                for (hovered, muted) in [(false, false), (true, false), (true, true)] {
                    let row = Rect::from_min_size(pos2(PAD, 200.0), vec2(width - PAD * 2.0, ROW_H));
                    let slots = TabTrailingSlots::new(row, hovered, muted, false);
                    let (mut placed, mut title_right) = (Rect::NOTHING, 0.0);
                    // As tab_row lays out a pinned tab.
                    let painted = frame(&ctx, size, |ui| {
                        placed = row_badge(ui.painter(), slots.title_right - 4.0, row.center().y, badge, &pal());
                        title_right = placed.left() - BADGE_GAP;
                        label(ui, row.left() + ITEM_TEXT_X, title_right, row.center().y, LONG, pal().text);
                    });
                    let context = format!("{badge:?} at {size:?}, width {width}, hovered {hovered}, muted {muted}");
                    assert!(row.contains_rect(placed), "the badge stays in its row: {context}");
                    for control in [slots.close, slots.mute].into_iter().flatten() {
                        assert!(placed.right() <= control.left() - 2.0, "the badge clears the {control:?} control: {context}");
                    }
                    let (_, title_clip) = painted.text(LONG);
                    assert!(title_clip.right() <= placed.left() - BADGE_GAP + 0.5, "the title stops before the badge: {context}");
                    assert!(title_right - (row.left() + ITEM_TEXT_X) >= 50.0, "the title keeps room to read: {context}");
                    match text {
                        Some(text) => {
                            let (bounds, clip) = painted.text(text);
                            assert!(placed.contains_rect(bounds) && clip.contains_rect(bounds), "{text:?} fits its pill unclipped: {context}");
                            assert_eq!(painted.fills.len(), 1);
                        }
                        None => assert_eq!(painted.dots.len(), 1, "a dot: {context}"),
                    }
                }
            }
        }
    }
}

#[test]
fn favorite_badges_sit_on_the_icon_corner_inside_their_tile() {
    let ctx = context();
    let badges = [Badge::Count(3), Badge::Count(1_234), Badge::Dot, Badge::Count(42)];
    for size in SIZES {
        for width in WIDTHS {
            let grid = SidebarRegions::new(Rect::from_min_max(pos2(0.0, 96.0), pos2(width, size.y - 46.0)), badges.len()).favorites.unwrap();
            let mut placed = Vec::new();
            let painted = frame(&ctx, size, |ui| {
                favorite_grid(ui, grid, Id::unique("badge-probe"), badges.len(), |ui, index, tile| {
                    // As the favorites grid draws a tile.
                    let icon = Rect::from_center_size(tile.center(), vec2(18.0, 18.0));
                    placed.push((tile, icon, tile_badge(ui.painter(), tile, icon, badges[index], &pal())));
                });
            });
            let context = format!("at {size:?}, width {width}");
            for (index, (tile, icon, badge)) in placed.iter().enumerate() {
                assert!(tile.contains_rect(*badge), "badge {index} stays in its tile {context}");
                assert!(grid.contains_rect(*badge), "badge {index} is not clipped by the grid {context}");
                assert!(badge.bottom() < icon.center().y, "badge {index} leaves the icon's middle {context}");
                assert!(badge.intersects(*icon), "badge {index} marks the icon's corner {context}");
            }
            for text in ["3", "99+", "42"] {
                let (bounds, clip) = painted.text(text);
                assert!(placed.iter().any(|(_, _, badge)| badge.contains_rect(bounds)) && clip.contains_rect(bounds), "{text:?} fits its pill {context}");
            }
            assert_eq!((painted.fills.len(), painted.dots.len()), (3, 1));
        }
    }
}

struct World {
    lib: Library,
    space: String,
    tabs: Vec<Tab>,
    unread: Unread,
}

impl World {
    /// Personal has a Space with folder Work › Mail › the Inbox pin, and
    /// favorites in Personal and Client.
    fn new() -> (Self, [String; 5]) {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        let personal = lib.spaces[&space].profile_id.clone();
        let client = lib.create_profile("Client").unwrap();
        let work = lib.create_folder(&space, None, "Work").unwrap();
        let mail = lib.create_folder(&space, Some(&work), "Mail").unwrap();
        let inbox = lib.add_pinned(&space, Some(&mail), "https://mail.example/", "Inbox").unwrap();
        let mine = lib.add_favorite(&personal, "https://chat.example/", "Chat").unwrap();
        let theirs = lib.add_favorite(&client, "https://chat.example/", "Chat").unwrap();
        let tab = |id: &str, space: &str, item: &str| Tab::from_row(TabRow {
            id: id.into(), space_id: space.into(), item_id: Some(item.into()), url: "https://mail.example/".into(), title: "Inbox".into(),
            order: "a0".into(), created_ms: 1, last_active_ms: 1,
        });
        let tabs = vec![tab("inbox-tab", &space, &inbox), tab("mine-tab", crate::tabs::FAVORITES_SPACE, &mine), tab("theirs-tab", crate::tabs::FAVORITES_SPACE, &theirs)];
        (World { lib, space, tabs, unread: Unread::default() }, [work, mail, inbox, mine, theirs])
    }

    /// One badge pass where `changed` tabs' titles changed in the background.
    fn change(&mut self, changed: &[&str]) {
        let watched = |title| self.tabs.iter().map(move |tab| Watched { tab: &tab.id, title, loaded: true, loading: false, visible: false });
        self.unread.update(watched("Inbox"));
        let titles: Vec<_> = self.tabs.iter().map(|tab| if changed.contains(&tab.id.as_str()) { "(2) Inbox" } else { "Inbox" }).collect();
        self.unread.update(self.tabs.iter().zip(&titles).map(|(tab, title)| Watched { tab: &tab.id, title, loaded: true, loading: false, visible: false }));
    }
}

#[test]
fn collapsed_folders_show_a_dot_for_a_badge_at_any_depth() {
    let (mut world, [work, mail, inbox, ..]) = World::new();
    assert!(!folder_unread(&world.lib, &world.tabs, &world.unread, &world.space, &work, ON));
    world.change(&["inbox-tab"]);
    assert_eq!(item_badge(&world.tabs, &world.unread, &inbox, ON), Some(Badge::Count(2)));
    assert!(folder_unread(&world.lib, &world.tabs, &world.unread, &world.space, &work, ON), "the outer folder shows the inner pin's badge");
    assert!(folder_unread(&world.lib, &world.tabs, &world.unread, &world.space, &mail, ON));
    assert!(!folder_unread(&world.lib, &world.tabs, &world.unread, &world.space, &work, OFF), "nothing with the switch off");
    world.change(&[]);
    assert!(!folder_unread(&world.lib, &world.tabs, &world.unread, &world.space, &work, ON), "viewing or reading clears the folder too");

    // The folder row's dot and its name, as pinned_tree draws them.
    let ctx = context();
    for size in SIZES {
        for width in WIDTHS {
            let row = Rect::from_min_size(pos2(PAD, 200.0), vec2(width - PAD * 2.0, ROW_H));
            let name = "A folder whose name is longer than the row";
            let mut dot = Rect::NOTHING;
            let painted = frame(&ctx, size, |ui| {
                dot = row_badge(ui.painter(), row.right() - 8.0, row.center().y, Badge::Dot, &pal());
                label(ui, row.left() + 46.0, dot.left() - BADGE_GAP, row.center().y, name, pal().text);
            });
            assert!(row.contains_rect(dot) && painted.dots.len() == 1, "at {size:?}, width {width}");
            assert!(painted.text(name).1.right() <= dot.left() - BADGE_GAP + 0.5, "the name stops before the dot at {size:?}, width {width}");
        }
    }
}

#[test]
fn badges_follow_their_own_profile_and_the_switch() {
    let (mut world, [.., mine, theirs]) = World::new();
    world.change(&["mine-tab", "theirs-tab"]);
    let personal = world.lib.spaces[&world.space].profile_id.clone();
    // The sidebar lists the Space's own profile's favorites, so only its badge can show.
    let shown: Vec<_> = world.lib.favorites(&personal).iter().filter_map(|(id, _)| item_badge(&world.tabs, &world.unread, id, ON).map(|badge| (id.clone(), badge))).collect();
    assert_eq!(shown, [(mine.clone(), Badge::Count(2))]);
    assert!(!world.lib.favorites(&personal).iter().any(|(id, _)| *id == theirs), "Client's favorite is never in Personal's sidebar");
    assert_eq!(item_badge(&world.tabs, &world.unread, &mine, OFF), None, "the switch off draws none");

    // Nothing is drawn without a badge.
    let ctx = context();
    let tile = Rect::from_min_size(pos2(10.0, 100.0), vec2(40.0, FAVORITE_HEIGHT));
    let painted = frame(&ctx, SIZES[0], |ui| {
        if let Some(badge) = item_badge(&world.tabs, &world.unread, &mine, OFF) { tile_badge(ui.painter(), tile, tile.shrink(11.0), badge, &pal()); }
    });
    assert!(painted.fills.is_empty() && painted.dots.is_empty());
}
