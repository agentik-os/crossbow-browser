//! `cargo xtask public-export --rev REV --out DIR [--commit-into TREE]
//! [--config FILE]`: a sanitized snapshot of one revision for the public
//! repository.
//!
//! It reads exactly the tree of REV from git's objects, never the working
//! tree, and applies the rules in that tree's `public-export.toml` (or in
//! FILE, for a revision that has none): excluded paths, literal rewrites in
//! text files, and Markdown links to excluded files reduced to their text.
//! Then, before anything is written, it checks that:
//!
//! - no deny pattern matches any exported path or content, binary files
//!   included (deny strings are also looked for as UTF-16 there), except
//!   where a `[[deny_exception]]` names the file and the pattern (for
//!   third-party files that must stay unmodified; report.txt lists those
//!   hits);
//! - every relative Markdown link, reference definition, `href`/`src` and
//!   anchor resolves;
//! - no file is over the size limit unless the config allows its path;
//! - every Cargo package's license is the expected one or a named exception;
//! - LICENSE and NOTICE are present.
//!
//! Every problem is listed, and nothing is written if there is one. DIR gets
//! `tree/` (the export), `manifest.tsv` (path, SHA-256, size, mode, rewrites
//! applied, links dropped) and `report.txt` (what was excluded, rewritten
//! and dropped). Two runs on the same revision write the same bytes.
//!
//! With `--commit-into`, TREE (a clean clone of the public repository) gets
//! the export as its tracked files, with files that are gone deleted, and
//! one commit by `Crossbow <crossbow@agentik-os.com>`, dated like REV, with
//! the message "Crossbow snapshot of DATE" and nothing else. It never
//! pushes. It refuses a TREE that is the source repository, shares a remote
//! with it, or has a remote the config names as private.

#[path = "public_export_markdown.rs"]
mod markdown;
#[path = "public_export_rules.rs"]
mod rules;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rules::{Glob, Regex, Table, Value};

const USAGE: &str = "usage: cargo xtask public-export --rev REV --out DIR [--commit-into TREE] [--config FILE]";
const CONFIG: &str = "public-export.toml";
const AUTHOR_NAME: &str = "Crossbow";
const AUTHOR_EMAIL: &str = "crossbow@agentik-os.com";
const REQUIRED: [&str; 2] = ["LICENSE", "NOTICE"];

