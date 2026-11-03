//! Settings → Account's browser sign-in through simulated input at both
//! sizes. A loopback fake issuer stands in for Clerk (authorize redirect,
//! token endpoint checking PKCE, a fixture ID token) and a loopback fake
//! sync service redeems the code there, as the Worker does. A fake default
//! browser follows the redirects. Nothing reaches Clerk or opens a browser.

use super::repair_tests::{EncryptionFixture, settings_test_frame, sizes};
use super::*;
use crate::site_permissions::{Apps, Handler};
use crate::sync_glue::sync_test_server::{Reply, Request, Server, SyncService};
use crate::sync_glue::{BrowserOffer, BrowserStep, Notice};
use crossbow_core::proto::ClerkExchangeRequest;
use crossbow_core::sync::browser_sign_in::{DONE_PAGE, challenge};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CLIENT: &str = "fixtureClient001";
const WAITING: &str = "Finish signing in in your browser";
const CONTINUE: &str = "Continue in Browser";
const LINK: &str = "Link Browser Sign-In…";

/// Tests here bind the registered loopback ports; they take turns.
static PORTS: Mutex<()> = Mutex::new(());

type Form = BTreeMap<String, String>;

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

fn form(text: &str) -> Form {
    text.split('&').filter(|pair| !pair.is_empty()).map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(key), decode(value))
    }).collect()
}

fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (i, &b)| bits | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(bits >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn unb64url(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let values: Vec<u32> = text.bytes().map(|c| ALPHABET.iter().position(|&a| a == c).unwrap() as u32).collect();
    let mut out = Vec::new();
    for chunk in values.chunks(4) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (i, &v)| bits | v << (18 - 6 * i));
        for i in 0..chunk.len() - 1 {
            out.push((bits >> (16 - 8 * i)) as u8);
        }
    }
    out
}

/// One HTTP/1.1 exchange over a fresh connection: (status, headers, body).
fn http(method: &str, url: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, String) {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, target) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let mut stream = TcpStream::connect(host).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut request = format!("{method} {} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n", if target.is_empty() { "/" } else { target }, body.len());
    for (name, value) in headers { request.push_str(&format!("{name}: {value}\r\n")); }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, head.to_string(), body.to_string())
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_string())
    })
}

/// What the fake issuer's sign-in page does once the person has seen it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Answer {
    Approve,
    Deny,
    /// Sends a code for another attempt's state.
    WrongState,
    /// The person never finishes.
    Ignore,
}

