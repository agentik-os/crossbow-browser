//! Isolated, real loopback fixtures. Never connects to the configured service.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub body: String,
    /// The Sec-WebSocket-Key of an upgrade request.
    pub websocket_key: Option<String>,
    /// The Authorization header, if any.
    pub authorization: Option<String>,
    /// The x-crossbow-dev-key header, if any.
    pub dev_key: Option<String>,
}

pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
    pub gate: Option<Arc<AtomicBool>>,
    pub stall: bool,
    pub socket: bool,
    /// Close the connection without an answer, as a network failure does.
    pub close: bool,
}

impl Reply {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self { status, body: body.into(), gate: None, stall: false, socket: false, close: false }
    }
    pub fn stall() -> Self {
        Self { stall: true, ..Self::json(200, "") }
    }
    pub fn close() -> Self {
        Self { close: true, ..Self::json(0, "") }
    }
    /// An accepted sync socket that stays open until the fixture stops or the
    /// device closes it. It carries only what a test sends with
    /// [`Server::notify`], as the service's carries nothing between changes:
    /// the worker reports Live and never retries, so no reconnect asks for a
    /// catch-up pull. A test that needs one refuses the upgrade with a plain
    /// error reply or ends the socket with [`Server::drop_sockets`].
    pub fn socket() -> Self {
        Self { socket: true, ..Self::json(101, "") }
    }
}

pub(crate) struct Server {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
    /// The sync sockets open now, by the device's address.
    sockets: Arc<Mutex<Vec<(SocketAddr, TcpStream)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread_sockets = sockets.clone();
        let handler = Arc::new(handler);
        let thread = std::thread::spawn(move || {
            let mut connections = Vec::new();
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, peer)) => {
                        let requests = thread_requests.clone();
                        let sockets = thread_sockets.clone();
                        let stop = thread_stop.clone();
                        let handler = handler.clone();
                        connections.push(std::thread::spawn(move || {
                            let Some(request) = read_request(&mut stream) else { return };
                            requests.lock().unwrap().push(request.clone());
                            let reply = handler(&request);
                            if reply.socket {
                                let Some(key) = &request.websocket_key else {
                                    let _ = write!(stream, "HTTP/1.1 400 Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                                    return;
                                };
                                let _ = write!(stream, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept_key(key));
                                sockets.lock().unwrap().push((peer, stream.try_clone().unwrap()));
                                // Open until the fixture stops or the device
                                // closes it; what the device sends is ignored.
                                // A device that already closed the socket makes macOS refuse
                                // the timeout (EINVAL); that connection is simply over.
                                if stream.set_read_timeout(Some(Duration::from_millis(5))).is_err() {
                                    sockets.lock().unwrap().retain(|(open, _)| *open != peer);
                                    return;
                                }
                                let mut buffer = [0; 1024];
                                while !stop.load(Ordering::SeqCst) {
                                    match stream.read(&mut buffer) {
                                        Ok(0) => break,
                                        Ok(_) => {}
                                        Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                                        Err(_) => break,
                                    }
                                }
                                sockets.lock().unwrap().retain(|(open, _)| *open != peer);
                                return;
                            }
                            let deadline = Instant::now() + Duration::from_secs(10);
                            while reply.stall || reply.gate.as_ref().is_some_and(|g| !g.load(Ordering::SeqCst)) {
                                if stop.load(Ordering::SeqCst) { return; }
                                assert!(Instant::now() < deadline, "fixture gate not released");
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            if reply.close { return; }
                            // Cancellation may close the connection before a late response.
                            let _ = write!(stream, "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.status, reply.body.len(), reply.body);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                    Err(error) => panic!("loopback accept: {error}"),
                }
            }
            for connection in connections { connection.join().unwrap(); }
        });
        Self { url, requests, sockets, stop, thread: Some(thread) }
    }

    pub fn wait_for(&self, path: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.requests.lock().unwrap().iter().filter(|r| r.path.starts_with(path)).count() < count {
            assert!(Instant::now() < deadline, "missing fixture request: {path}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Send `text` on every sync socket open now, as the service sends its
    /// events, and say how many it reached.
    pub fn notify(&self, text: &str) -> usize {
        let frame = text_frame(text);
        self.sockets.lock().unwrap().iter().filter(|(_, stream)| (&*stream).write_all(&frame).is_ok()).count()
    }

    /// End every sync socket open now without a close frame, as a lost
    /// connection or a restarted service does, and say how many it ended.
    pub fn drop_sockets(&self) -> usize {
        self.sockets.lock().unwrap().drain(..).filter(|(_, stream)| stream.shutdown(std::net::Shutdown::Both).is_ok()).count()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if self.thread.take().unwrap().join().is_err() && !std::thread::panicking() {
            panic!("loopback fixture thread failed");
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    // BSD can inherit the listener's nonblocking flag on accepted sockets.
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let (header_len, body_len) = loop {
        let n = stream.read(&mut buffer).ok()?;
        if n == 0 { return None; }
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let size = header.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
            }).unwrap_or(0);
            break (end + 4, size);
        }
    };
    while bytes.len() < header_len + body_len {
        let n = stream.read(&mut buffer).ok()?;
        if n == 0 { return None; }
        bytes.extend_from_slice(&buffer[..n]);
    }
    let header = String::from_utf8_lossy(&bytes[..header_len]);
    let mut first = header.lines().next().unwrap().split_whitespace();
    let websocket_key = header.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("sec-websocket-key").then(|| value.trim().to_string())
    });
    let authorization = header.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("authorization").then(|| value.trim().to_string())
    });
    let dev_key = header.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("x-crossbow-dev-key").then(|| value.trim().to_string())
    });
    Some(Request {
        method: first.next().unwrap().to_string(),
        path: first.next().unwrap().to_string(),
        body: String::from_utf8(bytes[header_len..header_len + body_len].to_vec()).unwrap(),
        websocket_key,
        authorization,
        dev_key,
    })
}

