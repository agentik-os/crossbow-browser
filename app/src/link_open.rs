//! Opening links from other apps: each one becomes a new Today tab in the
//! Space Air Traffic Control picks, which also picks its profile. A site set
//! to open in a profile goes to that profile's Space instead, unless an Air
//! Traffic Control rule matched it. The window comes forward and the last
//! link's tab is focused. With Settings → Links → Open links from other apps
//! in a Little Window, each opens in a Little Window of its own instead, in
//! the Space and profile the rules pick.

use super::*;
use super::site_open::{SiteRoute, Undo, notice_for, route_link};
use crossbow_core::link_routing::{Link, Routing, redact};
use crossbow_core::site_profiles::SiteProfiles;

/// A link's new tab and the Space it was routed to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub tab: String,
    pub space: String,
    pub rule: Option<usize>,
    pub fallback: bool,
    /// Whether its site's rule moved it, or could not.
    pub site: SiteRoute,
    /// The Space Air Traffic Control alone picked.
    pub from: String,
}

/// Links to open now. While first-run setup is open they stay queued.
pub(crate) fn ready_links(inbox: &crate::links::Inbox, setup_open: bool) -> Vec<Link> {
    if setup_open { Vec::new() } else { inbox.take() }
}

/// Add `url` as the first Today tab of the Space `routing` and `sites` pick.
/// None, and no tab, when no Space with a profile can take it.
pub(crate) fn place_link(lib: &Library, routing: &Routing, sites: &SiteProfiles, tabs: &mut Vec<Tab>, most_recent: &str, url: &str) -> Option<Placed> {
    let route = route_link(lib, routing, sites, tabs, most_recent, url)?;
    let tab = add_first_today_tab(tabs, &route.space, url, "");
    Some(Placed { tab, space: route.space, rule: route.rule, fallback: route.fallback, site: route.site, from: route.from })
}

/// Like a new tab: first in Today, saved with the session, its view made
/// when shown. Returns its id.
pub(crate) fn add_first_today_tab(tabs: &mut Vec<Tab>, space: &str, url: &str, title: &str) -> String {
    let mut today: Vec<&Tab> = tabs.iter().filter(|tab| tab.space_id == space && tab.item_id.is_none()).collect();
    today.sort_by(|a, b| a.order.cmp(&b.order).then(a.created_ms.cmp(&b.created_ms)));
    let order = key_between(None, today.first().map(|tab| tab.order.as_str()));
    let now = now_ms();
    let id = new_id();
    tabs.push(Tab {
        nav: Default::default(),
        id: id.clone(),
        space_id: space.into(),
        item_id: None,
        url: url.into(),
        title: title.into(),
        order,
        created_ms: now,
        last_active_ms: now,
        view: None,
        muted: false,
        dirty: true,
    });
    id
}