pub fn run(args: &[String], repo: &Path) -> Result<(), String> {
    let mut flags: HashMap<&str, &str> = HashMap::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        if !matches!(flag.as_str(), "--rev" | "--out" | "--commit-into" | "--config") {
            return Err(format!("unknown argument {flag:?}\n{USAGE}"));
        }
        let value = rest.next().ok_or(format!("{flag} needs a value"))?;
        if flags.insert(flag, value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let (Some(rev), Some(out)) = (flags.get("--rev"), flags.get("--out")) else { return Err(USAGE.into()) };
    let out = Path::new(out);
    check_out(out)?;
    let export = build(repo, rev, flags.get("--config").map(Path::new))?;
    let tree = match flags.get("--commit-into") {
        Some(tree) => Some(check_commit_target(repo, Path::new(tree), out, &export.config)?),
        None => None,
    };
    write_out(&export, out)?;
    let bytes: usize = export.files.iter().map(|f| f.bytes.len()).sum();
    let rewritten: usize = export.files.iter().flat_map(|f| &f.rewrites).map(|(_, n)| n).sum();
    println!(
        "public-export: {} files ({bytes} bytes) from {}; {} paths excluded, {rewritten} rewrites, {} links dropped",
        export.files.len(),
        &export.rev[..12],
        export.excluded.len(),
        export.dropped.len(),
    );
    println!("wrote {}: tree/, manifest.tsv, report.txt", out.display());
    if let Some(tree) = tree {
        commit(&export, &tree)?;
    }
    Ok(())
}

// -- config ------------------------------------------------------------------------

#[derive(Debug)]
struct Config {
    exclude: Vec<Glob>,
    rewrites: Vec<Rewrite>,
    deny: Vec<Deny>,
    deny_exceptions: Vec<DenyException>,
    unlink_excluded: bool,
    max_bytes: u64,
    allow_large: Vec<Glob>,
    license: String,
    license_exceptions: Vec<(String, String)>,
    private_remotes: Vec<String>,
}

#[derive(Debug)]
struct Rewrite {
    from: String,
    to: String,
    /// Only where no name character (`A-Z a-z 0-9 _ -`) touches either end.
    whole: bool,
}

#[derive(Debug)]
struct Deny {
    text: String,
    pattern: Pattern,
}

/// Deny patterns that don't apply to some paths: for third-party files that
/// must stay unmodified, where a pattern hits something unrelated.
#[derive(Debug)]
struct DenyException {
    paths: Vec<Glob>,
    patterns: Vec<String>,
}

#[derive(Debug)]
enum Pattern {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl Config {
    fn parse(text: &str) -> Result<Config, String> {
        let mut config = Config {
            exclude: Vec::new(),
            rewrites: Vec::new(),
            deny: Vec::new(),
            deny_exceptions: Vec::new(),
            unlink_excluded: true,
            max_bytes: 1_000_000,
            allow_large: Vec::new(),
            license: "Apache-2.0".into(),
            license_exceptions: Vec::new(),
            private_remotes: Vec::new(),
        };
        for table in rules::parse_toml(text)? {
            let at = |e: String| format!("[{}] at line {}: {e}", table.name, table.line);
            match (table.name.as_str(), table.array) {
                ("exclude", false) => {
                    allow_keys(&table, &["paths"]).map_err(at)?;
                    for path in list(&table, "paths").map_err(at)? {
                        config.exclude.push(Glob::new(&path).map_err(at)?);
                    }
                }
                ("rewrite", true) => {
                    allow_keys(&table, &["from", "to", "whole"]).map_err(at)?;
                    let from = string(&table, "from").map_err(at)?.filter(|s| !s.is_empty()).ok_or_else(|| at("needs a non-empty `from`".into()))?;
                    let to = string(&table, "to").map_err(at)?.ok_or_else(|| at("needs `to`".into()))?;
                    let whole = match get(&table, "whole") {
                        None => false,
                        Some(Value::Bool(b)) => *b,
                        Some(_) => return Err(at("`whole` must be true or false".into())),
                    };
                    config.rewrites.push(Rewrite { from, to, whole });
                }
                ("deny", false) => {
                    allow_keys(&table, &["strings", "regexes"]).map_err(at)?;
                    for text in list(&table, "strings").map_err(at)? {
                        if text.is_empty() {
                            return Err(at("deny strings can't be empty".into()));
                        }
                        config.deny.push(Deny { pattern: Pattern::Literal(text.to_ascii_lowercase().into_bytes()), text });
                    }
                    for text in list(&table, "regexes").map_err(at)? {
                        config.deny.push(Deny { pattern: Pattern::Regex(Regex::new(&text).map_err(at)?), text });
                    }
                }
                ("deny_exception", true) => {
                    allow_keys(&table, &["paths", "patterns"]).map_err(at)?;
                    let paths = list(&table, "paths").map_err(at)?.iter().map(|p| Glob::new(p)).collect::<Result<Vec<_>, _>>().map_err(at)?;
                    let patterns = list(&table, "patterns").map_err(at)?;
                    if paths.is_empty() || patterns.is_empty() {
                        return Err(at("needs `paths` and `patterns`".into()));
                    }
                    config.deny_exceptions.push(DenyException { paths, patterns });
                }
                ("markdown_links", false) => {
                    allow_keys(&table, &["excluded"]).map_err(at)?;
                    config.unlink_excluded = match string(&table, "excluded").map_err(at)?.as_deref() {
                        None | Some("unlink") => true,
                        Some("fail") => false,
                        Some(other) => return Err(at(format!("`excluded` is \"unlink\" or \"fail\", not {other:?}"))),
                    };
                }
                ("size", false) => {
                    allow_keys(&table, &["max_bytes", "allow"]).map_err(at)?;
                    match get(&table, "max_bytes") {
                        None => {}
                        Some(Value::Int(n)) if *n > 0 => config.max_bytes = *n as u64,
                        Some(_) => return Err(at("`max_bytes` must be a positive integer".into())),
                    }
                    for path in list(&table, "allow").map_err(at)? {
                        config.allow_large.push(Glob::new(&path).map_err(at)?);
                    }
                }
                ("license", false) => {
                    allow_keys(&table, &["expected"]).map_err(at)?;
                    if let Some(expected) = string(&table, "expected").map_err(at)? {
                        config.license = expected;
                    }
                }
                ("license_exception", true) => {
                    allow_keys(&table, &["package", "license"]).map_err(at)?;
                    let package = string(&table, "package").map_err(at)?.ok_or_else(|| at("needs `package`".into()))?;
                    let license = string(&table, "license").map_err(at)?.ok_or_else(|| at("needs `license`".into()))?;
                    config.license_exceptions.push((package, license));
                }
                ("commit_into", false) => {
                    allow_keys(&table, &["private_remotes"]).map_err(at)?;
                    config.private_remotes = list(&table, "private_remotes").map_err(at)?.iter().map(|r| remote_slug(r)).collect();
                }
                _ => return Err(format!("line {}: unknown table [{}]", table.line, table.name)),
            }
        }
        for pattern in config.deny_exceptions.iter().flat_map(|e| &e.patterns) {
            if !config.deny.iter().any(|d| d.text == *pattern) {
                return Err(format!("[[deny_exception]] names {pattern:?}, which is not in [deny]"));
            }
        }
        if config.deny.is_empty() {
            return Err("[deny] must list at least one string or regex".into());
        }
        if config.private_remotes.iter().all(|r| r.is_empty()) {
            return Err("[commit_into] private_remotes must name the private repository".into());
        }
        Ok(config)
    }
}

fn allow_keys(table: &Table, allowed: &[&str]) -> Result<(), String> {
    match table.keys.iter().find(|(key, _, _)| !allowed.contains(&key.as_str())) {
        Some((key, _, line)) => Err(format!("unknown key `{key}` on line {line}")),
        None => Ok(()),
    }
}

fn get<'a>(table: &'a Table, key: &str) -> Option<&'a Value> {
    table.keys.iter().find(|(k, _, _)| k == key).map(|(_, value, _)| value)
}