/// Sec-WebSocket-Accept for `key` (RFC 6455, section 4.2.2): the base64
/// SHA-1 of the key and the protocol's GUID. The app shares this file but
/// not the crate's WebSocket library, so the fixture computes it.
pub(crate) fn accept_key(key: &str) -> String {
    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let digest = sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    let mut out = String::with_capacity(28);
    for chunk in digest.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (i, &byte)| bits | u32::from(byte) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { BASE64[(bits >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// One text frame as a server sends it: final and unmasked (RFC 6455,
/// section 5.2).
fn text_frame(text: &str) -> Vec<u8> {
    let mut frame = vec![0x81];
    match text.len() {
        len @ 0..=125 => frame.push(len as u8),
        len @ 126..=0xFFFF => {
            frame.push(126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            frame.push(127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(text.as_bytes());
    frame
}

/// SHA-1 (FIPS 180-4), for the handshake above only.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 { message.push(0); }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() { w[i] = u32::from_be_bytes(word.try_into().unwrap()); }
        for i in 16..80 { w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1); }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, next);
        }
        for (value, add) in state.iter_mut().zip([a, b, c, d, e]) { *value = value.wrapping_add(add); }
    }
    let mut digest = [0u8; 20];
    for (bytes, value) in digest.chunks_mut(4).zip(state) { bytes.copy_from_slice(&value.to_be_bytes()); }
    digest
}

/// A stateful last-writer-wins sync service with the Worker's observable
/// contract: payloads are opaque, a strictly newer HLC string wins, tombstones
/// lose their payload, and a pull returns each row's latest version by `seq`.
/// `DELETE /v1/sync/data` empties it and starts a new sync epoch; a push that
/// names another epoch (absent meaning 0) is refused with 409, as the
/// Worker's room does (cloud/api/src/room.rs).
/// One service can back several devices, each through its own [`Server`].
#[derive(Default)]
pub(crate) struct SyncService {
    state: Mutex<(u64, Vec<serde_json::Value>)>,
    rejected_ids: Mutex<Vec<String>>,
    page_bytes: Mutex<Option<usize>>,
    sync_epoch: Mutex<u64>,
    /// Answer the delete route as the deployed Worker without it does: 404.
    lacks_delete: AtomicBool,
}

impl SyncService {
    /// Answers push and pull; any other path is left to the caller.
    pub fn reply(&self, request: &Request, device: &str) -> Option<Reply> {
        use serde_json::{Value, json};
        if request.path == "/v1/sync/data" && request.method == "DELETE" {
            if self.lacks_delete.load(Ordering::SeqCst) {
                return Some(Reply::json(404, r#"{"error":{"code":"not_found","message":"not found"}}"#));
            }
            let mut state = self.state.lock().unwrap();
            let mut epoch = self.sync_epoch.lock().unwrap();
            let deleted_rows = state.1.len();
            state.1.clear();
            state.0 += 1;
            *epoch += 1;
            return Some(Reply::json(200, json!({ "sync_epoch": *epoch, "deleted_rows": deleted_rows }).to_string()));
        }
        if request.path == "/v1/sync/push" {
            let body: Value = serde_json::from_str(&request.body).unwrap();
            if body["sync_epoch"].as_u64().unwrap_or(0) != *self.sync_epoch.lock().unwrap() {
                return Some(Reply::json(409, r#"{"error":{"code":"sync_epoch_changed","message":"this account's sync data was deleted since this device last synced"}}"#));
            }
            let changes = body["changes"].as_array().unwrap();
            let rejected = self.rejected_ids.lock().unwrap();
            if changes.iter().any(|c| rejected.iter().any(|id| c["id"] == id.as_str())) {
                return Some(Reply::json(400, r#"{"error":{"code":"bad_request","message":"fixture rejected row"}}"#));
            }
            let mut state = self.state.lock().unwrap();
            let (mut accepted, mut stale) = (0, Vec::new());
            for change in changes {
                let (kind, id, hlc) = (change["kind"].as_str().unwrap(), change["id"].as_str().unwrap(), change["hlc"].as_str().unwrap());
                if let Some(index) = state.1.iter().position(|r| r["kind"] == kind && r["id"] == id) {
                    let stored = state.1[index]["hlc"].as_str().unwrap().to_string();
                    if stored.as_str() >= hlc {
                        stale.push(json!({ "kind": kind, "id": id, "server_hlc": stored }));
                        continue;
                    }
                    state.1.remove(index);
                }
                state.0 += 1;
                let mut row = change.clone();
                if row["deleted"] == true { row["payload"] = json!(""); }
                row["seq"] = json!(state.0);
                row["device_id"] = json!(device);
                state.1.push(row);
                accepted += 1;
            }
            return Some(Reply::json(200, json!({ "accepted": accepted, "stale": stale, "cursor": state.0 }).to_string()));
        }
        let query = request.path.strip_prefix("/v1/sync/pull?")?;
        let param = |name: &str| query.split('&').find_map(|p| p.strip_prefix(name)?.strip_prefix('=')?.parse::<u64>().ok());
        let (since, limit) = (param("since").unwrap_or(0), param("limit").unwrap_or(1000) as usize);
        let state = self.state.lock().unwrap();
        let mut page: Vec<&Value> = state.1.iter().filter(|r| r["seq"].as_u64().unwrap() > since).collect();
        page.sort_by_key(|r| r["seq"].as_u64().unwrap());
        let mut more = page.len() > limit;
        page.truncate(limit);
        // The Worker also ends a page at a payload byte budget, keeping at
        // least one row (cloud/api/src/logic/pull.rs).
        if let Some(budget) = *self.page_bytes.lock().unwrap() {
            let (mut bytes, mut keep) = (0, 0);
            for row in &page {
                bytes += row["payload"].as_str().unwrap().len();
                if keep > 0 && bytes > budget { break; }
                keep += 1;
            }
            if keep < page.len() {
                page.truncate(keep);
                more = true;
            }
        }
        let cursor = page.last().map(|r| r["seq"].as_u64().unwrap()).unwrap_or(since);
        let sync_epoch = *self.sync_epoch.lock().unwrap();
        Some(Reply::json(200, json!({ "changes": page, "cursor": cursor, "more": more, "sync_epoch": sync_epoch }).to_string()))
    }

    /// Every stored row exactly as the server holds it.
    pub fn rows(&self) -> Vec<serde_json::Value> {
        self.state.lock().unwrap().1.clone()
    }

    pub fn cursor(&self) -> u64 {
        self.state.lock().unwrap().0
    }

    pub fn sync_epoch(&self) -> u64 {
        *self.sync_epoch.lock().unwrap()
    }

    /// Answer `DELETE /v1/sync/data` with 404, as a Worker without the route.
    pub fn lack_delete(&self) {
        self.lacks_delete.store(true, Ordering::SeqCst);
    }

    /// Act as a hostile or buggy server: edit or insert one stored row and give
    /// it a new sequence number so devices pull it.
    pub fn rewrite(&self, kind: &str, id: &str, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut state = self.state.lock().unwrap();
        let index = state.1.iter().position(|r| r["kind"] == kind && r["id"] == id).unwrap_or_else(|| {
            state.1.push(serde_json::json!({ "kind": kind, "id": id, "hlc": "0000000000001-000000-server", "deleted": false, "enc": "json", "payload": "", "device_id": "server" }));
            state.1.len() - 1
        });
        state.0 += 1;
        let seq = state.0;
        edit(&mut state.1[index]);
        state.1[index]["seq"] = serde_json::json!(seq);
    }

    /// End pull pages at this many payload bytes, as the Worker does at 4 MiB.
    pub fn page_bytes(&self, budget: usize) {
        *self.page_bytes.lock().unwrap() = Some(budget);
    }

    /// Answer pushes containing this id with a permanent `bad_request`.
    pub fn reject_id(&self, id: &str) {
        self.rejected_ids.lock().unwrap().push(id.into());
    }
}
