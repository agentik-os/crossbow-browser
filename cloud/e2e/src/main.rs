//! `CROSSBOW_DEV_KEY=<key> crossbow-e2e --base <url> [--proxy http://host:port]`
//!
//! Runs the whole account + sync + AI journey against a deployed environment
//! whose `/v1/auth/start` returns `dev_code` (the dev environment, given its
//! `DEV_CODE_KEY` secret in `CROSSBOW_DEV_KEY`), and prints PASS/FAIL per
//! step. Exit code 0 only if every step passed. The key is sent only to
//! `--base` and never printed.
//!
//! `--proxy` sends every request (HTTP and the WebSocket) through an HTTP
//! CONNECT proxy, for networks that reset TLS to some Cloudflare edge IPs.

use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbow_proto::{
    AuthStartResponse, AuthVerifyResponse, Change, DEV_KEY_HEADER, Encoding, Hlc, MeResponse,
    PROTOCOL_VERSION, PullResponse, PushResponse, ServerEvent,
};
use futures_util::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const STEPS: &[&str] = &[
    "health",
    "auth start with the dev key returns dev_code",
    "verify with wrong code -> 401 invalid_code",
    "verify with right code -> session A",
    "GET /v1/me (device A)",
    "sign in second device B",
    "WebSocket on B -> Hello",
    "push from A",
    "B receives Changed",
    "B pulls the change",
    "stale push reported in `stale`",
    "tombstone round-trips",
    "/v1/ai/* without session -> 401",
    "/v1/ai/health through proxy (spoofed header stripped)",
    "/v1/ai/complete streams text",
    "revoke unknown device -> 404",
    "revoke device B -> B token 401, B socket closed",
    "logout A -> A token 401",
];

const WAIT: Duration = Duration::from_secs(15);
const NO_DEV_CODE: &str =
    "no dev_code: run against the dev Worker with CROSSBOW_DEV_KEY set to its DEV_CODE_KEY secret";

