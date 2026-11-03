//! Deleting an account's sync data, and devices that still hold it.
use super::*;
use super::test_server::{Reply, Server, SyncService};

fn service_server(service: &Arc<SyncService>, device: &str) -> Server {
    let (service, device) = (service.clone(), device.to_string());
    Server::start(move |request| service.reply(request, &device).unwrap_or_else(|| panic!("unexpected fixture request: {} {}", request.method, request.path)))
}

fn worker_on(server: &Server, sync_epoch: Option<u64>) -> (Worker, std_mpsc::Receiver<(u64, SyncEvent)>) {
    let (mut worker, incoming) = worker_at(&server.url);
    worker.live.as_mut().unwrap().sync_epoch = sync_epoch;
    (worker, incoming)
}

fn events(incoming: &std_mpsc::Receiver<(u64, SyncEvent)>) -> Vec<SyncEvent> {
    incoming.try_iter().map(|(_, event)| event).collect()
}

/// A service holding one row from a device on epoch 0.
async fn holding_a_row(id: &str) -> (Arc<SyncService>, Server) {
    let service = Arc::new(SyncService::default());
    let server = service_server(&service, "device-a");
    let (mut worker, incoming) = worker_on(&server, Some(0));
    worker.push(vec![change(id)]).await;
    assert!(events(&incoming).iter().any(|e| matches!(e, SyncEvent::Pushed { accepted, .. } if accepted.len() == 1)));
    (service, server)
}

#[tokio::test]
async fn deleting_empties_only_this_accounts_store_and_reads_the_new_epoch_back() {
    let (service, server) = holding_a_row("mine").await;
    let (other, _other_server) = holding_a_row("theirs").await;
    let (mut worker, incoming) = worker_on(&server, Some(0));
    worker.delete_sync_data().await;
    let seen = events(&incoming);
    assert!(seen.iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { here: true })), "{seen:?}");
    assert!(service.rows().is_empty());
    assert_eq!(service.sync_epoch(), 1);
    assert_eq!(other.rows().len(), 1, "another account's store was touched");
    assert_eq!(other.sync_epoch(), 0);
    // The delete, then a readback of the stream from its start.
    let requests: Vec<_> = server.requests.lock().unwrap().iter().map(|r| format!("{} {}", r.method, r.path)).collect();
    assert_eq!(&requests[requests.len() - 2..], ["DELETE /v1/sync/data", &format!("GET /v1/sync/pull?since=0&limit={MAX_PULL_CHANGES}")]);
    // Nothing transfers from this device until sync is turned on again.
    assert!(worker.live.is_none());
    worker.push(vec![change("after")]).await;
    assert!(service.rows().is_empty());
}

#[tokio::test]
async fn deleting_again_is_fine() {
    let (service, server) = holding_a_row("mine").await;
    for epoch in [0, 1] {
        let (mut worker, incoming) = worker_on(&server, Some(epoch));
        worker.delete_sync_data().await;
        assert!(events(&incoming).iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { here: true })));
    }
    assert!(service.rows().is_empty());
    assert_eq!(service.sync_epoch(), 2);
}

#[tokio::test]
async fn a_device_holding_deleted_data_is_refused_and_never_uploads_it() {
    let (service, server) = holding_a_row("mine").await;
    let (mut deleting, _) = worker_on(&server, Some(0));
    deleting.delete_sync_data().await;

    // Its upload is refused whole, and it stops.
    let (mut stale, incoming) = worker_on(&server, Some(0));
    stale.push(vec![change("stale-edit")]).await;
    assert!(events(&incoming).iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { here: false })));
    assert!(stale.live.is_none());
    assert!(service.rows().is_empty());

    // A pull that names the new epoch stops it before anything is applied.
    let (mut stale, incoming) = worker_on(&server, Some(0));
    stale.pull_all().await;
    let seen = events(&incoming);
    assert!(seen.iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { here: false })));
    assert!(!seen.iter().any(|e| matches!(e, SyncEvent::Pulled { .. })), "{seen:?}");
    assert!(stale.live.is_none());

    // A Crossbow from before epochs sends none: refused too.
    let older = server.requests.lock().unwrap().len();
    let body = serde_json::to_string(&serde_json::json!({ "changes": [change("older-client")] })).unwrap();
    let response = reqwest::Client::new().post(format!("{}/v1/sync/push", server.url)).body(body).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 409);
    assert!(response.text().await.unwrap().contains(error_codes::SYNC_EPOCH_CHANGED));
    assert_eq!(server.requests.lock().unwrap().len(), older + 1);
    assert!(service.rows().is_empty());
}

