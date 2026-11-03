//! Encrypted library sync through the real worker, store and loopback service.
//! Each device runs the same outgoing/apply_page calls as the app's sync glue.

use super::*;
use super::test_server::SyncService;
use crate::sync::test_secrets::{Hidden, failure};
use crate::library::Library;
use crate::model::Boost;
use crate::store::Store;
use crate::sync::e2ee::{Binding, E2eeStatus, LibraryEncryption, LibraryKey, MARKER_ID, MARKER_KIND, MARKER_PLAINTEXT, PageError, RecoveryCode, seal};
use crate::sync::key_store::{KeyStore, MemoryKeyStore};
use data_encoding::BASE64URL_NOPAD;

const ACCOUNT: &str = "fixture-account";
/// Synthetic strings that must never reach the server once encrypted.
const SECRETS: [&str; 5] = ["plaintext-marker.example", "Plaintext Marker Title", "plaintext-marker-css", "Marker Folder", "crossbow-library-e2ee-v1"];

struct Device {
    lib: Library,
    keys: MemoryKeyStore,
    e2ee: LibraryEncryption,
    worker: Worker,
    events: std_mpsc::Receiver<(u64, SyncEvent)>,
    path: std::path::PathBuf,
    _server: Server,
    _dir: tempfile::TempDir,
}

impl Device {
    fn new(service: &Arc<SyncService>, name: &str) -> Device {
        let (shared, label) = (service.clone(), name.to_string());
        let server = Server::start(move |request| shared.reply(request, &label).unwrap_or_else(|| panic!("unexpected fixture request: {}", request.path)));
        let (worker, events) = worker_at(&server.url);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let keys = MemoryKeyStore::default();
        let (e2ee, warning) = LibraryEncryption::load(ACCOUNT, &lib, &keys);
        assert!(warning.is_none());
        Device { lib, keys, e2ee, worker, events, path, _server: server, _dir: dir }
    }

    fn cursor(&self) -> u64 {
        self.worker.live.as_ref().unwrap().cursor
    }

    /// Pull until caught up, acknowledging each page only after it commits.
    async fn pull(&mut self) -> Result<usize, PageError> {
        self.worker.pull_all().await;
        let mut applied = 0;
        while let Some((batch_id, changes, cursor, more)) = self.events.try_iter().find_map(|(_, e)| match e {
            SyncEvent::Pulled { batch_id, changes, cursor, more } => Some((batch_id, changes, cursor, more)),
            _ => None,
        }) {
            match self.e2ee.apply_page(&mut self.lib, &changes, cursor, more) {
                Ok(count) => {
                    applied += count;
                    self.worker.acknowledge_pull(batch_id, true).await;
                }
                Err(error) => {
                    self.worker.acknowledge_pull(batch_id, false).await;
                    // As the glue does: look past a refused page.
                    if self.e2ee.needs_scan() { self.scan().await; }
                    return Err(error);
                }
            }
        }
        Ok(applied)
    }

    /// Read the stream to its end without applying anything, as the glue does
    /// while locked or unreadable and before a reset is confirmed.
    async fn scan(&mut self) {
        self.worker.scan_ahead().await;
        for (_, event) in self.events.try_iter() {
            match event {
                SyncEvent::Scanned { changes, more } => self.e2ee.scan_page(&self.lib, &changes, more),
                SyncEvent::ScanFailed(error) => panic!("fixture scan failed: {error}"),
                _ => {}
            }
        }
    }

    /// Push one outgoing page and map results back by kind, id and clock.
    async fn push(&mut self) -> usize {
        let changes = self.e2ee.outgoing(&self.lib, MAX_PUSH_CHANGES).unwrap();
        let sent = changes.len();
        if sent > 0 { self.worker.push(changes).await; }
        for (_, event) in self.events.try_iter() {
            match event {
                SyncEvent::Pushed { accepted, .. } => {
                    for c in accepted { self.lib.store.mark_clean(&c.kind, &c.id, &c.hlc).unwrap(); }
                }
                SyncEvent::PushRejected { change, reason } => self.lib.store.reject_sync_change(&change, &reason).unwrap(),
                SyncEvent::PushFailed(error) => panic!("fixture push failed: {error}"),
                _ => {}
            }
        }
        sent
    }

    async fn sync(&mut self) -> Result<(), PageError> {
        self.pull().await?;
        self.push().await;
        self.pull().await.map(|_| ())
    }

    fn begin(&mut self) -> Result<(), String> {
        self.e2ee.begin(&self.lib, &self.keys)
    }

    fn confirm(&mut self) -> Result<(), String> {
        self.e2ee.confirm(&mut self.lib)
    }

    /// The visible note a refused or partly ignored page leaves.
    fn notice(&self) -> Option<String> {
        self.e2ee.notice().map(str::to_string)
    }

    /// The recovery code this device holds, as tests compare it.
    fn code(&self) -> Option<Hidden<String>> {
        self.e2ee.recovery_code().as_ref().map(Hidden::from)
    }

    /// Turn on as the UI does: begin, one fresh pull, then confirm.
    async fn turn_on(&mut self) -> RecoveryCode {
        self.begin().unwrap();
        let code = self.e2ee.recovery_code().unwrap();
        self.pull().await.unwrap();
        self.confirm().unwrap();
        code
    }

    fn add_marker_content(&mut self) -> (String, String, String) {
        let space = self.lib.spaces_ordered()[0].0.clone();
        let folder = self.lib.create_folder(&space, None, "Marker Folder").unwrap();
        let item = self.lib.add_pinned(&space, Some(&folder), "https://plaintext-marker.example/private", "Plaintext Marker Title").unwrap();
        let boost = self.lib.put_boost(None, &Boost { name: "Marker Boost".into(), host: "plaintext-marker.example".into(), css: "body { --plaintext-marker-css: 1 }".into(), js: String::new(), enabled: true }).unwrap();
        (folder, item, boost)
    }
}

fn same_library(a: &Library, b: &Library) {
    assert_eq!(a.profiles, b.profiles);
    assert_eq!(a.spaces, b.spaces);
    assert_eq!(a.folders, b.folders);
    assert_eq!(a.items, b.items);
    assert_eq!(a.boosts, b.boosts);
    assert_eq!(a.settings, b.settings);
}

fn server_text(service: &SyncService) -> String {
    service.rows().iter().map(|r| r["payload"].as_str().unwrap().to_string()).collect::<Vec<_>>().join("\n")
}

fn assert_ciphertext_only(service: &SyncService) {
    let text = server_text(service);
    for secret in SECRETS {
        assert!(!text.contains(secret), "server observed plaintext {secret:?}");
    }
    for row in service.rows() {
        if row["deleted"] == false {
            assert_eq!(row["enc"], "xchacha20_v1", "live row stored unencrypted: {row}");
        }
    }
}

#[tokio::test]
async fn turn_on_re_encrypts_everything_and_a_second_device_unlocks_the_same_library() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, item, boost) = a.add_marker_content();
    a.sync().await.unwrap();
    // Before encryption the fixture really can observe plaintext.
    assert!(SECRETS[..4].iter().all(|secret| server_text(&service).contains(secret)));
    let before: Vec<_> = service.rows().iter().map(|r| (r["kind"].clone(), r["id"].clone(), r["hlc"].clone())).collect();

    let code = a.turn_on().await;
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    a.sync().await.unwrap();
    assert_eq!(a.lib.store.dirty_count().unwrap(), 0, "accepted ciphertext must mark the original rows clean");
    assert_ciphertext_only(&service);
    for (kind, id, hlc) in before {
        let row = service.rows().into_iter().find(|r| r["kind"] == kind && r["id"] == id).unwrap();
        assert_ne!(row["hlc"], hlc, "{kind} {id} kept its plaintext version");
    }
    assert!(service.rows().iter().any(|r| r["kind"] == MARKER_KIND && r["id"] == MARKER_ID));

    // A new device without the code locks, applies nothing and pushes nothing.
    let mut b = Device::new(&service, "device-b");
    let b_space = b.lib.spaces_ordered()[0].0.clone();
    b.lib.update_space(&b_space, |s| s.name = "Device B Only".into()).unwrap();
    let rows_before = service.rows().len();
    assert_eq!(b.pull().await, Err(PageError::Locked));
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert_eq!(b.cursor(), 0);
    assert_eq!(b.lib.store.kv_get("sync_cursor").unwrap(), None);
    assert!(!b.lib.folders.contains_key(&folder) && !b.lib.items.contains_key(&item));
    assert_eq!(b.push().await, 0);
    assert_eq!(service.rows().len(), rows_before);
    assert!(!server_text(&service).contains("Device B Only"));
    assert_eq!(b.pull().await, Err(PageError::Locked), "locked devices keep refusing pages");
    assert_eq!(b.cursor(), 0);

    let wrong = RecoveryCode::for_key(&crate::sync::e2ee::LibraryKey::generate().unwrap());
    assert!(b.e2ee.unlock(wrong.as_str(), &b.lib, &b.keys).is_err());
    assert!(b.keys.load(ACCOUNT).unwrap().is_none(), "a wrong code stores nothing");
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    b.e2ee.unlock(&code.as_str().to_ascii_lowercase().replace('-', " "), &b.lib, &b.keys).unwrap();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);

    b.sync().await.unwrap();
    assert_eq!(b.cursor(), service.cursor());
    assert!(b.lib.folders.contains_key(&folder) && b.lib.items.contains_key(&item) && b.lib.boosts.contains_key(&boost));
    a.sync().await.unwrap();
    b.pull().await.unwrap();
    same_library(&a.lib, &b.lib);
    assert!(a.lib.spaces.values().any(|s| s.name == "Device B Only"));
    assert_ciphertext_only(&service);

    // The reconstructed library and its unlocked state survive reopening.
    let path = b.path.clone();
    let keys = std::mem::take(&mut b.keys);
    let reopened = Library::open(Store::open(&path).unwrap()).unwrap();
    same_library(&a.lib, &reopened);
    let (state, _) = LibraryEncryption::load(ACCOUNT, &reopened, &keys);
    assert_eq!(state.status(), E2eeStatus::On);
}

#[tokio::test]
async fn tampered_replayed_and_moved_winning_rows_are_skipped_with_a_notice_and_never_applied() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, item, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    let title = b.lib.items[&item].title.clone();
    let original = service.rows().into_iter().find(|r| r["id"] == item.as_str()).unwrap();
    // Only a version newer than the device's own can change it, so that is
    // what a hostile server has to send.
    let newer = serde_json::json!(Hlc::new(crate::now_ms() + 60_000, 0, "server").encode());
    let skipped = "1 synced item couldn't be decrypted on this device and wasn't applied. It may come from a device still using an earlier recovery code, or it may have been altered.";

    // One flipped ciphertext character, in the same page as a genuine edit.
    let space = a.lib.spaces_ordered()[0].0.clone();
    a.lib.update_space(&space, |s| s.name = "Renamed Beside A Tampered Row".into()).unwrap();
    a.push().await;
    service.rewrite("item", &item, |row| {
        let mut payload = row["payload"].as_str().unwrap().to_string().into_bytes();
        let last = payload.len() - 3;
        payload[last] = if payload[last] == b'A' { b'B' } else { b'A' };
        row["payload"] = serde_json::json!(String::from_utf8(payload).unwrap());
        row["hlc"] = newer.clone();
    });
    b.pull().await.unwrap();
    assert_eq!(b.cursor(), service.cursor(), "the rest of the page applies and the cursor moves on");
    assert_eq!(b.lib.store.kv_get("sync_cursor").unwrap(), Some(service.cursor().to_string()));
    assert_eq!(b.lib.items[&item].title, title, "the altered row is never applied");
    assert_eq!(b.lib.spaces[&space].name, "Renamed Beside A Tampered Row");
    assert_eq!(b.notice().as_deref(), Some(skipped));
    assert_eq!(b.lib.store.dirty_count().unwrap(), 0, "a row it can't read is not superseded either");

    // Authentic ciphertext replayed under a newer clock: the clock is bound.
    service.rewrite("item", &item, |row| row["payload"] = original["payload"].clone());
    b.pull().await.unwrap();
    assert_eq!(b.lib.items[&item].title, title);
    // The notice counts every skipped version since it was last dismissed.
    assert_eq!(b.notice().as_deref(), Some("2 synced items couldn't be decrypted on this device and weren't applied. They may come from a device still using an earlier recovery code, or they may have been altered."));
    b.e2ee.dismiss_notice(&b.lib).unwrap();
    assert_eq!(b.notice(), None);

    // Authentic ciphertext moved onto another row is also skipped.
    let folder_payload = service.rows().into_iter().find(|r| r["id"] == folder.as_str()).unwrap()["payload"].clone();
    service.rewrite("item", &item, |row| row["payload"] = folder_payload);
    a.pull().await.unwrap();
    b.pull().await.unwrap();
    assert_eq!((a.lib.items[&item].title.clone(), b.lib.items[&item].title.clone()), (title.clone(), title.clone()));
    assert_eq!(a.notice().as_deref(), Some(skipped));
    assert_eq!(b.cursor(), service.cursor());

    // A sealed marker this device can't open still locks it.
    let forged_hlc = Hlc::new(crate::now_ms() + 120_000, 0, "server");
    let forged = seal(&LibraryKey::generate().unwrap().library_cipher(), &Binding { account_id: ACCOUNT, kind: MARKER_KIND, id: MARKER_ID, hlc: &forged_hlc }, MARKER_PLAINTEXT).unwrap();
    service.rewrite(MARKER_KIND, MARKER_ID, |row| {
        row["enc"] = serde_json::json!("xchacha20_v1");
        row["payload"] = serde_json::json!(forged);
        row["hlc"] = serde_json::json!(forged_hlc.encode());
    });
    let cursor = b.cursor();
    assert_eq!(b.pull().await, Err(PageError::Locked));
    assert_eq!(b.cursor(), cursor);
}

