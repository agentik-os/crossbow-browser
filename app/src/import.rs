//! Locates Arc and Chrome data on this computer and runs the core importers.
//! Source files are only read, never modified. For isolated developer testing,
//! set `CROSSBOW_IMPORT_FIXTURES` to a root containing `Arc/` and `Chrome/Bookmarks`.
//! When set, missing fixtures never fall back to personal browser data or defaults.

use crossbow_core::import::{self, ImportReport};
use crossbow_core::library::Library;
use std::collections::HashSet;
use std::path::PathBuf;

fn fixture_root() -> Option<PathBuf> {
    std::env::var_os("CROSSBOW_IMPORT_FIXTURES").map(PathBuf::from)
}

fn arc_dir() -> Option<PathBuf> {
    if let Some(root) = fixture_root() {
        let dir = root.join("Arc");
        return (!root.as_os_str().is_empty() && dir.join("StorableSidebar.json").is_file()).then_some(dir);
    }
    #[cfg(target_os = "macos")]
    {
        let d = directories::BaseDirs::new()?.home_dir().join("Library/Application Support/Arc");
        if d.join("StorableSidebar.json").is_file() {
            return Some(d);
        }
    }
    #[cfg(target_os = "windows")]
    {
        // Arc for Windows keeps the same state files under its package folder.
        let local = directories::BaseDirs::new()?.data_local_dir().to_path_buf();
        let packages = local.join("Packages");
        if let Ok(rd) = std::fs::read_dir(&packages) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("TheBrowserCompany.Arc") {
                    let d = e.path().join("LocalCache/Local/Arc");
                    if d.join("StorableSidebar.json").is_file() {
                        return Some(d);
                    }
                }
            }
        }
    }
    None
}

pub fn arc_available() -> bool {
    arc_dir().is_some()
}

fn arc_machine_id() -> Option<String> {
    if fixture_root().is_some() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("defaults").args(["read", "company.thebrowser.Browser", "machineID"]).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !s.is_empty() {
            return Some(s);
        }
    }
    None
}

pub fn import_arc(lib: &mut Library) -> Result<ImportReport, String> {
    let dir = arc_dir().ok_or("Arc's data wasn't found on this computer.")?;
    let sidebar = std::fs::read_to_string(dir.join("StorableSidebar.json")).map_err(|e| format!("Couldn't read Arc's sidebar: {e}"))?;
    let expanded = std::fs::read_to_string(dir.join("StorableWindows.json")).map(|w| import::arc_expanded_folders(&w)).unwrap_or_else(|_| HashSet::new());
    let names = std::fs::read_to_string(dir.join("User Data/Local State")).map(|s| import::chromium_profile_names(&s)).unwrap_or_default();
    let data = import::parse_arc(&sidebar, arc_machine_id().as_deref(), &expanded)?;
    import::apply_arc(lib, &data, &names)
}

fn chrome_bookmarks_path() -> Option<PathBuf> {
    if let Some(root) = fixture_root() {
        let path = root.join("Chrome/Bookmarks");
        return (!root.as_os_str().is_empty() && path.is_file()).then_some(path);
    }
    let base = directories::BaseDirs::new()?;
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![base.home_dir().join("Library/Application Support/Google/Chrome/Default/Bookmarks")]
    } else if cfg!(target_os = "windows") {
        vec![base.data_local_dir().join("Google/Chrome/User Data/Default/Bookmarks")]
    } else {
        vec![base.config_dir().join("google-chrome/Default/Bookmarks"), base.config_dir().join("chromium/Default/Bookmarks")]
    };
    candidates.into_iter().find(|p| p.is_file())
}

pub fn chrome_available() -> bool {
    chrome_bookmarks_path().is_some()
}