fn string(table: &Table, key: &str) -> Result<Option<String>, String> {
    match get(table, key) {
        None => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn list(table: &Table, key: &str) -> Result<Vec<String>, String> {
    match get(table, key) {
        None => Ok(Vec::new()),
        Some(Value::List(items)) => Ok(items.clone()),
        Some(_) => Err(format!("`{key}` must be an array of strings")),
    }
}

// -- building the export -------------------------------------------------------------

#[derive(Debug)]
struct Export {
    /// The full commit id. It goes in the local manifest and report, never
    /// in the public tree or commit.
    rev: String,
    /// The commit's committer time, Unix seconds.
    time: i64,
    config: Config,
    config_sha256: String,
    config_source: &'static str,
    files: Vec<File>,
    /// Path, and the pattern that excluded it.
    excluded: Vec<(String, String)>,
    dropped: Vec<Dropped>,
    notes: Vec<String>,
}

#[derive(Debug)]
struct File {
    path: String,
    bytes: Vec<u8>,
    executable: bool,
    /// (rewrite index, replacements).
    rewrites: Vec<(usize, usize)>,
    dropped: usize,
}

#[derive(Debug)]
struct Dropped {
    file: String,
    line: usize,
    target: String,
    image: bool,
    /// The target is under an excluded path but wasn't in the revision either.
    missing: bool,
}

struct Entry {
    mode: String,
    kind: String,
    id: String,
    path: String,
}

fn build(repo: &Path, rev: &str, config_file: Option<&Path>) -> Result<Export, String> {
    if rev.starts_with('-') {
        return Err("--rev can't start with `-`".into());
    }
    let commit = git_text(git(repo).args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]))
        .map_err(|_| format!("{rev:?} is not a commit in {}", repo.display()))?;
    let time = git_text(git(repo).args(["show", "-s", "--format=%ct", &commit]))?
        .parse::<i64>()
        .map_err(|_| "the commit has no readable date")?;
    let entries = list_tree(repo, &commit)?;

    let (config_text, config_source) = match config_file {
        Some(file) => (std::fs::read(file).map_err(|e| format!("--config {}: {e}", file.display()))?, "--config file"),
        None => {
            let entry = entries
                .iter()
                .find(|e| e.path == CONFIG)
                .ok_or(format!("the revision has no {CONFIG}; add one or pass --config FILE"))?;
            (read_blobs(repo, &[entry.id.as_str()])?.remove(0), "the revision's public-export.toml")
        }
    };
    let config_sha256 = hex(&sha256(&config_text));
    let config_text = String::from_utf8(config_text).map_err(|_| format!("{CONFIG} is not UTF-8"))?;
    let config = Config::parse(&config_text).map_err(|e| format!("{CONFIG}: {e}"))?;

    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let mut excluded = Vec::new();
    let mut kept = Vec::new();
    for entry in &entries {
        if entry.path == CONFIG {
            excluded.push((entry.path.clone(), CONFIG.to_owned()));
        } else if let Some(glob) = config.exclude.iter().find(|g| g.matches(&entry.path)) {
            excluded.push((entry.path.clone(), glob.text.clone()));
        } else if entry.kind != "blob" || !matches!(entry.mode.as_str(), "100644" | "100755") {
            problems.push(format!("kind: {} is a {} (mode {}); only regular files are exported", entry.path, entry.kind, entry.mode));
        } else {
            kept.push(entry);
        }
    }
    for glob in &config.exclude {
        if !entries.iter().any(|e| glob.matches(&e.path)) {
            notes.push(format!("exclude pattern {:?} matched nothing", glob.text));
        }
    }

    let ids: Vec<&str> = kept.iter().map(|e| e.id.as_str()).collect();
    let blobs = read_blobs(repo, &ids)?;
    let mut files: Vec<File> = kept
        .iter()
        .zip(blobs)
        .map(|(entry, bytes)| File { path: entry.path.clone(), bytes, executable: entry.mode == "100755", rewrites: Vec::new(), dropped: 0 })
        .collect();

    // Rewrites, in config order, on text files only.
    for file in &mut files {
        let Some(mut text) = as_text(&file.bytes).map(str::to_owned) else { continue };
        for (index, rule) in config.rewrites.iter().enumerate() {
            let (next, count) = rewrite(&text, rule);
            if count > 0 {
                file.rewrites.push((index + 1, count));
                text = next;
            }
        }
        if !file.rewrites.is_empty() {
            file.bytes = text.into_bytes();
        }
    }
    for (index, rule) in config.rewrites.iter().enumerate() {
        if !files.iter().any(|f| f.rewrites.iter().any(|(i, _)| *i == index + 1)) {
            notes.push(format!("rewrite #{} ({:?}) applied nowhere", index + 1, rule.from));
        }
    }

    // Markdown links.
    let paths = Paths::new(&entries, &files, &config.exclude);
    let mut dropped = Vec::new();
    for file in &mut files {
        if !is_markdown(&file.path) {
            continue;
        }
        let Some(text) = as_text(&file.bytes) else { continue };
        let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for link in markdown::links(text) {
            let place = format!("{}:{}", file.path, link.line);
            match paths.resolve(&file.path, &link.target) {
                Resolved::Fine => {}
                Resolved::Excluded { missing } if config.unlink_excluded => {
                    if edits.iter().any(|(span, _)| span.end > link.span.start) {
                        problems.push(format!("link: {place}: a link to an excluded file sits inside another one; fix it in the source"));
                    }
                    edits.push((link.span.clone(), text[link.text.clone()].to_owned()));
                    dropped.push(Dropped { file: file.path.clone(), line: link.line, target: link.target.clone(), image: link.image, missing });
                }
                Resolved::Excluded { .. } => problems.push(format!("link: {place}: {} points at an excluded file", link.target)),
                Resolved::Broken(why) => problems.push(format!("link: {place}: {} {why}", link.target)),
            }
        }
        for reference in markdown::references(text) {
            let place = format!("{}:{}", file.path, reference.line);
            match paths.resolve(&file.path, &reference.target) {
                Resolved::Fine => {}
                Resolved::Excluded { .. } => problems.push(format!("link: {place}: {} points at an excluded file and can't be reduced to text", reference.target)),
                Resolved::Broken(why) => problems.push(format!("link: {place}: {} {why}", reference.target)),
            }
        }
        if !edits.is_empty() {
            let mut out = text.to_owned();
            for (span, replacement) in edits.iter().rev() {
                out.replace_range(span.clone(), replacement);
            }
            file.dropped = edits.len();
            file.bytes = out.into_bytes();
        }
    }

    check(&config, &files, &mut problems, &mut notes);
    if !problems.is_empty() {
        problems.sort();
        problems.dedup();
        return Err(format!("public export refused: {} problem(s); nothing was written\n{}", problems.len(), problems.join("\n")));
    }
    Ok(Export { rev: commit, time, config, config_sha256, config_source, files, excluded, dropped, notes })
}