struct Issuer {
    url: String,
    answer: Arc<Mutex<Answer>>,
    authorized: Arc<Mutex<Vec<Form>>>,
    /// Each token request's form and User-Agent.
    redeemed: Arc<Mutex<Vec<(Form, String)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Issuer {
    fn start(answer: Answer) -> Issuer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (answer, authorized, redeemed, stop) = (Arc::new(Mutex::new(answer)), Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let shared = (url.clone(), answer.clone(), authorized.clone(), redeemed.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let (url, answer, authorized, redeemed, stop) = shared;
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => Issuer::answer(stream, &url, &answer, &authorized, &redeemed),
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Issuer { url, answer, authorized, redeemed, stop, thread: Some(thread) }
    }

    fn answer(mut stream: TcpStream, url: &str, answer: &Mutex<Answer>, authorized: &Mutex<Vec<Form>>, redeemed: &Mutex<Vec<(Form, String)>>) {
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let head_end = loop {
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break end; }
            match stream.read(&mut buffer) { Ok(0) | Err(_) => return, Ok(n) => bytes.extend_from_slice(&buffer[..n]) }
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).to_string();
        let length: usize = header(&head, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        while bytes.len() < head_end + 4 + length {
            match stream.read(&mut buffer) { Ok(0) | Err(_) => return, Ok(n) => bytes.extend_from_slice(&buffer[..n]) }
        }
        let body = String::from_utf8_lossy(&bytes[head_end + 4..]).to_string();
        let mut line = head.lines().next().unwrap_or_default().split(' ');
        let (method, target) = (line.next().unwrap_or_default(), line.next().unwrap_or_default());
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let reply = |status: &str, extra: &str, body: &str| format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let out = match (method, path) {
            ("GET", "/oauth/authorize") => {
                let q = form(query);
                let n = { let mut a = authorized.lock().unwrap(); a.push(q.clone()); a.len() - 1 };
                let (redirect, state) = (&q["redirect_uri"], &q["state"]);
                match *answer.lock().unwrap() {
                    Answer::Approve => reply("302 Found", &format!("Location: {redirect}?code=code-{n}&state={state}\r\n"), ""),
                    Answer::Deny => reply("302 Found", &format!("Location: {redirect}?error=access_denied&error_description=The+user+denied&state={state}\r\n"), ""),
                    Answer::WrongState => reply("302 Found", &format!("Location: {redirect}?code=code-{n}&state=forged-state\r\n"), ""),
                    Answer::Ignore => reply("200 OK", "Content-Type: text/html\r\n", "<p>Sign in to Crossbow</p>"),
                }
            }
            ("POST", "/oauth/token") => {
                let f = form(&body);
                let agent = header(&head, "user-agent").unwrap_or_default();
                let first_use = !redeemed.lock().unwrap().iter().any(|(r, _)| r.get("code") == f.get("code"));
                redeemed.lock().unwrap().push((f.clone(), agent));
                let index = f.get("code").and_then(|c| c.strip_prefix("code-")).and_then(|n| n.parse::<usize>().ok());
                let issued = index.and_then(|n| authorized.lock().unwrap().get(n).cloned());
                let valid = issued.as_ref().is_some_and(|a| {
                    first_use
                        && f.get("grant_type").map(String::as_str) == Some("authorization_code")
                        && f.get("client_id").map(String::as_str) == Some(CLIENT)
                        && f.get("redirect_uri") == a.get("redirect_uri")
                        && a.get("code_challenge_method").map(String::as_str) == Some("S256")
                        && f.get("code_verifier").map(|v| challenge(v)) == a.get("code_challenge").cloned()
                        && !f.contains_key("client_secret")
                });
                match issued.filter(|_| valid) {
                    Some(a) => {
                        let claims = serde_json::json!({
                            "iss": url, "aud": CLIENT, "azp": CLIENT, "sub": "user_fixture", "nonce": a["nonce"],
                            "email": "ada@gmail.example", "email_verified": true, "name": "Ada Lovelace", "iat": 1, "exp": 4_000_000_000u64,
                        });
                        let jwt = format!("{}.{}.{}", b64url(br#"{"alg":"RS256","kid":"ins_fixture"}"#), b64url(claims.to_string().as_bytes()), b64url(b"fixture-signature"));
                        let tokens = serde_json::json!({ "id_token": jwt, "access_token": "at-never-leaves", "refresh_token": "rt-never-leaves", "token_type": "Bearer" });
                        reply("200 OK", "Content-Type: application/json\r\n", &tokens.to_string())
                    }
                    None => reply("400 Bad Request", "Content-Type: application/json\r\n", r#"{"error":"invalid_grant"}"#),
                }
            }
            _ => reply("404 Not Found", "", ""),
        };
        let _ = stream.write_all(out.as_bytes());
    }

    fn set(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn authorized(&self) -> Vec<Form> {
        self.authorized.lock().unwrap().clone()
    }

    /// The sign-in page requests, once at least `count` have arrived: the
    /// fake browser sends each from its own thread, after the page shows
    /// that it is waiting.
    fn authorized_at_least(&self, count: usize) -> Vec<Form> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let authorized = self.authorized();
            if authorized.len() >= count { return authorized; }
            assert!(std::time::Instant::now() < deadline, "{count} sign-in page requests expected within 10 s, {} arrived", authorized.len());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn redeemed(&self) -> Vec<(Form, String)> {
        self.redeemed.lock().unwrap().clone()
    }
}

impl Drop for Issuer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

/// The default browser, faked: it follows the issuer's redirect back to the
/// loopback listener, as a browser does, and keeps the page it got. Every
/// URL handed to it is kept in `opened`; it loads only loopback pages, so
/// nothing reaches the network. With `inbox`, it is Crossbow: the system
/// sends the URL back as a link from another app, and the tab loads what
/// that inbox gives.
struct FakeBrowser {
    default: Handler,
    own: Option<String>,
    pages: Arc<Mutex<Vec<String>>>,
    opened: Arc<Mutex<Vec<String>>>,
    inbox: Option<Arc<crate::links::Inbox>>,
}

impl Apps for FakeBrowser {
    fn handler(&self, _url: &str) -> Option<Handler> { Some(self.default.clone()) }
    fn open(&mut self, url: &str) -> bool {
        self.opened.lock().unwrap().push(url.to_string());
        let url = match &self.inbox {
            Some(inbox) => {
                assert!(inbox.deliver(url, crate::links::Source::AppleEvent), "an ordinary link from another app");
                match inbox.take().as_slice() {
                    [crossbow_core::link_routing::Link::Page(page)] => page.clone(),
                    other => panic!("{other:?}"),
                }
            }
            None => url.to_string(),
        };
        if !url.starts_with("http://127.0.0.1") { return true }
        let pages = self.pages.clone();
        std::thread::spawn(move || {
            let (status, head, body) = http("GET", &url, &[], "");
            let page = match header(&head, "location") {
                Some(location) if status == 302 => http("GET", &location, &[], "").2,
                _ => body,
            };
            pages.lock().unwrap().push(page);
        });
        true
    }
    fn own_bundle(&self) -> Option<String> { self.own.clone() }
    fn declares(&self, _key: &str) -> bool { true }
}

fn safari(pages: &Arc<Mutex<Vec<String>>>) -> Box<FakeBrowser> {
    Box::new(FakeBrowser { default: Handler { name: "Safari".into(), bundle_id: "com.apple.Safari".into() }, own: Some("com.agentik.crossbow".into()), pages: pages.clone(), opened: Default::default(), inbox: None })
}

/// How the fake sync service answers.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Health {
    Offers,
    /// Offers, naming a Clerk-hosted issuer whose Account Portal can be
    /// derived; sign-in still goes to the loopback issuer.
    Portal,
    DoesNot,
    Broken,
}

/// The Clerk-hosted issuer `Health::Portal` names, and its Account Portal.
const PORTAL_ISSUER: &str = "https://fixture-instance-0.clerk.accounts.dev";
const PORTAL: &str = "https://fixture-instance-0.accounts.dev/user";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Redeem { Redeem, LinkRequired, Unavailable, ProviderDown, Close, InUse }

struct Service {
    server: Option<Server>,
    health: Arc<Mutex<Health>>,
    redeem: Arc<Mutex<Redeem>>,
    /// Whether the email-code account has a browser sign-in linked.
    linked: Arc<AtomicBool>,
    confirmed: Arc<AtomicUsize>,
}

impl Service {
    fn start(issuer: &Issuer) -> Service {
        let (health, redeem, linked, confirmed) = (Arc::new(Mutex::new(Health::Offers)), Arc::new(Mutex::new(Redeem::Redeem)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
        let sync = Arc::new(SyncService::default());
        let issuer_url = issuer.url.clone();
        let shared = (health.clone(), redeem.clone(), linked.clone(), confirmed.clone());
        let server = Server::start(move |request: &Request| {
            let (health, redeem, linked, confirmed) = &shared;
            if let Some(reply) = sync.reply(request, "device-browser") { return reply; }
            let account = |id: &str, email: &str| serde_json::json!({
                "account": { "id": id, "email": email, "created_at": 1 },
                "device": { "id": "device-browser", "name": "Fixture", "platform": "macos", "created_at": 1, "last_seen_at": 1 },
            });
            // The Worker's side: redeem the code at the issuer, then check the nonce.
            let identity = |body: &str| -> Option<serde_json::Value> {
                let x: ClerkExchangeRequest = serde_json::from_str(body).ok()?;
                let form = format!("grant_type=authorization_code&code={}&redirect_uri={}&client_id={CLIENT}&code_verifier={}", x.code, x.redirect_uri.replace(':', "%3A").replace('/', "%2F"), x.code_verifier);
                let (status, _, tokens) = http("POST", &format!("{issuer_url}/oauth/token"), &[("Content-Type", "application/x-www-form-urlencoded"), ("User-Agent", "fixture-worker")], &form);
                if status != 200 { return None; }
                let tokens: serde_json::Value = serde_json::from_str(&tokens).ok()?;
                let payload = tokens["id_token"].as_str()?.split('.').nth(1)?.to_string();
                let claims: serde_json::Value = serde_json::from_slice(&unb64url(&payload)).ok()?;
                (claims["nonce"] == x.nonce.as_str() && claims["iss"] == issuer_url.as_str() && claims["aud"] == CLIENT).then_some(claims)
            };
            let refused = |status: u16, code: &str| Reply::json(status, serde_json::json!({ "error": { "code": code, "message": "fixture" } }).to_string());
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/v1/health") => match *health.lock().unwrap() {
                    Health::Offers => Reply::json(200, serde_json::json!({ "ok": true, "protocol": 1, "service": "crossbow-api", "version": "0.1.0", "clerk": { "issuer": issuer_url, "client_id": CLIENT, "authorize_endpoint": format!("{issuer_url}/oauth/authorize") } }).to_string()),
                    Health::Portal => Reply::json(200, serde_json::json!({ "ok": true, "protocol": 1, "service": "crossbow-api", "version": "0.1.0", "clerk": { "issuer": PORTAL_ISSUER, "client_id": CLIENT, "authorize_endpoint": format!("{issuer_url}/oauth/authorize") } }).to_string()),
                    Health::DoesNot => Reply::json(200, r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"0.1.0"}"#),
                    Health::Broken => refused(500, "internal"),
                },
                ("POST", "/v1/auth/clerk") => match *redeem.lock().unwrap() {
                    Redeem::Redeem => match identity(&request.body) {
                        Some(claims) => {
                            let mut reply = account("acc_browser", claims["email"].as_str().unwrap());
                            reply["session_token"] = "cbs_browser".into();
                            reply["display_name"] = claims["name"].clone();
                            Reply::json(200, reply.to_string())
                        }
                        None => refused(401, "sign_in_rejected"),
                    },
                    Redeem::LinkRequired => refused(409, "link_required"),
                    Redeem::Unavailable => refused(503, "clerk_unavailable"),
                    Redeem::ProviderDown => refused(502, "provider_unavailable"),
                    Redeem::Close => Reply::close(),
                    Redeem::InUse => refused(409, "identity_in_use"),
                },
                ("POST", "/v1/auth/clerk/link") => match (*redeem.lock().unwrap(), identity(&request.body)) {
                    (Redeem::InUse, Some(_)) => refused(409, "identity_in_use"),
                    (_, Some(claims)) => Reply::json(200, serde_json::json!({ "link_id": "lnk_fixture", "email": claims["email"], "expires_in": 300 }).to_string()),
                    (_, None) => refused(401, "sign_in_rejected"),
                },
                ("POST", "/v1/auth/clerk/link/confirm") => {
                    confirmed.fetch_add(1, Ordering::SeqCst);
                    linked.store(true, Ordering::SeqCst);
                    Reply::json(204, "")
                }
                ("POST", "/v1/auth/start") => Reply::json(200, r#"{"challenge_id":"fixture-challenge","expires_in":600,"dev_code":"123456"}"#),
                ("POST", "/v1/auth/verify") => {
                    let mut reply = account("fixture-account", "fixture@example.com");
                    reply["session_token"] = "cbs_email".into();
                    Reply::json(200, reply.to_string())
                }
                ("GET", "/v1/me") => {
                    let signed_in_by_browser = request.authorization.as_deref() == Some("Bearer cbs_browser");
                    let mut me = if signed_in_by_browser { account("acc_browser", "ada@gmail.example") } else { account("fixture-account", "fixture@example.com") };
                    me["devices"] = serde_json::json!([]);
                    me["browser_sign_in"] = (signed_in_by_browser || linked.load(Ordering::SeqCst)).into();
                    Reply::json(200, me.to_string())
                }
                ("POST", "/v1/auth/logout") => Reply::json(200, "{}"),
                ("GET", "/v1/sync/ws") => Reply::socket(),
                (method, path) => panic!("unexpected fixture request: {method} {path}"),
            }
        });
        Service { server: Some(server), health, redeem, linked, confirmed }
    }

    fn take(&mut self) -> Server {
        self.server.take().unwrap()
    }
}

/// A signed-out Account page on this service, with the fake default browser.
fn page(service: &mut Service, size: egui::Vec2, pages: &Arc<Mutex<Vec<String>>>) -> EncryptionFixture {
    let mut f = EncryptionFixture::signed_out(service.take(), size);
    f.sync.browser.apps = safari(pages);
    f
}

fn port_held(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// The registered port an attempt listened on: the first free one, so not
/// 47231 while another process holds it.
fn port_of(authorization: &Form) -> u16 {
    let port = authorization["redirect_uri"].strip_prefix("http://127.0.0.1:").and_then(|rest| rest.strip_suffix("/callback")).and_then(|port| port.parse().ok());
    port.filter(|port| crossbow_core::sync::browser_sign_in::PORTS.contains(port)).expect("a registered loopback redirect")
}

/// Escape as the app dispatches it: a browser sign-in in progress on the
/// Account page ends first; otherwise the sheet would close. True when the
/// sheet would close.
fn escape(f: &mut EncryptionFixture) -> bool {
    let mut closes = false;
    for pressed in [true, false] {
        let events = vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }];
        settings_test_frame(&f.ctx, Page::Account, f.size, 1.0, &mut f.frame, events, |ui, pal| {
            let ctx = ui.ctx().clone();
            f.sync.poll(&mut f.lib, &ctx);
            if crate::app::take_dismiss_shortcut(&ctx) && !cancel_account_step(Some(&f.state), &mut f.sync) {
                closes = true;
            }
            let now = ctx.input(|i| i.time);
            account_page(&mut f.state, &mut f.sync, &mut f.lib, ui, pal, now);
        });
    }
    f.settle();
    closes
}

#[test]
fn continue_in_browser_signs_in_through_the_issuer_and_survives_a_reopen() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let issuer = Issuer::start(Answer::Approve);
        let mut service = Service::start(&issuer);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let mut f = page(&mut service, size, &pages);
        f.visible(CONTINUE);
        f.visible("Get a Sign-in Code");
        f.click(CONTINUE);
        f.until("browser sign-in", |f| f.sync.signed_in());
        f.visible(&Notice::SignedIn.text());
        f.visible("Signed in as Ada Lovelace");
        assert_eq!(f.sync.session.as_ref().map(|s| (s.account_id.as_str(), s.email.as_str(), s.display_name.as_deref())), Some(("acc_browser", "ada@gmail.example", Some("Ada Lovelace"))));

        // RFC 8252 and PKCE, end to end.
        let authorized = issuer.authorized();
        assert_eq!(authorized.len(), 1);
        let a = &authorized[0];
        assert_eq!((a["response_type"].as_str(), a["client_id"].as_str(), a["redirect_uri"].as_str(), a["scope"].as_str(), a["code_challenge_method"].as_str()),
            ("code", CLIENT, "http://127.0.0.1:47231/callback", "openid email profile", "S256"));
        let redeemed = issuer.redeemed();
        assert_eq!(redeemed.len(), 1, "one code, redeemed once");
        assert_eq!(redeemed[0].1, "fixture-worker", "only the sync service redeems the code; the app never calls the token endpoint");
        assert_eq!(challenge(&redeemed[0].0["code_verifier"]), a["code_challenge"]);
        let exchange = f._server.requests.lock().unwrap().iter().find(|r| r.path == "/v1/auth/clerk").cloned().unwrap();
        let x: ClerkExchangeRequest = serde_json::from_str(&exchange.body).unwrap();
        assert_eq!((x.nonce.as_str(), x.redirect_uri.as_str()), (a["nonce"].as_str(), a["redirect_uri"].as_str()));
        assert!(!pages.lock().unwrap().is_empty() && pages.lock().unwrap()[0].contains(DONE_PAGE), "{:?}", pages.lock().unwrap());
        assert!(!port_held(47231), "the listener closed after its one answer");

        // Quit and relaunch: the session and its name are still there.
        f.sync = SyncGlue::with_key_store(&f._database.dir, &f.lib, || {}, Box::new(crossbow_core::sync::key_store::MemoryKeyStore::default()));
        assert!(f.sync.signed_in());
        assert_eq!(f.sync.session.as_ref().and_then(|s| s.display_name.clone()).as_deref(), Some("Ada Lovelace"));
        f.visible("Signed in as Ada Lovelace");
        assert!(!f.settle().labels().iter().any(|l| l == &Notice::SignedIn.text()), "a notice isn't persisted");
    }
}

#[test]
fn waiting_can_be_canceled_by_button_or_escape_and_try_again_starts_fresh() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let issuer = Issuer::start(Answer::Ignore);
        let mut service = Service::start(&issuer);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let mut f = page(&mut service, size, &pages);
        f.click(CONTINUE);
        f.visible(WAITING);
        let port = port_of(&issuer.authorized_at_least(1)[0]);
        assert!(port_held(port), "listening while the browser is open");
        f.click("Cancel");
        f.visible(&Notice::Cancelled.text());
        assert!(!port_held(port), "Cancel closes the listener at once");
        assert_eq!(f.sync.browser.step, BrowserStep::Idle);

        // Try Again, from the keyboard: a new attempt with new secrets.
        f.focus("Try Again");
        f.key(egui::Key::Enter, egui::Modifiers::NONE);
        f.visible(WAITING);
        let authorized = issuer.authorized_at_least(2);
        assert_eq!(authorized.len(), 2);
        let port = port_of(&authorized[1]);
        assert_ne!(authorized[0]["state"], authorized[1]["state"]);
        assert_ne!(authorized[0]["nonce"], authorized[1]["nonce"]);
        assert_ne!(authorized[0]["code_challenge"], authorized[1]["code_challenge"]);

        // Escape ends the wait, not the sheet.
        assert!(!escape(&mut f), "Escape cancels the browser sign-in first");
        f.visible(&Notice::Cancelled.text());
        assert!(!port_held(port));
        assert!(escape(&mut f), "with nothing in progress, Escape closes Settings");

        assert!(!f.sync.signed_in());
        assert!(f._server.requests.lock().unwrap().iter().all(|r| r.path != "/v1/auth/clerk"), "nothing was redeemed");
    }
}

#[test]
fn every_failure_says_what_to_do_next_and_try_again_keeps_nothing() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let cases: [(&str, Answer, Redeem, Notice); 7] = [
            ("denied in the browser", Answer::Deny, Redeem::Redeem, Notice::Denied),
            ("another attempt's state", Answer::WrongState, Redeem::Redeem, Notice::Mismatch),
            ("email already used", Answer::Approve, Redeem::LinkRequired, Notice::LinkRequired),
            ("not configured any more", Answer::Approve, Redeem::Unavailable, Notice::NotConfigured),
            ("provider down", Answer::Approve, Redeem::ProviderDown, Notice::ProviderUnavailable),
            ("network failure", Answer::Approve, Redeem::Close, Notice::Network),
            ("identity in use", Answer::Approve, Redeem::InUse, Notice::IdentityInUse),
        ];
        for (what, answer, redeem, expected) in cases {
            let issuer = Issuer::start(answer);
            let mut service = Service::start(&issuer);
            *service.redeem.lock().unwrap() = redeem;
            let pages = Arc::new(Mutex::new(Vec::new()));
            let mut f = page(&mut service, size, &pages);
            f.click(CONTINUE);
            f.until(what, |f| f.sync.browser.notice.is_some());
            assert_eq!(f.sync.browser.notice.as_ref(), Some(&expected), "{what}");
            f.visible(&expected.text());
            assert!(!f.sync.signed_in() && !f.sync.busy, "{what}");
            assert!(!port_held(47231), "{what}: the listener closed");
            if expected.retry() {
                *service.redeem.lock().unwrap() = Redeem::Redeem;
                issuer.set(Answer::Approve);
                f.click("Try Again");
                f.until(what, |f| f.sync.signed_in());
                f.visible(&Notice::SignedIn.text());
            } else {
                assert!(!f.settle().labels().iter().any(|l| l == "Try Again"), "{what}: no Try Again");
                f.visible("Get a Sign-in Code");
            }
        }

