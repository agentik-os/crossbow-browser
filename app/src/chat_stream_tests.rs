use super::*;
use super::tests::{CHAT_TEST, configured, configured_store};
use crate::ai::SettingsDraft;
use crossbow_ai::local::LocalEndpoint;
use crossbow_core::store::Store;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(8);
pub(crate) const PARTIAL: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Synthetic partial\"},\"finish_reason\":null}]}\n\n";
pub(crate) const COMPLETE: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" answer\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
const INCOMPLETE: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";

/// Each explicit release allows exactly one response fragment. All sockets and
/// channel waits are finite; the fixture has no credentials or browser data.
pub(crate) struct StreamServer {
    pub(crate) endpoint: LocalEndpoint,
    request: mpsc::Receiver<String>,
    pub(crate) release: mpsc::Sender<()>,
    handle: std::thread::JoinHandle<()>,
}

impl StreamServer {
    pub(crate) fn new(replies: &[&[&str]]) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = LocalEndpoint { port: listener.local_addr().unwrap().port(), ..Default::default() };
        let replies: Vec<Vec<String>> = replies.iter().map(|parts| parts.iter().map(|part| (*part).to_owned()).collect()).collect();
        let (sent, request) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            for parts in replies {
                let until = Instant::now() + DEADLINE;
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => std::thread::sleep(Duration::from_millis(2)),
                        Err(error) => panic!("stream fixture accept failed: {error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(DEADLINE)).unwrap();
                socket.set_write_timeout(Some(DEADLINE)).unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0 && request.len() + count <= 131072);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let head = std::str::from_utf8(&request[..end]).unwrap();
                        let length = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|value| value.parse::<usize>().ok())).unwrap();
                        if request.len() >= end + 4 + length { break; }
                    }
                }
                sent.send(String::from_utf8(request).unwrap()).unwrap();
                let length: usize = parts.iter().map(String::len).sum();
                let header = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
                let _ = socket.write_all(header.as_bytes());
                for part in parts {
                    // Consume every release even when a cancellation closes the
                    // socket, so one request cannot consume the next one's gate.
                    wait.recv_timeout(DEADLINE).unwrap();
                    let _ = socket.write_all(part.as_bytes());
                }
            }
        });
        Self { endpoint, request, release, handle }
    }

    pub(crate) fn received(&self) -> String { self.request.recv_timeout(DEADLINE).unwrap() }
    pub(crate) fn no_request(&self) -> bool { matches!(self.request.try_recv(), Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected)) }
    pub(crate) fn join(self) { self.handle.join().unwrap(); }
}

fn payload(request: &str) -> serde_json::Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn until(chat: &mut Session, lib: &Library, space: &str, predicate: impl Fn(&Session) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !predicate(chat) {
        chat.poll(lib, space, &None);
        assert!(Instant::now() < deadline, "stream session did not settle: {:?}", chat.error);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Observe every worker repaint, including its terminal wake after permit
/// release. A fixture socket closing alone does not establish client teardown.
pub(crate) struct WorkerWakes(mpsc::Receiver<()>);

impl WorkerWakes {
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        let (sent, received) = mpsc::channel();
        ctx.set_repaint_observer(move |_| {
            if std::thread::current().name() == Some("crossbow-ai-chat") { let _ = sent.send(()); }
        });
        Self(received)
    }

    pub(crate) fn wait(&self) { self.0.recv_timeout(DEADLINE).unwrap(); }
}

// Cancellation releases capacity only after the worker actually stops. A test
// beginning the next explicit request must allow that teardown, not weaken it.
fn send(chat: &mut Session, lib: &Library, space: &str) {
    send_with_context(chat, lib, space, &egui::Context::default());
}

fn send_with_context(chat: &mut Session, lib: &Library, space: &str, ctx: &egui::Context) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        chat.send(lib, space, &None, ctx);
        if chat.busy() { return; }
        assert_eq!(chat.error.as_deref(), Some(LocalError::Busy.to_string().as_str()));
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn streamed_success_and_failed_retry_keep_only_completed_turns_and_no_persistence() {
    let _serial = CHAT_TEST.lock().unwrap();
    let server = StreamServer::new(&[&[PARTIAL, COMPLETE], &[PARTIAL, INCOMPLETE], &[PARTIAL, COMPLETE]]);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("stream.sqlite");
    let (lib, space, mut chat) = configured_store(Store::open(&path).unwrap(), server.endpoint);
    let profile = chat.profile.clone();
    assert!(!chat.stream);
    chat.stream = true;
    chat.draft = "Explicit synthetic prompt".into();
    send(&mut chat, &lib, &space);
    let first = payload(&server.received());
    assert_eq!(first["stream"], true);
    assert_eq!(first["messages"], serde_json::json!([{"role":"user","content":"Explicit synthetic prompt"}]));
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.partial.is_empty());
    assert!(chat.busy() && chat.messages.is_empty());
    assert_eq!(chat.partial, "Synthetic partial");
    assert_eq!(chat.draft, "Explicit synthetic prompt");
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.busy());
    assert!(chat.error.is_none() && chat.partial.is_empty() && chat.draft.is_empty());
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[1].content, "Synthetic partial answer");

    chat.draft = "Synthetic follow-up".into();
    send(&mut chat, &lib, &space);
    let followup = payload(&server.received());
    assert_eq!(followup["messages"].as_array().unwrap().len(), 3);
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.partial.is_empty());
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.busy());
    assert!(chat.error.is_some());
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.partial, "Synthetic partial");
    assert_eq!(chat.draft, "Synthetic follow-up");
    assert!(server.no_request(), "a failed stream must not trigger a retry or fallback");

    send(&mut chat, &lib, &space);
    assert!(chat.partial.is_empty());
    assert_eq!(payload(&server.received()), followup, "retry must exclude the incomplete answer");
    server.release.send(()).unwrap();
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.busy());
    assert_eq!(chat.messages.len(), 4);
    assert!(chat.partial.is_empty() && chat.error.is_none());
    server.join();
    drop(chat);
    drop(lib);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let rows: i64 = conn.query_row("SELECT count(*) FROM kv WHERE v LIKE '%Synthetic partial%' OR v LIKE '%Explicit synthetic%' OR v LIKE '%Synthetic follow-up%'", [], |row| row.get(0)).unwrap();
    assert_eq!(rows, 0);
    let mut reopened = Library::open(Store::open(&path).unwrap()).unwrap();
    let chat = Session::load(&mut reopened, &profile, &None);
    assert!(!chat.stream && chat.partial.is_empty() && chat.messages.is_empty() && chat.draft.is_empty());
    assert_eq!(chat.selection.unwrap().model.as_deref(), Some("fixture-model"));
}