#[tokio::test]
async fn a_remote_marker_tombstone_never_disables_encryption() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    service.rewrite(MARKER_KIND, MARKER_ID, |row| {
        row["deleted"] = serde_json::json!(true);
        row["payload"] = serde_json::json!("");
        row["enc"] = serde_json::json!("json");
        row["hlc"] = serde_json::json!(Hlc::new(crate::now_ms() + 60_000, 0, "server").encode());
    });
    a.pull().await.unwrap();
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    assert!(a.lib.store.kv_get("e2ee_library").unwrap().is_some());
    // The device puts its marker back above the deletion.
    a.push().await;
    let marker = service.rows().into_iter().find(|r| r["kind"] == MARKER_KIND).unwrap();
    assert_eq!((marker["deleted"].clone(), marker["enc"].clone()), (serde_json::json!(false), serde_json::json!("xchacha20_v1")));
    let space = a.lib.spaces_ordered()[0].0.clone();
    a.lib.update_space(&space, |s| s.name = "Renamed After Tombstone".into()).unwrap();
    a.push().await;
    assert!(!server_text(&service).contains("Renamed After Tombstone"));

    // Without a live marker, a new device still locks on any sealed row and
    // verifies the code against it.
    service.rewrite(MARKER_KIND, MARKER_ID, |row| {
        row["deleted"] = serde_json::json!(true);
        row["payload"] = serde_json::json!("");
        row["enc"] = serde_json::json!("json");
        row["hlc"] = serde_json::json!(Hlc::new(crate::now_ms() + 120_000, 0, "server").encode());
    });
    let mut c = Device::new(&service, "device-c");
    assert_eq!(c.pull().await, Err(PageError::Locked));
    assert!(c.e2ee.unlock(RecoveryCode::for_key(&crate::sync::e2ee::LibraryKey::generate().unwrap()).as_str(), &c.lib, &c.keys).is_err());
    c.e2ee.unlock(code.as_str(), &c.lib, &c.keys).unwrap();
    c.pull().await.unwrap();
    assert!(c.lib.spaces.values().any(|s| s.name == "Renamed After Tombstone"));
}

#[tokio::test]
async fn cancel_keeps_json_and_sign_out_keeps_the_account_locked_until_the_code_is_entered_again() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.add_marker_content();
    assert_eq!(a.push().await, 0, "nothing is pushed before the first pull says whether the account is encrypted");
    a.sync().await.unwrap();
    a.e2ee.begin(&a.lib, &a.keys).unwrap();
    a.e2ee.cancel(&a.keys).unwrap();
    let space = a.lib.spaces_ordered()[0].0.clone();
    a.lib.update_space(&space, |s| s.name = "Plaintext Marker Title After Cancel".into()).unwrap();
    a.sync().await.unwrap();
    assert!(service.rows().iter().all(|r| r["enc"] == "json"));
    assert!(server_text(&service).contains("Plaintext Marker Title After Cancel"));
    assert!(a.keys.load(ACCOUNT).unwrap().is_none());
    assert!(!service.rows().iter().any(|r| r["kind"] == MARKER_KIND));

    let code = a.turn_on().await;
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);
    LibraryEncryption::forget_key(ACCOUNT, &a.keys).unwrap();
    assert!(a.keys.load(ACCOUNT).unwrap().is_none());
    // Signing in again starts from cursor 0, Locked, and needs the code again.
    let (mut again, _) = LibraryEncryption::load(ACCOUNT, &a.lib, &a.keys);
    assert_eq!(again.status(), E2eeStatus::Locked);
    let (worker, events) = worker_at(&a._server.url);
    a.worker = worker;
    a.events = events;
    std::mem::swap(&mut a.e2ee, &mut again);
    assert_eq!(a.pull().await, Err(PageError::Locked));
    assert_eq!(a.push().await, 0);
    a.e2ee.unlock(code.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn stale_and_rejected_ciphertext_map_back_to_the_original_local_rows() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (_, item, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    let _ = b.pull().await;
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();

    // Concurrent edits: B's later clock wins; A's push is stale and A converges.
    a.lib.update_item(&item, |i| i.title = "Edited On A".into()).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    b.lib.update_item(&item, |i| i.title = "Edited On B".into()).unwrap();
    b.push().await;
    a.push().await;
    assert_eq!(a.lib.store.dirty_count().unwrap(), 1, "a stale version is not marked clean");
    a.pull().await.unwrap();
    assert_eq!(a.lib.items[&item].title, "Edited On B");
    assert_eq!(a.lib.store.dirty_count().unwrap(), 0);

    // A permanent server rejection pauses exactly that local version.
    service.reject_id(&item);
    a.lib.update_item(&item, |i| i.title = "Rejected Edit".into()).unwrap();
    a.push().await;
    let reason = a.lib.store.sync_rejection_reason().unwrap().expect("rejected ciphertext must pause its local row");
    assert!(reason.contains("kept locally"));
    assert!(a.e2ee.outgoing(&a.lib, MAX_PUSH_CHANGES).unwrap().is_empty());
    assert_eq!(a.lib.items[&item].title, "Rejected Edit");

    // Data that fits as plaintext but not once sealed is paused locally too.
    let boost = Boost { name: "Large".into(), host: "large.example".into(), css: "x".repeat(50_000), js: String::new(), enabled: true };
    assert!(serde_json::to_string(&boost).unwrap().len() < MAX_PAYLOAD_BYTES);
    let id = a.lib.put_boost(None, &boost).unwrap();
    a.lib.store.retry_sync_rejections().unwrap();
    assert!(a.e2ee.outgoing(&a.lib, MAX_PUSH_CHANGES).unwrap().iter().all(|c| c.id != id));
    assert!(a.lib.store.sync_rejection_reason().unwrap().unwrap().contains("64 KiB"));
    assert!(!service.rows().iter().any(|r| r["id"] == id.as_str()));
    assert_eq!(a.lib.boosts[&id], boost);
}

#[tokio::test]
async fn a_synced_row_too_large_to_encrypt_blocks_turn_on_until_it_is_shrunk() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut boost = Boost { name: "Large".into(), host: "large.example".into(), css: "x".repeat(50_000), js: String::new(), enabled: true };
    assert!(serde_json::to_string(&boost).unwrap().len() < MAX_PAYLOAD_BYTES);
    let id = a.lib.put_boost(None, &boost).unwrap();
    a.sync().await.unwrap();
    assert!(service.rows().iter().any(|r| r["id"] == id.as_str() && r["enc"] == "json"), "the large row synced as plaintext");

    let refused = a.begin().unwrap_err();
    assert_eq!(refused, "1 item is too large to encrypt (Boost). Shrink or delete it, then try again.");
    assert_eq!(a.e2ee.status(), E2eeStatus::Off);
    assert!(a.keys.load(ACCOUNT).unwrap().is_none());

    // Shrunk, it turns on and every live row on the server becomes ciphertext.
    boost.css = "body { color: red; }".into();
    a.lib.put_boost(Some(&id), &boost).unwrap();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);

    // A row that grows while the code is on screen stops Confirm, not Cancel.
    let service = Arc::new(SyncService::default());
    let mut b = Device::new(&service, "device-b");
    b.sync().await.unwrap();
    b.begin().unwrap();
    b.pull().await.unwrap();
    let boost = Boost { css: "x".repeat(50_000), ..boost };
    b.lib.put_boost(None, &boost).unwrap();
    b.lib.put_boost(None, &boost).unwrap();
    assert_eq!(b.confirm().unwrap_err(), "2 items are too large to encrypt (Boost). Shrink or delete them, then try again.");
    assert_eq!(b.e2ee.status(), E2eeStatus::Confirming);
    assert_eq!(b.lib.store.kv_get("e2ee_library").unwrap(), None);
    assert_eq!(b.lib.store.entity_payload(MARKER_KIND, MARKER_ID).unwrap(), None);
    b.e2ee.cancel(&b.keys).unwrap();
    assert_eq!(b.e2ee.status(), E2eeStatus::Off);
    assert!(b.keys.load(ACCOUNT).unwrap().is_none());
}

#[tokio::test]
async fn the_devices_own_plaintext_echo_after_turn_on_is_skipped_not_refused() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    a.begin().unwrap();
    a.pull().await.unwrap();
    // Pushed while the code is on screen, so the cursor is behind its echo.
    let space = a.lib.spaces_ordered()[0].0.clone();
    a.lib.update_space(&space, |s| s.name = "Renamed While Confirming".into()).unwrap();
    assert_eq!(a.push().await, 1);
    a.confirm().unwrap();
    let cursor = a.cursor();
    assert!(a.pull().await.is_ok(), "an obsolete plaintext version is harmless");
    assert!(a.cursor() > cursor);
    assert_eq!(a.lib.spaces[&space].name, "Renamed While Confirming");
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn an_unencrypted_winner_is_superseded_by_the_sealed_local_version_and_never_applied() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (_, item, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    let title = a.lib.items[&item].title.clone();

    // A legacy client (or the server) writes newer plaintext versions.
    let injected = Hlc::new(crate::now_ms() + 60_000, 0, "legacy");
    let plaintext = serde_json::to_string(&crate::model::Item { title: "Injected Title".into(), ..a.lib.items[&item].clone() }).unwrap();
    service.rewrite("item", &item, |row| {
        row["enc"] = serde_json::json!("json");
        row["hlc"] = serde_json::json!(injected.encode());
        row["payload"] = serde_json::json!(plaintext);
    });
    service.rewrite("item", "injected-plaintext", |row| {
        row["hlc"] = serde_json::json!(injected.encode());
        row["payload"] = serde_json::json!(r#"{"kind":"pinned","url":"https://injected.example/","title":"Injected"}"#);
    });
    let cursor = a.cursor();
    assert!(a.pull().await.is_ok(), "the page is not refused");
    assert_eq!(a.cursor(), service.cursor(), "the cursor moves past ignored rows");
    assert_eq!(a.lib.items[&item].title, title, "plaintext is never applied");
    assert!(!a.lib.items.contains_key("injected-plaintext"));
    assert!(a.cursor() > cursor);
    // Only counts are kept for the notice, never row ids.
    let notice = a.notice().expect("ignored rows are reported");
    assert!(notice.starts_with("Ignored 2 unencrypted changes from another device.") && !notice.contains(&item), "{notice}");

    // The local version is queued again, sealed, under a clock that wins.
    a.push().await;
    let row = service.rows().into_iter().find(|r| r["id"] == item.as_str()).unwrap();
    assert_eq!(row["enc"], "xchacha20_v1");
    assert!(row["hlc"].as_str().unwrap() > injected.encode().as_str());
    // A row this device never had is skipped, not deleted for everyone.
    let other = service.rows().into_iter().find(|r| r["id"] == "injected-plaintext").unwrap();
    assert_eq!(other["deleted"], false);

    // Another unlocked device converges on the sealed version.
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.pull().await.unwrap();
    assert_eq!(b.lib.items[&item].title, title);
    assert!(!b.lib.items.contains_key("injected-plaintext"));
}

#[tokio::test]
async fn an_offline_edit_made_before_turn_on_survives_on_both_devices() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut b = Device::new(&service, "device-b");
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    let space = a.lib.spaces_ordered()[0].0.clone();
    assert!(b.lib.spaces.contains_key(&space));

    // B edits while offline; only later does A turn encryption on.
    b.lib.update_space(&space, |s| s.name = "B-OFFLINE-EDIT".into()).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let code = a.turn_on().await;
    a.sync().await.unwrap();

    // B comes back: it pulls first, locks, unlocks and uploads its edit sealed.
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    assert_eq!(b.lib.spaces[&space].name, "B-OFFLINE-EDIT");
    assert_eq!(a.lib.spaces[&space].name, "B-OFFLINE-EDIT", "turning on must not overwrite an older unsynced edit");
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn turning_on_needs_a_fresh_pull_so_a_concurrent_turn_on_locks_instead() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut b = Device::new(&service, "device-b");
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    a.begin().unwrap();
    b.begin().unwrap();
    a.pull().await.unwrap();
    a.confirm().unwrap();
    a.sync().await.unwrap();

    // B has not pulled since it began: it may not confirm on stale knowledge.
    assert!(b.confirm().is_err(), "B confirmed without checking for another device's key");
    assert_eq!(b.e2ee.status(), E2eeStatus::Confirming);
    assert_eq!(b.pull().await, Err(PageError::Locked), "A's ciphertext arrives while B's code is on screen");
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert!(b.confirm().is_err());
    assert_eq!(b.push().await, 0);
    assert_ciphertext_only(&service);
}