        // The browser never answers: a timeout, with Try Again.
        let issuer = Issuer::start(Answer::Ignore);
        let mut service = Service::start(&issuer);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let mut f = page(&mut service, size, &pages);
        f.sync.browser.timeout = Duration::from_millis(300);
        f.click(CONTINUE);
        f.until("timeout", |f| f.sync.browser.notice.is_some());
        assert_eq!(f.sync.browser.notice, Some(Notice::TimedOut));
        f.visible(&Notice::TimedOut.text());
        f.visible("Try Again");
        assert!(!port_held(47231));

        let before = issuer.authorized().len();

        // All three registered ports busy.
        let held: Vec<TcpListener> = [47231, 47232, 47233].iter().filter_map(|&p| TcpListener::bind(("127.0.0.1", p)).ok()).collect();
        assert_eq!(held.len(), 3, "the registered ports were free before the test");
        let mut f = page(&mut Service::start(&issuer), size, &pages);
        f.click(CONTINUE);
        f.visible(&Notice::PortsBusy.text());
        drop(held);
        assert_eq!(issuer.authorized().len(), before, "no browser without a listener");
    }
}

#[test]
fn only_a_service_that_offers_it_shows_browser_sign_in() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let issuer = Issuer::start(Answer::Approve);
        let mut service = Service::start(&issuer);
        *service.health.lock().unwrap() = Health::DoesNot;
        let health = service.health.clone();
        let pages = Arc::new(Mutex::new(Vec::new()));
        let mut f = page(&mut service, size, &pages);
        f.until("server check", |f| f.sync.browser.offer == BrowserOffer::Unavailable);
        let shown = f.settle();
        assert!(!shown.labels().iter().any(|l| l == CONTINUE), "not configured: no browser sign-in");
        f.visible("Get a Sign-in Code");

        // A service that can't be asked says so and can be asked again.
        *health.lock().unwrap() = Health::Broken;
        f.sync.recheck_browser_sign_in();
        f.until("server check", |f| f.sync.browser.offer == BrowserOffer::Unreachable);
        f.visible("Check Again");
        *health.lock().unwrap() = Health::Offers;
        f.click("Check Again");
        f.until("server check", |f| matches!(f.sync.browser.offer, BrowserOffer::Available(_)));
        f.visible(CONTINUE);
    }
}