fn list_tree(repo: &Path, commit: &str) -> Result<Vec<Entry>, String> {
    let raw = git_bytes(git(repo).args(["ls-tree", "-r", "-z", "--full-tree", commit]))?;
    let mut entries = Vec::new();
    for record in raw.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let tab = record.iter().position(|&b| b == b'\t').ok_or("unexpected `git ls-tree` output")?;
        let meta = String::from_utf8_lossy(&record[..tab]);
        let path = String::from_utf8(record[tab + 1..].to_vec()).map_err(|_| "a path in the revision is not UTF-8")?;
        let mut parts = meta.split(' ').map(str::to_owned);
        let (Some(mode), Some(kind), Some(id)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("unexpected `git ls-tree` output".into());
        };
        entries.push(Entry { mode, kind, id, path });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Blob contents through one `git cat-file --batch`.
fn read_blobs(repo: &Path, ids: &[&str]) -> Result<Vec<Vec<u8>>, String> {
    let mut child = git(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git cat-file: {e}"))?;
    let input: Vec<u8> = ids.iter().flat_map(|id| format!("{id}\n").into_bytes()).collect();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let mut raw = Vec::new();
    child.stdout.take().expect("piped stdout").read_to_end(&mut raw).map_err(|e| format!("git cat-file: {e}"))?;
    let wrote = writer.join().map_err(|_| "git cat-file: writer panicked")?;
    let status = child.wait().map_err(|e| format!("git cat-file: {e}"))?;
    if wrote.is_err() || !status.success() {
        return Err(format!("git cat-file failed ({status})"));
    }
    let mut blobs = Vec::with_capacity(ids.len());
    let mut at = 0;
    for id in ids {
        let bad = || format!("git cat-file: unexpected output for {id}");
        let header_end = at + raw[at..].iter().position(|&b| b == b'\n').ok_or_else(bad)?;
        let header = String::from_utf8_lossy(&raw[at..header_end]).into_owned();
        let mut parts = header.split(' ');
        if parts.next() != Some(*id) || parts.next() != Some("blob") {
            return Err(bad());
        }
        let size: usize = parts.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
        let (start, end) = (header_end + 1, header_end + 1 + size);
        if raw.get(end) != Some(&b'\n') {
            return Err(bad());
        }
        blobs.push(raw[start..end].to_vec());
        at = end + 1;
    }
    Ok(blobs)
}

/// Text means UTF-8 without NUL; anything else is binary and never rewritten.
fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) { None } else { std::str::from_utf8(bytes).ok() }
}

fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn rewrite(text: &str, rule: &Rewrite) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut rest = text;
    while let Some(at) = rest.find(&rule.from) {
        let end = at + rule.from.len();
        let before = if at > 0 { rest.as_bytes().get(at - 1) } else { out.as_bytes().last() };
        let touches = before.is_some_and(|&b| is_name_byte(b)) || rest.as_bytes().get(end).is_some_and(|&b| is_name_byte(b));
        if rule.whole && touches {
            out.push_str(&rest[..end]);
        } else {
            out.push_str(&rest[..at]);
            out.push_str(&rule.to);
            count += 1;
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    (out, count)
}

// -- links ------------------------------------------------------------------------------

enum Resolved {
    Fine,
    /// An excluded file or folder, or a path under an exclude pattern that
    /// isn't in the revision (such as never-committed evidence).
    Excluded { missing: bool },
    Broken(String),
}

/// What exists in the revision and what the export keeps, for resolving links.
struct Paths {
    kept: HashSet<String>,
    kept_dirs: HashSet<String>,
    all: HashSet<String>,
    all_dirs: HashSet<String>,
    anchors: HashMap<String, HashSet<String>>,
    exclude: Vec<Glob>,
}

impl Paths {
    fn new(entries: &[Entry], files: &[File], exclude: &[Glob]) -> Paths {
        let dirs = |paths: &mut dyn Iterator<Item = &str>| {
            let mut dirs = HashSet::new();
            for path in paths {
                let mut at = path;
                while let Some((parent, _)) = at.rsplit_once('/') {
                    dirs.insert(parent.to_owned());
                    at = parent;
                }
            }
            dirs
        };
        Paths {
            kept: files.iter().map(|f| f.path.clone()).collect(),
            kept_dirs: dirs(&mut files.iter().map(|f| f.path.as_str())),
            all: entries.iter().map(|e| e.path.clone()).collect(),
            all_dirs: dirs(&mut entries.iter().map(|e| e.path.as_str())),
            anchors: files
                .iter()
                .filter(|f| is_markdown(&f.path))
                .filter_map(|f| Some((f.path.clone(), markdown::anchors(as_text(&f.bytes)?))))
                .collect(),
            exclude: exclude.to_vec(),
        }
    }

    fn resolve(&self, from: &str, target: &str) -> Resolved {
        let target = target.trim();
        let scheme = target.split_once(':').is_some_and(|(s, _)| {
            s.starts_with(|c: char| c.is_ascii_alphabetic()) && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        });
        if scheme || target.starts_with("//") {
            return Resolved::Fine;
        }
        let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
        let path = path.split('?').next().unwrap_or("");
        let Some(path) = percent_decode(path) else { return Resolved::Broken("is not a valid path".into()) };
        let joined = if path.is_empty() {
            from.to_owned()
        } else if let Some(rooted) = path.strip_prefix('/') {
            rooted.to_owned()
        } else {
            match from.rsplit_once('/') {
                Some((dir, _)) => format!("{dir}/{path}"),
                None => path,
            }
        };
        let Some(path) = normalize(&joined) else { return Resolved::Broken("leaves the repository".into()) };
        if self.kept.contains(&path) {
            if fragment.is_empty() || !is_markdown(&path) {
                return Resolved::Fine;
            }
            let fragment = percent_decode(fragment).unwrap_or_default().to_lowercase();
            return match self.anchors.get(&path) {
                Some(anchors) if anchors.contains(&fragment) => Resolved::Fine,
                _ => Resolved::Broken(format!("has no heading #{fragment}")),
            };
        }
        if path.is_empty() || self.kept_dirs.contains(&path) {
            Resolved::Fine
        } else if self.all.contains(&path) || self.all_dirs.contains(&path) {
            Resolved::Excluded { missing: false }
        } else if self.exclude.iter().any(|g| g.matches(&path)) {
            Resolved::Excluded { missing: true }
        } else {
            Resolved::Broken("does not exist in the revision".into())
        }
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `a/./b/../c` → `a/c`; `None` if it climbs above the root.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

// -- checks ------------------------------------------------------------------------------

fn check(config: &Config, files: &[File], problems: &mut Vec<String>, notes: &mut Vec<String>) {
    for file in files {
        let binary = as_text(&file.bytes).is_none();
        deny_scan(&format!("name of {}", file.path), file.path.as_bytes(), false, &config.deny, &[], problems, notes);
        let excepted: Vec<&str> = config
            .deny_exceptions
            .iter()
            .filter(|e| e.paths.iter().any(|g| g.matches(&file.path)))
            .flat_map(|e| e.patterns.iter().map(String::as_str))
            .collect();
        deny_scan(&file.path, &file.bytes, binary, &config.deny, &excepted, problems, notes);
        let size = file.bytes.len() as u64;
        if size > config.max_bytes && !config.allow_large.iter().any(|g| g.matches(&file.path)) {
            problems.push(format!("size: {} is {size} bytes, over {}; allow its path under [size] or leave it out", file.path, config.max_bytes));
        }
    }
    for exception in &config.deny_exceptions {
        for glob in &exception.paths {
            if !files.iter().any(|f| glob.matches(&f.path)) {
                notes.push(format!("deny exception path {:?} matched nothing", glob.text));
            }
        }
    }
    for glob in &config.allow_large {
        if !files.iter().any(|f| glob.matches(&f.path)) {
            notes.push(format!("size allowance {:?} matched nothing", glob.text));
        }
    }
    for required in REQUIRED {
        if !files.iter().any(|f| f.path == required) {
            problems.push(format!("missing: {required} must be in the export"));
        }
    }
    check_licenses(config, files, problems, notes);
}

/// Every hit of every deny pattern in one path or one file's bytes, by line
/// (text) or byte offset (binary). Hits of `excepted` patterns become notes.
fn deny_scan(what: &str, bytes: &[u8], binary: bool, deny: &[Deny], excepted: &[&str], problems: &mut Vec<String>, notes: &mut Vec<String>) {
    let lower = bytes.to_ascii_lowercase();
    for rule in deny {
        let mut hits = Vec::new();
        match &rule.pattern {
            Pattern::Literal(needle) => {
                hits.extend(find_all(&lower, needle));
                if binary {
                    let text = String::from_utf8_lossy(needle);
                    let le: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
                    let be: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
                    hits.extend(find_all(&lower, &le));
                    hits.extend(find_all(&lower, &be));
                }
            }
            Pattern::Regex(regex) => {
                let mut from = 0;
                while from <= lower.len() {
                    match regex.find_from(&lower, from) {
                        Ok(Some(at)) => {
                            hits.push(at);
                            from = at + 1 + lower[at..].iter().position(|&b| b == b'\n').unwrap_or(lower.len() - at);
                        }
                        Ok(None) => break,
                        Err(()) => {
                            problems.push(format!("deny: {what}: regex {:?} gave up; simplify it", rule.text));
                            break;
                        }
                    }
                }
            }
        }
        if hits.is_empty() {
            continue;
        }
        hits.sort_unstable();
        let places: Vec<String> = if binary {
            hits.dedup();
            hits.iter().map(|at| format!("byte {at}")).collect()
        } else {
            let (mut line, mut counted) = (1, 0);
            let mut lines: Vec<usize> = hits
                .iter()
                .map(|&at| {
                    line += lower[counted..at].iter().filter(|&&b| b == b'\n').count();
                    counted = at;
                    line
                })
                .collect();
            lines.dedup();
            lines.iter().map(usize::to_string).collect()
        };
        let shown = places.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        let more = if places.len() > 8 { format!(" (+{} more)", places.len() - 8) } else { String::new() };
        let unit = if binary { "at" } else { "line" };
        if excepted.contains(&rule.text.as_str()) {
            notes.push(format!("deny exception: {what}: {:?} {unit} {shown}{more}", rule.text));
        } else {
            problems.push(format!("deny: {what}: {:?} {unit} {shown}{more}", rule.text));
        }
    }
}

fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let Some(&first) = needle.first() else { return found };
    let mut i = 0;
    while let Some(p) = haystack.get(i..).and_then(|rest| rest.iter().position(|&b| b == first)) {
        let at = i + p;
        if haystack[at..].starts_with(needle) {
            found.push(at);
        }
        i = at + 1;
    }
    found
}

#[derive(Debug, PartialEq)]
enum License {
    Value(String),
    Workspace,
    File,
}

#[derive(Debug, Default)]
struct Manifest {
    package: Option<String>,
    license: Option<License>,
    workspace: bool,
    workspace_license: Option<String>,
}

/// The license fields of a Cargo.toml, read line by line.
fn cargo_manifest(text: &str) -> Manifest {
    let mut manifest = Manifest::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_start_matches('[').split(']').next().unwrap_or("").trim().to_owned();
            manifest.workspace |= section == "workspace";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.split(" #").next().unwrap_or("").trim());
        let unquoted = value.trim_matches('"').to_owned();
        match (section.as_str(), key) {
            ("package", "name") => manifest.package = Some(unquoted),
            ("package", "license") if value.replace(' ', "") == "{workspace=true}" => manifest.license = Some(License::Workspace),
            ("package", "license") => manifest.license = Some(License::Value(unquoted)),
            ("package", "license.workspace") if value == "true" => manifest.license = Some(License::Workspace),
            ("package", "license-file" | "license-file.workspace") => manifest.license = Some(License::File),
            ("workspace.package", "license") => manifest.workspace_license = Some(unquoted),
            _ => {}
        }
    }
    manifest
}