impl Device {
    /// Reset as the UI does: a new key, its code, then confirm.
    async fn reset(&mut self) -> RecoveryCode {
        let code = self.begin_reset().await;
        self.confirm_reset().unwrap();
        code
    }

    /// Start a reset and show its code; confirming waits for a scan.
    async fn begin_reset(&mut self) -> RecoveryCode {
        self.e2ee.begin_reset(&self.lib).unwrap();
        assert!(!self.e2ee.reset_ready());
        self.scan().await;
        assert!(self.e2ee.reset_ready());
        self.e2ee.recovery_code().unwrap()
    }

    fn confirm_reset(&mut self) -> Result<(), String> {
        self.e2ee.confirm_reset(&mut self.lib, &self.keys)
    }

    /// Hide or restore the record's table from a second connection, as a
    /// momentary SQLite failure would.
    fn hide_records(&self, hidden: bool) {
        let (from, to) = if hidden { ("kv", "kv_hidden") } else { ("kv_hidden", "kv") };
        rusqlite::Connection::open(&self.path).unwrap().execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}")).unwrap();
    }
}

#[tokio::test]
async fn a_reset_resolves_two_devices_that_turned_on_at_once() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut b = Device::new(&service, "device-b");
    a.add_marker_content();
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    // Both checked for another key before either one uploaded its own.
    a.begin().unwrap();
    b.begin().unwrap();
    a.pull().await.unwrap();
    b.pull().await.unwrap();
    a.confirm().unwrap();
    std::thread::sleep(Duration::from_millis(5));
    b.confirm().unwrap();
    a.push().await;
    b.push().await;
    // B's marker is newer, so A locks. B also refuses A's rows if any of
    // them won; which ones do depends only on the random node ids.
    assert_eq!(a.pull().await, Err(PageError::Locked));
    assert_eq!(a.push().await, 0);
    let _ = b.pull().await;

    // A resets: its library is the source of truth under a new key.
    let code = a.reset().await;
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    a.sync().await.unwrap();
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);

    // B sees the new marker, locks, and the new code unlocks it.
    assert_eq!(b.pull().await, Err(PageError::Locked));
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert_eq!(b.push().await, 0);
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_fresh_device_that_resets_deletes_nothing_and_the_library_follows_the_new_code() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, item, boost) = a.add_marker_content();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();

    // A new device without the code resets. It can't read A's rows, and it
    // must not remove them: A still has the only copy of the library.
    let mut c = Device::new(&service, "device-c");
    assert_eq!(c.pull().await, Err(PageError::Locked));
    let code = c.reset().await;
    c.sync().await.unwrap();
    c.sync().await.unwrap();
    for id in [&folder, &item, &boost] {
        let row = service.rows().into_iter().find(|r| r["id"] == id.as_str()).unwrap();
        assert_eq!(row["deleted"], false, "{id} was removed from sync by a device that never had it");
    }
    assert_ciphertext_only(&service);

    // A learns the account was re-keyed, enters the new code and keeps its
    // library, which it then seals under the new key for everyone.
    assert_eq!(a.pull().await, Err(PageError::Locked));
    assert_eq!(a.push().await, 0);
    a.e2ee.unlock(code.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.folders.contains_key(&folder) && a.lib.items.contains_key(&item) && a.lib.boosts.contains_key(&boost));
    c.sync().await.unwrap();
    assert!(c.lib.folders.contains_key(&folder) && c.lib.items.contains_key(&item) && c.lib.boosts.contains_key(&boost));
    let mut d = Device::new(&service, "device-d");
    assert_eq!(d.pull().await, Err(PageError::Locked));
    d.e2ee.unlock(code.as_str(), &d.lib, &d.keys).unwrap();
    d.sync().await.unwrap();
    assert!(d.lib.items.contains_key(&item));
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn two_near_simultaneous_resets_lose_neither_devices_unsynced_row() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    a.sync().await.unwrap();
    let space = a.lib.spaces_ordered()[0].0.clone();
    let a_only = a.lib.create_folder(&space, None, "A only").unwrap();
    let b_only = b.lib.create_folder(&space, None, "B only").unwrap();

    let ka = a.reset().await;
    std::thread::sleep(Duration::from_millis(5));
    let kb = b.reset().await;
    for _ in 0..2 {
        let _ = a.sync().await;
        let _ = b.sync().await;
    }
    // Whichever marker won, the other device enters that code.
    if a.e2ee.status() == E2eeStatus::Locked { a.e2ee.unlock(kb.as_str(), &a.lib, &a.keys).unwrap(); }
    if b.e2ee.status() == E2eeStatus::Locked { b.e2ee.unlock(ka.as_str(), &b.lib, &b.keys).unwrap(); }
    for _ in 0..2 {
        a.sync().await.unwrap();
        b.sync().await.unwrap();
    }
    assert_eq!((a.e2ee.status(), b.e2ee.status()), (E2eeStatus::On, E2eeStatus::On));
    for id in [&a_only, &b_only] {
        assert!(a.lib.folders.contains_key(id) && b.lib.folders.contains_key(id), "{id} was lost");
        let row = service.rows().into_iter().find(|r| r["id"] == id.as_str()).unwrap();
        assert_eq!((row["deleted"].clone(), row["enc"].clone()), (serde_json::json!(false), serde_json::json!("xchacha20_v1")));
    }
}

#[tokio::test]
async fn an_old_key_device_never_stalls_new_key_devices_and_its_rows_follow_after_unlock() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.add_marker_content();
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    let mut e = Device::new(&service, "device-e");
    for d in [&mut b, &mut e] {
        assert_eq!(d.pull().await, Err(PageError::Locked));
        d.e2ee.unlock(k1.as_str(), &d.lib, &d.keys).unwrap();
        d.sync().await.unwrap();
    }
    a.pull().await.unwrap();

    // B goes offline and pins a tab; meanwhile A resets.
    let space = b.lib.spaces_ordered()[0].0.clone();
    let offline = b.lib.add_pinned(&space, None, "https://offline.example/", "Pinned Offline").unwrap();
    let k2 = a.reset().await;
    a.sync().await.unwrap();
    a.sync().await.unwrap();

    // B reconnects: it pulls before it uploads, so it locks instead of
    // uploading under the old key.
    b.e2ee.reconnecting();
    assert_eq!(b.push().await, 0, "an encrypted device uploaded before catching up after a reconnect");
    assert_eq!(b.pull().await, Err(PageError::Locked));

    // E never noticed a disconnect (a stale socket) and uploads under the old
    // key anyway. New-key devices skip that row and keep syncing.
    let e_space = e.lib.spaces_ordered()[0].0.clone();
    e.lib.update_space(&e_space, |s| s.name = "Renamed Under The Old Key".into()).unwrap();
    assert_eq!(e.push().await, 1);
    let cursor = a.cursor();
    a.pull().await.unwrap();
    assert!(a.cursor() > cursor, "the cursor moves past a row this device can't decrypt");
    assert!(a.notice().is_some_and(|n| n.contains("couldn't be decrypted")), "{:?}", a.notice());
    assert_ne!(a.lib.spaces[&e_space].name, "Renamed Under The Old Key");

    // Once B and E enter the new code, their rows reach A sealed under it.
    b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    assert_eq!(e.pull().await, Err(PageError::Locked));
    e.e2ee.unlock(k2.as_str(), &e.lib, &e.keys).unwrap();
    e.sync().await.unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.items.contains_key(&offline));
    assert_eq!(a.lib.spaces[&e_space].name, "Renamed Under The Old Key");
    let mut c = Device::new(&service, "device-c");
    assert_eq!(c.pull().await, Err(PageError::Locked));
    c.e2ee.unlock(k2.as_str(), &c.lib, &c.keys).unwrap();
    c.sync().await.unwrap();
    assert!(c.lib.items.contains_key(&offline));
    assert_ciphertext_only(&service);
}

impl Device {
    /// Quit and relaunch: state reloaded from disk, a new worker at the
    /// stored cursor.
    fn restart(&mut self) {
        let (reloaded, warning) = LibraryEncryption::load(ACCOUNT, &self.lib, &self.keys);
        assert!(warning.is_none());
        self.e2ee = reloaded;
        let held = self.lib.store.kv_get("sync_cursor").unwrap().map_or(0, |c| c.parse().unwrap());
        let (mut worker, events) = worker_at(&self._server.url);
        worker.live.as_mut().unwrap().cursor = held;
        self.worker = worker;
        self.events = events;
    }
}

#[tokio::test]
async fn a_reset_interrupted_before_its_next_pull_still_completes_after_a_restart() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    let code = b.reset().await;
    // Quit (or a server sign-out) before the next complete pull.
    b.restart();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    b.sync().await.unwrap();
    b.sync().await.unwrap();

    assert_eq!(a.pull().await, Err(PageError::Locked));
    a.e2ee.unlock(code.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.folders.contains_key(&folder));
    b.sync().await.unwrap();
    assert!(b.lib.folders.contains_key(&folder));
    let mut c = Device::new(&service, "device-c");
    assert_eq!(c.pull().await, Err(PageError::Locked));
    c.e2ee.unlock(code.as_str(), &c.lib, &c.keys).unwrap();
    c.sync().await.unwrap();
    c.sync().await.unwrap();
    assert!(c.lib.folders.contains_key(&folder));
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_row_uploaded_in_plaintext_just_before_the_marker_reaches_encrypted_devices_after_unlock() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut b = Device::new(&service, "device-b");
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    let code = a.turn_on().await;
    a.push().await;

    // B hasn't pulled the marker yet and uploads a new row as JSON.
    let space = b.lib.spaces_ordered()[0].0.clone();
    let row = b.lib.add_pinned(&space, None, "https://window.example/", "Pinned In The Window").unwrap();
    assert_eq!(b.push().await, 1);
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    // Re-sealing after unlock survives a restart before the next pull.
    b.restart();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    b.sync().await.unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.items.contains_key(&row), "the window row never reached the encrypted device");
    let server = service.rows().into_iter().find(|r| r["id"] == row.as_str()).unwrap();
    assert_eq!(server["enc"], "xchacha20_v1", "the server's plaintext copy was never superseded");
    let mut c = Device::new(&service, "device-c");
    assert_eq!(c.pull().await, Err(PageError::Locked));
    c.e2ee.unlock(code.as_str(), &c.lib, &c.keys).unwrap();
    c.sync().await.unwrap();
    assert!(c.lib.items.contains_key(&row));
    assert_ciphertext_only(&service);
}

const USED_BEFORE: &str = "That's the recovery code this device used before. Encryption was reset on another device since then. Enter the newest recovery code.";

#[tokio::test]
async fn the_code_this_device_used_before_is_named_after_another_device_resets() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    let k2 = b.reset().await;
    b.sync().await.unwrap();

    assert_eq!(a.pull().await, Err(PageError::Locked));
    assert_eq!(a.e2ee.unlock(k1.as_str(), &a.lib, &a.keys).unwrap_err(), USED_BEFORE);
    assert_eq!(a.e2ee.status(), E2eeStatus::Locked, "the old key is never adopted");
    a.e2ee.unlock(k2.as_str(), &a.lib, &a.keys).unwrap();

    // The same words after the user's own Sign Out, when the server sends a
    // sealed marker under an unknown key: v1 can't tell that from a re-key.
    a.sync().await.unwrap();
    LibraryEncryption::forget_key(ACCOUNT, &a.keys).unwrap();
    a.restart();
    let hlc = Hlc::new(crate::now_ms() + 60_000, 0, "server");
    let forged = seal(&LibraryKey::generate().unwrap().library_cipher(), &Binding { account_id: ACCOUNT, kind: MARKER_KIND, id: MARKER_ID, hlc: &hlc }, MARKER_PLAINTEXT).unwrap();
    service.rewrite(MARKER_KIND, MARKER_ID, |row| {
        row["enc"] = serde_json::json!("xchacha20_v1");
        row["deleted"] = serde_json::json!(false);
        row["payload"] = serde_json::json!(forged);
        row["hlc"] = serde_json::json!(hlc.encode());
    });
    assert_eq!(a.pull().await, Err(PageError::Locked));
    assert_eq!(a.e2ee.unlock(k2.as_str(), &a.lib, &a.keys).unwrap_err(), USED_BEFORE);
}