#[test]
fn linking_names_both_emails_and_binds_only_when_link_is_pressed() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let issuer = Issuer::start(Answer::Approve);
        let mut service = Service::start(&issuer);
        let (confirmed, redeem) = (service.confirmed.clone(), service.redeem.clone());
        let pages = Arc::new(Mutex::new(Vec::new()));
        let mut f = page(&mut service, size, &pages);
        // Email codes still sign in.
        f.sign_in();
        assert!(f.sync.signed_in());
        f.until("me", |f| f.sync.browser.linked == Some(false));
        let question = "Link ada@gmail.example to fixture@example.com?";

        // Cancel: nothing bound.
        f.click(LINK);
        f.visible(question);
        f.click("Cancel");
        f.visible(&Notice::NotLinked.text());
        // Escape: nothing bound either, and the sheet stays.
        f.click(LINK);
        f.visible(question);
        assert!(!escape(&mut f));
        f.visible(&Notice::NotLinked.text());
        assert_eq!(confirmed.load(Ordering::SeqCst), 0, "nothing is linked without Link");

        // Another account's identity: refused by name.
        *redeem.lock().unwrap() = Redeem::InUse;
        f.click(LINK);
        f.visible(&Notice::IdentityInUse.text());
        *redeem.lock().unwrap() = Redeem::Redeem;

        // Link.
        f.click(LINK);
        f.visible(question);
        f.click("Link");
        f.visible(&Notice::Linked.text());
        assert_eq!(confirmed.load(Ordering::SeqCst), 1);
        assert_eq!(f.sync.browser.linked, Some(true));
        assert!(!f.settle().labels().iter().any(|l| l == LINK), "linked: no second link offered");
        let requests = f._server.requests.lock().unwrap().clone();
        assert!(requests.iter().filter(|r| r.path.starts_with("/v1/auth/clerk/link")).all(|r| r.authorization.as_deref() == Some("Bearer cbs_email")), "linking uses this session");
        assert_eq!(f.sync.session.as_ref().map(|s| s.account_id.as_str()), Some("fixture-account"), "the account keeps its id");
    }
}

