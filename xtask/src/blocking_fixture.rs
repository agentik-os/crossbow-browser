//! `cargo xtask blocking-fixture [--port N] [--lists DIR]`: a page on this
//! Mac for trying content blocking by hand. Nothing leaves the machine.
//!
//! It writes two fixture lists to DIR (default `target/blocking-fixture`),
//! then serves `http://127.0.0.1:N/` (default port 8737) until stopped. The
//! page's "third-party" requests go to `localhost` on the same port, a
//! different site by Crossbow's site rule. In Crossbow, add the lists under
//! Settings → Privacy → Content Blocking (`fixture-ads.txt` under Block ads
//! and trackers, `fixture-cookies.txt` under Block cookie banners), turn
//! off the bundled EasyList there (its `$generichide` exceptions turn rules
//! for every page off on 127.0.0.1 and localhost), then open the page. Each
//! element on the page says what to expect:
//!
//! - Ads list on: the localhost ad script and image are blocked (the shield
//!   counts 2), the first-party image under `/fixture-ad/` loads (the rule
//!   is `$third-party`), `.fixture-ad` and `.fixture-site-ad` are hidden in
//!   the page and in the localhost frame, `.fixture-kept` stays (unhidden on
//!   127.0.0.1), and the `.fixture-ad` inside a shadow root stays: shadow
//!   DOM isn't covered.
//! - Cookie list on: the banner is hidden and `/fixture-cmp.js` is blocked
//!   (one more in the count). No answer is sent: the log shows no
//!   `consent:` line. The page stays scroll-locked and the consent-gated box
//!   keeps waiting, which is what a site that needs an answer does.
//! - Blocking off on this site (from the shield): everything loads and
//!   shows after the reload, the banner included; Accept or Reject there is
//!   the only thing that writes a `consent:` line.
//!
//! The server prints each request's path and each banner answer; `/log`
//! shows them too.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const ADS_LIST: &str = include_str!("blocking_fixture/fixture-ads.txt");
pub const COOKIES_LIST: &str = include_str!("blocking_fixture/fixture-cookies.txt");
const PORT: u16 = 8737;

/// What the server saw: a path per request, and each banner answer.
#[derive(Clone, Default)]
pub struct Log {
    lines: Arc<Mutex<Vec<String>>>,
    echo: bool,
}