#[tokio::test]
async fn a_clock_at_the_largest_encodable_value_never_panics_or_poisons_the_device() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (_, item, _) = a.add_marker_content();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    let title = a.lib.items[&item].title.clone();
    let largest = "9999999999999-999999-legacy";
    let plaintext = serde_json::to_string(&crate::model::Item { title: "At The Clock Limit".into(), ..a.lib.items[&item].clone() }).unwrap();
    service.rewrite("item", &item, |row| {
        row["enc"] = serde_json::json!("json");
        row["hlc"] = serde_json::json!(largest);
        row["payload"] = serde_json::json!(plaintext);
    });
    a.pull().await.unwrap();
    assert_eq!(a.lib.items[&item].title, title, "plaintext is never applied");
    assert!(a.notice().is_some_and(|n| n.contains("Ignored an unencrypted change")), "{:?}", a.notice());
    let local = a.lib.store.entity_hlc("item", &item).unwrap().unwrap();
    assert!(Hlc::parse(&local.encode()).is_ok(), "the local clock became unparseable: {}", local.encode());

    // A device holding a row at that clock can't seal it again, so it says so
    // instead of turning on.
    let service = Arc::new(SyncService::default());
    let mut d = Device::new(&service, "device-d");
    d.sync().await.unwrap();
    service.rewrite("item", "at-the-limit", |row| {
        row["hlc"] = serde_json::json!(largest);
        row["payload"] = serde_json::json!(r#"{"kind":"pinned","url":"https://limit.example/","title":"Limit"}"#);
    });
    d.pull().await.unwrap();
    assert_eq!(d.lib.store.entity_hlc("item", "at-the-limit").unwrap().map(|h| h.encode()).as_deref(), Some(largest));
    assert_eq!(d.begin().unwrap_err(), "1 synced item has a clock that can't be advanced, so it can't be encrypted. It's stored on this device even if it isn't shown.");
    assert_eq!(d.e2ee.status(), E2eeStatus::Off);
}

#[tokio::test]
async fn a_failed_record_read_is_temporary_not_unreadable() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.add_marker_content();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    // A momentary SQLite failure while the record is read.
    let other = rusqlite::Connection::open(&a.path).unwrap();
    other.execute_batch("ALTER TABLE kv RENAME TO kv_hidden").unwrap();
    let (state, _) = LibraryEncryption::load(ACCOUNT, &a.lib, &a.keys);
    assert_ne!(state.status(), E2eeStatus::Unreadable, "a read error is not an unreadable record");
    a.e2ee = state;
    assert_eq!(a.push().await, 0);
    other.execute_batch("ALTER TABLE kv_hidden RENAME TO kv").unwrap();
    // Try Again loads it.
    a.restart();
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    a.sync().await.unwrap();
}

#[tokio::test]
async fn a_locked_device_stops_automatic_pulls_until_unlock_or_sync_now() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    assert!(b.e2ee.has_check_row());

    // The glue sends PausePulls after a refused page once a check row is known.
    let pulls = |d: &Device| d._server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count();
    b.worker.pulls_paused = true;
    let paused = pulls(&b);
    b.worker.pull_auto().await;
    assert_eq!(pulls(&b), paused, "polls, notifications and retries don't download the locked page again");
    assert!(b.events.try_iter().all(|(_, e)| !matches!(e, SyncEvent::Pulled { .. })));

    // Unlock sends Sync Now, which pulls and resumes automatic pulls.
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.worker.pull_now().await;
    assert_eq!(pulls(&b), paused + 1);
    assert!(!b.worker.pulls_paused);
    b.pull().await.unwrap();
    assert!(b.lib.folders.contains_key(&folder));
    let resumed = pulls(&b);
    b.worker.pull_auto().await;
    assert_eq!(pulls(&b), resumed + 1, "automatic pulls run again once unlocked");
}

#[tokio::test]
async fn a_change_notice_while_pulls_are_paused_has_a_locked_device_scan_instead_of_pull() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    // D locks and scans to the end; the glue then pauses its pulls.
    let mut d = Device::new(&service, "device-d");
    assert_eq!(d.pull().await, Err(PageError::Locked));
    assert!(d.e2ee.has_check_row() && !d.e2ee.needs_scan());
    d.worker.pulls_paused = true;
    // A resets later.
    let k2 = a.reset().await;
    a.sync().await.unwrap();
    assert!(d.e2ee.unlock(k2.as_str(), &d.lib, &d.keys).is_err(), "D still holds the earlier marker");

    // The server's WebSocket notice: no pull, one request to scan.
    let pulls = |d: &Device| d._server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count();
    let paused = pulls(&d);
    d.worker.server_cursor(service.cursor()).await;
    assert_eq!(pulls(&d), paused, "a paused device downloaded the refused page again");
    let events: Vec<SyncEvent> = d.events.try_iter().map(|(_, event)| event).collect();
    assert!(matches!(events.as_slice(), [SyncEvent::RemoteChanged]), "{events:?}");
    d.worker.server_cursor(service.cursor()).await;
    assert!(d.events.try_iter().next().is_none(), "the same change was reported twice");

    // The glue scans again, and A's new code verifies.
    d.e2ee.rescan();
    assert!(d.e2ee.needs_scan());
    d.scan().await;
    d.e2ee.unlock(k2.as_str(), &d.lib, &d.keys).unwrap();
    // Unlock sends Sync Now, which ends the pause.
    d.worker.pull_now().await;
    assert!(!d.worker.pulls_paused);
    d.sync().await.unwrap();
    d.sync().await.unwrap();
    assert!(d.lib.folders.contains_key(&folder));

    // Unpaused, a notice pulls as before.
    let space = a.lib.spaces_ordered()[0].0.clone();
    a.lib.add_pinned(&space, None, "https://after-the-notice.example/", "After The Notice").unwrap();
    a.sync().await.unwrap();
    let before = pulls(&d);
    d.worker.server_cursor(service.cursor()).await;
    assert_eq!(pulls(&d), before + 1);
    assert!(d.events.try_iter().all(|(_, event)| !matches!(event, SyncEvent::RemoteChanged)));
}

/// The documented recovery after losing a device and its code is a Reset on
/// a new device. Rows sealed under the lost key stay on the server ahead of
/// the new marker and fill whole pages, so a further device must look past
/// the page it holds for the account's newest marker.
async fn a_new_device_unlocks_past_pages_of_rows_under_a_lost_key(page_bytes: Option<usize>) {
    let service = Arc::new(SyncService::default());
    if let Some(budget) = page_bytes { service.page_bytes(budget); }
    let mut a = Device::new(&service, "device-a");
    let space = a.lib.spaces_ordered()[0].0.clone();
    for i in 0..1005 { a.lib.add_pinned(&space, None, &format!("https://example.com/{i}"), &format!("Item {i}")).unwrap(); }
    a.pull().await.unwrap();
    while a.push().await > 0 {}
    a.pull().await.unwrap();
    a.turn_on().await;
    while a.push().await > 0 {}
    a.pull().await.unwrap();
    let lost = service.rows().iter().filter(|r| r["kind"] != MARKER_KIND && r["enc"] == "xchacha20_v1").count();
    assert!(lost > 1005);

    // A and its recovery code are lost. A new device resets.
    let mut d = Device::new(&service, "device-d");
    let d_space = d.lib.spaces_ordered()[0].0.clone();
    d.lib.update_space(&d_space, |s| s.name = "Library Of The Reset Device".into()).unwrap();
    assert_eq!(d.pull().await, Err(PageError::Locked));
    let code = d.reset().await;
    for _ in 0..3 {
        d.pull().await.unwrap();
        while d.push().await > 0 {}
    }
    d.pull().await.unwrap();
    assert_eq!(d.e2ee.status(), E2eeStatus::On);
    let marker = service.rows().into_iter().find(|r| r["kind"] == MARKER_KIND).unwrap();
    assert!(service.rows().iter().filter(|r| r["seq"].as_u64() < marker["seq"].as_u64()).count() >= 1000, "the old rows fill the first page");

    // A third device: its held page has only rows under the lost key.
    let mut e = Device::new(&service, "device-e");
    assert_eq!(e.pull().await, Err(PageError::Locked));
    assert_eq!(e.cursor(), 0, "a locked device commits nothing");
    assert!(e.lib.store.kv_get("sync_cursor").unwrap().is_none());
    e.e2ee.unlock(code.as_str(), &e.lib, &e.keys).expect("the newest code unlocks");
    e.sync().await.unwrap();
    e.pull().await.unwrap();
    assert_eq!(e.e2ee.status(), E2eeStatus::On);
    assert_eq!(e.lib.spaces.get(&d_space).map(|s| s.name.as_str()), Some("Library Of The Reset Device"), "the reset device's library arrives");
    assert!(e.lib.items.is_empty(), "rows under the lost key are never applied");
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_new_device_unlocks_with_the_newest_code_past_a_full_page_of_rows_under_a_lost_key() {
    a_new_device_unlocks_past_pages_of_rows_under_a_lost_key(None).await;
}

#[tokio::test]
async fn a_new_device_unlocks_with_the_newest_code_past_pages_cut_by_a_byte_budget() {
    // The Worker also ends pages at 4 MiB of payload; scaled down here.
    a_new_device_unlocks_past_pages_of_rows_under_a_lost_key(Some(16 * 1024)).await;
}

#[tokio::test]
async fn skipped_and_ignored_counts_add_up_across_pages_and_survive_a_restart() {
    let service = Arc::new(SyncService::default());
    // One row per page.
    service.page_bytes(1);
    let mut a = Device::new(&service, "device-a");
    a.add_marker_content();
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    let sealed = service.rows().iter().filter(|r| r["kind"] != MARKER_KIND && r["enc"] == "xchacha20_v1").count();
    assert!(sealed > 3);

    // A fresh device resets without A's code: every row A sealed is skipped.
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.reset().await;
    b.sync().await.unwrap();
    let skipped = format!("{sealed} synced items couldn't be decrypted on this device and weren't applied.");
    assert!(b.notice().is_some_and(|n| n.contains(&skipped)), "{:?}", b.notice());

    // A legacy client writes three plaintext rows, each on its own page.
    let legacy = Hlc::new(crate::now_ms() + 60_000, 0, "legacy");
    for id in ["legacy-1", "legacy-2", "legacy-3"] {
        service.rewrite("item", id, |row| {
            row["hlc"] = serde_json::json!(legacy.encode());
            row["payload"] = serde_json::json!(r#"{"kind":"pinned","url":"https://legacy.example/","title":"Legacy"}"#);
        });
    }
    b.pull().await.unwrap();
    let notice = b.notice().unwrap();
    assert!(notice.contains("Ignored 3 unencrypted changes") && notice.contains(&skipped), "{notice}");
    // The counts survive a restart; what the reset did this session doesn't.
    b.restart();
    let counted = notice.strip_prefix("Encryption was reset. Other devices need the new recovery code to sync. ").unwrap();
    assert_eq!(b.notice().as_deref(), Some(counted), "a restart still shows what this device lacks");
    // Signing in again pulls from cursor 0; versions already counted aren't counted twice.
    b.repull_from_start();
    b.pull().await.unwrap();
    assert_eq!(b.notice().as_deref(), Some(counted), "a re-pull counted the same versions again");
    // Until the user dismisses them.
    b.e2ee.dismiss_notice(&b.lib).unwrap();
    b.restart();
    assert_eq!(b.notice(), None);
    b.repull_from_start();
    b.pull().await.unwrap();
    assert_eq!(b.notice(), None, "a dismissed notice came back on a re-pull");
}

impl Device {
    /// A new worker at cursor 0, as after signing in again.
    fn repull_from_start(&mut self) {
        let (worker, events) = worker_at(&self._server.url);
        self.worker = worker;
        self.events = events;
    }
}

#[tokio::test]
async fn a_row_at_the_clock_limit_never_blocks_unlock_or_reset_and_is_reported() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut d = Device::new(&service, "device-d");
    a.sync().await.unwrap();
    d.sync().await.unwrap();
    a.pull().await.unwrap();
    a.begin().unwrap();
    let code = a.e2ee.recovery_code().unwrap();
    a.pull().await.unwrap();
    a.confirm().unwrap();
    // Before A uploads: a forged plaintext row at the largest clock, which
    // D applies while still off. Its payload isn't a valid item, so it is
    // stored but never shown.
    service.rewrite("item", "at-the-limit", |row| {
        row["hlc"] = serde_json::json!("9999999999999-999999-legacy");
        row["payload"] = serde_json::json!(r#"{"kind":"pinned","url":"https://limit.example/","title":"Limit"}"#);
    });
    d.pull().await.unwrap();
    assert!(d.lib.store.entity_hlc("item", "at-the-limit").unwrap().is_some() && !d.lib.items.contains_key("at-the-limit"));
    for _ in 0..3 { a.push().await; }
    a.pull().await.unwrap();
    assert_eq!(d.pull().await, Err(PageError::Locked));

    // Unlock always has a way out; the row that can't be sealed is reported.
    d.e2ee.unlock(code.as_str(), &d.lib, &d.keys).expect("unlock proceeds past a clock that can't advance");
    assert_eq!(d.e2ee.status(), E2eeStatus::On);
    d.sync().await.unwrap();
    let unsealed = "1 item has a sync clock that can't be advanced, so it wasn't sealed again under this recovery code.";
    assert!(d.notice().is_some_and(|n| n.contains(unsealed)), "{:?}", d.notice());
    d.restart();
    assert!(d.notice().is_some_and(|n| n.contains(unsealed)), "{:?}", d.notice());
    // Reset proceeds too.
    d.begin_reset().await;
    d.confirm_reset().expect("reset proceeds past a clock that can't advance");
    assert_eq!(d.e2ee.status(), E2eeStatus::On);
}

#[tokio::test]
async fn an_unlock_that_fails_to_save_never_loses_the_reseal_or_rejects_the_code() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut b = Device::new(&service, "device-b");
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    let code = a.turn_on().await;
    a.push().await;
    // B uploads a row as JSON just before it pulls A's marker.
    let space = b.lib.spaces_ordered()[0].0.clone();
    let row = b.lib.add_pinned(&space, None, "https://window.example/", "Pinned In The Window").unwrap();
    assert_eq!(b.push().await, 1);
    assert_eq!(b.pull().await, Err(PageError::Locked));

    // The record can't be written for a moment: nothing is stored.
    b.hide_records(true);
    assert!(b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).is_err());
    b.hide_records(false);
    assert!(b.keys.load(ACCOUNT).unwrap().is_none(), "the key is stored only after the record names it");
    b.restart();
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);

    // The key store fails after the record named the key: still locked after
    // a restart, and the same code unlocks.
    b.keys.fail_writes(true);
    assert!(b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).is_err());
    b.keys.fail_writes(false);
    b.restart();
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked, "a key that was never stored can't turn the device on");
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).expect("the code is never rejected after a failed save");
    b.sync().await.unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.items.contains_key(&row), "the re-seal ran");
    let server = service.rows().into_iter().find(|r| r["id"] == row.as_str()).unwrap();
    assert_eq!(server["enc"], "xchacha20_v1");
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_stored_key_the_record_doesnt_name_is_sealed_again_after_loading() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let mut b = Device::new(&service, "device-b");
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    let code = a.turn_on().await;
    a.push().await;
    let space = b.lib.spaces_ordered()[0].0.clone();
    let row = b.lib.add_pinned(&space, None, "https://window.example/", "Pinned In The Window").unwrap();
    assert_eq!(b.push().await, 1);
    assert_eq!(b.pull().await, Err(PageError::Locked));
    // An earlier build stored the key, then crashed before writing the record.
    b.keys.save(ACCOUNT, &RecoveryCode::parse(code.as_str()).unwrap()).unwrap();
    b.restart();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert!(b.lib.store.kv_get("e2ee_library").unwrap().is_some_and(|r| r.contains("\"reseal\":true")));
    b.sync().await.unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.items.contains_key(&row), "the re-seal ran");
    assert_ciphertext_only(&service);
}