fn check_licenses(config: &Config, files: &[File], problems: &mut Vec<String>, notes: &mut Vec<String>) {
    let manifests: BTreeMap<&str, Manifest> = files
        .iter()
        .filter(|f| f.path == "Cargo.toml" || f.path.ends_with("/Cargo.toml"))
        .map(|f| (f.path.as_str(), cargo_manifest(&String::from_utf8_lossy(&f.bytes))))
        .collect();
    let mut used = HashSet::new();
    for (path, manifest) in &manifests {
        let Some(package) = &manifest.package else { continue };
        let license = match &manifest.license {
            Some(License::Value(license)) => Some(license.clone()),
            Some(License::Workspace) => {
                let mut dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
                loop {
                    let candidate = if dir.is_empty() { "Cargo.toml".to_owned() } else { format!("{dir}/Cargo.toml") };
                    if let Some(root) = manifests.get(candidate.as_str()).filter(|m| m.workspace) {
                        break root.workspace_license.clone();
                    }
                    if dir.is_empty() {
                        break None;
                    }
                    dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
                }
            }
            Some(License::File) | None => None,
        };
        let expected = match config.license_exceptions.iter().find(|(name, _)| name == package) {
            Some((name, license)) => {
                used.insert(name.clone());
                license
            }
            None => &config.license,
        };
        match license {
            Some(license) if license == *expected => {}
            Some(license) => problems.push(format!("license: {path}: package {package} is {license:?}, expected {expected:?}")),
            None => problems.push(format!("license: {path}: package {package} has no license expression (expected {expected:?})")),
        }
    }
    for (package, _) in &config.license_exceptions {
        if !used.contains(package) {
            notes.push(format!("license exception for {package} matched no package"));
        }
    }
}

// -- output ------------------------------------------------------------------------------

fn staging(out: &Path) -> Result<PathBuf, String> {
    let name = out.file_name().ok_or("--out needs a folder name")?;
    Ok(out.with_file_name(format!("{}.partial", name.to_string_lossy())))
}