#[test]
fn review_stream_server_join_does_not_release_a_delayed_client_worker() {
    let _serial = CHAT_TEST.lock().unwrap();
    let server = StreamServer::new(&[&[PARTIAL, COMPLETE]]);
    let endpoint = server.endpoint;
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (ended, terminal) = mpsc::channel();
    let first = std::cell::Cell::new(true);
    let task = ChatTask::spawn_streaming(endpoint, "fixture-model".into(), vec![Message::user("Synthetic teardown probe")], move || {
        if first.replace(false) {
            entered.send(()).unwrap();
            released.recv_timeout(DEADLINE).unwrap();
        } else {
            ended.send(()).unwrap();
        }
    }).unwrap();
    server.received();
    server.release.send(()).unwrap();
    entry.recv_timeout(DEADLINE).unwrap();
    task.cancel();
    server.release.send(()).unwrap();
    server.join();
    let blocked = matches!(ChatTask::spawn(endpoint, "fixture-model".into(), vec![Message::user("Must not start")], || {}), Err(LocalError::Busy));
    release.send(()).unwrap();
    terminal.recv_timeout(DEADLINE).unwrap();
    assert!(blocked, "joining the fixture must not bypass the delayed client's permit");
    assert_eq!(task.poll(), Some(Err(LocalError::Cancelled)));
}

#[test]
fn review_stream_invalidation_keeps_capacity_until_client_teardown() {
    let _serial = CHAT_TEST.lock().unwrap();
    let server = StreamServer::new(&[&[PARTIAL, COMPLETE]]);
    let (lib, space, mut chat) = configured(server.endpoint);
    let ctx = egui::Context::default();
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (ended, terminal) = mpsc::channel();
    let released = std::sync::Mutex::new(released);
    let first = std::sync::atomic::AtomicBool::new(true);
    ctx.set_repaint_observer(move |_| {
        if std::thread::current().name() != Some("crossbow-ai-chat") { return; }
        if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            entered.send(()).unwrap();
            released.lock().unwrap().recv_timeout(DEADLINE).unwrap();
        } else {
            ended.send(()).unwrap();
        }
    });
    chat.stream = true;
    chat.draft = "Synthetic invalidation teardown probe".into();
    chat.send(&lib, &space, &None, &ctx);
    assert!(chat.busy());
    server.received();
    server.release.send(()).unwrap();
    entry.recv_timeout(DEADLINE).unwrap();
    // This session has never rendered an editor, so invalidation makes no
    // Context call while the deliberately delayed observer holds its lock.
    chat.poll(&lib, &space, &Some(("fixture-api".into(), "other-account".into())));
    assert!(!chat.busy() && chat.selection.is_none());
    assert!(chat.draft.is_empty() && chat.partial.is_empty() && chat.messages.is_empty());
    let endpoint = server.endpoint;
    server.release.send(()).unwrap();
    server.join();
    let blocked = matches!(ChatTask::spawn(endpoint, "fixture-model".into(), vec![Message::user("Must not start")], || {}), Err(LocalError::Busy));
    release.send(()).unwrap();
    terminal.recv_timeout(DEADLINE).unwrap();
    assert!(blocked, "invalidating a session and joining its server does not release the client's permit");
}