struct Args {
    base: String,
    proxy: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut base = None;
    let mut proxy = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => base = it.next(),
            "--proxy" => proxy = it.next(),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let base = base.ok_or("--base <url> is required")?.trim_end_matches('/').to_string();
    if !base.starts_with("https://") && !base.starts_with("http://") {
        return Err("--base must be an http(s) URL".into());
    }
    Ok(Args { base, proxy })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("os randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Client {
    base: String,
    proxy: Option<String>,
    /// `CROSSBOW_DEV_KEY`: sent as `x-crossbow-dev-key` on auth starts.
    dev_key: Option<String>,
    http: reqwest::Client,
}

struct Reply {
    status: StatusCode,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
    fn expect(&self, status: u16) -> Result<&Value, String> {
        if self.status.as_u16() == status {
            Ok(&self.body)
        } else {
            Err(format!("expected {status}, got {} {}", self.status, short(&self.body)))
        }
    }
    fn parse<T: serde::de::DeserializeOwned>(&self, status: u16) -> Result<T, String> {
        let v = self.expect(status)?;
        serde_json::from_value(v.clone()).map_err(|e| format!("bad body ({e}): {}", short(v)))
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 200 { format!("{}…", &s[..200]) } else { s }
}

impl Client {
    fn new(base: String, proxy: Option<String>, dev_key: Option<String>) -> Result<Self, String> {
        let mut b = reqwest::Client::builder().timeout(Duration::from_secs(60)).user_agent("crossbow-e2e/0.1");
        if let Some(p) = &proxy {
            b = b.proxy(reqwest::Proxy::all(p).map_err(|e| e.to_string())?);
        }
        Ok(Self { base, proxy, dev_key, http: b.build().map_err(|e| e.to_string())? })
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Result<Reply, String> {
        let mut rb = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        for (k, v) in extra {
            rb = rb.header(*k, *v);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
        let body = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
        Ok(Reply { status, body })
    }

    /// `POST /v1/auth/start`, with the dev key when there is one.
    async fn start(&self, email: &str) -> Result<AuthStartResponse, String> {
        let key: Vec<(&str, &str)> = self.dev_key.iter().map(|k| (DEV_KEY_HEADER, k.as_str())).collect();
        self.call(Method::POST, "/v1/auth/start", None, Some(json!({ "email": email })), &key).await?.parse(200)
    }

    async fn sign_in(&self, email: &str, device_name: &str) -> Result<AuthVerifyResponse, String> {
        let start: AuthStartResponse =
            self.start(email).await?;
        let code = start.dev_code.ok_or(NO_DEV_CODE)?;
        self.call(
            Method::POST,
            "/v1/auth/verify",
            None,
            Some(json!({ "challenge_id": start.challenge_id, "code": code, "device_name": device_name, "platform": "linux" })),
            &[],
        )
        .await?
        .parse(200)
    }

    async fn push(&self, token: &str, changes: Vec<Change>) -> Result<PushResponse, String> {
        self.call(Method::POST, "/v1/sync/push", Some(token), Some(json!({ "changes": changes })), &[])
            .await?
            .parse(200)
    }

    async fn pull(&self, token: &str, since: u64) -> Result<PullResponse, String> {
        self.call(Method::GET, &format!("/v1/sync/pull?since={since}&limit=100"), Some(token), None, &[])
            .await?
            .parse(200)
    }

    /// Open `/v1/sync/ws` and forward every frame into a channel.
    async fn websocket(&self, token: &str) -> Result<mpsc::UnboundedReceiver<WsEvent>, String> {
        let url = format!("{}/v1/sync/ws", self.base.replacen("http", "ws", 1));
        let mut req = url.as_str().into_client_request().map_err(|e| e.to_string())?;
        req.headers_mut().insert("authorization", format!("Bearer {token}").parse().map_err(|_| "header")?);
        let (tx, rx) = mpsc::unbounded_channel();
        let ws = match &self.proxy {
            None => tokio_tungstenite::connect_async(req).await.map_err(|e| format!("connect: {e}"))?.0,
            Some(proxy) => {
                let host = req.uri().host().ok_or("no host")?.to_string();
                let port = req.uri().port_u16().unwrap_or(443);
                let stream = connect_via_proxy(proxy, &host, port).await?;
                tokio_tungstenite::client_async_tls(req, stream).await.map_err(|e| format!("connect: {e}"))?.0
            }
        };
        tokio::spawn(async move {
            let (_write, mut read) = ws.split();
            while let Some(msg) = read.next().await {
                let ev = match msg {
                    Ok(Message::Text(t)) => match serde_json::from_str::<ServerEvent>(&t) {
                        Ok(e) => WsEvent::Event(e),
                        Err(_) => WsEvent::Other(t.to_string()),
                    },
                    Ok(Message::Close(frame)) => {
                        let _ = tx.send(WsEvent::Closed(frame.map(|f| u16::from(f.code))));
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        let _ = tx.send(WsEvent::Error(e.to_string()));
                        break;
                    }
                };
                if tx.send(ev).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}

async fn connect_via_proxy(proxy: &str, host: &str, port: u16) -> Result<TcpStream, String> {
    let addr = proxy.trim_start_matches("http://").trim_end_matches('/');
    let mut s = TcpStream::connect(addr).await.map_err(|e| format!("proxy connect: {e}"))?;
    s.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if s.read(&mut byte).await.map_err(|e| e.to_string())? == 0 || buf.len() > 8192 {
            return Err("proxy closed during CONNECT".into());
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf);
    if !head.split_whitespace().nth(1).is_some_and(|c| c == "200") {
        return Err(format!("proxy refused CONNECT: {}", head.lines().next().unwrap_or("")));
    }
    Ok(s)
}

#[derive(Debug)]
#[allow(dead_code)] // payloads are read through Debug in failure messages
enum WsEvent {
    Event(ServerEvent),
    Other(String),
    Closed(Option<u16>),
    Error(String),
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<WsEvent>) -> Result<WsEvent, String> {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .map_err(|_| format!("no WebSocket message within {}s", WAIT.as_secs()))?
        .ok_or_else(|| "WebSocket reader ended".to_string())
}

fn change(kind: &str, id: &str, hlc: Hlc, deleted: bool, payload: &str) -> Change {
    Change { kind: kind.into(), id: id.into(), hlc, deleted, enc: Encoding::Json, payload: payload.into() }
}

struct Report {
    results: Vec<(String, Result<String, String>)>,
}

impl Report {
    fn record<T>(&mut self, step: usize, r: Result<(T, String), String>) -> Result<T, ()> {
        match r {
            Ok((v, detail)) => {
                self.results.push((STEPS[step].to_string(), Ok(detail)));
                Ok(v)
            }
            Err(e) => {
                self.results.push((STEPS[step].to_string(), Err(e)));
                Err(())
            }
        }
    }
}

fn check(cond: bool, msg: impl Into<String>) -> Result<(), String> {
    if cond { Ok(()) } else { Err(msg.into()) }
}

async fn journey(c: &Client, rep: &mut Report) -> Result<(), ()> {
    let email = format!("e2e-{}@example.com", rand_hex(6));

    // 0 health
    rep.record(0, async {
        let r = c.call(Method::GET, "/v1/health", None, None, &[]).await?;
        let v = r.expect(200)?;
        check(v["ok"] == true && v["protocol"] == PROTOCOL_VERSION, format!("unexpected body {}", short(v)))?;
        Ok(((), format!("{} {}", v["service"].as_str().unwrap_or("?"), v["version"].as_str().unwrap_or("?"))))
    }.await)?;

    // 1 auth start
    let start: AuthStartResponse = rep.record(1, async {
        let s = c.start(&email).await?;
        let code = s.dev_code.as_deref().ok_or(NO_DEV_CODE)?;
        check(code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()), "dev_code is not 6 digits")?;
        check(s.expires_in == 600, format!("expires_in = {}", s.expires_in))?;
        let detail = format!("challenge {}, expires_in {}s", s.challenge_id, s.expires_in);
        Ok((s, detail))
    }.await)?;
    let code = start.dev_code.clone().unwrap_or_default();

    // 2 wrong code
    rep.record(2, async {
        let wrong = format!("{:06}", (code.parse::<u32>().unwrap_or(0) + 1) % 1_000_000);
        let r = c.call(Method::POST, "/v1/auth/verify", None,
            Some(json!({ "challenge_id": start.challenge_id, "code": wrong, "device_name": "e2e device A", "platform": "macos" })), &[]).await?;
        check(r.status == 401 && r.code() == "invalid_code", format!("got {} {}", r.status, short(&r.body)))?;
        Ok(((), "401 invalid_code".into()))
    }.await)?;

    // 3 right code
    let a: AuthVerifyResponse = rep.record(3, async {
        let v: AuthVerifyResponse = c.call(Method::POST, "/v1/auth/verify", None,
            Some(json!({ "challenge_id": start.challenge_id, "code": code, "device_name": "e2e device A", "platform": "macos" })), &[])
            .await?.parse(200)?;
        check(v.session_token.starts_with("cbs_") && v.session_token.len() == 47, "token format")?;
        check(v.account.email == email, "account email mismatch")?;
        check(v.device.platform == "macos" && v.device.name == "e2e device A", "device fields")?;
        let detail = format!("account {}, device {}", v.account.id, v.device.id);
        Ok((v, detail))
    }.await)?;
    let ta = a.session_token.clone();

    // 4 me
    rep.record(4, async {
        let me: MeResponse = c.call(Method::GET, "/v1/me", Some(&ta), None, &[]).await?.parse(200)?;
        check(me.account.id == a.account.id && me.device.id == a.device.id, "me does not match sign-in")?;
        check(me.devices.iter().any(|d| d.id == a.device.id), "device A missing from devices")?;
        Ok(((), format!("{} device(s)", me.devices.len())))
    }.await)?;

    // 5 second device
    let b: AuthVerifyResponse = rep.record(5, async {
        let v = c.sign_in(&email, "e2e device B").await?;
        check(v.account.id == a.account.id, "device B landed in another account")?;
        check(v.device.id != a.device.id, "device B reused device A's id")?;
        let me: MeResponse = c.call(Method::GET, "/v1/me", Some(&ta), None, &[]).await?.parse(200)?;
        check(me.devices.len() == 2, format!("expected 2 devices, got {}", me.devices.len()))?;
        let detail = format!("device {}", v.device.id);
        Ok((v, detail))
    }.await)?;
    let tb = b.session_token.clone();

    // 6 websocket hello
    let (mut ws, hello_cursor) = rep.record(6, async {
        let mut rx = c.websocket(&tb).await?;
        match next_event(&mut rx).await? {
            WsEvent::Event(ServerEvent::Hello { cursor }) => Ok(((rx, cursor), format!("hello cursor {cursor}"))),
            other => Err(format!("expected Hello, got {other:?}")),
        }
    }.await)?;

    // 7 push from A
    let t0 = now_ms();
    let h1 = Hlc::new(t0, 0, "e2eA");
    let item_id = format!("item-{}", rand_hex(4));
    let note_id = format!("note-{}", rand_hex(4));
    let item_payload = r#"{"title":"Crossbow e2e","url":"https://example.com/"}"#;
    let note_payload = r#"{"text":"hello from device A"}"#;
    let pushed: PushResponse = rep.record(7, async {
        let p = c.push(&ta, vec![
            change("item", &item_id, h1.clone(), false, item_payload),
            change("note", &note_id, Hlc::new(t0, 1, "e2eA"), false, note_payload),
        ]).await?;
        check(p.accepted == 2 && p.stale.is_empty(), format!("accepted {}, stale {}", p.accepted, p.stale.len()))?;
        check(p.cursor == hello_cursor + 2, format!("cursor {} (hello was {hello_cursor})", p.cursor))?;
        let detail = format!("accepted 2, cursor {}", p.cursor);
        Ok((p, detail))
    }.await)?;

    // 8 B receives Changed
    rep.record(8, async {
        loop {
            match next_event(&mut ws).await? {
                WsEvent::Event(ServerEvent::Changed { cursor, by_device }) => {
                    check(by_device == a.device.id, format!("by_device {by_device}"))?;
                    check(cursor == pushed.cursor, format!("cursor {cursor}"))?;
                    return Ok(((), format!("changed cursor {cursor} by {by_device}")));
                }
                WsEvent::Event(_) | WsEvent::Other(_) => continue,
                other => return Err(format!("socket ended: {other:?}")),
            }
        }
    }.await)?;

    // 9 B pulls
    rep.record(9, async {
        let p = c.pull(&tb, hello_cursor).await?;
        check(p.changes.len() == 2 && !p.more, format!("{} changes, more {}", p.changes.len(), p.more))?;
        check(p.cursor == pushed.cursor, format!("cursor {}", p.cursor))?;
        let item = p.changes.iter().find(|x| x.change.id == item_id).ok_or("item missing")?;
        check(item.change.payload == item_payload && item.change.hlc == h1, "item payload/hlc mismatch")?;
        check(item.device_id == a.device.id, "item device_id is not A")?;
        let note = p.changes.iter().find(|x| x.change.id == note_id).ok_or("note missing")?;
        check(note.change.payload == note_payload, "note payload mismatch")?;
        Ok(((), format!("2 changes, cursor {}", p.cursor)))
    }.await)?;

    // 10 stale push
    rep.record(10, async {
        let old = Hlc::new(t0 - 1000, 0, "e2eB");
        let p = c.push(&tb, vec![change("item", &item_id, old, false, r#"{"title":"stale"}"#)]).await?;
        check(p.accepted == 0 && p.stale.len() == 1, format!("accepted {}, stale {}", p.accepted, p.stale.len()))?;
        check(p.stale[0].id == item_id && p.stale[0].server_hlc == h1, "stale entry does not carry the server HLC")?;
        Ok(((), format!("stale server_hlc {}", p.stale[0].server_hlc)))
    }.await)?;

    // 11 tombstone
    rep.record(11, async {
        let h2 = Hlc::new(now_ms().max(t0 + 1), 0, "e2eA");
        let p = c.push(&ta, vec![change("item", &item_id, h2.clone(), true, "")]).await?;
        check(p.accepted == 1, format!("accepted {}", p.accepted))?;
        let pulled = c.pull(&tb, pushed.cursor).await?;
        check(pulled.changes.len() == 1, format!("{} changes", pulled.changes.len()))?;
        let t = &pulled.changes[0];
        check(t.change.id == item_id && t.change.deleted && t.change.payload.is_empty() && t.change.hlc == h2,
            "tombstone fields wrong")?;
        Ok(((), format!("deleted=true payload=\"\" seq {}", t.seq)))
    }.await)?;

    // 12 ai without session
    rep.record(12, async {
        let r = c.call(Method::GET, "/v1/ai/health", None, None, &[("x-crossbow-account", "acc_00000000000000000000000000000000")]).await?;
        check(r.status == 401 && r.code() == "unauthorized", format!("got {} {}", r.status, short(&r.body)))?;
        Ok(((), "401 unauthorized".into()))
    }.await)?;

    // 13 ai health (with a spoofed account header that must be replaced)
    rep.record(13, async {
        let spoof = "acc_ffffffffffffffffffffffffffffffff";
        let r = c.call(Method::GET, "/v1/ai/health", Some(&ta), None, &[("X-Crossbow-Account", spoof), ("X-Crossbow-Device", "dev_spoof")]).await?;
        let v = r.expect(200)?;
        check(v["ok"] == true, "ok is not true")?;
        let models = v["models"].as_array().filter(|m| !m.is_empty()).ok_or("no models")?;
        check(v["account"] == a.account.id.as_str(), format!("AI worker saw account {} (spoof not stripped?)", v["account"]))?;
        Ok(((), format!("models {}, gateway {}", models.iter().filter_map(|m| m.as_str()).collect::<Vec<_>>().join(","), v["gateway"])))
    }.await)?;

    // 14 ai complete
    rep.record(14, async {
        let resp = c.http.post(format!("{}/v1/ai/complete", c.base)).bearer_auth(&ta)
            .json(&json!({ "messages": [{ "role": "user", "content": "Reply with exactly one word: pong" }] }))
            .send().await.map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        let model = resp.headers().get("x-crossbow-ai-model").and_then(|v| v.to_str().ok()).unwrap_or("?").to_string();
        let mut stream = resp.bytes_stream();
        let (mut chunks, mut text) = (0usize, Vec::new());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream: {e}"))?;
            if !chunk.is_empty() { chunks += 1; }
            text.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&text).trim().to_string();
        check(status == 200, format!("status {status}: {}", text.chars().take(200).collect::<String>()))?;
        check(!text.is_empty(), "empty completion")?;
        let lower = text.to_lowercase();
        check(lower.contains("pong") && !lower.contains("pongpong"), format!("unexpected completion {text:?} (tokens doubled?)"))?;
        Ok(((), format!("{chunks} chunk(s) from {model}: {:?}", text.chars().take(60).collect::<String>())))
    }.await)?;

    // 15 revoke unknown device
    rep.record(15, async {
        let r = c.call(Method::DELETE, &format!("/v1/devices/dev_{}", rand_hex(16)), Some(&ta), None, &[]).await?;
        check(r.status == 404 && r.code() == "not_found", format!("got {} {}", r.status, short(&r.body)))?;
        Ok(((), "404 not_found".into()))
    }.await)?;

    // 16 revoke B
    rep.record(16, async {
        let r = c.call(Method::DELETE, &format!("/v1/devices/{}", b.device.id), Some(&ta), None, &[]).await?;
        r.expect(204)?;
        let me = c.call(Method::GET, "/v1/me", Some(&tb), None, &[]).await?;
        check(me.status == 401 && me.code() == "unauthorized", format!("B /v1/me got {}", me.status))?;
        let closed = loop {
            match next_event(&mut ws).await? {
                WsEvent::Closed(code) => break code,
                WsEvent::Error(_) => break None,
                _ => continue,
            }
        };
        let me_a: MeResponse = c.call(Method::GET, "/v1/me", Some(&ta), None, &[]).await?.parse(200)?;
        check(me_a.devices.len() == 1, "revoked device still listed")?;
        Ok(((), format!("B token 401, socket closed (code {closed:?})")))
    }.await)?;

    // 17 logout A
    rep.record(17, async {
        c.call(Method::POST, "/v1/auth/logout", Some(&ta), None, &[]).await?.expect(204)?;
        let me = c.call(Method::GET, "/v1/me", Some(&ta), None, &[]).await?;
        check(me.status == 401 && me.code() == "unauthorized", format!("A /v1/me got {}", me.status))?;
        Ok(((), "204, then 401".into()))
    }.await)?;

    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}");
            }
            eprintln!("usage: CROSSBOW_DEV_KEY=<dev key> crossbow-e2e --base <url> [--proxy http://127.0.0.1:8899]");
            return ExitCode::from(2);
        }
    };
    let dev_key = std::env::var("CROSSBOW_DEV_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if dev_key.is_none() {
        eprintln!("note: CROSSBOW_DEV_KEY is not set, so no step can sign in");
    }
    let client = match Client::new(args.base.clone(), args.proxy.clone(), dev_key) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    println!("crossbow-e2e against {}{}", args.base, args.proxy.as_deref().map(|p| format!(" via {p}")).unwrap_or_default());
    let mut rep = Report { results: Vec::new() };
    let _ = journey(&client, &mut rep).await;

    let mut failed = 0;
    for (i, name) in STEPS.iter().enumerate() {
        match rep.results.get(i) {
            Some((_, Ok(detail))) => println!("PASS  {:>2}  {name}  ({detail})", i + 1),
            Some((_, Err(e))) => {
                failed += 1;
                println!("FAIL  {:>2}  {name}  ({e})", i + 1)
            }
            None => {
                failed += 1;
                println!("FAIL  {:>2}  {name}  (not run: an earlier step failed)", i + 1)
            }
        }
    }
    println!("{} passed, {failed} failed", STEPS.len() - failed);
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve one auth start on loopback; returns the request's header lines.
    async fn start_through(dev_key: Option<&str>) -> Vec<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
            }
            let body = r#"{"challenge_id":"chl_fixture","expires_in":600}"#;
            let reply = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            s.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).lines().map(|l| l.to_ascii_lowercase()).collect::<Vec<_>>()
        });
        let c = Client::new(base, None, dev_key.map(String::from)).unwrap();
        let start = c.start("fixture@example.com").await.unwrap();
        assert_eq!(start.challenge_id, "chl_fixture");
        server.await.unwrap()
    }

    #[tokio::test]
    async fn the_dev_key_goes_with_auth_start_only_when_set() {
        let with = start_through(Some("fixture-dev-key")).await;
        assert!(with.iter().any(|l| l == "x-crossbow-dev-key: fixture-dev-key"), "{with:?}");
        let without = start_through(None).await;
        assert!(!without.iter().any(|l| l.starts_with("x-crossbow-dev-key")), "{without:?}");
    }
}