const RESET_UNSAVED: &str = "Couldn't store the new key, so the reset wasn't applied. Try again, or Cancel to keep the current code.";
const RESET_SENT: &str = "That's the recovery code this device used before it reset encryption, and the reset already reached the sync server. Enter the new recovery code.";

/// A key store that quits the app while it stores a key: the panic stands in
/// for the process ending between the record write and the key save.
struct QuitOnSave(MemoryKeyStore);

impl KeyStore for QuitOnSave {
    fn load(&self, account: &str) -> Result<Option<LibraryKey>, String> {
        self.0.load(account)
    }

    fn save(&self, _: &str, _: &LibraryKey) -> Result<(), String> {
        panic!("fixture: the app quit while the new key was being stored")
    }

    fn clear(&self, account: &str) -> Result<(), String> {
        self.0.clear(account)
    }
}

impl Device {
    /// The record's exact text and the marker row's stored columns
    /// (clock, deleted, payload, dirty), read through a second connection.
    fn reset_state(&self) -> (Option<String>, Option<(String, i64, String, i64)>) {
        use rusqlite::OptionalExtension;
        let other = rusqlite::Connection::open(&self.path).unwrap();
        let record = other.query_row("SELECT v FROM kv WHERE k = 'e2ee_library'", [], |r| r.get(0)).optional().unwrap();
        let marker = other.query_row("SELECT hlc, deleted, payload, dirty FROM entities WHERE kind = ?1 AND id = ?2", [MARKER_KIND, MARKER_ID], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().unwrap();
        (record, marker)
    }

    /// Quit and relaunch with the library read back from disk.
    fn reopen(&mut self) {
        self.lib = Library::open(Store::open(&self.path).unwrap()).unwrap();
        self.restart();
    }

    /// Confirm the reset, quit while its key is being stored, and relaunch.
    fn confirm_reset_and_quit(&mut self) {
        let quit = QuitOnSave(self.keys.clone());
        let (e2ee, lib) = (&mut self.e2ee, &mut self.lib);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e2ee.confirm_reset(lib, &quit)));
        assert!(outcome.is_err(), "the fixture's key save always quits");
        self.reopen();
    }
}

#[tokio::test]
async fn a_reset_whose_key_cant_be_stored_changes_nothing_so_cancel_keeps_the_current_code() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    assert_eq!(b.lib.store.dirty_count().unwrap(), 0);
    let before = b.reset_state();
    assert!(before.0.is_some() && before.1.as_ref().is_some_and(|marker| marker.3 == 0));

    // The record can't be written: nothing changes and the code stays on screen.
    let k2 = b.begin_reset().await;
    b.hide_records(true);
    assert!(b.confirm_reset().is_err());
    b.hide_records(false);
    assert_eq!(b.e2ee.status(), E2eeStatus::Resetting);
    assert_eq!(b.reset_state(), before);

    // The key store refuses the new key: the same call puts the record and
    // the marker row back exactly, and the new code stays on screen.
    b.keys.fail_writes(true);
    assert_eq!(b.confirm_reset().unwrap_err(), RESET_UNSAVED);
    assert_eq!(b.e2ee.status(), E2eeStatus::Resetting);
    assert_eq!(b.code(), Some(Hidden::from(&k2)));
    assert_eq!(b.reset_state(), before, "the record or the marker row changed");
    let rows = service.rows();
    assert_eq!(b.push().await, 0);
    assert_eq!(service.rows(), rows);

    // Cancel keeps the current code, also after reopening.
    b.e2ee.cancel_reset();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k1)));
    b.keys.fail_writes(false);
    b.reopen();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k1)));
    let space = a.lib.spaces_ordered()[0].0.clone();
    let row = a.lib.add_pinned(&space, None, "https://after-cancel.example/", "After Cancel").unwrap();
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    assert!(b.lib.items.contains_key(&row), "B no longer syncs under the current code");
    assert!(a.pull().await.is_ok(), "A was locked by a reset that wasn't applied");

    // Try Again repeats the same confirm with the same code.
    let k3 = b.begin_reset().await;
    b.keys.fail_writes(true);
    assert_eq!(b.confirm_reset().unwrap_err(), RESET_UNSAVED);
    b.keys.fail_writes(false);
    assert!(b.e2ee.reset_ready());
    b.confirm_reset().expect("Try Again stores the same new key");
    assert_eq!(b.code(), Some(Hidden::from(&k3)));
    b.reopen();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k3)));
    assert!(!b.reset_state().0.unwrap().contains("undo"), "a stored reset keeps its way back");
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    assert_eq!(a.pull().await, Err(PageError::Locked));
    a.e2ee.unlock(k3.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.folders.contains_key(&folder) && a.lib.items.contains_key(&row));
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_locked_device_whose_reset_key_cant_be_stored_still_unlocks_with_the_accounts_code() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut d = Device::new(&service, "device-d");
    assert_eq!(d.pull().await, Err(PageError::Locked));
    let before = d.reset_state();
    assert!(before.0.is_some() && before.1.is_none(), "a new locked device has no marker row");

    d.begin_reset().await;
    d.keys.fail_writes(true);
    assert_eq!(d.confirm_reset().unwrap_err(), RESET_UNSAVED);
    d.keys.fail_writes(false);
    assert_eq!(d.e2ee.status(), E2eeStatus::Resetting);
    assert_eq!(d.reset_state(), before, "the record or the marker row changed");
    d.e2ee.cancel_reset();
    assert_eq!(d.e2ee.status(), E2eeStatus::Locked);
    d.reopen();
    assert_eq!(d.e2ee.status(), E2eeStatus::Locked);
    d.e2ee.unlock(k1.as_str(), &d.lib, &d.keys).expect("the account's code still unlocks");
    d.sync().await.unwrap();
    d.sync().await.unwrap();
    assert!(d.lib.folders.contains_key(&folder));
    assert!(a.pull().await.is_ok(), "A was locked by a reset that wasn't applied");
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_reset_interrupted_before_its_key_is_stored_is_finished_by_the_new_code_or_undone_by_the_previous_one() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    let before = b.reset_state();

    // The app quits after the record and new marker are written but before
    // the key is stored: Locked after the restart, with nothing uploaded.
    b.begin_reset().await;
    b.confirm_reset_and_quit();
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert_ne!(b.reset_state(), before);
    let rows = service.rows();
    assert_eq!(b.push().await, 0);
    assert_eq!(service.rows(), rows);

    // The previous code puts the record and the marker row back exactly.
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).expect("the previous code undoes the unsent reset");
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k1)));
    assert_eq!(b.reset_state(), before, "the record or the marker row changed");
    b.reopen();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    b.sync().await.unwrap();
    assert!(a.pull().await.is_ok(), "A was locked by a reset that was undone");

    // Again, but this time the new code finishes the reset. Once its marker
    // has reached the server, the previous code is refused.
    let k3 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    let marker = b.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    b.lib.store.mark_clean(MARKER_KIND, MARKER_ID, &marker).unwrap();
    assert_eq!(b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap_err(), RESET_SENT);
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    b.e2ee.unlock(k3.as_str(), &b.lib, &b.keys).expect("the new code finishes the reset");
    b.reopen();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k3)));
    assert!(!b.reset_state().0.unwrap().contains("undo"));
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    assert_eq!(a.pull().await, Err(PageError::Locked));
    a.e2ee.unlock(k3.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.folders.contains_key(&folder) && b.lib.folders.contains_key(&folder));
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_reset_lands_above_the_accounts_marker_when_that_clock_runs_ahead() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    // A's clock runs ten minutes ahead: it observes a row written then.
    let ahead = Hlc::new(crate::now_ms() + 600_000, 0, "skewed");
    service.rewrite("note", "from-the-future", |row| {
        row["hlc"] = serde_json::json!(ahead.encode());
        row["payload"] = serde_json::json!("{}");
    });
    a.pull().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let a_marker = a.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();

    // C's record becomes unreadable, so it has no note of A's marker.
    let mut c = Device::new(&service, "device-c");
    assert_eq!(c.pull().await, Err(PageError::Locked));
    c.lib.store.kv_set("e2ee_library", "not json").unwrap();
    c.restart();
    assert_eq!(c.e2ee.status(), E2eeStatus::Unreadable);
    let k2 = c.reset().await;
    let c_marker = c.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    assert!(c_marker > a_marker, "{} is not above {}", c_marker.encode(), a_marker.encode());
    c.sync().await.unwrap();
    assert_eq!(c.e2ee.status(), E2eeStatus::On);
    assert!(!c.notice().unwrap_or_default().contains("more recently"));

    assert_eq!(a.pull().await, Err(PageError::Locked));
    assert!(a.e2ee.unlock(k1.as_str(), &a.lib, &a.keys).is_err());
    a.e2ee.unlock(k2.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);
}

const OUTRUN: &str = "Another device set up encryption more recently than this reset. Enter that device's newest recovery code, or reset again.";

#[tokio::test]
async fn a_reset_outrun_by_a_newer_one_says_so_instead_of_claiming_success() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    let k1 = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();

    a.reset().await;
    a.push().await;
    assert!(a.notice().is_some_and(|n| n.contains("Encryption was reset.")), "{:?}", a.notice());
    // B resets afterwards, so its marker wins.
    std::thread::sleep(Duration::from_millis(5));
    let kb = b.reset().await;
    b.sync().await.unwrap();
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(a.pull().await, Err(PageError::Locked));
    let notice = a.notice().unwrap_or_default();
    assert!(notice.contains(OUTRUN) && !notice.contains("Encryption was reset."), "{notice}");
    a.e2ee.unlock(kb.as_str(), &a.lib, &a.keys).unwrap();
    assert!(!a.notice().unwrap_or_default().contains(OUTRUN), "a stale notice stays on screen");
    a.sync().await.unwrap();
}

