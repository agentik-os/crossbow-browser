use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn prepare_root(data_dir: &Path) -> io::Result<PathBuf> {
    let root = data_dir.join("Chromium");
    if root.to_str().is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Chromium root must be UTF-8"));
    }
    fs::create_dir_all(&root)?;
    let root = fs::canonicalize(root)?;
    if root.to_str().is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Chromium root must be UTF-8"));
    }
    prepare_directory(&root.join("Default"))?;
    purge_detached(&root);
    Ok(root)
}

pub(super) fn profile_directory_name(profile_id: &str) -> io::Result<String> {
    // Match the protocol's entity-ID alphabet without silently stripping bytes.
    if profile_id.is_empty()
        || profile_id.len() > 64
        || !profile_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid profile ID"));
    }
    // Hex preserves case-distinct IDs even on case-insensitive filesystems and
    // keeps every profile separate from Chromium's reserved Default/Guest names.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::from("Profile-");
    for byte in profile_id.bytes() {
        name.push(HEX[(byte >> 4) as usize] as char);
        name.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(name)
}

/// Whether `profile_id` has its own website data folder in `data_dir`; a
/// profile without one starts with empty website data when it first loads.
pub fn has_profile_data(data_dir: &Path, profile_id: &str) -> bool {
    profile_directory_name(profile_id).is_ok_and(|name| fs::symlink_metadata(data_dir.join("Chromium").join(name)).is_ok_and(|meta| meta.is_dir()))
}

pub(super) fn prepare_profile_path(
    data_dir: &Path,
    root: &Path,
    profile_id: &str,
) -> io::Result<PathBuf> {
    let name = profile_directory_name(profile_id)?;
    if fs::canonicalize(data_dir.join("Chromium"))? != root {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "profile root differs from the running engine"));
    }
    // CEF 154's ChromeBrowserContext requires a direct child of root_cache_path,
    // not an arbitrary descendant. Do not move or delete legacy Profiles/<id>.
    let path = root.join(name);
    prepare_directory(&path)?;
    Ok(path)
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || fs::canonicalize(path)? != path {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "profile path must be a real directory under the engine root"));
    }
    // Existing directories can be read-only. Detect failure before CEF can
    // silently substitute in-memory storage; never touch an existing file.
    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let serial = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = path.join(format!(".crossbow-write-check-{}-{serial}", std::process::id()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&probe) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = file.write_all(b"0");
        drop(file);
        let cleanup = fs::remove_file(probe);
        result?;
        return cleanup;
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate a profile write check"))
}

/// A deleted profile's Chromium folder, moved aside inside the same root.
#[derive(Debug)]
pub struct DetachedProfileData {
    root: PathBuf,
    original: PathBuf,
    detached: PathBuf,
}

impl DetachedProfileData {
    pub(super) fn original(&self) -> &Path {
        &self.original
    }

    /// Put the folder back after the profile could not be deleted.
    pub fn restore(self) -> io::Result<()> {
        own_directory(&self.root, &self.detached)?;
        match fs::symlink_metadata(&self.original) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(&self.detached, &self.original),
            Err(error) => Err(error),
            Ok(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, "the profile folder was recreated")),
        }
    }

    /// Delete the folder. On failure it stays aside and is removed the next
    /// time the engine starts.
    pub fn remove(self) -> io::Result<()> {
        own_directory(&self.root, &self.detached)?;
        fs::remove_dir_all(&self.detached)
    }
}

const DETACHED_PREFIX: &str = ".crossbow-deleted-";

/// Refuse anything but a real directory directly inside `root`.
fn own_directory(root: &Path, path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || path.parent() != Some(root) || fs::canonicalize(path)? != path {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing to delete anything but a profile's own folder"));
    }
    Ok(())
}