/// Crossbow as the default browser: the system sends what it is handed back
/// to Crossbow as a link from another app.
fn crossbow(pages: &Arc<Mutex<Vec<String>>>, opened: &Arc<Mutex<Vec<String>>>) -> Box<FakeBrowser> {
    let me = Handler { name: "Crossbow".into(), bundle_id: "com.agentik.crossbow".into() };
    Box::new(FakeBrowser { default: me, own: Some("com.agentik.crossbow".into()), pages: pages.clone(), opened: opened.clone(), inbox: Some(Arc::new(crate::links::Inbox::new())) })
}

/// Another default browser, keeping what it is handed in `opened`.
fn recording_safari(pages: &Arc<Mutex<Vec<String>>>, opened: &Arc<Mutex<Vec<String>>>) -> Box<FakeBrowser> {
    let mut browser = safari(pages);
    browser.opened = opened.clone();
    browser
}

/// Every app the system knows declines.
struct Declines;

impl Apps for Declines {
    fn handler(&self, _url: &str) -> Option<Handler> { None }
    fn open(&mut self, _url: &str) -> bool { false }
    fn own_bundle(&self) -> Option<String> { Some("com.agentik.crossbow".into()) }
    fn declares(&self, _key: &str) -> bool { true }
}

#[test]
fn with_crossbow_as_the_default_browser_the_sign_in_page_goes_out_as_any_link() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let issuer = Issuer::start(Answer::Approve);
        let mut handed = Vec::new();
        for default in ["Safari", "Crossbow"] {
            let mut service = Service::start(&issuer);
            let (pages, opened) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
            let mut f = page(&mut service, size, &pages);
            f.sync.browser.apps = if default == "Crossbow" { crossbow(&pages, &opened) } else { recording_safari(&pages, &opened) };
            f.click(CONTINUE);
            f.until("browser sign-in", |f| f.sync.signed_in());
            f.visible(&Notice::SignedIn.text());
            assert!(pages.lock().unwrap().iter().any(|page| page.contains(DONE_PAGE)), "{default}: the tab reached the listener");
            let opened = opened.lock().unwrap().clone();
            assert_eq!(opened.len(), 1, "{default}: handed to the system once");
            assert!(opened[0].starts_with(&format!("{}/oauth/authorize?", issuer.url)), "{opened:?}");
            handed.push(form(opened[0].split_once('?').unwrap().1).into_keys().collect::<Vec<_>>());

            if default == "Crossbow" {
                // It came back as a link from another app (FakeBrowser checks the
                // inbox took it), so it opens where Air Traffic Control sends
                // links: a Today tab in the most recent Space, with its profile.
                f.lib.ensure_defaults().unwrap();
                let space = f.lib.spaces_ordered()[0].0.clone();
                let route = crate::app::site_open::route_link(&f.lib, &Default::default(), &Default::default(), &[], &space, &opened[0]).expect("a Space takes it");
                assert_eq!((route.space.as_str(), route.rule), (space.as_str(), None));
            }
        }
        assert_eq!(handed[0], handed[1], "the same handoff whichever browser is the default");
    }
}