#[test]
fn cancelled_stream_keeps_only_labeled_partial_until_explicit_retry() {
    let _serial = CHAT_TEST.lock().unwrap();
    let server = StreamServer::new(&[&[PARTIAL, COMPLETE], &[PARTIAL, COMPLETE]]);
    let (lib, space, mut chat) = configured(server.endpoint);
    chat.stream = true;
    chat.draft = "Cancel this synthetic turn".into();
    send(&mut chat, &lib, &space);
    let original = payload(&server.received());
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.partial.is_empty());
    chat.cancel();
    assert!(!chat.busy() && chat.messages.is_empty());
    assert_eq!(chat.partial, "Synthetic partial");
    assert_eq!(chat.error.as_deref(), Some(LocalError::Cancelled.to_string().as_str()));
    server.release.send(()).unwrap();
    for _ in 0..4 { chat.poll(&lib, &space, &None); }
    assert!(chat.messages.is_empty() && server.no_request());
    send(&mut chat, &lib, &space);
    assert!(chat.partial.is_empty());
    assert_eq!(payload(&server.received()), original);
    server.release.send(()).unwrap();
    server.release.send(()).unwrap();
    until(&mut chat, &lib, &space, |chat| !chat.busy());
    assert_eq!(chat.messages.len(), 2);
    assert!(chat.error.is_none());
    server.join();
}

#[test]
fn stream_progress_revalidates_saved_destination_profile_and_account() {
    let _serial = CHAT_TEST.lock().unwrap();
    for boundary in ["destination", "deleted-profile", "space", "account"] {
        let server = StreamServer::new(&[&[PARTIAL, PARTIAL, COMPLETE]]);
        let (mut lib, space, mut chat) = configured(server.endpoint);
        let ctx = egui::Context::default();
        let wakes = WorkerWakes::new(&ctx);
        chat.stream = true;
        chat.draft = "Keep inside the original owner".into();
        send_with_context(&mut chat, &lib, &space, &ctx);
        server.received();
        server.release.send(()).unwrap();
        until(&mut chat, &lib, &space, |chat| !chat.partial.is_empty());
        wakes.wait(); // First progress was consumed before releasing the next.
        let account = if boundary == "account" { Some(("fixture-api".into(), "other-account".into())) } else { None };
        match boundary {
            "destination" => {
                let mut settings = SettingsDraft::load(&lib, &chat.profile);
                settings.set_model(Some("other-model".into()));
                settings.save(&mut lib, Some(&chat.profile));
                assert!(settings.error.is_none());
            }
            "deleted-profile" => {
                let row = lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == "profile" && row.id == chat.profile).unwrap();
                lib.store.put_entity(&crossbow_core::store::EntityRow { deleted: true, ..row }, false).unwrap();
            }
            "space" => lib.spaces.get_mut(&space).unwrap().profile_id.clear(),
            _ => {},
        }
        server.release.send(()).unwrap();
        wakes.wait(); // Second progress precedes invalidation and terminal wake.
        let deadline = Instant::now() + DEADLINE;
        while chat.busy() {
            chat.poll(&lib, &space, &account);
            assert!(Instant::now() < deadline, "ownership change did not invalidate progress: {boundary}");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(chat.selection.is_none() && chat.partial.is_empty() && chat.messages.is_empty() && chat.draft.is_empty(), "retained content after {boundary}");
        assert!(!chat.stream);
        server.release.send(()).unwrap();
        server.join();
        wakes.wait(); // Keep CHAT_TEST until the invalidated client's permit is gone.
    }
}

#[test]
fn leaving_stream_chat_discards_partial_and_choice_with_no_new_request() {
    let (mut lib, space, mut chat) = configured(LocalEndpoint::default());
    let profile = chat.profile.clone();
    chat.stream = true;
    chat.partial = "An incomplete synthetic answer".into();
    let mut slot = Some(chat);
    reconcile(&mut slot, &lib, &space, &None, false);
    assert!(slot.is_none());
    let reopened = Session::load(&mut lib, &profile, &None);
    assert!(!reopened.stream && reopened.partial.is_empty() && reopened.messages.is_empty());
}