#[tokio::test]
async fn a_later_edit_sorts_above_a_skipped_row_this_device_cant_open() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    a.turn_on().await;
    a.sync().await.unwrap();
    let space = a.lib.spaces_ordered()[0].0.clone();
    // A newer version under another key (here: a replay under a later clock).
    let ahead = Hlc::new(crate::now_ms() + 3_600_000, 0, "server");
    service.rewrite("space", &space, |row| row["hlc"] = serde_json::json!(ahead.encode()));
    a.pull().await.unwrap();
    assert!(a.notice().is_some_and(|n| n.contains("couldn't be decrypted")), "{:?}", a.notice());
    a.lib.update_space(&space, |s| s.name = "Edited After The Skipped Row".into()).unwrap();
    let local = a.lib.store.entity_hlc("space", &space).unwrap().unwrap();
    assert!(local > ahead, "{} sorts below the skipped {}", local.encode(), ahead.encode());
    a.push().await;
    let row = service.rows().into_iter().find(|r| r["id"] == space.as_str()).unwrap();
    assert_eq!(row["hlc"], local.encode());
    assert_eq!(a.lib.store.dirty_count().unwrap(), 0);
}

#[tokio::test]
async fn profile_moves_renames_and_deletions_stay_sealed_and_reach_the_other_device() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();

    let space = a.lib.spaces_ordered()[0].0.clone();
    let old = a.lib.spaces[&space].profile_id.clone();
    let moved = a.lib.create_named_profile("Sealed Profile Name").unwrap();
    a.lib.set_space_profile(&space, &moved).unwrap();
    a.lib.rename_profile(&moved, "Sealed Renamed Profile").unwrap();
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);
    assert!(!server_text(&service).contains("Sealed"), "profile names stay sealed");
    b.pull().await.unwrap();
    same_library(&a.lib, &b.lib);
    assert_eq!(b.lib.spaces[&space].profile_id, moved);
    assert!(b.lib.items.values().filter(|item| item.space_id.as_deref() == Some(space.as_str())).all(|item| item.profile_id == moved));

    assert!(a.lib.spaces_using_profile(&old).is_empty());
    a.lib.delete_profile(&old).unwrap();
    a.sync().await.unwrap();
    assert_ciphertext_only(&service);
    b.pull().await.unwrap();
    same_library(&a.lib, &b.lib);
    assert!(!b.lib.profiles.contains_key(&old));
}

const SUPERSEDED: &str = "Encryption was reset on another device after this one started a reset. Enter the newest recovery code.";
const REPLACED: &str = "That's the code of a reset this device started and then replaced with a newer one. Enter the newest recovery code.";
const UNCHECKED_RESET: &str = "Couldn't check the server for a newer reset from another device, so this device's reset wasn't finished. Connect, then try again.";
const DROPPED: &str = "That's the code of a reset this device dropped when the account's current recovery code was entered. Enter that code again.";
const UNCHECKED: &str = "Couldn't check the server for a newer recovery code. If you just reset encryption on another device, connect and try again.";

/// A and B on one code, both synced. Returns the code and A's folder.
async fn two_devices_on_one_code(service: &Arc<SyncService>) -> (Device, Device, RecoveryCode, String) {
    let mut a = Device::new(service, "device-a");
    let (folder, _, _) = a.add_marker_content();
    a.sync().await.unwrap();
    let code = a.turn_on().await;
    a.sync().await.unwrap();
    let mut b = Device::new(service, "device-b");
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap();
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    a.pull().await.unwrap();
    (a, b, code, folder)
}

/// AD4: the code a device holds prints redacted when an assert over it
/// fails.
#[tokio::test]
async fn a_failed_assert_on_a_devices_code_prints_it_redacted() {
    let service = Arc::new(SyncService::default());
    let (a, b, _, _) = two_devices_on_one_code(&service).await;
    let groups: Vec<String> = a.e2ee.recovery_code().unwrap().as_str().split('-').skip(1).map(str::to_owned).collect();
    let messages = [
        failure(|| assert_ne!(a.code(), b.code(), "both devices hold the code")),
        failure(|| panic!("{:?}", a.code())),
    ];
    for (n, message) in messages.iter().enumerate() {
        assert!(!groups.iter().any(|group| message.contains(group.as_str())), "failure {n} printed the code");
        assert!(message.contains("Hidden(..)"), "failure {n} printed no redacted value");
    }
}

#[tokio::test]
async fn a_failed_save_of_the_new_code_keeps_the_previous_codes_way_back_after_an_interrupted_reset() {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, k1, folder) = two_devices_on_one_code(&service).await;
    let before = b.reset_state();
    let k2 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);

    // The keychain declines the new code's key: still Locked, nothing to push.
    b.keys.fail_writes(true);
    let refused = b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).unwrap_err();
    b.keys.fail_writes(false);
    assert!(refused.starts_with("The code is correct, but it couldn't be stored."), "{refused}");
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert!(b.reset_state().0.unwrap().contains("\"undo\""), "the way back was dropped before the key was stored");
    assert_eq!(b.push().await, 0);

    // The account's code still undoes the unsent reset, also after a restart.
    b.reopen();
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).expect("the previous code undoes the unsent reset");
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k1)));
    assert_eq!(b.reset_state(), before, "the record or the marker row changed");
    b.sync().await.unwrap();
    assert!(a.pull().await.is_ok(), "A was locked by a reset that was undone");
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    assert!(b.lib.folders.contains_key(&folder));
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn the_accounts_code_undoes_every_unsent_reset_and_an_abandoned_code_never_completes_one() {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, k1, folder) = two_devices_on_one_code(&service).await;
    let before = b.reset_state();

    // Two resets in a row, each cut short by a quit before its key is stored;
    // the second starts from the Locked page the first left.
    let k2 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    let k3 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);

    // The abandoned first code is refused and changes nothing.
    let held = b.reset_state();
    let rows = service.rows();
    assert_eq!(b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).unwrap_err(), REPLACED);
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert_eq!(b.reset_state(), held, "a refused code changed the record or the marker row");
    assert_eq!(b.push().await, 0);
    assert_eq!(service.rows(), rows);

    // The account's code undoes both, also after a restart.
    b.reopen();
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).expect("the account's code undoes every unsent reset");
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(&k1)));
    assert_eq!(b.reset_state(), before, "the record or the marker row changed");
    b.sync().await.unwrap();
    assert!(a.pull().await.is_ok(), "A was locked by a reset that was undone");

    // Again; this time the newest code finishes the second reset, and the
    // abandoned first code still never does.
    let k4 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    let k5 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    assert_eq!(b.e2ee.unlock(k4.as_str(), &b.lib, &b.keys).unwrap_err(), REPLACED);
    b.e2ee.unlock(k5.as_str(), &b.lib, &b.keys).expect("the newest code finishes its reset");
    b.reopen();
    assert_eq!(b.code(), Some(Hidden::from(&k5)));
    assert!(!b.reset_state().0.unwrap().contains("undo"));
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    assert_eq!(a.pull().await, Err(PageError::Locked));
    for abandoned in [&k1, &k3, &k4] {
        assert!(a.e2ee.unlock(abandoned.as_str(), &a.lib, &a.keys).is_err());
    }
    a.e2ee.unlock(k5.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.folders.contains_key(&folder) && b.lib.folders.contains_key(&folder));
    assert_ciphertext_only(&service);
}

/// B's reset is cut short before its key is stored; A, unaware, resets too.
/// `b_ahead` runs B's clock ten minutes ahead, so A's later marker sorts
/// below B's unsent one.
async fn a_reset_elsewhere_supersedes_an_unsent_reset_here(b_ahead: bool) {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, k1, folder) = two_devices_on_one_code(&service).await;
    if b_ahead {
        let ahead = Hlc::new(crate::now_ms() + 600_000, 0, b.lib.node());
        b.lib.store.kv_set("hlc_last", &ahead.encode()).unwrap();
        b.reopen();
    }
    let k2 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    let own = b.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    let k3 = a.reset().await;
    a.sync().await.unwrap();
    a.sync().await.unwrap();
    let elsewhere = a.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    if b_ahead { assert!(elsewhere < own, "A's later marker should sort below B's"); }

    // B reads the stream to its end, as the glue does before judging a code.
    assert_eq!(b.pull().await, Err(PageError::Locked));
    let held = b.reset_state();
    assert_eq!(b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).unwrap_err(), SUPERSEDED);
    assert_eq!(b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap_err(), USED_BEFORE);
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert_eq!(b.reset_state(), held, "a refused code changed the record or the marker row");
    assert_eq!(b.push().await, 0);

    // The newest code unlocks; B's unsent marker is dropped, never pushed.
    b.e2ee.unlock(k3.as_str(), &b.lib, &b.keys).expect("the newest code unlocks");
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_ne!(b.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap(), Some(own.clone()), "B's abandoned marker is still queued");
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    let markers: Vec<_> = service.rows().into_iter().filter(|row| row["kind"] == MARKER_KIND).collect();
    assert_eq!(markers.len(), 1);
    assert_ne!(markers[0]["hlc"], own.encode(), "B's abandoned reset reached the server");
    assert!(a.pull().await.is_ok(), "A was locked by B's abandoned reset");
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    assert!(b.lib.folders.contains_key(&folder));
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_reset_elsewhere_supersedes_an_unsent_reset_here_even_when_this_clock_runs_ahead() {
    a_reset_elsewhere_supersedes_an_unsent_reset_here(true).await;
}

#[tokio::test]
async fn a_reset_elsewhere_supersedes_an_unsent_reset_here_with_clocks_in_step() {
    a_reset_elsewhere_supersedes_an_unsent_reset_here(false).await;
}

/// B's reset is cut short; A resets; B, outrun, resets again instead of
/// entering A's code, and that reset is cut short too. Then B scans afresh.
/// Returns A, B, the codes from before (K1), of B's first reset (K2), of
/// A's reset (K3, the account's) and of B's second reset (K4), A's folder
/// and B's unsent marker clock.
async fn reset_again_over_a_reset_elsewhere(service: &Arc<SyncService>, b_ahead: bool) -> (Device, Device, [RecoveryCode; 4], String, Hlc) {
    let (mut a, mut b, k1, folder) = two_devices_on_one_code(service).await;
    if b_ahead { b.run_ahead(600_000); }
    let k2 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    let k3 = a.reset().await;
    a.sync().await.unwrap();
    a.sync().await.unwrap();
    let elsewhere = a.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    assert_eq!(b.pull().await, Err(PageError::Locked));
    let k4 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    let own = b.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    assert!(elsewhere < own, "a second reset lands above the marker it knows of");
    b.e2ee.rescan();
    b.scan().await;
    (a, b, [k1, k2, k3, k4], folder, own)
}

/// A's code, the account's current one, has unlocked B; B's unsent reset is
/// dropped without ever being uploaded.
async fn the_accounts_code_unlocked(service: &SyncService, a: &mut Device, b: &mut Device, k3: &RecoveryCode, folder: &str, own: &Hlc) {
    assert_eq!(b.e2ee.status(), E2eeStatus::On);
    assert_eq!(b.code(), Some(Hidden::from(k3)));
    assert_ne!(b.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().as_ref(), Some(own), "B's abandoned marker is still queued");
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    let markers: Vec<_> = service.rows().into_iter().filter(|row| row["kind"] == MARKER_KIND).collect();
    assert_eq!(markers.len(), 1);
    assert_ne!(markers[0]["hlc"], own.encode(), "B's abandoned reset reached the server");
    assert!(a.pull().await.is_ok(), "A was locked by B's abandoned reset");
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    assert!(b.lib.folders.contains_key(folder));
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(service);
}

/// With `refusals`, B first tries the earlier codes.
async fn the_accounts_code_unlocks_after_a_second_reset_cut_short_over_a_reset_elsewhere(b_ahead: bool, refusals: bool) {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, [k1, k2, k3, _], folder, own) = reset_again_over_a_reset_elsewhere(&service, b_ahead).await;
    // The earlier codes are refused with their reasons and change nothing.
    if refusals {
        let (held, rows) = (b.reset_state(), service.rows());
        assert_eq!(b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap_err(), USED_BEFORE);
        assert_eq!(b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).unwrap_err(), REPLACED);
        assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
        assert_eq!(b.reset_state(), held, "a refused code changed the record or the marker row");
        assert_eq!(b.push().await, 0);
        assert_eq!(service.rows(), rows);
    }
    // And after a restart.
    b.reopen();
    b.e2ee.rescan();
    b.scan().await;
    b.e2ee.unlock(k3.as_str(), &b.lib, &b.keys).expect("the account's current code unlocks");
    the_accounts_code_unlocked(&service, &mut a, &mut b, &k3, &folder, &own).await;
}