/// Move a profile's own folder aside for deletion. Ok(None) when it has none.
/// Only a real directory that is a direct child of the canonical Chromium root
/// qualifies; symlinks, files and anything elsewhere are refused untouched.
pub(super) fn detach_profile_data(data_dir: &Path, profile_id: &str, running_root: Option<&Path>) -> io::Result<Option<DetachedProfileData>> {
    let name = profile_directory_name(profile_id)?;
    let root = match fs::canonicalize(data_dir.join("Chromium")) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if running_root.is_some_and(|running| running != root) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "profile root differs from the running engine"));
    }
    let original = root.join(&name);
    match fs::symlink_metadata(&original) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => own_directory(&root, &original)?,
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let detached = root.join(format!("{DETACHED_PREFIX}{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        if fs::symlink_metadata(&detached).is_ok() { continue; }
        fs::rename(&original, &detached)?;
        return Ok(Some(DetachedProfileData { root, original, detached }));
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate a deleted-profile name"))
}

/// Remove folders an interrupted deletion left aside. Links are skipped.
fn purge_detached(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(|name| name.starts_with(DETACHED_PREFIX)) { continue; }
        let path = entry.path();
        if own_directory(root, &path).is_ok() {
            if let Err(error) = fs::remove_dir_all(&path) {
                log::warn!("Could not remove deleted profile data: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "crossbow-profile-path-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn profile_path_obeys_exact_cef_direct_child_contract() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let old_path = root.join("Profiles").join("hallmark-profile");
        assert_ne!(old_path.parent(), Some(root.as_path()));
        let path = prepare_profile_path(&fixture.0, &root, "hallmark-profile").unwrap();
        assert!(path.is_absolute());
        assert_eq!(path.parent(), Some(root.as_path()));
        assert_eq!(fs::canonicalize(&path).unwrap(), path);
    }

    #[test]
    fn profile_data_is_found_only_in_the_profiles_own_folder() {
        let fixture = Fixture::new();
        assert!(!has_profile_data(&fixture.0, "hallmark-profile"), "no Chromium folder yet");
        let root = prepare_root(&fixture.0).unwrap();
        assert!(!has_profile_data(&fixture.0, "hallmark-profile"));
        prepare_profile_path(&fixture.0, &root, "hallmark-profile").unwrap();
        assert!(has_profile_data(&fixture.0, "hallmark-profile"));
        assert!(!has_profile_data(&fixture.0, "other-profile"));
        assert!(!has_profile_data(&fixture.0, "../Default"), "an invalid ID names no folder");
        fs::write(root.join(profile_directory_name("file-profile").unwrap()), b"not a folder").unwrap();
        assert!(!has_profile_data(&fixture.0, "file-profile"));
    }

    #[test]
    fn invalid_ids_are_rejected_instead_of_sanitized() {
        for id in ["", ".", "..", "../A", "A/B", "A\\B", "/A", "a.b", "a b", "a\0b", "é", "Ａ"] {
            assert_eq!(profile_directory_name(id).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{id:?}");
        }
        assert!(profile_directory_name(&"a".repeat(64)).is_ok());
        assert!(profile_directory_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn names_are_injective_including_case_and_previously_stripped_underscores() {
        let ids = ["a", "A", "ab", "a_b", "a-b", "AB", "Ab", "Default", "CON", "Profile-a", "_"];
        let names: HashSet<_> = ids.into_iter().map(|id| profile_directory_name(id).unwrap().to_ascii_lowercase()).collect();
        assert_eq!(names.len(), ids.len());
        assert_eq!(profile_directory_name("A_a-").unwrap(), "Profile-415f612d");
        assert!(names.iter().all(|name| name.starts_with("profile-")));
        assert!(!names.contains("default"));
    }

    #[test]
    fn case_distinct_ids_use_distinct_real_directories() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let a = prepare_profile_path(&fixture.0, &root, "A").unwrap();
        let b = prepare_profile_path(&fixture.0, &root, "a").unwrap();
        fs::write(a.join("sentinel"), b"profile A").unwrap();
        assert!(!b.join("sentinel").exists());
        assert_ne!(a, b);
    }

    #[test]
    fn reuse_preserves_existing_files_and_removes_only_its_probe() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let path = prepare_profile_path(&fixture.0, &root, "profile-a").unwrap();
        fs::write(path.join("sentinel"), b"existing data").unwrap();
        fs::write(path.join(".crossbow-write-check-foreign"), b"not our file").unwrap();
        let reused = prepare_profile_path(&fixture.0, &root, "profile-a").unwrap();
        assert_eq!(reused, path);
        assert_eq!(fs::read(reused.join("sentinel")).unwrap(), b"existing data");
        assert_eq!(fs::read(reused.join(".crossbow-write-check-foreign")).unwrap(), b"not our file");
        assert_eq!(fs::read_dir(reused).unwrap().count(), 2);
    }

    #[test]
    fn legacy_nested_data_is_left_untouched() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let legacy = root.join("Profiles").join("profile-a");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("sentinel"), b"legacy data").unwrap();
        let path = prepare_profile_path(&fixture.0, &root, "profile-a").unwrap();
        assert_eq!(fs::read(legacy.join("sentinel")).unwrap(), b"legacy data");
        assert_eq!(fs::read_dir(&legacy).unwrap().count(), 1);
        assert_eq!(fs::read_dir(path).unwrap().count(), 0);
    }

    #[test]
    fn root_and_profile_paths_are_scoped_to_the_running_engine() {
        let fixture_a = Fixture::new();
        let fixture_b = Fixture::new();
        let root_a = prepare_root(&fixture_a.0).unwrap();
        let root_b = prepare_root(&fixture_b.0).unwrap();
        assert_eq!(
            prepare_profile_path(&fixture_b.0, &root_a, "same-id").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
        let path_a = prepare_profile_path(&fixture_a.0, &root_a, "same-id").unwrap();
        let path_b = prepare_profile_path(&fixture_b.0, &root_b, "same-id").unwrap();
        assert_ne!(path_a, path_b);
    }

    #[test]
    fn root_creation_failure_is_returned_without_overwriting() {
        let fixture = Fixture::new();
        let blocked = fixture.0.join("Chromium");
        fs::write(&blocked, b"not a directory").unwrap();
        assert!(prepare_root(&fixture.0).is_err());
        assert_eq!(fs::read(blocked).unwrap(), b"not a directory");
    }

    #[test]
    fn default_directory_failure_is_returned_without_overwriting() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("Chromium")).unwrap();
        let blocked = fixture.0.join("Chromium/Default");
        fs::write(&blocked, b"not a directory").unwrap();
        assert!(prepare_root(&fixture.0).is_err());
        assert_eq!(fs::read(blocked).unwrap(), b"not a directory");
    }

    #[test]
    fn profile_directory_failure_is_returned_without_overwriting() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let blocked = root.join(profile_directory_name("profile-a").unwrap());
        fs::write(&blocked, b"not a directory").unwrap();
        assert!(prepare_profile_path(&fixture.0, &root, "profile-a").is_err());
        assert_eq!(fs::read(blocked).unwrap(), b"not a directory");
    }

    #[test]
    fn invalid_id_does_not_create_any_profile_directory() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        assert!(prepare_profile_path(&fixture.0, &root, "../escape").is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert!(!fixture.0.join("escape").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_profile_cannot_alias_another_profile_or_escape_root() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let real = prepare_profile_path(&fixture.0, &root, "real").unwrap();
        let alias = root.join(profile_directory_name("alias").unwrap());
        let outside = root.join(profile_directory_name("outside").unwrap());
        symlink(&real, &alias).unwrap();
        symlink(&fixture.0, &outside).unwrap();
        assert!(prepare_profile_path(&fixture.0, &root, "alias").is_err());
        assert!(prepare_profile_path(&fixture.0, &root, "outside").is_err());
        assert!(fs::symlink_metadata(alias).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(outside).unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn data_directory_aliases_resolve_to_one_canonical_root() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let data = fixture.0.join("data");
        let alias = fixture.0.join("alias");
        let root = prepare_root(&data).unwrap();
        symlink(&data, &alias).unwrap();
        assert_eq!(prepare_root(&alias).unwrap(), root);
        assert_eq!(
            prepare_profile_path(&alias, &root, "profile-a").unwrap(),
            prepare_profile_path(&data, &root, "profile-a").unwrap(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_root_is_rejected_not_lossily_aliased() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let fixture = Fixture::new();
        let data = fixture.0.join(OsStr::from_bytes(b"data-\xff"));
        assert_eq!(prepare_root(&data).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn existing_read_only_profile_is_rejected_and_its_data_is_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let path = prepare_profile_path(&fixture.0, &root, "profile-a").unwrap();
        fs::write(path.join("sentinel"), b"existing data").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).unwrap();
        let result = prepare_profile_path(&fixture.0, &root, "profile-a");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"existing data");
    }

    fn sentinel(path: &Path, text: &[u8]) {
        fs::write(path.join("sentinel"), text).unwrap();
    }

    #[test]
    fn detached_profile_data_is_removed_without_touching_other_profiles() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let deleted = prepare_profile_path(&fixture.0, &root, "deleted").unwrap();
        let kept = prepare_profile_path(&fixture.0, &root, "kept").unwrap();
        sentinel(&deleted, b"deleted cookies");
        sentinel(&kept, b"kept cookies");
        sentinel(&root.join("Default"), b"default");

        let detached = detach_profile_data(&fixture.0, "deleted", None).unwrap().unwrap();
        assert_eq!(detached.original(), deleted);
        assert!(!deleted.exists(), "the profile path is free at once");
        assert_eq!(detached.detached.parent(), Some(root.as_path()));
        assert_eq!(fs::read(detached.detached.join("sentinel")).unwrap(), b"deleted cookies");
        detached.remove().unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2, "only Default and the kept profile remain");
        assert_eq!(fs::read(kept.join("sentinel")).unwrap(), b"kept cookies");
        assert_eq!(fs::read(root.join("Default/sentinel")).unwrap(), b"default");
    }

    #[test]
    fn detached_profile_data_can_be_restored_after_a_failed_deletion() {
        let fixture = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let path = prepare_profile_path(&fixture.0, &root, "restored").unwrap();
        sentinel(&path, b"still here");
        detach_profile_data(&fixture.0, "restored", Some(&root)).unwrap().unwrap().restore().unwrap();
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"still here");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn missing_profile_data_or_root_is_nothing_to_delete() {
        let fixture = Fixture::new();
        assert!(detach_profile_data(&fixture.0, "never-opened", None).unwrap().is_none());
        assert!(!fixture.0.join("Chromium").exists(), "checking never creates the root");
        let root = prepare_root(&fixture.0).unwrap();
        assert!(detach_profile_data(&fixture.0, "never-opened", Some(&root)).unwrap().is_none());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn deletion_refuses_invalid_ids_files_and_other_engine_roots() {
        let fixture = Fixture::new();
        let other = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let other_root = prepare_root(&other.0).unwrap();
        sentinel(&root.join("Default"), b"default");
        for id in ["", "..", "../escape", "a/b", "Default/.."] {
            assert_eq!(detach_profile_data(&fixture.0, id, None).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{id:?}");
        }
        let blocked = root.join(profile_directory_name("file").unwrap());
        fs::write(&blocked, b"not a directory").unwrap();
        assert_eq!(detach_profile_data(&fixture.0, "file", None).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&blocked).unwrap(), b"not a directory");
        let path = prepare_profile_path(&fixture.0, &root, "elsewhere").unwrap();
        assert_eq!(detach_profile_data(&fixture.0, "elsewhere", Some(&other_root)).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(path.is_dir());
        assert_eq!(fs::read(root.join("Default/sentinel")).unwrap(), b"default");
    }

    #[cfg(unix)]
    #[test]
    fn deletion_never_follows_a_symlinked_profile_folder() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let real = prepare_profile_path(&fixture.0, &root, "real").unwrap();
        sentinel(&real, b"real profile");
        sentinel(&outside.0, b"outside data");
        let to_outside = root.join(profile_directory_name("outside").unwrap());
        let to_real = root.join(profile_directory_name("alias").unwrap());
        symlink(&outside.0, &to_outside).unwrap();
        symlink(&real, &to_real).unwrap();
        for id in ["outside", "alias"] {
            assert_eq!(detach_profile_data(&fixture.0, id, None).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{id}");
        }
        assert!(fs::symlink_metadata(&to_outside).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(outside.0.join("sentinel")).unwrap(), b"outside data");
        assert_eq!(fs::read(real.join("sentinel")).unwrap(), b"real profile");
    }

    #[cfg(unix)]
    #[test]
    fn leftover_detached_folders_are_purged_at_start_without_following_links() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let root = prepare_root(&fixture.0).unwrap();
        let kept = prepare_profile_path(&fixture.0, &root, "kept").unwrap();
        sentinel(&kept, b"kept");
        sentinel(&outside.0, b"outside data");
        let leftover = root.join(".crossbow-deleted-Profile-6c656674-1-0");
        fs::create_dir(&leftover).unwrap();
        sentinel(&leftover, b"leftover cookies");
        let link = root.join(".crossbow-deleted-link");
        symlink(&outside.0, &link).unwrap();
        prepare_root(&fixture.0).unwrap();
        assert!(!leftover.exists());
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "links are skipped, not followed");
        assert_eq!(fs::read(outside.0.join("sentinel")).unwrap(), b"outside data");
        assert_eq!(fs::read(kept.join("sentinel")).unwrap(), b"kept");
    }
}
