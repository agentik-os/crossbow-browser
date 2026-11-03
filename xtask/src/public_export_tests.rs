//! The export against fixture repositories made here, under TMPDIR. Each
//! rule must fail closed: a refused export writes nothing, and a refused
//! `--commit-into` leaves the clone as it was. Nothing is pushed.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// 2025-11-01T12:00:00Z, the date of every fixture commit.
const FIXTURE_TIME: &str = "1761998400 +0000";

const RULES: &str = r#"# Fixture rules.
[exclude]
paths = ["docs/internal/**", "AGENTS.md", "public-export.toml"]

[[rewrite]]
from = "/home/fixture/work/widget"
to = "<repo>"

[[rewrite]]
from = "example.com/acme/widget"
to = "example.com/acme/widget-public"
whole = true

[deny]
strings = ["/home/fixture", "codename-zebra"]
regexes = ['acme/widget([^a-z0-9_-]|$)']

[markdown_links]
excluded = "unlink"

[size]
max_bytes = 4096
allow = ["assets/large.bin"]

[license]
expected = "Apache-2.0"

[commit_into]
private_remotes = ["acme/widget"]
"#;

const README: &str = "# Widget

Read the [guide](docs/guide.md) and [its start](docs/guide.md#getting-started).
The [plan](docs/internal/plan.md) is private, and so is ![a chart](docs/internal/chart.png).
Same file: [usage](#usage-notes).

## Usage notes

```text
[not a link](docs/missing.md)
```

Inline `[code](docs/missing.md)` is not a link either.
";

const BASE: &[(&str, &[u8])] = &[
    ("LICENSE", b"Apache License\nVersion 2.0, January 2004 (fixture)\n"),
    ("NOTICE", b"Widget\nCopyright 2025 Fixture\n"),
    ("Cargo.toml", b"[workspace]\nmembers = [\"crates/one\"]\n\n[workspace.package]\nlicense = \"Apache-2.0\"\n"),
    ("crates/one/Cargo.toml", b"[package]\nname = \"one\"\nlicense.workspace = true\n"),
    (
        "crates/one/src/lib.rs",
        b"// Built in /home/fixture/work/widget/crates/one.\n// See https://example.com/acme/widget and https://example.com/acme/widget-api.\n",
    ),
    ("README.md", README.as_bytes()),
    ("docs/guide.md", b"# Guide\n\n## Getting started\n\nBack to the [readme](../README.md).\n"),
    ("docs/internal/plan.md", b"codename-zebra lives in /home/fixture/work/widget\n"),
    ("docs/internal/chart.png", b"\x89PNG\0\0codename-zebra"),
    ("AGENTS.md", b"Agent notes: codename-zebra\n"),
    ("public-export.toml", RULES.as_bytes()),
    ("assets/icon.bin", b"\x89PNG\0\0\x01\x02fixture"),
];

/// A scratch folder under TMPDIR, new for each call.
fn scratch(name: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("public-export-{}-{name}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// git for fixtures: no global or system config, a fixed identity and date.
fn fixture_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false"])
        .args(["-c", "core.hooksPath=/dev/null", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", FIXTURE_TIME)
        .env("GIT_COMMITTER_DATE", FIXTURE_TIME)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim_end().to_owned()
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    /// A source repository holding BASE plus an executable script, uncommitted.
    fn new(name: &str) -> Fixture {
        let root = scratch(name);
        let repo = root.join("source");
        std::fs::create_dir_all(&repo).unwrap();
        fixture_git(&repo, &["init", "--quiet"]);
        let fixture = Fixture { root, repo };
        for (path, bytes) in BASE {
            fixture.write(path, bytes);
        }
        fixture.write("tools/run.sh", b"#!/bin/sh\necho widget\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(fixture.repo.join("tools/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        fixture
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let full = self.repo.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }

    fn remove(&self, path: &str) {
        std::fs::remove_file(self.repo.join(path)).unwrap();
    }

    fn commit(&self) -> String {
        fixture_git(&self.repo, &["add", "-A"]);
        fixture_git(&self.repo, &["commit", "--quiet", "--allow-empty", "-m", "fixture"]);
        fixture_git(&self.repo, &["rev-parse", "HEAD"])
    }

    /// Runs the command as `cargo xtask public-export` would, into a new
    /// `--out` folder.
    fn export(&self, rev: &str, extra: &[&str]) -> (Result<(), String>, PathBuf) {
        let out = self.root.join(format!("out-{}", NEXT.fetch_add(1, Ordering::SeqCst)));
        let mut args = vec!["--rev".to_owned(), rev.to_owned(), "--out".to_owned(), out.to_string_lossy().into_owned()];
        args.extend(extra.iter().map(|a| a.to_string()));
        (run(&args, &self.repo), out)
    }

    /// An export that must be refused: nothing at `--out`, and the message.
    fn refused(&self, rev: &str, extra: &[&str]) -> String {
        let (result, out) = self.export(rev, extra);
        let error = result.expect_err("the export should be refused");
        assert!(!out.exists(), "a refused export wrote {}", out.display());
        assert!(!staging(&out).unwrap().exists());
        error
    }

    /// A clone of the public repository: a new repository with this origin.
    fn public_clone(&self, origin: &str) -> PathBuf {
        let dir = self.root.join(format!("public-{}", NEXT.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        fixture_git(&dir, &["init", "--quiet"]);
        fixture_git(&dir, &["remote", "add", "origin", origin]);
        dir
    }
}

/// Every file under `dir` but `.git`, by relative path.
fn files_in(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, found);
            } else {
                let relative = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                found.insert(relative, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(dir, dir, &mut found);
    found
}

fn text(out: &Path, path: &str) -> String {
    std::fs::read_to_string(out.join(path)).unwrap()
}

// -- the small languages -------------------------------------------------------------

#[test]
fn sha256_matches_the_standard_vectors() {
    assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(
        hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(hex(&sha256(&vec![b'a'; 1_000_000])), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
}

#[test]
fn day_is_the_utc_calendar_date() {
    assert_eq!(day(0), "1970-01-01");
    assert_eq!(day(951_782_400), "2000-02-29");
    assert_eq!(day(1_761_998_400), "2025-11-01");
    assert_eq!(day(1_798_761_599), "2026-12-31");
}

#[test]
fn globs_match_folders_names_and_double_stars() {
    let roadmap = Glob::new("docs/roadmap/**").unwrap();
    assert!(roadmap.matches("docs/roadmap/STATE.json"));
    assert!(roadmap.matches("docs/roadmap/a/b.md"));
    assert!(!roadmap.matches("docs/roadmapper.md"));
    let arc = Glob::new("docs/research/arc-*").unwrap();
    assert!(arc.matches("docs/research/arc-features.json"));
    assert!(!arc.matches("docs/research/installed-cli-contracts.md"));
    let name = Glob::new("AGENTS.md").unwrap();
    assert!(name.matches("AGENTS.md") && name.matches("crates/x/AGENTS.md"));
    assert!(!Glob::new("/AGENTS.md").unwrap().matches("crates/x/AGENTS.md"));
    assert!(Glob::new("docs/evidence").unwrap().matches("docs/evidence/w3/plan.md"));
    assert!(Glob::new("**/secret?.txt").unwrap().matches("a/b/secret1.txt"));
    assert!(Glob::new("a/**b").is_err() && Glob::new("../x").is_err() && Glob::new("").is_err());
}

#[test]
fn regexes_match_case_insensitively_and_reject_what_they_cannot_parse() {
    let found = |pattern: &str, text: &str| Regex::new(pattern).unwrap().find_from(text.to_ascii_lowercase().as_bytes(), 0).unwrap();
    let private = "acme/widget([^a-z0-9_-]|$)";
    assert_eq!(found(private, "see ACME/Widget."), Some(4));
    assert_eq!(found(private, "git@x:acme/widget.git"), Some(6));
    assert_eq!(found(private, "end acme/widget"), Some(4));
    assert_eq!(found(private, "acme/widget\nnext"), Some(0));
    assert_eq!(found(private, "acme/widget-public acme/widget_x acme/widgets"), None);
    assert_eq!(found(r"tok_(red|blue)_[a-z0-9]{8,}", "key TOK_RED_abcd1234ef"), Some(4));
    assert_eq!(found(r"^\s*secret = \d+$", "a\n  secret = 42\nb"), Some(2));
    assert_eq!(found(r"(ab)+c", "xxababababc"), Some(2));
    assert_eq!(found(r"a.c", "a\nc"), None);
    assert_eq!(found(r"[^\w]x", "_x ax"), None);
    assert_eq!(found(r"(a?){2}b", "b"), Some(0));
    for bad in ["(", "a)", "[a-", "*a", r"\bword", "(?i)x", "a{2,1}", r"\p{L}"] {
        assert!(Regex::new(bad).is_err(), "{bad} should be rejected");
    }
}

#[test]
fn the_config_rejects_unknown_tables_keys_and_an_empty_deny_list() {
    assert!(Config::parse(RULES).is_ok());
    let unknown_table = format!("{RULES}\n[publish]\nremote = \"x\"\n");
    assert!(Config::parse(&unknown_table).unwrap_err().contains("unknown table [publish]"));
    let unknown_key = RULES.replace("[size]\n", "[size]\nmax = 3\n");
    assert!(Config::parse(&unknown_key).unwrap_err().contains("unknown key `max`"));
    let no_deny = RULES.replace("strings = [\"/home/fixture\", \"codename-zebra\"]\n", "").replace("regexes = ['acme/widget([^a-z0-9_-]|$)']\n", "");
    assert!(Config::parse(&no_deny).unwrap_err().contains("[deny] must list"));
    let no_private = RULES.replace("private_remotes = [\"acme/widget\"]", "private_remotes = []");
    assert!(Config::parse(&no_private).unwrap_err().contains("private_remotes"));
    assert!(Config::parse("[deny]\nstrings = [\"a\"\n").is_err());
    assert!(Config::parse("[deny]\nstrings = \"a\" trailing\n").is_err());
}

// -- exporting a revision --------------------------------------------------------------

#[test]
fn a_clean_revision_exports_its_tree_without_excluded_paths() {
    let fixture = Fixture::new("clean");
    let rev = fixture.commit();
    let (result, out) = fixture.export(&rev, &[]);
    result.unwrap();
    let tree = files_in(&out.join("tree"));
    let paths: Vec<&str> = tree.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        [
            "Cargo.toml",
            "LICENSE",
            "NOTICE",
            "README.md",
            "assets/icon.bin",
            "crates/one/Cargo.toml",
            "crates/one/src/lib.rs",
            "docs/guide.md",
            "tools/run.sh"
        ]
    );
    let readme = text(&out, "tree/README.md");
    assert!(readme.contains("Read the [guide](docs/guide.md) and [its start](docs/guide.md#getting-started).\n"));
    assert!(readme.contains("The plan is private, and so is a chart.\n"), "{readme}");
    assert!(readme.contains("[not a link](docs/missing.md)") && readme.contains("`[code](docs/missing.md)`"));

    let manifest = text(&out, "manifest.tsv");
    assert!(manifest.contains(&format!("# rev {rev}\n")));
    assert!(manifest.contains("path\tsha256\tbytes\tmode\trewrites\tlinks_dropped\n"));
    let readme_line = format!("README.md\t{}\t{}\t100644\t-\t2\n", hex(&sha256(readme.as_bytes())), readme.len());
    assert!(manifest.contains(&readme_line), "{manifest}");
    assert!(manifest.lines().any(|l| l.starts_with("tools/run.sh\t") && l.contains("\t100755\t")));
    assert!(manifest.lines().any(|l| l.starts_with("crates/one/src/lib.rs\t") && l.ends_with("\t1:1,2:1\t0")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("tree/tools/run.sh")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    let report = text(&out, "report.txt");
    for line in [
        "  AGENTS.md  [AGENTS.md]",
        "  docs/internal/plan.md  [docs/internal/**]",
        "  public-export.toml  [public-export.toml]",
        "  README.md:4 link -> docs/internal/plan.md",
        "  README.md:4 image -> docs/internal/chart.png",
    ] {
        assert!(report.contains(line), "{line:?} missing from\n{report}");
    }
}

#[test]
fn the_export_reads_the_revision_never_the_working_tree() {
    let fixture = Fixture::new("working-tree");
    let rev = fixture.commit();
    fixture.write("README.md", b"# Changed after the commit: codename-zebra\n");
    fixture.write("untracked.txt", b"codename-zebra\n");
    fixture.write(".gitignore", b"ignored.txt\n");
    fixture.write("ignored.txt", b"codename-zebra\n");
    let (result, out) = fixture.export(&rev, &[]);
    result.unwrap();
    let tree = files_in(&out.join("tree"));
    assert!(tree["README.md"].starts_with(b"# Widget\n"));
    assert!(!tree.contains_key("untracked.txt") && !tree.contains_key("ignored.txt") && !tree.contains_key(".gitignore"));
}

#[test]
fn an_excluded_path_never_reaches_the_export_and_links_to_it_lose_only_the_link() {
    let fixture = Fixture::new("excluded");
    fixture.write("crates/one/AGENTS.md", b"nested agent notes, codename-zebra\n");
    fixture.write("docs/guide.md", b"# Guide\n\n## Getting started\n\nSee [the agents](../AGENTS.md#notes), [inside](internal/), [gone](internal/never.md) and [the readme](../README.md).
");
    let rev = fixture.commit();
    let (result, out) = fixture.export(&rev, &[]);
    result.unwrap();
    let tree = files_in(&out.join("tree"));
    assert!(tree.keys().all(|p| !p.contains("AGENTS") && !p.starts_with("docs/internal") && p != "public-export.toml"));
    assert_eq!(text(&out, "tree/docs/guide.md"), "# Guide

## Getting started

See the agents, inside, gone and [the readme](../README.md).
");
    let report = text(&out, "report.txt");
    assert!(report.contains("  docs/guide.md:5 link -> internal/never.md (not in the revision either)
"), "{report}");
    assert!(report.contains("  docs/guide.md:5 link -> internal/
"), "{report}");

    // Set to fail, the same links refuse the export instead.
    let failing = RULES.replace("excluded = \"unlink\"", "excluded = \"fail\"");
    fixture.write("public-export.toml", failing.as_bytes());
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("link: docs/guide.md:5: ../AGENTS.md#notes points at an excluded file"), "{error}");
}

#[test]
#[cfg(unix)]
fn a_kept_symlink_fails() {
    let fixture = Fixture::new("symlink");
    std::os::unix::fs::symlink("../LICENSE", fixture.repo.join("docs/LICENSE")).unwrap();
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("kind: docs/LICENSE is a blob (mode 120000)"), "{error}");
}

#[test]
fn rewrites_change_text_files_and_only_whole_names() {
    let fixture = Fixture::new("rewrite");
    let rev = fixture.commit();
    let (result, out) = fixture.export(&rev, &[]);
    result.unwrap();
    assert_eq!(
        text(&out, "tree/crates/one/src/lib.rs"),
        "// Built in <repo>/crates/one.\n// See https://example.com/acme/widget-public and https://example.com/acme/widget-api.\n"
    );
    let report = text(&out, "report.txt");
    assert!(report.contains("  #1 \"/home/fixture/work/widget\" -> \"<repo>\": 1 in 1 files\n    crates/one/src/lib.rs (1)\n"), "{report}");
    assert!(report.contains("  #2 \"example.com/acme/widget\" -> \"example.com/acme/widget-public\" (whole): 1 in 1 files\n"), "{report}");
}

#[test]
fn a_rewrite_never_touches_binary_files_so_their_deny_hits_fail() {
    let fixture = Fixture::new("rewrite-binary");
    fixture.write("assets/build.bin", b"\0\x01built in /home/fixture/work/widget\0");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("deny: assets/build.bin: \"/home/fixture\" at byte 11"), "{error}");
}

#[test]
fn a_private_url_the_rewrite_does_not_cover_fails() {
    let fixture = Fixture::new("private-url");
    fixture.write("docs/clone.md", b"# Clone\n\nUse git@example.com:ACME/widget.git or\nhttps://example.com/acme/widget-public.\n");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("deny: docs/clone.md: \"acme/widget([^a-z0-9_-]|$)\" line 3"), "{error}");
    assert!(!error.contains("line 4"), "{error}");
}

#[test]
fn a_deny_hit_in_content_fails_with_every_line() {
    let fixture = Fixture::new("deny-content");
    fixture.write("crates/one/src/names.rs", b"// one\n// Codename-Zebra\n// two\n// CODENAME-ZEBRA and codename-zebra\n");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("deny: crates/one/src/names.rs: \"codename-zebra\" line 2, 4\n") || error.ends_with("line 2, 4"), "{error}");
}

#[test]
fn a_deny_hit_in_a_file_name_fails() {
    let fixture = Fixture::new("deny-name");
    fixture.write("notes/Codename-Zebra/plan.txt", b"harmless\n");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("deny: name of notes/Codename-Zebra/plan.txt: \"codename-zebra\""), "{error}");
}

#[test]
fn a_deny_hit_in_a_binary_file_fails_in_ascii_and_utf16() {
    let fixture = Fixture::new("deny-binary");
    fixture.write("assets/a.bin", b"\x89PNG\0\0CODENAME-ZEBRA\0");
    let wide: Vec<u8> = "\u{feff}codename-zebra".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fixture.write("assets/b.bin", &wide);
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("deny: assets/a.bin: \"codename-zebra\" at byte 6"), "{error}");
    assert!(error.contains("deny: assets/b.bin: \"codename-zebra\" at byte 2"), "{error}");
}

#[test]
fn a_broken_link_anchor_or_reference_fails() {
    let fixture = Fixture::new("links");
    fixture.write(
        "docs/broken.md",
        b"# Broken\n\nA [missing file](nowhere.md), a [missing heading](guide.md#nowhere),\n[outside](../../x.md), and [fine](guide.md#getting-started).\n\n[ref]: internal/plan.md\n<a href=\"missing.html\">x</a>\n",
    );
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    for expected in [
        "link: docs/broken.md:3: nowhere.md does not exist in the revision",
        "link: docs/broken.md:3: guide.md#nowhere has no heading #nowhere",
        "link: docs/broken.md:4: ../../x.md leaves the repository",
        "link: docs/broken.md:6: internal/plan.md points at an excluded file and can't be reduced to text",
        "link: docs/broken.md:7: missing.html does not exist in the revision",
    ] {
        assert!(error.contains(expected), "{expected:?} missing from\n{error}");
    }
    assert!(!error.contains("getting-started"), "{error}");
}

#[test]
fn an_oversized_file_fails_unless_its_path_is_allowed() {
    let fixture = Fixture::new("size");
    fixture.write("assets/large.bin", &[7u8; 5000]);
    let rev = fixture.commit();
    fixture.export(&rev, &[]).0.unwrap();
    fixture.write("assets/huge.bin", &[7u8; 4097]);
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("size: assets/huge.bin is 4097 bytes, over 4096"), "{error}");
    assert!(!error.contains("assets/large.bin"), "{error}");
}

#[test]
fn a_package_that_is_not_apache_fails_unless_named_as_an_exception() {
    let fixture = Fixture::new("license");
    fixture.write("crates/two/Cargo.toml", b"[package]\nname = \"two\"\nlicense = \"MIT\"\n");
    fixture.write("crates/three/Cargo.toml", b"[package]\nname = \"three\"\n");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("license: crates/two/Cargo.toml: package two is \"MIT\", expected \"Apache-2.0\""), "{error}");
    assert!(error.contains("license: crates/three/Cargo.toml: package three has no license expression"), "{error}");
    assert!(!error.contains("crates/one"), "{error}");

    fixture.write("crates/three/Cargo.toml", b"[package]\nname = \"three\"\nlicense = { workspace = true }\n");
    let excepted = format!("{RULES}\n[[license_exception]]\npackage = \"two\"\nlicense = \"MIT\"\n");
    fixture.write("public-export.toml", excepted.as_bytes());
    let rev = fixture.commit();
    fixture.export(&rev, &[]).0.unwrap();
}

#[test]
fn a_missing_license_or_notice_fails() {
    let fixture = Fixture::new("required");
    fixture.remove("LICENSE");
    fixture.remove("NOTICE");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("missing: LICENSE must be in the export") && error.contains("missing: NOTICE must be in the export"), "{error}");

    // Excluding it counts as missing too.
    let fixture = Fixture::new("required-excluded");
    fixture.write("public-export.toml", RULES.replace("\"AGENTS.md\",", "\"AGENTS.md\", \"LICENSE\",").as_bytes());
    let rev = fixture.commit();
    assert!(fixture.refused(&rev, &[]).contains("missing: LICENSE"));
}

#[test]
fn a_revision_without_the_config_fails_unless_one_is_given() {
    let fixture = Fixture::new("no-config");
    fixture.remove("public-export.toml");
    let rev = fixture.commit();
    assert!(fixture.refused(&rev, &[]).contains("the revision has no public-export.toml"));
    let config = fixture.root.join("rules.toml");
    std::fs::write(&config, RULES).unwrap();
    let (result, out) = fixture.export(&rev, &["--config", config.to_str().unwrap()]);
    result.unwrap();
    assert!(text(&out, "manifest.tsv").contains("(--config file)"));
}

#[test]
fn two_runs_of_one_revision_write_the_same_bytes() {
    let fixture = Fixture::new("twice");
    let rev = fixture.commit();
    let (first, a) = fixture.export(&rev, &[]);
    let (second, b) = fixture.export("HEAD", &[]);
    first.unwrap();
    second.unwrap();
    assert_eq!(files_in(&a), files_in(&b));
    assert!(fixture.export(&rev, &["--rev", &rev]).0.unwrap_err().contains("given twice"));
}

// -- committing into the public clone ---------------------------------------------------

fn head_tree(dir: &Path) -> Vec<String> {
    fixture_git(dir, &["ls-tree", "-r", "--name-only", "HEAD"]).lines().map(str::to_owned).collect()
}

#[test]
fn commit_into_replaces_the_tracked_files_and_deletes_what_is_gone() {
    let fixture = Fixture::new("commit");
    let rev = fixture.commit();
    let public = fixture.public_clone("https://example.com/acme/widget-public.git");
    let public_arg = public.to_str().unwrap().to_owned();
    let (result, out) = fixture.export(&rev, &["--commit-into", &public_arg]);
    result.unwrap();
    let exported = files_in(&out.join("tree"));
    assert_eq!(files_in(&public), exported);
    assert_eq!(head_tree(&public), exported.keys().cloned().collect::<Vec<_>>());
    let log = fixture_git(&public, &["log", "--format=%an <%ae>|%cn <%ce>|%ad|%B", "--date=raw"]);
    assert_eq!(log, "Crossbow <crossbow@agentik-os.com>|Crossbow <crossbow@agentik-os.com>|1761998400 +0000|Crossbow snapshot of 2025-11-01");
    assert!(!log.contains(&rev[..7]) && !log.contains("main") && !log.contains("Co-Authored-By"));

    // The next revision deletes a file and a folder; the next commit drops them.
    fixture.remove("tools/run.sh");
    fixture.write("docs/added.md", b"# Added\n");
    let next = fixture.commit();
    fixture.export(&next, &["--commit-into", &public_arg]).0.unwrap();
    let tree = head_tree(&public);
    assert!(!tree.contains(&"tools/run.sh".to_owned()) && tree.contains(&"docs/added.md".to_owned()));
    assert!(!public.join("tools").exists());
    assert_eq!(fixture_git(&public, &["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(fixture_git(&public, &["status", "--porcelain", "--untracked-files=all"]), "");

    // The same revision again changes nothing and makes no commit.
    fixture.export(&next, &["--commit-into", &public_arg]).0.unwrap();
    assert_eq!(fixture_git(&public, &["rev-list", "--count", "HEAD"]), "2");
}

#[test]
fn commit_into_refuses_a_dirty_tree() {
    let fixture = Fixture::new("dirty");
    let rev = fixture.commit();
    let public = fixture.public_clone("https://example.com/acme/widget-public.git");
    std::fs::write(public.join("README.md"), "old\n").unwrap();
    fixture_git(&public, &["add", "README.md"]);
    fixture_git(&public, &["commit", "--quiet", "-m", "old"]);
    let public_arg = public.to_str().unwrap().to_owned();

    std::fs::write(public.join("README.md"), "edited\n").unwrap();
    let error = fixture.refused(&rev, &["--commit-into", &public_arg]);
    assert!(error.contains("has uncommitted changes or untracked files"), "{error}");
    assert_eq!(std::fs::read_to_string(public.join("README.md")).unwrap(), "edited\n");

    fixture_git(&public, &["checkout", "--quiet", "--", "README.md"]);
    std::fs::write(public.join("stray.txt"), "untracked\n").unwrap();
    let error = fixture.refused(&rev, &["--commit-into", &public_arg]);
    assert!(error.contains("has uncommitted changes or untracked files"), "{error}");
    assert_eq!(fixture_git(&public, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(files_in(&public).len(), 2);
}

#[test]
fn commit_into_refuses_the_private_repository() {
    let fixture = Fixture::new("private");
    let rev = fixture.commit();
    for origin in [
        "git@example.com:acme/widget.git",
        "https://example.com/ACME/widget",
        "https://example.com/acme/widget.git/",
        "ssh://git@example.com/acme/widget",
    ] {
        let public = fixture.public_clone(origin);
        let error = fixture.refused(&rev, &["--commit-into", public.to_str().unwrap()]);
        assert!(error.contains("which the config names as private"), "{origin}: {error}");
        assert!(files_in(&public).is_empty());
    }

    // A push URL counts as much as the fetch URL.
    let public = fixture.public_clone("https://example.com/acme/widget-public.git");
    fixture_git(&public, &["remote", "set-url", "--push", "origin", "git@example.com:acme/widget.git"]);
    assert!(fixture.refused(&rev, &["--commit-into", public.to_str().unwrap()]).contains("names as private"));

    // So does a remote the source repository uses, a clone of the source, and the source itself.
    fixture_git(&fixture.repo, &["remote", "add", "origin", "https://example.com/acme/other-private.git"]);
    let public = fixture.public_clone("https://example.com/acme/other-private");
    assert!(fixture.refused(&rev, &["--commit-into", public.to_str().unwrap()]).contains("which the source repository also uses"));
    let public = fixture.public_clone(fixture.repo.to_str().unwrap());
    assert!(fixture.refused(&rev, &["--commit-into", public.to_str().unwrap()]).contains("points at the source repository"));
    assert!(fixture.refused(&rev, &["--commit-into", fixture.repo.to_str().unwrap()]).contains("is the source repository"));
}

#[test]
fn a_deny_exception_covers_only_its_files_and_patterns() {
    let fixture = Fixture::new("deny-exception");
    let excepted = format!("{RULES}\n[[deny_exception]]\npaths = [\"vendor/list.txt\"]\npatterns = [\"codename-zebra\"]\n");
    fixture.write("public-export.toml", excepted.as_bytes());
    fixture.write("vendor/list.txt", b"||codename-zebra.example^\n||acme/widget^\n");
    fixture.write("vendor/other.txt", b"codename-zebra\n");
    fixture.write("vendor/codename-zebra/list.txt", b"fine\n");
    let rev = fixture.commit();
    let error = fixture.refused(&rev, &[]);
    assert!(error.contains("deny: vendor/list.txt: \"acme/widget([^a-z0-9_-]|$)\" line 2"), "{error}");
    assert!(error.contains("deny: vendor/other.txt: \"codename-zebra\" line 1"), "{error}");
    assert!(error.contains("deny: name of vendor/codename-zebra/list.txt"), "{error}");
    assert!(!error.contains("deny: vendor/list.txt: \"codename-zebra\""), "{error}");

    // Without the other hits the export passes, and the report lists the excepted one.
    fixture.write("vendor/list.txt", b"||codename-zebra.example^\n");
    fixture.remove("vendor/other.txt");
    fixture.remove("vendor/codename-zebra/list.txt");
    let rev = fixture.commit();
    let (result, out) = fixture.export(&rev, &[]);
    result.unwrap();
    assert!(text(&out, "report.txt").contains("  deny exception: vendor/list.txt: \"codename-zebra\" line 1\n"));

    let unknown = format!("{RULES}\n[[deny_exception]]\npaths = [\"x\"]\npatterns = [\"nope\"]\n");
    assert!(Config::parse(&unknown).unwrap_err().contains("names \"nope\", which is not in [deny]"));
}