#[test]
fn manage_account_opens_the_account_portal_only_for_a_linked_browser_sign_in() {
    let _turn = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    for size in sizes() {
        let issuer = Issuer::start(Answer::Approve);
        let mut service = Service::start(&issuer);
        let (health, linked) = (service.health.clone(), service.linked.clone());
        let (pages, opened) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let mut f = page(&mut service, size, &pages);
        assert!(!f.settle().labels().iter().any(|l| l == MANAGE_ACCOUNT), "signed out: none");
        f.click(CONTINUE);
        f.until("browser sign-in", |f| f.sync.signed_in() && f.sync.browser.linked == Some(true));
        assert!(!f.settle().labels().iter().any(|l| l == MANAGE_ACCOUNT), "an issuer with no Account Portal: none");

        // Relaunch against a server naming a Clerk-hosted issuer.
        *health.lock().unwrap() = Health::Portal;
        f.sync = SyncGlue::with_key_store(&f._database.dir, &f.lib, || {}, Box::new(crossbow_core::sync::key_store::MemoryKeyStore::default()));
        f.sync.browser.apps = recording_safari(&pages, &opened);
        f.until("linked, on a portal server", |f| f.sync.browser.linked == Some(true) && f.sync.browser.available().is_some());
        f.visible(MANAGE_ACCOUNT);
        f.fits_width();
        assert_eq!(f.sync.account_portal().as_deref(), Some(PORTAL));
        f.click(MANAGE_ACCOUNT);
        assert_eq!(opened.lock().unwrap().as_slice(), [PORTAL], "the portal's profile page, handed to the system");
        assert_eq!(f.sync.browser.notice, None);
        assert!(f.sync.signed_in(), "nothing about this Mac's sign-in changes");

        // Crossbow as the default browser: the same handoff, back as a link.
        f.sync.browser.apps = crossbow(&pages, &opened);
        f.click(MANAGE_ACCOUNT);
        assert_eq!(opened.lock().unwrap().as_slice(), [PORTAL, PORTAL]);

        // No app takes it: said, with nothing else changed.
        f.sync.browser.apps = Box::new(Declines);
        f.click(MANAGE_ACCOUNT);
        f.visible(&Notice::PortalUnopened.text());
        assert!(!f.settle().labels().iter().any(|l| l == "Try Again"));
        f.visible(MANAGE_ACCOUNT);

        // Signed out: gone.
        f.sync.sign_out(&f.lib);
        assert!(!f.settle().labels().iter().any(|l| l == MANAGE_ACCOUNT), "signed out: none");
        // Nor does the glue open it when asked directly, as a menu might.
        let handed = opened.lock().unwrap().len();
        f.sync.browser.apps = recording_safari(&pages, &opened);
        assert_eq!(f.sync.account_portal(), None);
        f.sync.manage_account();
        assert_eq!(opened.lock().unwrap().len(), handed, "signed out: nothing handed over");
        // Even if a late answer said linked after the sign-out.
        f.sync.browser.linked = Some(true);
        assert_eq!(f.sync.account_portal(), None, "signed out: never");
        f.sync.browser.linked = None;

        // Signed in with an email code to an account with no browser sign-in linked: none.
        f.sign_in();
        f.until("me", |f| f.sync.browser.linked == Some(false));
        f.visible(LINK);
        assert!(!f.settle().labels().iter().any(|l| l == MANAGE_ACCOUNT), "nothing linked: none");
        assert_eq!(f.sync.account_portal(), None);
        f.sync.manage_account();
        assert_eq!(opened.lock().unwrap().len(), handed, "nothing linked: nothing handed over");

        // The same email-code session once its account has a browser sign-in
        // linked, as after Link on another Mac: shown. The portal manages the
        // account's Clerk user however this session signed in, and the row
        // says only that the account has one linked.
        linked.store(true, Ordering::SeqCst);
        f.sync = SyncGlue::with_key_store(&f._database.dir, &f.lib, || {}, Box::new(crossbow_core::sync::key_store::MemoryKeyStore::default()));
        f.sync.browser.apps = recording_safari(&pages, &opened);
        f.until("an email-code session on a linked account", |f| f.sync.browser.linked == Some(true) && f.sync.browser.available().is_some());
        assert_eq!(f.sync.session.as_ref().map(|s| s.email.as_str()), Some("fixture@example.com"), "still the email-code session");
        f.visible(MANAGE_ACCOUNT);
        f.visible("Linked. Change its email, password or connected accounts in your browser.");
        f.fits_width();
        f.click(MANAGE_ACCOUNT);
        assert_eq!(opened.lock().unwrap().len(), handed + 1);
        assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some(PORTAL));
    }
}