#[tokio::test]
async fn the_accounts_code_unlocks_after_a_second_reset_cut_short_over_a_reset_elsewhere_with_clocks_in_step() {
    the_accounts_code_unlocks_after_a_second_reset_cut_short_over_a_reset_elsewhere(false, false).await;
}

#[tokio::test]
async fn the_accounts_code_unlocks_after_a_second_reset_cut_short_over_a_reset_elsewhere_even_when_this_clock_runs_ahead() {
    the_accounts_code_unlocks_after_a_second_reset_cut_short_over_a_reset_elsewhere(true, true).await;
}

impl Device {
    /// Make inserts into `table` whose new row matches `when` fail, as a
    /// momentary SQLite error would, until called again with `None`.
    fn fail_inserts(&self, table: &str, when: Option<&str>) {
        let other = rusqlite::Connection::open(&self.path).unwrap();
        other.execute_batch("DROP TRIGGER IF EXISTS fixture_fault").unwrap();
        if let Some(when) = when {
            other.execute_batch(&format!("CREATE TRIGGER fixture_fault BEFORE INSERT ON {table} WHEN {when} BEGIN SELECT RAISE(ABORT, 'fixture fault'); END")).unwrap();
        }
    }
}

/// Where the unlock with the account's code stops after B reset again.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// Every write of the record fails, as in the sixth recheck's probe.
    RecordWrite,
    /// The marker row can't be written back.
    MarkerWrite,
    /// The record that names the account's key can't be written.
    KeyRecordWrite,
    /// The key store refuses the key.
    KeySave,
    /// The app quits while the key is being stored.
    QuitAtKeySave,
}

/// The unlock is cut short at `cut`. With `restart`, B relaunches before the
/// code is entered again: the disk then holds what a quit just before the
/// failed step would leave.
async fn the_accounts_code_unlocks_after_the_unlock_is_cut_short(cut: Cut, restart: bool) {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, [_, k2, k3, k4], folder, own) = reset_again_over_a_reset_elsewhere(&service, false).await;
    let key_id = BASE64URL_NOPAD.encode(&RecoveryCode::parse(k3.as_str()).unwrap().key_id());
    let rows = service.rows();
    match cut {
        Cut::RecordWrite => b.hide_records(true),
        Cut::MarkerWrite => b.fail_inserts("entities", Some(&format!("NEW.kind = '{MARKER_KIND}'"))),
        Cut::KeyRecordWrite => b.fail_inserts("kv", Some(&format!("NEW.k = 'e2ee_library' AND instr(NEW.v, '{key_id}') > 0"))),
        Cut::KeySave => b.keys.fail_writes(true),
        Cut::QuitAtKeySave => {
            let (e2ee, lib, quit) = (&mut b.e2ee, &b.lib, QuitOnSave(b.keys.clone()));
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e2ee.unlock(k3.as_str(), lib, &quit))).is_err());
        }
    }
    if !matches!(cut, Cut::QuitAtKeySave) {
        assert!(b.e2ee.unlock(k3.as_str(), &b.lib, &b.keys).is_err(), "{cut:?} didn't stop the unlock");
    }
    match cut {
        Cut::RecordWrite => b.hide_records(false),
        Cut::MarkerWrite | Cut::KeyRecordWrite => b.fail_inserts("", None),
        Cut::KeySave => b.keys.fail_writes(false),
        Cut::QuitAtKeySave => {}
    }
    if restart { b.reopen(); }
    // B is still locked and has sent nothing.
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked, "after {cut:?}");
    assert_eq!(b.push().await, 0, "after {cut:?}");
    assert_eq!(service.rows(), rows, "after {cut:?}");
    // Entered again, after the scan that every Unlock starts.
    b.e2ee.rescan();
    b.scan().await;
    // Past the first step, B's own reset codes are refused as dropped for
    // the account's code, and change nothing.
    if !matches!(cut, Cut::RecordWrite) {
        let held = b.reset_state();
        for code in [&k2, &k4] {
            assert_eq!(b.e2ee.unlock(code.as_str(), &b.lib, &b.keys).unwrap_err(), DROPPED, "after {cut:?} (restart: {restart})");
        }
        assert_eq!(b.reset_state(), held, "a refused code changed the record or the marker row");
        assert_eq!(b.push().await, 0);
        assert_eq!(service.rows(), rows);
    }
    if let Err(error) = b.e2ee.unlock(k3.as_str(), &b.lib, &b.keys) {
        panic!("after {cut:?} (restart: {restart}) the account's code was refused: {error}");
    }
    the_accounts_code_unlocked(&service, &mut a, &mut b, &k3, &folder, &own).await;
}

#[tokio::test]
async fn the_accounts_code_still_unlocks_after_its_record_write_fails_over_a_second_reset() {
    for restart in [false, true] { the_accounts_code_unlocks_after_the_unlock_is_cut_short(Cut::RecordWrite, restart).await; }
}

#[tokio::test]
async fn the_accounts_code_still_unlocks_after_its_marker_write_fails_over_a_second_reset() {
    for restart in [false, true] { the_accounts_code_unlocks_after_the_unlock_is_cut_short(Cut::MarkerWrite, restart).await; }
}

#[tokio::test]
async fn the_accounts_code_still_unlocks_after_the_write_naming_its_key_fails_over_a_second_reset() {
    for restart in [false, true] { the_accounts_code_unlocks_after_the_unlock_is_cut_short(Cut::KeyRecordWrite, restart).await; }
}

#[tokio::test]
async fn the_accounts_code_still_unlocks_after_its_key_save_fails_over_a_second_reset() {
    for restart in [false, true] { the_accounts_code_unlocks_after_the_unlock_is_cut_short(Cut::KeySave, restart).await; }
}

#[tokio::test]
async fn the_accounts_code_still_unlocks_after_a_quit_while_its_key_is_stored_over_a_second_reset() {
    the_accounts_code_unlocks_after_the_unlock_is_cut_short(Cut::QuitAtKeySave, true).await;
}

#[tokio::test]
async fn a_reset_cut_short_is_never_finished_without_a_scan_so_a_later_reset_elsewhere_wins() {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, _k1, folder) = two_devices_on_one_code(&service).await;
    // B's marker would sort above A's later one.
    b.run_ahead(600_000);
    let k2 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    let own = b.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    let k3 = a.reset().await;
    a.sync().await.unwrap();
    a.sync().await.unwrap();
    let elsewhere = a.lib.store.entity_hlc(MARKER_KIND, MARKER_ID).unwrap().unwrap();
    assert!(elsewhere < own);

    // B starts offline, so the scan before Unlock can't run: its own code
    // doesn't finish the reset, and a code it can't check isn't called wrong.
    let held = b.reset_state();
    assert_eq!(b.e2ee.unlock_unchecked(k2.as_str(), &b.lib, &b.keys).unwrap_err(), UNCHECKED_RESET);
    assert_eq!(b.e2ee.unlock_unchecked(k3.as_str(), &b.lib, &b.keys).unwrap_err(), UNCHECKED);
    assert_eq!(b.e2ee.status(), E2eeStatus::Locked);
    assert_eq!(b.reset_state(), held, "a refused code changed the record or the marker row");
    assert_eq!(b.push().await, 0);

    // Online, the scan finds A's reset: B's code is superseded and A's unlocks.
    assert_eq!(b.pull().await, Err(PageError::Locked));
    assert_eq!(b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).unwrap_err(), SUPERSEDED);
    b.e2ee.unlock(k3.as_str(), &b.lib, &b.keys).expect("the newest code unlocks");
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    let markers: Vec<_> = service.rows().into_iter().filter(|row| row["kind"] == MARKER_KIND).collect();
    assert_eq!(markers.len(), 1);
    assert_ne!(markers[0]["hlc"], own.encode(), "B's unfinished reset reached the server");
    assert!(a.pull().await.is_ok(), "A was locked out by B's unfinished reset");
    assert_eq!(a.e2ee.status(), E2eeStatus::On);
    assert!(b.lib.folders.contains_key(&folder));
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_reset_cut_short_with_no_reset_elsewhere_is_finished_by_its_code_once_the_scan_runs() {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, _k1, folder) = two_devices_on_one_code(&service).await;
    let k2 = b.begin_reset().await;
    b.confirm_reset_and_quit();
    assert_eq!(b.e2ee.unlock_unchecked(k2.as_str(), &b.lib, &b.keys).unwrap_err(), UNCHECKED_RESET);
    b.e2ee.rescan();
    b.scan().await;
    b.e2ee.unlock(k2.as_str(), &b.lib, &b.keys).expect("the reset's own code finishes it");
    b.sync().await.unwrap();
    b.sync().await.unwrap();
    assert_eq!(a.pull().await, Err(PageError::Locked));
    a.e2ee.unlock(k2.as_str(), &a.lib, &a.keys).unwrap();
    a.sync().await.unwrap();
    assert!(a.lib.folders.contains_key(&folder));
    assert_ciphertext_only(&service);
}

/// Sync both devices until neither has anything left, then prove a further
/// round writes nothing: reconciling profiles never loops.
async fn settle(service: &SyncService, a: &mut Device, b: &mut Device) {
    for _ in 0..3 {
        a.sync().await.unwrap();
        b.sync().await.unwrap();
    }
    let rows = service.rows();
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    assert_eq!(service.rows(), rows, "a further round wrote again");
    assert_eq!((a.lib.store.dirty_count().unwrap(), b.lib.store.dirty_count().unwrap()), (0, 0));
}

#[tokio::test]
async fn a_pin_added_elsewhere_while_its_space_moved_profile_follows_the_space_on_both_devices() {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, k1, _) = two_devices_on_one_code(&service).await;
    // B, signed out and Locked, moves a space to a new profile and deletes the old one.
    LibraryEncryption::forget_key(ACCOUNT, &b.keys).unwrap();
    b.restart();
    let space = b.lib.spaces_ordered()[0].0.clone();
    let old = b.lib.spaces[&space].profile_id.clone();
    let moved = b.lib.create_named_profile("Locked Profile Secret").unwrap();
    b.lib.set_space_profile(&space, &moved).unwrap();
    b.lib.delete_profile(&old).unwrap();
    assert_eq!(b.push().await, 0);
    // A meanwhile pins a tab into that space under the old profile.
    let pin = a.lib.add_pinned(&space, None, "https://concurrent.example/", "Concurrent Pin").unwrap();
    a.sync().await.unwrap();
    assert_eq!(b.pull().await, Err(PageError::Locked));
    b.e2ee.unlock(k1.as_str(), &b.lib, &b.keys).unwrap();
    settle(&service, &mut a, &mut b).await;

    for lib in [&a.lib, &b.lib] {
        assert_eq!(lib.spaces[&space].profile_id, moved);
        assert_eq!(lib.items[&pin].profile_id, moved, "the pin kept a profile its space no longer uses");
        assert!(!lib.profiles.contains_key(&old));
    }
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(&service);
    assert!(!server_text(&service).contains("Locked Profile Secret"));
}

#[tokio::test]
async fn a_pin_edited_elsewhere_with_its_old_profile_follows_its_moved_space_without_unencrypted_sync() {
    let service = Arc::new(SyncService::default());
    let mut a = Device::new(&service, "device-a");
    a.sync().await.unwrap();
    let mut b = Device::new(&service, "device-b");
    b.sync().await.unwrap();
    a.sync().await.unwrap();
    let space = a.lib.spaces_ordered()[0].0.clone();
    let pin = a.lib.add_pinned(&space, None, "https://shared.example/", "Shared Pin").unwrap();
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    assert!(b.lib.items.contains_key(&pin));

    // B moves the space; A then renames the pin, whose whole row carries the
    // old profile. A's clock runs ahead, so its row wins last-writer-wins
    // over B's move of the pin, however close together the two edits are.
    let ahead = Hlc::new(crate::now_ms() + 600_000, 0, a.lib.node());
    a.lib.store.kv_set("hlc_last", &ahead.encode()).unwrap();
    a.reopen();
    let moved = b.lib.create_named_profile("Moved").unwrap();
    b.lib.set_space_profile(&space, &moved).unwrap();
    a.lib.update_item(&pin, |item| item.title = "Renamed Elsewhere".into()).unwrap();
    a.sync().await.unwrap();
    settle(&service, &mut a, &mut b).await;

    for lib in [&a.lib, &b.lib] {
        assert_eq!(lib.spaces[&space].profile_id, moved);
        assert_eq!(lib.items[&pin].profile_id, moved, "the pin kept a profile its space no longer uses");
        assert_eq!(lib.items[&pin].title, "Renamed Elsewhere", "the edit from the other device was lost");
    }
    same_library(&a.lib, &b.lib);
}