impl Log {
    fn push(&self, line: String) {
        if self.echo { println!("{line}"); }
        self.lines.lock().unwrap().push(line);
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

pub fn run(args: &[String], root: &Path) -> Result<(), String> {
    let value = |flag: &str| args.iter().position(|arg| arg == flag).map(|at| args.get(at + 1).ok_or(format!("{flag} needs a value")));
    let port = match value("--port") {
        Some(port) => port?.parse::<u16>().map_err(|_| "--port needs a number".to_owned())?,
        None => PORT,
    };
    let dir = match value("--lists") {
        Some(dir) => PathBuf::from(dir?),
        None => root.join("target/blocking-fixture"),
    };
    write_lists(&dir)?;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("127.0.0.1:{port}: {e}; try --port"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    println!("Lists: {}", dir.display());
    println!("Page:  http://127.0.0.1:{port}/  (third-party requests go to localhost:{port})");
    println!("Stop with Control-C.");
    serve(listener, Log { echo: true, ..Log::default() });
    Ok(())
}

pub fn write_lists(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in [("fixture-ads.txt", ADS_LIST), ("fixture-cookies.txt", COOKIES_LIST)] {
        std::fs::write(dir.join(name), text).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(())
}

/// Answers each connection on its own thread, one request each.
pub fn serve(listener: TcpListener, log: Log) {
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or_default();
    for stream in listener.incoming().flatten() {
        let log = log.clone();
        std::thread::spawn(move || answer(stream, port, &log));
    }
}

fn answer(mut stream: TcpStream, port: u16, log: &Log) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = String::new();
    let mut reader = BufReader::new(&stream);
    if reader.read_line(&mut head).is_err() { return; }
    // The rest of the request head; a GET has no body.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 2) { line.clear(); }
    let mut words = head.split_whitespace();
    let (method, target) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, kind, body) = if method != "GET" {
        log.push(format!("{method} {path} (refused)"));
        (405, "text/plain; charset=utf-8", "GET only\n".to_owned())
    } else if path == "/consent" {
        match query.strip_prefix("answer=").filter(|answer| matches!(*answer, "accept" | "reject")) {
            Some(answer) => {
                log.push(format!("consent: {answer}"));
                (204, "", String::new())
            }
            None => {
                log.push("GET /consent (no answer)".to_owned());
                (400, "text/plain; charset=utf-8", "answer=accept or answer=reject\n".to_owned())
            }
        }
    } else {
        log.push(format!("GET {path}"));
        match route(path, port) {
            Some((kind, body)) => (200, kind, body),
            None if path == "/log" => (200, "text/plain; charset=utf-8", log.lines().iter().map(|line| format!("{line}\n")).collect()),
            None => (404, "text/plain; charset=utf-8", "Not here\n".to_owned()),
        }
    };
    let reason = match status { 200 => "OK", 204 => "No Content", 400 => "Bad Request", 404 => "Not Found", _ => "Method Not Allowed" };
    let mut response = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\nCache-Control: no-store\r\n");
    if status != 204 { response.push_str(&format!("Content-Type: {kind}\r\nContent-Length: {}\r\n", body.len())); }
    response.push_str("\r\n");
    response.push_str(&body);
    let _ = stream.write_all(response.as_bytes());
}

const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const SVG: &str = "image/svg+xml";
const TEXT: &str = "text/plain; charset=utf-8";

fn route(path: &str, port: u16) -> Option<(&'static str, String)> {
    let svg = |label: &str, fill: &str| format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"220\" height=\"40\"><rect width=\"220\" height=\"40\" fill=\"{fill}\"/><text x=\"10\" y=\"25\" font-family=\"sans-serif\" font-size=\"13\">{label}</text></svg>");
    let text = |id: &str, said: &str| format!("document.getElementById('{id}').textContent = '{said}';\n");
    Some(match path {
        "/" => (HTML, page(port)),
        "/frame.html" => (HTML, FRAME.to_owned()),
        "/first-party.js" => (JS, text("first-party", "First-party script ran. Expected: always.")),
        "/fixture-ad/ad.js" => (JS, text("third-party", "Third-party ad script ran. Expected: only with blocking off.")),
        "/fixture-ad/banner.svg" => (SVG, svg("Third-party ad image", "#f4c7c3")),
        "/fixture-ad/own.svg" => (SVG, svg("First-party image under /fixture-ad/", "#c8e6c9")),
        "/fixture-cmp.js" => (JS, CMP.to_owned()),
        "/lists/fixture-ads.txt" => (TEXT, ADS_LIST.to_owned()),
        "/lists/fixture-cookies.txt" => (TEXT, COOKIES_LIST.to_owned()),
        _ => return None,
    })
}

fn page(port: u16) -> String {
    let third = format!("http://localhost:{port}");
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Content blocking fixture</title>
<style>
  body {{ font: 14px/1.5 -apple-system, sans-serif; margin: 24px; min-height: 300vh; }}
  body.fixture-locked {{ overflow: hidden; }}
  body > div:not(#fixture-cookie-banner) {{ border: 1px solid #999; padding: 8px; margin: 8px 0; }}
  #fixture-cookie-banner {{ position: fixed; inset: auto 0 0 0; background: #222; color: #fff; padding: 16px; }}
</style></head>
<body class="fixture-locked">
<h1>Content blocking fixture</h1>
<p>Every box says what to expect. The server's log is at <a href="/log">/log</a>.</p>
<div id="first-party">First-party script didn't run. Expected: it always runs.</div>
<div id="third-party">Third-party ad script didn't run. Expected with the ads list on.</div>
<script src="/first-party.js"></script>
<script src="{third}/fixture-ad/ad.js"></script>
<div>Third-party image, blocked with the ads list on: <img alt="" src="{third}/fixture-ad/banner.svg"></div>
<div>First-party image, always shown: <img alt="" src="/fixture-ad/own.svg"></div>
<div class="fixture-ad">Generic cosmetic target (.fixture-ad). Expected: hidden with the ads list on.</div>
<div class="fixture-site-ad">This site's cosmetic target (.fixture-site-ad). Expected: hidden with the ads list on.</div>
<div class="fixture-kept">Unhidden on 127.0.0.1 (.fixture-kept). Expected: always shown.</div>
<div id="shadow-host"></div>
<script>
  document.getElementById('shadow-host').attachShadow({{ mode: 'open' }}).innerHTML =
    '<div class="fixture-ad">Inside a shadow root (.fixture-ad). Expected: shown, shadow DOM is not covered.</div>';
</script>
<div>Cross-site frame from localhost; its .fixture-ad line hides with the ads list on:
  <iframe src="{third}/frame.html" width="520" height="90"></iframe></div>
<div id="consent-gated">Consent-gated content: waiting for the consent manager. With the cookie list on it keeps waiting.</div>
<p>The page is scroll-locked until the banner gets an answer. With the cookie list on, it stays locked.</p>
<div id="fixture-cookie-banner">
  We use cookies. Expected: hidden with the cookie list on, and no answer is sent.
  <button type="button" data-answer="accept">Accept</button>
  <button type="button" data-answer="reject">Reject</button>
</div>
<script src="/fixture-cmp.js"></script>
</body></html>
"#
    )
}

const FRAME: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Fixture frame</title></head>
<body style="font: 13px sans-serif; margin: 8px;">
<div>Frame text. Expected: always shown.</div>
<div class="fixture-ad">Frame's .fixture-ad. Expected: hidden with the ads list on.</div>
</body></html>
"#;

/// The fixture's consent manager: it answers only when a button is pressed.
const CMP: &str = r#"(() => {
  const gated = document.getElementById('consent-gated');
  gated.textContent = 'Consent-gated content: the consent manager loaded and waits for an answer.';
  for (const button of document.querySelectorAll('#fixture-cookie-banner button')) {
    button.addEventListener('click', () => {
      const answer = button.dataset.answer;
      fetch('/consent?answer=' + answer).then(() => {
        document.getElementById('fixture-cookie-banner').remove();
        document.body.classList.remove('fixture-locked');
        gated.textContent = 'Consent-gated content: answered "' + answer + '".';
      });
    });
  }
})();
"#;

#[cfg(test)]
#[path = "blocking_fixture_tests.rs"]
mod tests;