#[tokio::test]
async fn a_device_new_to_the_account_takes_the_epoch_its_first_pull_names() {
    let (service, server) = holding_a_row("mine").await;
    let (mut deleting, _) = worker_on(&server, Some(0));
    deleting.delete_sync_data().await;
    let (mut fresh, incoming) = worker_on(&server, None);
    // Before any pull it doesn't know the epoch, so it uploads nothing.
    fresh.push(vec![change("too-early")]).await;
    assert!(service.rows().is_empty());
    fresh.pull_all().await;
    let seen = events(&incoming);
    let adopted = seen.iter().position(|e| matches!(e, SyncEvent::SyncEpoch(1))).expect("the epoch is reported");
    let pulled = seen.iter().position(|e| matches!(e, SyncEvent::Pulled { .. })).unwrap();
    assert!(adopted < pulled, "the epoch is reported before the page it came with");
    fresh.acknowledge_pull(match &seen[pulled] { SyncEvent::Pulled { batch_id, .. } => *batch_id, _ => unreachable!() }, true).await;
    fresh.push(vec![change("fresh-upload")]).await;
    assert_eq!(service.rows().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["fresh-upload"]);
}

#[tokio::test]
async fn a_server_without_the_delete_route_changes_nothing() {
    let (service, server) = holding_a_row("mine").await;
    service.lack_delete();
    let (mut worker, incoming) = worker_on(&server, Some(0));
    worker.delete_sync_data().await;
    let seen = events(&incoming);
    assert!(seen.iter().any(|e| matches!(e, SyncEvent::DeleteFailed(reason) if reason.starts_with("unsupported"))), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { .. })));
    assert!(worker.live.is_some(), "sync keeps running");
    assert_eq!(service.rows().len(), 1);
}

#[tokio::test]
async fn a_server_that_refuses_the_method_is_one_without_the_route() {
    // However the refusal is worded, a 4xx other than 401 means this server
    // doesn't offer the delete; a 5xx is a failure worth trying again.
    for (status, body, unsupported) in [
        (405, "Method Not Allowed", true),
        (405, r#"{"error":{"code":"method_not_allowed","message":"fixture"}}"#, true),
        (400, r#"{"error":{"code":"bad_request","message":"fixture"}}"#, true),
        (503, r#"{"error":{"code":"unavailable","message":"fixture"}}"#, false),
    ] {
        let server = Server::start(move |_| Reply::json(status, body));
        let (mut worker, incoming) = worker_on(&server, Some(0));
        worker.delete_sync_data().await;
        let seen = events(&incoming);
        assert!(seen.iter().any(|e| matches!(e, SyncEvent::DeleteFailed(reason) if reason.starts_with("unsupported") == unsupported)), "{status} {body}: {seen:?}");
        assert!(worker.live.is_some());
    }
}

#[tokio::test]
async fn a_reply_outside_the_contract_is_not_taken_as_a_delete() {
    // An epoch that doesn't move on, a readback that still holds rows, or a
    // body that isn't the contract's: each changes nothing here.
    for (delete, readback) in [
        (r#"{"sync_epoch":0,"deleted_rows":1}"#, r#"{"changes":[],"cursor":2,"more":false,"sync_epoch":0}"#),
        (r#"{"sync_epoch":1,"deleted_rows":1}"#, r#"{"changes":[],"cursor":2,"more":false,"sync_epoch":0}"#),
        (r#"{"ok":true}"#, r#"{"changes":[],"cursor":2,"more":false,"sync_epoch":1}"#),
    ] {
        let server = Server::start(move |request| if request.method == "DELETE" { Reply::json(200, delete) } else { Reply::json(200, readback) });
        let (mut worker, incoming) = worker_on(&server, Some(0));
        worker.delete_sync_data().await;
        let seen = events(&incoming);
        assert!(seen.iter().any(|e| matches!(e, SyncEvent::DeleteFailed(_))), "{delete} {readback}: {seen:?}");
        assert!(!seen.iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { .. })));
        assert!(worker.live.is_some());
    }
}

#[tokio::test]
async fn a_delete_whose_readback_failed_is_still_known_as_this_devices_own() {
    // The delete went through but the readback couldn't run: the next pull
    // names the new epoch, and this device says it deleted the data itself.
    let service = Arc::new(SyncService::default());
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (shared, count) = (service.clone(), reads.clone());
    let server = Server::start(move |request| {
        if request.method == "GET" && count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Reply::json(503, r#"{"error":{"code":"unavailable","message":"fixture network down"}}"#);
        }
        shared.reply(request, "device-a").unwrap()
    });
    let (mut worker, incoming) = worker_on(&server, Some(0));
    worker.delete_sync_data().await;
    assert!(events(&incoming).iter().any(|e| matches!(e, SyncEvent::DeleteFailed(_))));
    assert_eq!(service.sync_epoch(), 1);
    worker.pull_all().await;
    assert!(events(&incoming).iter().any(|e| matches!(e, SyncEvent::SyncDataDeleted { here: true })));
}

#[tokio::test]
async fn deleting_with_a_rejected_session_ends_the_session_and_deletes_nothing() {
    let (service, server) = holding_a_row("mine").await;
    let rejecting = Server::start(|_| Reply::json(401, r#"{"error":{"code":"unauthorized","message":"fixture revoked"}}"#));
    let (mut worker, incoming) = worker_on(&rejecting, Some(0));
    worker.delete_sync_data().await;
    assert!(events(&incoming).iter().any(|e| matches!(e, SyncEvent::SignedOut)));
    assert!(worker.live.is_none());
    assert_eq!(service.rows().len(), 1);
    drop(server);
}