pub fn import_chrome_bookmarks(lib: &mut Library) -> Result<ImportReport, String> {
    let path = chrome_bookmarks_path().ok_or("Chrome bookmarks weren't found on this computer.")?;
    let json = std::fs::read_to_string(&path).map_err(|e| format!("Couldn't read Chrome bookmarks: {e}"))?;
    let nodes = import::parse_chromium_bookmarks(&json)?;
    import::apply_bookmarks(lib, &nodes, "Chrome", "Chrome")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_core::model::Theme;
    use crossbow_core::store::Store;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/import")
    }

    // Each environment override lives in its own process, never in parallel tests.
    fn isolated(test: &str, root: &Path) -> bool {
        if std::env::var("CROSSBOW_IMPORT_TEST_CHILD").ok().as_deref() == Some(test) {
            assert_eq!(fixture_root().as_deref(), Some(root));
            return false;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test])
            .env("CROSSBOW_IMPORT_TEST_CHILD", test)
            .env("CROSSBOW_IMPORT_FIXTURES", root)
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("1 passed"), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        true
    }

    fn library() -> Library {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| { s.name = "My workspace".into(); s.theme = Theme::preset(7); }).unwrap();
        lib
    }

    fn archive_snapshot(lib: &Library) -> Vec<crossbow_core::store::ArchivedTab> {
        lib.profiles.keys().flat_map(|profile| lib.store.archive(profile, 100).unwrap()).collect()
    }

    #[test]
    fn fixture_entry_points_are_isolated_and_retry_safe() {
        if isolated("import::tests::fixture_entry_points_are_isolated_and_retry_safe", &fixtures()) {
            return;
        }
        assert!(arc_available());
        assert!(chrome_available());
        assert_eq!(arc_machine_id(), None);
        let mut lib = library();
        let initial = lib.spaces_ordered()[0].clone();
        let arc = import_arc(&mut lib).unwrap();
        assert_eq!((arc.spaces, arc.folders, arc.pinned, arc.favorites, arc.archived), (2, 1, 4, 2, 1));
        let chrome = import_chrome_bookmarks(&mut lib).unwrap();
        assert_eq!((chrome.spaces, chrome.folders, chrome.pinned), (1, 2, 3));
        assert_eq!(lib.spaces[&initial.0], initial.1);
        let entities = lib.store.all_entities().unwrap();
        let archive = archive_snapshot(&lib);
        assert_eq!(archive.len(), 1);
        let revision = lib.revision;
        let arc = import_arc(&mut lib).unwrap();
        let chrome = import_chrome_bookmarks(&mut lib).unwrap();
        assert_eq!((arc.spaces, arc.pinned, arc.favorites, arc.archived, arc.already_imported), (0, 0, 0, 0, 2));
        assert_eq!((chrome.spaces, chrome.pinned, chrome.already_imported), (0, 0, 1));
        assert_eq!(lib.store.all_entities().unwrap(), entities);
        assert_eq!(archive_snapshot(&lib), archive);
        assert_eq!(lib.revision, revision);
    }

    #[test]
    fn malformed_fixture_entry_points_leave_library_unchanged() {
        if isolated("import::tests::malformed_fixture_entry_points_leave_library_unchanged", &fixtures().join("malformed")) {
            return;
        }
        assert!(arc_available());
        assert!(chrome_available());
        assert_eq!(arc_machine_id(), None);
        let mut lib = library();
        let entities = lib.store.all_entities().unwrap();
        let revision = lib.revision;
        assert!(import_arc(&mut lib).unwrap_err().contains("sidebar file couldn't be read"));
        assert!(import_chrome_bookmarks(&mut lib).unwrap_err().contains("bookmarks file couldn't be read"));
        assert_eq!(lib.store.all_entities().unwrap(), entities);
        assert_eq!(lib.revision, revision);
        assert!(archive_snapshot(&lib).is_empty());
    }

    fn assert_sources_unavailable() {
        assert!(!arc_available());
        assert!(!chrome_available());
        assert_eq!(arc_machine_id(), None);
        let mut lib = library();
        let entities = lib.store.all_entities().unwrap();
        let revision = lib.revision;
        assert_eq!(import_arc(&mut lib).unwrap_err(), "Arc's data wasn't found on this computer.");
        assert_eq!(import_chrome_bookmarks(&mut lib).unwrap_err(), "Chrome bookmarks weren't found on this computer.");
        assert_eq!(lib.store.all_entities().unwrap(), entities);
        assert_eq!(lib.revision, revision);
    }

    #[test]
    fn missing_fixture_root_never_falls_back_to_personal_data() {
        if isolated("import::tests::missing_fixture_root_never_falls_back_to_personal_data", &fixtures().join("missing-source-root")) {
            return;
        }
        assert_sources_unavailable();
    }

    #[test]
    fn empty_fixture_root_never_falls_back_to_personal_data() {
        if isolated("import::tests::empty_fixture_root_never_falls_back_to_personal_data", Path::new("")) {
            return;
        }
        assert_sources_unavailable();
    }
}
