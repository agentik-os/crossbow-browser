use super::*;
use crate::update::fixtures::{Raw, fake_archive, minimal_bundle, raw_tar_gz};

fn unpack_bytes(bytes: &[u8]) -> (tempfile::TempDir, Result<Bundle, ArchiveError>) {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("update.tar.gz");
    fs::write(&archive, bytes).unwrap();
    let into = dir.path().join("out");
    fs::create_dir(&into).unwrap();
    let result = unpack(&archive, &into);
    (dir, result)
}

/// A valid bundle's entries followed by `extra`.
fn with(extra: &[Raw]) -> Vec<u8> {
    let (plist, script) = minimal_bundle("0.2.0", 7);
    let mut entries = vec![
        Raw::Dir("Crossbow.app/"),
        Raw::Dir("Crossbow.app/Contents/"),
        Raw::File("Crossbow.app/Contents/Info.plist", plist.as_bytes()),
        Raw::Dir("Crossbow.app/Contents/MacOS/"),
        Raw::File("Crossbow.app/Contents/MacOS/crossbow", script.as_bytes()),
    ];
    entries.extend_from_slice(extra);
    raw_tar_gz(&entries)
}

/// Every file under `root`, relative, sorted.
fn listing(root: &Path) -> Vec<String> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(at).unwrap().flatten() {
            let path = entry.path();
            out.push(path.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn a_packed_app_unpacks_with_its_symlinks_and_info() {
    let dir = tempfile::tempdir().unwrap();
    let (unpacked, result) = unpack_bytes(&fake_archive(dir.path(), "0.2.0", 7));
    let bundle = result.unwrap();
    assert_eq!(bundle, Bundle { version: Version(0, 2, 0), build: 7, minimum_macos: MacVersion(11, 0, 0), executable: "crossbow".into() });
    let app = unpacked.path().join("out/Crossbow.app");
    let engine = app.join("Contents/Frameworks/Engine.framework");
    assert_eq!(fs::read_link(engine.join("Versions/Current")).unwrap(), Path::new("A"));
    assert_eq!(fs::read(engine.join("Engine")).unwrap(), b"engine", "the symlinks resolve inside");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(app.join("Contents/MacOS/crossbow")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(app.join("Contents/Info.plist")).unwrap().permissions().mode() & 0o777, 0o644);
}

#[test]
fn a_leading_dot_slash_is_fine() {
    let (plist, script) = minimal_bundle("0.2.0", 7);
    let bytes = raw_tar_gz(&[Raw::Dir("./"), Raw::Dir("./Crossbow.app/"), Raw::File("./Crossbow.app/Contents/Info.plist", plist.as_bytes()), Raw::File("./Crossbow.app/Contents/MacOS/crossbow", script.as_bytes())]);
    assert!(unpack_bytes(&bytes).1.is_ok());
}

#[test]
fn paths_outside_the_bundle_are_refused_and_nothing_escapes() {
    for (entry, expected) in [
        (Raw::File("Crossbow.app/../escaped.txt", b"x"), ArchiveError::Parent),
        (Raw::File("Crossbow.app/Contents/../../escaped.txt", b"x"), ArchiveError::Parent),
        (Raw::File("../escaped.txt", b"x"), ArchiveError::Parent),
        (Raw::File("/tmp/escaped.txt", b"x"), ArchiveError::Parent),
        (Raw::File("escaped.txt", b"x"), ArchiveError::Outside),
        (Raw::File("Other.app/Contents/Info.plist", b"x"), ArchiveError::Outside),
        (Raw::File("._Crossbow.app", b"x"), ArchiveError::AppleDouble),
        (Raw::File("Crossbow.app/Contents/._Info.plist", b"x"), ArchiveError::AppleDouble),
    ] {
        let (dir, result) = unpack_bytes(&with(&[entry]));
        assert_eq!(result, Err(expected));
        assert_eq!(listing(dir.path()), vec!["out", "update.tar.gz"], "nothing left behind or outside");
    }
}

#[test]
fn symlinks_that_point_out_are_refused() {
    for (link, target) in [
        ("Crossbow.app/Contents/out", "../../.."),
        ("Crossbow.app/Contents/out", "../.."),
        ("Crossbow.app/out", ".."),
        ("Crossbow.app/Contents/out", "/etc"),
        ("Crossbow.app/Contents/out", "MacOS/../../.."),
        ("Crossbow.app/Contents/out", ""),
    ] {
        let (dir, result) = unpack_bytes(&with(&[Raw::Symlink(link, target)]));
        assert_eq!(result, Err(ArchiveError::Symlink), "{link} -> {target}");
        assert_eq!(listing(dir.path()), vec!["out", "update.tar.gz"]);
    }
    // Climbing to the bundle itself is inside.
    assert!(unpack_bytes(&with(&[Raw::Symlink("Crossbow.app/Contents/Resources", "../Contents/MacOS")])).1.is_ok());
    assert!(unpack_bytes(&with(&[Raw::Symlink("Crossbow.app/Contents/root", "..")])).1.is_ok());
}

#[test]
fn nothing_is_written_through_a_symlink_even_one_named_in_another_case() {
    // A symlink to the bundle root, then a second one below it climbing two
    // levels: lexically one level, really two. Then a write through it.
    let entries = [
        Raw::Symlink("Crossbow.app/A", "."),
        Raw::Symlink("Crossbow.app/a/x", "../.."),
        Raw::File("Crossbow.app/a/X/escaped.txt", b"x"),
    ];
    let (dir, result) = unpack_bytes(&with(&entries));
    assert!(matches!(result, Err(ArchiveError::ThroughSymlink | ArchiveError::Symlink)), "{result:?}");
    assert_eq!(listing(dir.path()), vec!["out", "update.tar.gz"]);
    let (dir, result) = unpack_bytes(&with(&[Raw::Symlink("Crossbow.app/Contents/Lib", "MacOS"), Raw::File("Crossbow.app/Contents/Lib/extra", b"x")]));
    assert_eq!(result, Err(ArchiveError::ThroughSymlink));
    assert_eq!(listing(dir.path()), vec!["out", "update.tar.gz"]);
}

#[test]
fn hard_links_devices_and_duplicates_are_refused() {
    assert_eq!(unpack_bytes(&with(&[Raw::HardLink("Crossbow.app/Contents/link", "Crossbow.app/Contents/Info.plist")])).1, Err(ArchiveError::Special));
    assert_eq!(unpack_bytes(&with(&[Raw::Fifo("Crossbow.app/Contents/pipe")])).1, Err(ArchiveError::Special));
    assert_eq!(unpack_bytes(&with(&[Raw::File("Crossbow.app/Contents/MacOS/crossbow", b"second")])).1, Err(ArchiveError::Duplicate));
    assert_eq!(unpack_bytes(&with(&[Raw::File("Crossbow.app/Contents/MacOS", b"file over folder")])).1, Err(ArchiveError::Duplicate));
    assert_eq!(unpack_bytes(&raw_tar_gz(&[Raw::File("Crossbow.app", b"not a folder")])).1, Err(ArchiveError::Special));
}

#[test]
fn archives_without_a_valid_crossbow_info_plist_are_refused() {
    assert_eq!(unpack_bytes(b"not gzip").1, Err(ArchiveError::Format));
    assert_eq!(unpack_bytes(&raw_tar_gz(&[Raw::Dir("Crossbow.app/")])).1, Err(ArchiveError::InfoPlist));
    let other = crate::update::fixtures::info_plist("0.2.0", 7, "11.0").replace("app.crossbow.Crossbow", "com.example.Other");
    let (_, script) = minimal_bundle("0.2.0", 7);
    let bytes = raw_tar_gz(&[Raw::File("Crossbow.app/Contents/Info.plist", other.as_bytes()), Raw::File("Crossbow.app/Contents/MacOS/crossbow", script.as_bytes())]);
    assert_eq!(unpack_bytes(&bytes).1, Err(ArchiveError::Identity));
    let semver_build = crate::update::fixtures::info_plist("0.2.0", 7, "11.0").replace("<string>7</string>", "<string>0.2.0</string>");
    let bytes = raw_tar_gz(&[Raw::File("Crossbow.app/Contents/Info.plist", semver_build.as_bytes()), Raw::File("Crossbow.app/Contents/MacOS/crossbow", script.as_bytes())]);
    assert_eq!(unpack_bytes(&bytes).1, Err(ArchiveError::InfoPlist), "CFBundleVersion must be a build number");
    let (plist, _) = minimal_bundle("0.2.0", 7);
    let bytes = raw_tar_gz(&[Raw::File("Crossbow.app/Contents/Info.plist", plist.as_bytes()), Raw::Symlink("Crossbow.app/Contents/MacOS/crossbow", "../Info.plist")]);
    assert_eq!(unpack_bytes(&bytes).1, Err(ArchiveError::InfoPlist), "the executable must be a plain file");
}

#[test]
fn size_limits_hold() {
    // A header claiming a file larger than the limit is refused before any byte is read.
    let mut header = tar::Header::new_gnu();
    header.set_path("Crossbow.app/Contents/huge").unwrap();
    header.set_size(MAX_FILE + 1);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    let mut bytes = header.as_bytes().to_vec();
    bytes.extend_from_slice(&[0; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &bytes).unwrap();
    assert_eq!(unpack_bytes(&gz.finish().unwrap()).1, Err(ArchiveError::TooLarge));
}

/// `headers` PAX global headers of `size` bytes each, then a valid bundle.
fn after_global_headers(headers: usize, size: usize) -> Vec<u8> {
    let (plist, script) = minimal_bundle("0.2.0", 7);
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    let body = vec![b'x'; size];
    for _ in 0..headers {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(size as u64);
        header.set_cksum();
        builder.append(&header, &body[..]).unwrap();
    }
    for (path, body, mode) in [("Crossbow.app/Contents/Info.plist", plist.as_bytes(), 0o644), ("Crossbow.app/Contents/MacOS/crossbow", script.as_bytes(), 0o755)] {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder.append(&header, body).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn every_header_counts_toward_the_limits_pax_global_ones_included() {
    assert!(unpack_bytes(&after_global_headers(3, 64)).1.is_ok(), "a few are read past");
    assert_eq!(unpack_bytes(&after_global_headers(MAX_ENTRIES, 0)).1, Err(ArchiveError::TooLarge), "with the bundle's two files, one entry too many");
    assert!(unpack_bytes(&after_global_headers(MAX_ENTRIES - 2, 0)).1.is_ok(), "exactly at the limit");
}

#[test]
fn everything_decompressed_counts_toward_a_cap_headers_included() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("update.tar.gz");
    fs::write(&archive, after_global_headers(1, 64 * 1024)).unwrap();
    let small = |cap: u64| {
        let app = dir.path().join(format!("out-{cap}")).join(APP);
        fs::create_dir_all(app.parent().unwrap()).unwrap();
        unpack_from(fs::File::open(&archive).unwrap(), &app, cap)
    };
    assert_eq!(small(32 * 1024), Err(ArchiveError::TooLarge), "a header's data counts");
    assert_eq!(small(1 << 20), Ok(()));
    assert_eq!(MAX_STREAM, MAX_UNPACKED + MAX_ENTRIES as u64 * 1024);
}