impl CrossbowApp {
    /// Open links from other apps. Runs from `logic`, which eframe calls even
    /// while the window is hidden or minimized.
    pub(super) fn receive_links(&mut self, ctx: &egui::Context) {
        let links = ready_links(crate::links::inbox(), self.ui.onboarding.is_some());
        if links.is_empty() { return; }
        let in_window = self.link_window.enabled();
        let mut opened = None;
        for link in links {
            let Link::Page(url) = link else { continue };
            match little::deliver_link(&self.lib, self.link_routing.saved(), &self.site_profiles, in_window, &mut self.tabs, &mut self.littles, &self.active_space, &url) {
                Some(little::Delivered::Tab(placed)) => {
                    log::info!("Opened a link from another app ({}){}", redact(&url), if placed.fallback { " in a fallback Space" } else { "" });
                    let undo = Undo { tab: placed.tab.clone(), url: url.clone(), space: placed.from.clone() };
                    if let Some(notice) = notice_for(&self.lib, &placed.site, &url, Some(undo)) { self.site_notice = Some(notice); }
                    opened = Some(placed.tab);
                }
                Some(little::Delivered::Window { .. }) => log::info!("Opened a link from another app ({}) in a Little Window", redact(&url)),
                None => log::warn!("Could not open a link from another app ({}): no Space with a profile can take it", redact(&url)),
            }
        }
        if in_window {
            // Each new window comes forward on its own; the main window stays where it is.
            crate::links::unhide_app();
            self.show_new_windows(ctx);
            return;
        }
        if let Some(tab) = opened {
            // The link replaces the startup command bar, menus and Peek. An
            // open Settings sheet keeps its drafts and stays over the tab.
            cancel_command(ctx, &mut self.ui.command, &mut self.ui.focus, self.ui.find.as_mut());
            self.actions.retain(|action| !matches!(action, Action::OpenCommandBar { .. }));
            self.ui.menu = None;
            let now = ctx.input(|i| i.time);
            if let Some(peek) = &mut self.ui.peek { peek.closing_at.get_or_insert(now); }
            self.activate(&tab);
        }
        crate::links::unhide_app();
        if ctx.input(|i| i.viewport().minimized == Some(true)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::{Inbox, Source};
    use crossbow_core::link_routing::{Destination, Match, Rule};
    use crossbow_core::store::TabRow;

    struct Fixture {
        lib: Library,
        tabs: Vec<Tab>,
        personal: String,
        work: String,
        profile_a: String,
        profile_b: String,
    }

    /// Personal uses profile A; Work uses profile B and has one Today tab.
    fn fixture() -> Fixture {
        let mut lib = Library::open(Store::open(std::path::Path::new(":memory:")).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let personal = lib.spaces_ordered()[0].0.clone();
        let profile_a = lib.spaces[&personal].profile_id.clone();
        let profile_b = lib.create_profile("B").unwrap();
        let work = lib.create_space("Work", "W", Theme::preset(1), &profile_b).unwrap();
        let tabs = vec![Tab::from_row(TabRow {
            id: "work-tab".into(), space_id: work.clone(), item_id: None, url: "https://work.example/".into(),
            title: "Work".into(), order: "V".into(), created_ms: 1, last_active_ms: 2,
        })];
        Fixture { lib, tabs, personal, work, profile_a, profile_b }
    }

    fn rule(kind: Match, value: &str, space: &str) -> Rule {
        Rule::new(crossbow_core::new_id(), kind, value, space.into()).unwrap()
    }

    fn opened<'a>(f: &'a Fixture, placed: &Placed) -> &'a Tab {
        f.tabs.iter().find(|tab| tab.id == placed.tab).expect("the link has a tab")
    }

    #[test]
    fn a_routed_link_opens_as_the_first_today_tab_of_its_space_and_profile() {
        let mut f = fixture();
        let routing = Routing { rules: vec![rule(Match::HostIs, "github.com", &f.work)], default: Destination::MostRecent };
        let placed = place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.personal, "https://github.com/rust-lang/rust?tab=readme").unwrap();
        assert_eq!((placed.space.as_str(), placed.rule, placed.fallback), (f.work.as_str(), Some(0), false));
        let tab = opened(&f, &placed);
        assert_eq!((tab.space_id.as_str(), tab.item_id.as_deref(), tab.url.as_str()), (f.work.as_str(), None, "https://github.com/rust-lang/rust?tab=readme"));
        assert!(tab.dirty && tab.view.is_none(), "saved with the session; its view opens when shown");
        assert_eq!(tab_profile(&f.lib, tab), Some(f.profile_b.as_str()), "the web view is created in the Space's profile");
        assert!(tab.order.as_str() < "V", "new tabs open at the top of Today");
        assert_eq!(f.tabs.iter().find(|tab| tab.id == "work-tab").unwrap().url, "https://work.example/", "other tabs are untouched");

        // Unmatched links go to the most recent Space, in its profile.
        let placed = place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.personal, "https://example.com/").unwrap();
        assert_eq!((placed.space.as_str(), placed.rule), (f.personal.as_str(), None));
        assert_eq!(tab_profile(&f.lib, opened(&f, &placed)), Some(f.profile_a.as_str()));
        let placed = place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.work, "https://example.com/").unwrap();
        assert_eq!(placed.space, f.work, "most recent follows the Space in use");

        // A chosen default takes unmatched links; rules keep precedence.
        let routing = Routing { default: Destination::Space(f.work.clone()), ..routing };
        assert_eq!(place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.personal, "https://example.com/").unwrap().space, f.work);
        assert_eq!(f.tabs.iter().filter(|tab| tab.space_id == f.work && tab.item_id.is_none()).count(), 4);
    }

    #[test]
    fn a_deleted_destination_never_opens_under_its_old_profile() {
        let mut f = fixture();
        let routing = Routing { rules: vec![rule(Match::UrlContains, "zoom.us", &f.work)], default: Destination::Space(f.work.clone()) };
        f.lib.delete_space(&f.work).unwrap();
        let placed = place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.personal, "https://zoom.us/j/123").unwrap();
        assert_eq!((placed.space.as_str(), placed.rule, placed.fallback), (f.personal.as_str(), Some(0), true));
        assert_eq!(tab_profile(&f.lib, opened(&f, &placed)), Some(f.profile_a.as_str()));
        // No Space with a profile: nothing opens and no tab is invented.
        let count = f.tabs.len();
        f.lib.profiles.clear();
        assert_eq!(place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.personal, "https://zoom.us/j/123"), None);
        assert_eq!(f.tabs.len(), count);
    }

    #[test]
    fn links_wait_while_setup_is_open_then_open_once_in_order() {
        let inbox = Inbox::new();
        inbox.deliver("https://first.example/", Source::AppleEvent);
        inbox.deliver("crossbow://", Source::AppleEvent);
        inbox.deliver("https://second.example/", Source::LaunchArgument);
        assert!(ready_links(&inbox, true).is_empty(), "setup is open");
        assert!(ready_links(&inbox, true).is_empty());
        assert_eq!(ready_links(&inbox, false), [Link::Page("https://first.example/".into()), Link::App, Link::Page("https://second.example/".into())]);
        assert!(ready_links(&inbox, false).is_empty());

        let mut f = fixture();
        let routing = Routing::default();
        inbox.deliver("https://a.example/", Source::AppleEvent);
        inbox.deliver("https://b.example/", Source::AppleEvent);
        let placed: Vec<Placed> = ready_links(&inbox, false).into_iter().filter_map(|link| match link {
            Link::Page(url) => place_link(&f.lib, &routing, &SiteProfiles::default(), &mut f.tabs, &f.personal, &url),
            Link::App => None,
        }).collect();
        let urls: Vec<&str> = placed.iter().map(|placed| opened(&f, placed).url.as_str()).collect();
        assert_eq!(urls, ["https://a.example/", "https://b.example/"]);
        let today: Vec<&str> = f.tabs.iter().filter(|tab| tab.space_id == f.personal).map(|tab| tab.url.as_str()).collect();
        assert_eq!(today.len(), 2, "each link opened once");
    }
}