#[tokio::test]
async fn a_favorite_added_elsewhere_to_a_deleted_profile_is_deleted_on_both_devices() {
    let service = Arc::new(SyncService::default());
    let (mut a, mut b, _, _) = two_devices_on_one_code(&service).await;
    let doomed = a.lib.create_named_profile("Doomed Profile").unwrap();
    a.sync().await.unwrap();
    b.sync().await.unwrap();
    assert!(b.lib.profiles.contains_key(&doomed));

    // A adds a favorite to that profile; B, not yet aware of it, deletes the profile.
    let favorite = a.lib.add_favorite(&doomed, "https://late-favorite.example/", "Late Favorite").unwrap();
    a.sync().await.unwrap();
    b.lib.delete_profile(&doomed).unwrap();
    settle(&service, &mut a, &mut b).await;

    for lib in [&a.lib, &b.lib] {
        assert!(!lib.profiles.contains_key(&doomed));
        assert!(!lib.items.contains_key(&favorite), "a favorite of a deleted profile survived");
    }
    same_library(&a.lib, &b.lib);
    assert_ciphertext_only(&service);
}

impl Device {
    /// Run this device's clock `ms` ahead of the others from now on.
    fn run_ahead(&mut self, ms: u64) {
        let ahead = Hlc::new(crate::now_ms() + ms, 0, self.lib.node());
        self.lib.store.kv_set("hlc_last", &ahead.encode()).unwrap();
        self.reopen();
    }

    /// The clock of this device's own version of `space`.
    fn space_version(&self, space: &str) -> Hlc {
        self.lib.store.entity_hlc(kinds::SPACE, space).unwrap().unwrap()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ahead { Neither, W, T }

/// W and T are on the account's current code. T, offline, moves a Space to
/// another profile while W resets, and with `w_renames` W then renames it.
/// With `old_key_move`, L, still on the earlier code, moves it again, so its
/// newest server version is one neither W nor T can open and each keeps its
/// own. W then pins a tab there. Returns W, T, the Space, the pin, and how
/// many rows the server took in `rounds` syncs of W and T from then on.
async fn pin_into_a_space_held_in_two_versions(service: &Arc<SyncService>, old_key_move: bool, ahead: Ahead, w_renames: bool, rounds: usize) -> (Device, Device, String, String, u64) {
    let (mut w, mut t, k1, _) = two_devices_on_one_code(service).await;
    let mut l = Device::new(service, "device-l");
    assert_eq!(l.pull().await, Err(PageError::Locked));
    l.e2ee.unlock(k1.as_str(), &l.lib, &l.keys).unwrap();
    l.sync().await.unwrap();
    l.sync().await.unwrap();
    let q = w.lib.create_named_profile("Profile Q").unwrap();
    let r = w.lib.create_named_profile("Profile R").unwrap();
    match ahead {
        Ahead::Neither => {}
        Ahead::W => w.run_ahead(600_000),
        Ahead::T => t.run_ahead(600_000),
    }
    // L's move must stay the newest server version.
    if ahead != Ahead::Neither { l.run_ahead(1_200_000); }
    w.sync().await.unwrap();
    for _ in 0..2 {
        t.sync().await.unwrap();
        l.sync().await.unwrap();
        w.sync().await.unwrap();
    }
    let space = w.lib.spaces_ordered()[0].0.clone();
    t.lib.set_space_profile(&space, &q).unwrap();
    let k2 = w.reset().await;
    if w_renames { w.lib.update_space(&space, |space| space.name = "Renamed After The Reset".into()).unwrap(); }
    w.sync().await.unwrap();
    w.sync().await.unwrap();
    if old_key_move {
        // L's connection still counts as caught up (residual 6).
        l.lib.set_space_profile(&space, &r).unwrap();
        assert!(l.push().await > 0);
    }
    assert_eq!(t.pull().await, Err(PageError::Locked));
    t.e2ee.unlock(k2.as_str(), &t.lib, &t.keys).unwrap();
    for _ in 0..2 { let _ = t.sync().await; }
    let _ = w.sync().await;
    let pin = w.lib.add_pinned(&space, None, "https://ping-pong.example/", "Ping Pong").unwrap();
    let start = service.cursor();
    for _ in 0..rounds {
        let _ = w.sync().await;
        let _ = t.sync().await;
    }
    let writes = service.cursor() - start;
    (w, t, space, pin, writes)
}

/// The device holding the newer version of the Space decides the pin's
/// profile on both devices: the pin is written once by W and, when T holds
/// the newer version, once more by T. Then nothing is written again.
async fn a_pin_settles_in_a_space_held_in_two_versions(ahead: Ahead, w_renames: bool, w_newer: bool) {
    let service = Arc::new(SyncService::default());
    let (mut w, mut t, space, pin, writes) = pin_into_a_space_held_in_two_versions(&service, true, ahead, w_renames, 20).await;
    assert_ne!(w.lib.spaces[&space], t.lib.spaces[&space], "the fixture no longer holds two versions of the Space");
    assert_eq!(w.space_version(&space) > t.space_version(&space), w_newer);
    assert_eq!(writes, if w_newer { 1 } else { 2 }, "20 rounds wrote {writes} rows");
    let rows = service.rows();
    w.sync().await.unwrap();
    t.sync().await.unwrap();
    assert_eq!(service.rows(), rows, "a further round wrote again");
    assert_eq!(w.lib.items[&pin], t.lib.items[&pin]);
    let newest = if w_newer { &w } else { &t };
    assert_eq!(w.lib.items[&pin].profile_id, newest.lib.spaces[&space].profile_id);
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_pin_in_a_space_two_devices_hold_in_different_versions_is_not_rewritten_back_and_forth() {
    a_pin_settles_in_a_space_held_in_two_versions(Ahead::Neither, false, false).await;
}

#[tokio::test]
async fn a_pin_in_a_space_held_in_two_versions_settles_when_the_other_device_runs_ahead() {
    a_pin_settles_in_a_space_held_in_two_versions(Ahead::T, false, false).await;
}

#[tokio::test]
async fn a_pin_in_a_space_held_in_two_versions_settles_when_the_pinning_device_runs_ahead() {
    // A reset keeps each row's own clock, so T's move is still newer.
    a_pin_settles_in_a_space_held_in_two_versions(Ahead::W, false, false).await;
    // W's rename after the reset is newer.
    a_pin_settles_in_a_space_held_in_two_versions(Ahead::W, true, true).await;
}

impl Device {
    /// Repair `pin` as a Crossbow from before pins kept their Space's version
    /// does: take this device's Space profile and drop the field.
    fn repair_as_an_older_crossbow(&mut self, pin: &str, space: &str) {
        let profile = self.lib.spaces[space].profile_id.clone();
        if self.lib.items[pin].profile_id != profile {
            self.lib.put_item_as_older_crossbow(pin, |item| { item.profile_id = profile; item.space_version = None; }).unwrap();
        }
    }
}

/// W, holding the older version of the Space, runs an older Crossbow that
/// repairs the pin after every sync (the sixth recheck's probe). T repairs
/// it once for its own version and profile and then leaves W's choice.
#[tokio::test]
async fn a_pin_an_older_crossbow_rewrites_is_answered_once_per_version_of_its_space() {
    let service = Arc::new(SyncService::default());
    let (mut w, mut t, space, pin, _) = pin_into_a_space_held_in_two_versions(&service, true, Ahead::Neither, false, 0).await;
    assert!(w.space_version(&space) < t.space_version(&space));
    let start = service.cursor();
    for _ in 0..20 {
        let _ = t.sync().await;
        let _ = w.sync().await;
        w.repair_as_an_older_crossbow(&pin, &space);
    }
    // W's pin, T's repair, W's rewrite.
    let writes = service.cursor() - start;
    assert_eq!(writes, 3, "20 rounds wrote {writes} rows");
    assert_eq!(w.lib.items[&pin], t.lib.items[&pin]);
    assert_eq!(t.lib.items[&pin].profile_id, w.lib.spaces[&space].profile_id);

    // The note survives a restart.
    t.reopen();
    let rows = service.rows();
    for _ in 0..2 {
        let _ = t.sync().await;
        let _ = w.sync().await;
        w.repair_as_an_older_crossbow(&pin, &space);
    }
    assert_eq!(service.rows(), rows, "T answered the older Crossbow again after a restart");

    // A newer version of the Space is a new basis: T repairs the pin once,
    // and as W can open that version, both settle on it.
    t.lib.update_space(&space, |space| space.name = "Renamed On T".into()).unwrap();
    let start = service.cursor();
    for _ in 0..20 {
        let _ = t.sync().await;
        let _ = w.sync().await;
        w.repair_as_an_older_crossbow(&pin, &space);
    }
    // The Space, T's repair of the pin, and W's of the fixture's other pin
    // there, whose newest version only L can open.
    let writes = service.cursor() - start;
    assert_eq!(writes, 3, "20 rounds after the rename wrote {writes} rows");
    assert_eq!(w.lib.items[&pin], t.lib.items[&pin]);
    assert_eq!(w.lib.spaces[&space], t.lib.spaces[&space]);
    assert_eq!(w.lib.items[&pin].profile_id, w.lib.spaces[&space].profile_id);
    assert_ciphertext_only(&service);
}

/// One rename of the pin by an older Crossbow on T, which holds the newer
/// version, costs W's repair and T's answer, then nothing.
#[tokio::test]
async fn one_edit_by_an_older_crossbow_settles_after_one_repair_each() {
    let service = Arc::new(SyncService::default());
    let (mut w, mut t, space, pin, _) = pin_into_a_space_held_in_two_versions(&service, true, Ahead::Neither, false, 3).await;
    let start = service.cursor();
    t.lib.put_item_as_older_crossbow(&pin, |item| { item.custom_title = Some("Renamed by an older Crossbow".into()); item.space_version = None; }).unwrap();
    for _ in 0..20 {
        let _ = w.sync().await;
        let _ = t.sync().await;
    }
    let writes = service.cursor() - start;
    assert_eq!(writes, 3, "20 rounds wrote {writes} rows");
    assert_eq!(w.lib.items[&pin], t.lib.items[&pin]);
    assert_eq!(w.lib.items[&pin].custom_title.as_deref(), Some("Renamed by an older Crossbow"));
    assert_eq!(w.lib.items[&pin].profile_id, t.lib.spaces[&space].profile_id);
    assert_ciphertext_only(&service);
}

/// Z1, with every device on this version: a pin from before pins kept their
/// Space's version, in a Space W and T hold in two versions. T, holding the
/// newer one, repairs it; W renames its copy before it receives the repair,
/// so W's edit is newer. T still decides, as for any other pin: both settle
/// on its profile with the rename, and nothing more is written.
#[tokio::test]
async fn a_pin_from_before_space_versions_renamed_on_the_older_holder_still_settles_on_the_newer_one() {
    let service = Arc::new(SyncService::default());
    let (mut w, mut t, space, pin, _) = pin_into_a_space_held_in_two_versions(&service, true, Ahead::Neither, false, 0).await;
    assert!(t.space_version(&space) > w.space_version(&space));
    w.lib.put_item_as_older_crossbow(&pin, |item| item.space_version = None).unwrap();
    let _ = w.sync().await;
    let _ = t.sync().await;
    assert_eq!(t.lib.items[&pin].profile_id, t.lib.spaces[&space].profile_id, "T repaired the pin first");
    std::thread::sleep(std::time::Duration::from_millis(5));
    w.lib.update_item(&pin, |item| item.custom_title = Some("Renamed on W".into())).unwrap();
    let start = service.cursor();
    for _ in 0..20 {
        let _ = w.sync().await;
        let _ = t.sync().await;
    }
    // W's rename and T's repair.
    let writes = service.cursor() - start;
    assert_eq!(writes, 2, "20 rounds wrote {writes} rows");
    t.reopen();
    let rows = service.rows();
    let _ = t.sync().await;
    let _ = w.sync().await;
    assert_eq!(service.rows(), rows, "a further round after T restarted wrote again");
    assert_eq!(w.lib.items[&pin], t.lib.items[&pin]);
    assert_eq!(t.lib.items[&pin].custom_title.as_deref(), Some("Renamed on W"));
    assert_eq!(t.lib.items[&pin].profile_id, t.lib.spaces[&space].profile_id, "the device holding the newer version decides");
    assert_ciphertext_only(&service);
}

#[tokio::test]
async fn a_pin_in_a_space_both_devices_hold_in_the_same_version_is_written_once() {
    let service = Arc::new(SyncService::default());
    let (w, t, space, pin, writes) = pin_into_a_space_held_in_two_versions(&service, false, Ahead::Neither, false, 20).await;
    assert_eq!(w.space_version(&space), t.space_version(&space));
    assert_eq!(writes, 1, "a pin whose Space both devices agree on was written again");
    assert_eq!(w.lib.items[&pin], t.lib.items[&pin]);
    assert_eq!(w.lib.items[&pin].profile_id, w.lib.spaces[&space].profile_id);
    assert_ciphertext_only(&service);
}
