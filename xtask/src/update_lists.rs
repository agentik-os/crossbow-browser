//! `cargo xtask update-lists [--from URL]`: replaces the lists that come
//! with Crossbow, `crates/crossbow-core/lists/easylist.txt` and
//! `easyprivacy.txt`, with the current ones from easylist.to, and rewrites
//! `SOURCES` beside them. Run by hand only: nothing else in Crossbow
//! fetches a list, and tests use loopback fixtures.
//!
//! Each list is fetched over HTTPS with curl, then checked: at most 16 MB,
//! UTF-8 text without NUL, an Adblock Plus header, its own title, a version
//! no older than the bundled one, and at least three quarters of its rules.
//! Only when both pass are both written, each through a partial file renamed
//! over the old one; otherwise every file stays as it was. Then review the
//! diff, update the pinned hashes in
//! `crates/crossbow-core/src/content_blocking/bundled_tests.rs`, run the
//! tests and commit.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Each file and the title its header must give.
pub const LISTS: [(&str, &str); 2] = [("easylist.txt", "EasyList"), ("easyprivacy.txt", "EasyPrivacy")];
pub const BASE: &str = "https://easylist.to/easylist/";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const HEADER: &str = "# Bundled filter lists, copied unmodified. Licence: CC BY-SA 3.0 Unported (see LICENSE.md).\n# Written by `cargo xtask update-lists`.\n";

pub fn run(args: &[String], root: &Path) -> Result<(), String> {
    let base = match args {
        [] => BASE,
        [flag, base] if flag == "--from" => base.as_str(),
        _ => return Err("usage: cargo xtask update-lists [--from URL]".into()),
    };
    let dir = root.join("crates/crossbow-core/lists");
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs() / 86_400;
    for line in update(base, &dir, &civil(days))? { println!("{line}"); }
    println!("Review the diff, update the pinned hashes in crates/crossbow-core/src/content_blocking/bundled_tests.rs, run the tests, then commit.");
    Ok(())
}

/// Fetches both lists from `base`, checks them against the files in `dir`,
/// and writes both and `SOURCES` only when both pass. A line per list.
pub fn update(base: &str, dir: &Path, today: &str) -> Result<Vec<String>, String> {
    let mut fetched = Vec::new();
    for (file, title) in LISTS {
        let url = format!("{base}{file}");
        let bytes = fetch(&url).map_err(|e| format!("{file}: couldn't fetch it ({e})"))?;
        let current = fs::read_to_string(dir.join(file)).ok();
        let version = check(&bytes, title, current.as_deref()).map_err(|e| format!("{file} {e}"))?;
        fetched.push((file, url, bytes, version, current));
    }
    let names: Vec<&str> = fetched.iter().map(|(file, ..)| *file).chain(["SOURCES"]).collect();
    let written = (|| {
        let mut sources = String::from(HEADER);
        let mut summary = Vec::new();
        for (file, url, bytes, version, current) in &fetched {
            write_partial(dir, file, bytes)?;
            let sha256 = shasum(&partial(dir, file))?;
            sources.push_str(&format!("{file} version={version} fetched={today} bytes={} sha256={sha256} source={url}\n", bytes.len()));
            let was = current.as_deref().and_then(version_of).unwrap_or("none");
            summary.push(format!("{file}: version {version} (was {was}), {} bytes, {} rules", bytes.len(), rules(std::str::from_utf8(bytes).unwrap_or_default())));
        }
        write_partial(dir, "SOURCES", sources.as_bytes())?;
        // Renamed only once every file is written.
        for name in &names {
            fs::rename(partial(dir, name), dir.join(name)).map_err(|e| format!("couldn't write {name}: {e}"))?;
        }
        Ok(summary)
    })();
    if written.is_err() {
        for name in &names { let _ = fs::remove_file(partial(dir, name)); }
    }
    written
}

/// `url`'s body. Only HTTPS, or plain HTTP to 127.0.0.1 for test fixtures.
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let loopback = url.starts_with("http://127.0.0.1:");
    if !url.starts_with("https://") && !loopback { return Err("only https:// addresses are fetched".into()); }
    let proto = if loopback { "=http" } else { "=https" };
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location", "--max-time", "120", "--max-filesize", &MAX_BYTES.to_string()])
        .args(["--proto", proto, "--proto-redir", proto])
        .args(if loopback { &["--noproxy", "*"][..] } else { &[][..] })
        .arg(url)
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !output.status.success() { return Err(format!("curl {}", output.status)); }
    Ok(output.stdout)
}

/// The list's version when it may replace `current`, or why not.
fn check(bytes: &[u8], title: &str, current: Option<&str>) -> Result<String, String> {
    if bytes.len() > MAX_BYTES { return Err("is larger than 16 MB".into()); }
    if bytes.contains(&0) { return Err("isn't text: it has a NUL byte".into()); }
    let text = std::str::from_utf8(bytes).map_err(|_| "isn't text: it isn't UTF-8".to_owned())?;
    if !text.starts_with("[Adblock Plus") { return Err("isn't an Adblock Plus list: its first line isn't the header".into()); }
    if !text.lines().any(|line| line == format!("! Title: {title}")) { return Err(format!("isn't {title}: its title line differs")); }
    let version = version_of(text).ok_or("has no version line")?;
    if let Some(current) = current {
        if let Some(old) = version_of(current) && version < old { return Err(format!("is older than the bundled one ({version} < {old})")); }
        let (new, old) = (rules(text), rules(current));
        if new * 4 < old * 3 { return Err(format!("has far fewer rules than the bundled one ({new} < 75% of {old})")); }
    }
    Ok(version.to_owned())
}

/// `! Version: 202611010524`: digits only, so versions compare as text.
fn version_of(text: &str) -> Option<&str> {
    text.lines().find_map(|line| line.strip_prefix("! Version: ")).filter(|version| !version.is_empty() && version.bytes().all(|c| c.is_ascii_digit()))
}

fn rules(text: &str) -> usize {
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('!') && !line.starts_with('[')).count()
}

fn partial(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(".{name}.partial"))
}

fn write_partial(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    fs::write(partial(dir, name), bytes).map_err(|e| format!("couldn't write {name}: {e}"))
}

fn shasum(path: &Path) -> Result<String, String> {
    let output = Command::new("shasum").args(["-a", "256"]).arg(path).output().map_err(|e| format!("couldn't run shasum: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let sum = text.split_whitespace().next().filter(|sum| output.status.success() && sum.len() == 64).ok_or("shasum gave no SHA-256")?;
    Ok(sum.to_owned())
}

/// Days since 1970-01-01 as `YYYY-MM-DD`, in UTC.
pub fn civil(days: u64) -> String {
    // Howard Hinnant's days-to-civil, from 0000-03-01.
    let z = days + 719_468;
    let (era, day_of_era) = (z / 146_097, z % 146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
#[path = "update_lists_tests.rs"]
mod tests;