fn check_out(out: &Path) -> Result<(), String> {
    if out.exists() {
        let empty = std::fs::read_dir(out).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !out.is_dir() || !empty {
            return Err(format!("--out {} must be a new or empty folder", out.display()));
        }
    }
    let staging = staging(out)?;
    if staging.exists() {
        return Err(format!("{} is left from an earlier run; remove it first", staging.display()));
    }
    Ok(())
}

fn write_out(export: &Export, out: &Path) -> Result<(), String> {
    let staging = staging(out)?;
    let written = (|| -> Result<(), String> {
        let tree = staging.join("tree");
        std::fs::create_dir_all(&tree).map_err(|e| format!("{}: {e}", tree.display()))?;
        for file in &export.files {
            write_file(&tree, file)?;
        }
        let manifest = staging.join("manifest.tsv");
        std::fs::write(&manifest, manifest_text(export)).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let report = staging.join("report.txt");
        std::fs::write(&report, report_text(export)).map_err(|e| format!("{}: {e}", report.display()))
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if out.exists() {
        std::fs::remove_dir(out).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    std::fs::rename(&staging, out).map_err(|e| format!("{}: {e}", out.display()))
}

fn write_file(root: &Path, file: &File) -> Result<(), String> {
    let path = root.join(&file.path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &file.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if file.executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn manifest_text(export: &Export) -> String {
    let bytes: usize = export.files.iter().map(|f| f.bytes.len()).sum();
    let mut text = String::new();
    let _ = writeln!(text, "# Crossbow public export manifest");
    let _ = writeln!(text, "# rev {}", export.rev);
    let _ = writeln!(text, "# config sha256 {} ({})", export.config_sha256, export.config_source);
    let _ = writeln!(text, "# files {}, bytes {bytes}; rewrites are [[rewrite]] numbers with counts", export.files.len());
    let _ = writeln!(text, "path\tsha256\tbytes\tmode\trewrites\tlinks_dropped");
    for file in &export.files {
        let rewrites = if file.rewrites.is_empty() {
            "-".to_owned()
        } else {
            file.rewrites.iter().map(|(i, n)| format!("{i}:{n}")).collect::<Vec<_>>().join(",")
        };
        let mode = if file.executable { "100755" } else { "100644" };
        let _ = writeln!(text, "{}\t{}\t{}\t{mode}\t{rewrites}\t{}", file.path, hex(&sha256(&file.bytes)), file.bytes.len(), file.dropped);
    }
    text
}

fn report_text(export: &Export) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "Crossbow public export of {}", export.rev);
    let _ = writeln!(text, "Config sha256 {} ({})", export.config_sha256, export.config_source);
    let _ = writeln!(text, "\nExcluded ({}):", export.excluded.len());
    for (path, pattern) in &export.excluded {
        let _ = writeln!(text, "  {path}  [{pattern}]");
    }
    let _ = writeln!(text, "\nRewrites:");
    for (index, rule) in export.config.rewrites.iter().enumerate() {
        let uses: Vec<(&str, usize)> = export
            .files
            .iter()
            .filter_map(|f| f.rewrites.iter().find(|(i, _)| *i == index + 1).map(|(_, n)| (f.path.as_str(), *n)))
            .collect();
        let total: usize = uses.iter().map(|(_, n)| n).sum();
        let _ = writeln!(text, "  #{} {:?} -> {:?}{}: {total} in {} files", index + 1, rule.from, rule.to, if rule.whole { " (whole)" } else { "" }, uses.len());
        for (path, n) in uses {
            let _ = writeln!(text, "    {path} ({n})");
        }
    }
    let _ = writeln!(text, "\nLinks dropped, text kept ({}):", export.dropped.len());
    for drop in &export.dropped {
        let kind = if drop.image { "image" } else { "link" };
        let missing = if drop.missing { " (not in the revision either)" } else { "" };
        let _ = writeln!(text, "  {}:{} {kind} -> {}{missing}", drop.file, drop.line, drop.target);
    }
    let _ = writeln!(text, "\nNotes ({}):", export.notes.len());
    for note in &export.notes {
        let _ = writeln!(text, "  {note}");
    }
    text
}

// -- commit into the public clone ------------------------------------------------------

/// The owner/name a remote URL points at, lowercased, in any URL form.
fn remote_slug(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = if url.to_ascii_lowercase().ends_with(".git") { &url[..url.len() - 4] } else { url };
    let parts: Vec<&str> = url.split(['/', ':', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [.., owner, name] => format!("{owner}/{name}").to_ascii_lowercase(),
        [name] => name.to_ascii_lowercase(),
        [] => String::new(),
    }
}

fn remote_urls(dir: &Path) -> Result<Vec<String>, String> {
    let mut urls = Vec::new();
    for remote in git_text(git(dir).arg("remote"))?.lines() {
        for push in [false, true] {
            let mut cmd = git(dir);
            cmd.args(["remote", "get-url", "--all"]);
            if push {
                cmd.arg("--push");
            }
            urls.extend(git_text(cmd.arg(remote))?.lines().map(str::to_owned));
        }
    }
    Ok(urls)
}

fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

fn check_commit_target(repo: &Path, tree: &Path, out: &Path, config: &Config) -> Result<PathBuf, String> {
    let refuse = |why: String| Err(format!("--commit-into {}: {why}; nothing was written", tree.display()));
    let Some(dir) = canonical(tree) else { return refuse("does not exist".into()) };
    let top = git_text(git(&dir).args(["rev-parse", "--show-toplevel"])).ok().and_then(|t| canonical(Path::new(&t)));
    if top.as_deref() != Some(dir.as_path()) {
        return refuse("is not the top of a git work tree".into());
    }
    let common = |d: &Path| git_text(git(d).args(["rev-parse", "--path-format=absolute", "--git-common-dir"])).ok().and_then(|c| canonical(Path::new(&c)));
    let source_common = common(repo);
    if common(&dir).is_some() && common(&dir) == source_common {
        return refuse("is the source repository or one of its worktrees".into());
    }
    let source_top = canonical(repo);
    let source_slugs: HashSet<String> = remote_urls(repo)?.iter().map(|u| remote_slug(u)).collect();
    for url in remote_urls(&dir)? {
        let slug = remote_slug(&url);
        if config.private_remotes.contains(&slug) {
            return refuse(format!("a remote points at {slug}, which the config names as private"));
        }
        if source_slugs.contains(&slug) {
            return refuse(format!("a remote points at {slug}, which the source repository also uses"));
        }
        let local = canonical(Path::new(url.trim_start_matches("file://")));
        if local.is_some() && (local == source_top || local == source_common) {
            return refuse("a remote points at the source repository".into());
        }
    }
    let status = git_bytes(git(&dir).args(["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none"]))?;
    if !status.is_empty() {
        return refuse("has uncommitted changes or untracked files".into());
    }
    let out_parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if canonical(out_parent).is_some_and(|p| p.starts_with(&dir)) {
        return refuse("contains --out".into());
    }
    Ok(dir)
}

fn commit(export: &Export, tree: &Path) -> Result<(), String> {
    let wanted: BTreeSet<&str> = export.files.iter().map(|f| f.path.as_str()).collect();
    let tracked = git_bytes(git(tree).args(["ls-files", "-z"]))?;
    for path in tracked.split(|&b| b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path);
        if wanted.contains(path.as_ref()) {
            continue;
        }
        let full = tree.join(path.as_ref());
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", full.display())),
        }
        let mut dir = full.parent();
        while let Some(d) = dir.filter(|d| *d != tree) {
            if std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    for file in &export.files {
        write_file(tree, file)?;
    }
    git_bytes(git(tree).args(["add", "-u"]))?;
    let mut add = git(tree)
        .args(["add", "--force", "--pathspec-from-file=-", "--pathspec-file-nul"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git add: {e}"))?;
    let list: Vec<u8> = export.files.iter().flat_map(|f| f.path.bytes().chain([0])).collect();
    add.stdin.take().expect("piped stdin").write_all(&list).map_err(|e| format!("git add: {e}"))?;
    let status = add.wait().map_err(|e| format!("git add: {e}"))?;
    if !status.success() {
        return Err(format!("git add failed ({status})"));
    }
    let staged = git_bytes(git(tree).args(["ls-files", "-z"]))?;
    let staged: BTreeSet<String> = staged.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    if staged.iter().map(String::as_str).collect::<BTreeSet<_>>() != wanted {
        return Err("the staged files don't match the export; nothing was committed".into());
    }
    if git(tree).args(["diff", "--cached", "--quiet"]).status().map_err(|e| format!("git diff: {e}"))?.success() {
        println!("--commit-into: the public tree already matches this export; no commit");
        return Ok(());
    }
    let message = format!("Crossbow snapshot of {}", day(export.time));
    let date = format!("{} +0000", export.time);
    let author = format!("{AUTHOR_NAME} <{AUTHOR_EMAIL}>");
    git_bytes(
        git(tree)
            .args(["-c", &format!("user.name={AUTHOR_NAME}"), "-c", &format!("user.email={AUTHOR_EMAIL}")])
            .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false"])
            .args(["commit", "--quiet", "--no-verify", "--cleanup=verbatim", "--author", &author, "--message", &message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date),
    )?;
    let shown = git_text(git(tree).args(["log", "-1", "--format=%an <%ae>%n%cn <%ce>%n%B"]))?;
    let expected = format!("{author}\n{author}\n{message}");
    if shown != expected {
        return Err(format!("the commit in {} doesn't read back as expected; check it before publishing", tree.display()));
    }
    let id = git_text(git(tree).args(["rev-parse", "--short", "HEAD"]))?;
    println!("--commit-into: committed {id} \"{message}\" in {}; not pushed", tree.display());
    Ok(())
}

/// `YYYY-MM-DD` in UTC.
fn day(unix: i64) -> String {
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// -- git ------------------------------------------------------------------------------------

/// `git -C dir`, ignoring any repository the caller's environment points at.
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).env("GIT_LITERAL_PATHSPECS", "1");
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_NAMESPACE"] {
        cmd.env_remove(var);
    }
    cmd
}

fn git_bytes(cmd: &mut Command) -> Result<Vec<u8>, String> {
    let output = cmd.stdin(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
    if !output.status.success() {
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(output.stdout)
}

fn git_text(cmd: &mut Command) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git_bytes(cmd)?).trim_end().to_owned())
}

// -- SHA-256 -----------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
        0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
        0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
        0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
        0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0u8; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
#[path = "public_export_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "wrangler_example_tests.rs"]
mod wrangler_example_tests;
