//! `SyncRoom`: one Durable Object per account (`idFromName(account_id)`), SQLite
//! storage. Holds the entity store and the sequence counter, and fans out
//! `ServerEvent`s to the account's WebSockets (Hibernation API).
//!
//! Only the API Worker can reach it (through the `SYNC_ROOM` namespace), so the
//! internal headers it reads are trusted.

use crossbow_proto::{
    Change, DeleteSyncDataResponse, Hlc, PullResponse, PushRequest, PushResponse, ServerChange, ServerEvent,
    error_codes,
};
use serde::Deserialize;
use worker::{
    DurableObject, Env, Method, Request, Response, Result, SqlStorage, SqlStorageValue, State,
    WebSocket, WebSocketIncomingMessage, WebSocketPair, WebSocketRequestResponsePair,
    durable_object,
};

use crate::auth::device_tag;
use crate::logic::epoch::{check_push, plan_delete};
use crate::logic::error::ApiError;
use crate::logic::lww::{StoredVersion, encoding_from_str, encoding_str, plan_push};
use crate::logic::pull::{MAX_PULL_BYTES, page, parse_pull_query};
use crate::rt::{H_DEVICE, H_SESSION_TAG, error_response, json, no_content};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entities (
  kind TEXT NOT NULL,
  id TEXT NOT NULL,
  hlc TEXT NOT NULL,
  deleted INTEGER NOT NULL,
  enc TEXT NOT NULL,
  payload TEXT NOT NULL,
  seq INTEGER NOT NULL,
  device_id TEXT NOT NULL,
  PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS entities_seq ON entities(seq);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v INTEGER NOT NULL);
";

/// Close code sent to sockets whose session or device lost access.
const CLOSE_UNAUTHORIZED: u16 = 4401;

#[durable_object]
pub struct SyncRoom {
    state: State,
    #[allow(dead_code)]
    env: Env,
}

#[derive(Deserialize)]
struct MetaRow {
    v: f64,
}

#[derive(Deserialize)]
struct CountRow {
    n: f64,
}

#[derive(Deserialize)]
struct VersionRow {
    hlc: String,
    deleted: f64,
}

#[derive(Deserialize)]
struct EntityRow {
    kind: String,
    id: String,
    hlc: String,
    deleted: f64,
    enc: String,
    payload: String,
    seq: f64,
    device_id: String,
}

#[derive(Deserialize)]
struct DisconnectBody {
    tag: String,
}

fn int(v: u64) -> SqlStorageValue {
    SqlStorageValue::Integer(v as i64)
}

fn text(v: &str) -> SqlStorageValue {
    SqlStorageValue::String(v.to_string())
}

fn meta_get(sql: &SqlStorage, k: &str) -> Result<Option<u64>> {
    let rows: Vec<MetaRow> = sql.exec("SELECT v FROM meta WHERE k = ?", vec![text(k)])?.to_array()?;
    Ok(rows.first().map(|r| r.v as u64))
}

fn meta_set(sql: &SqlStorage, k: &str, v: u64) -> Result<()> {
    sql.exec(
        "INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        vec![text(k), int(v)],
    )?;
    Ok(())
}

fn current_seq(sql: &SqlStorage) -> Result<u64> {
    Ok(meta_get(sql, "seq")?.unwrap_or(0))
}

/// Starts at 0 and moves up each time the account's sync data is deleted.
fn sync_epoch(sql: &SqlStorage) -> Result<u64> {
    Ok(meta_get(sql, "sync_epoch")?.unwrap_or(0))
}

/// Live (non-tombstone) rows, cached in `meta` under `live`.
fn live_rows(sql: &SqlStorage) -> Result<u64> {
    if let Some(v) = meta_get(sql, "live")? {
        return Ok(v);
    }
    let rows: Vec<CountRow> =
        sql.exec("SELECT COUNT(*) AS n FROM entities WHERE deleted = 0", None)?.to_array()?;
    Ok(rows.first().map(|r| r.n as u64).unwrap_or(0))
}

fn api_error(e: ApiError) -> Result<Response> {
    error_response(&e)
}

impl SyncRoom {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn broadcast(&self, event: &ServerEvent) {
        let Ok(msg) = serde_json::to_string(event) else { return };
        for ws in self.state.get_websockets() {
            // A socket that is closing may refuse the send; the others still get it.
            let _ = ws.send_with_str(&msg);
        }
    }

    async fn push(&self, mut req: Request) -> Result<Response> {
        let device = req.headers().get(H_DEVICE)?.unwrap_or_default();
        let input: PushRequest = match req.json().await {
            Ok(v) => v,
            Err(_) => return api_error(ApiError::bad_request("malformed push body")),
        };

        // From here to the response there is no `await`: reads, the LWW plan and
        // the writes run in one event, so the push commits atomically.
        let sql = self.sql();
        // A device holding deleted data names an earlier epoch, and a client
        // from before epochs none: neither may store it again.
        if let Err(e) = check_push(sync_epoch(&sql)?, input.sync_epoch) {
            return api_error(e);
        }
        let seq_before = current_seq(&sql)?;
        let live_before = live_rows(&sql)?;
        let mut lookup_err: Option<worker::Error> = None;
        let plan = plan_push(
            input.changes,
            |kind, id| {
                let res = sql
                    .exec("SELECT hlc, deleted FROM entities WHERE kind = ? AND id = ?", vec![text(kind), text(id)])
                    .and_then(|c| c.to_array::<VersionRow>());
                match res {
                    Ok(rows) => rows
                        .into_iter()
                        .next()
                        .map(|r| StoredVersion { hlc: r.hlc, deleted: r.deleted != 0.0 }),
                    Err(e) => {
                        lookup_err.get_or_insert(e);
                        None
                    }
                }
            },
            seq_before,
            live_before,
        );
        if let Some(e) = lookup_err {
            return Err(e);
        }
        let plan = match plan {
            Ok(p) => p,
            Err(_) => {
                return api_error(ApiError::payload_too_large(
                    "account entity quota exceeded (100000 live rows)",
                ));
            }
        };

        for w in &plan.writes {
            let c = &w.change;
            sql.exec(
                "INSERT INTO entities (kind, id, hlc, deleted, enc, payload, seq, device_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(kind, id) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted, \
                 enc = excluded.enc, payload = excluded.payload, seq = excluded.seq, device_id = excluded.device_id",
                vec![
                    text(&c.kind),
                    text(&c.id),
                    text(&c.hlc.encode()),
                    int(c.deleted as u64),
                    text(encoding_str(c.enc)),
                    text(&c.payload),
                    int(w.seq),
                    text(&device),
                ],
            )?;
        }
        let accepted = plan.writes.len() as u32;
        if accepted > 0 {
            meta_set(&sql, "seq", plan.cursor)?;
            meta_set(&sql, "live", plan.live)?;
            self.broadcast(&ServerEvent::Changed { cursor: plan.cursor, by_device: device });
        }
        json(200, &PushResponse { accepted, stale: plan.stale, cursor: plan.cursor })
    }

    fn pull(&self, req: &Request) -> Result<Response> {
        let url = req.url()?;
        let q = match parse_pull_query(url.query()) {
            Ok(q) => q,
            Err(e) => return api_error(e),
        };
        let sql = self.sql();
        let cursor = sql.exec(
            "SELECT kind, id, hlc, deleted, enc, payload, seq, device_id FROM entities \
             WHERE seq > ? ORDER BY seq LIMIT ?",
            vec![int(q.since), int(q.limit as u64 + 1)],
        )?;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        for row in cursor.next::<EntityRow>() {
            let r = row?;
            let hlc = Hlc::parse(&r.hlc).map_err(|e| worker::Error::RustError(e.to_string()))?;
            bytes += r.payload.len();
            rows.push(ServerChange {
                seq: r.seq as u64,
                device_id: r.device_id,
                change: Change {
                    kind: r.kind,
                    id: r.id,
                    hlc,
                    deleted: r.deleted != 0.0,
                    enc: encoding_from_str(&r.enc),
                    payload: r.payload,
                },
            });
            // Stop reading once over budget; `page` trims and sets `more`.
            if bytes > MAX_PULL_BYTES {
                break;
            }
        }
        let (changes, cursor, more) = page(rows, q);
        json(200, &PullResponse { changes, cursor, more, sync_epoch: sync_epoch(&sql)? })
    }

    /// Delete every row of this account (library rows, tombstones and the
    /// encryption marker) and start a new sync epoch. There is no `await`
    /// between the count and the response, so it commits atomically, like a
    /// push. Deleting again is fine: the store stays empty, one epoch on.
    fn delete_data(&self, req: &Request) -> Result<Response> {
        let device = req.headers().get(H_DEVICE)?.unwrap_or_default();
        let (deleted, seq) = self.clear()?;
        // Every device's cursor is now behind: they pull and read the new epoch.
        self.broadcast(&ServerEvent::Changed { cursor: seq, by_device: device });
        json(200, &deleted)
    }

    /// Delete Account: the rows go as in `delete_data`, and every socket of
    /// the account closes as unauthorized, so its signed-in devices stop.
    fn delete_account(&self) -> Result<Response> {
        let (deleted, _) = self.clear()?;
        for ws in self.state.get_websockets() {
            let _ = ws.close(Some(CLOSE_UNAUTHORIZED), Some("account deleted"));
        }
        json(200, &deleted)
    }

    /// Delete every row and move the epoch on: (what was deleted, the new seq).
    fn clear(&self) -> Result<(DeleteSyncDataResponse, u64)> {
        let sql = self.sql();
        let rows: Vec<CountRow> = sql.exec("SELECT COUNT(*) AS n FROM entities", None)?.to_array()?;
        let deleted_rows = rows.first().map(|r| r.n as u64).unwrap_or(0);
        let plan = plan_delete(current_seq(&sql)?, sync_epoch(&sql)?);
        sql.exec("DELETE FROM entities", None)?;
        meta_set(&sql, "seq", plan.seq)?;
        meta_set(&sql, "live", 0)?;
        meta_set(&sql, "sync_epoch", plan.sync_epoch)?;
        Ok((DeleteSyncDataResponse { sync_epoch: plan.sync_epoch, deleted_rows }, plan.seq))
    }

    fn accept_socket(&self, req: &Request) -> Result<Response> {
        let device = req.headers().get(H_DEVICE)?.unwrap_or_default();
        let session = req.headers().get(H_SESSION_TAG)?.unwrap_or_default();
        let pair = WebSocketPair::new()?;
        let dtag = device_tag(&device);
        self.state.accept_websocket_with_tags(&pair.server, &[dtag.as_str(), session.as_str()]);
        let hello = ServerEvent::Hello { cursor: current_seq(&self.sql())? };
        pair.server.send(&hello)?;
        Response::from_websocket(pair.client)
    }

    async fn disconnect(&self, mut req: Request) -> Result<Response> {
        let body: DisconnectBody = match req.json().await {
            Ok(b) => b,
            Err(_) => return api_error(ApiError::bad_request("malformed disconnect body")),
        };
        for ws in self.state.get_websockets_with_tag(&body.tag) {
            let _ = ws.close(Some(CLOSE_UNAUTHORIZED), Some("unauthorized"));
        }
        no_content()
    }
}

impl DurableObject for SyncRoom {
    fn new(state: State, env: Env) -> Self {
        if let Err(e) = state.storage().sql().exec(SCHEMA, None) {
            worker::console_error!("sync room schema: {e}");
        }
        // Keep-alive pings are answered by the runtime without waking the object.
        if let Ok(pair) = WebSocketRequestResponsePair::new("ping", "pong") {
            state.set_websocket_auto_response(&pair);
        }
        Self { state, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let path = req.path();
        match (req.method(), path.as_str()) {
            (Method::Post, "/push") => self.push(req).await,
            (Method::Get, "/pull") => self.pull(&req),
            (Method::Get, "/ws") => self.accept_socket(&req),
            (Method::Delete, "/data") => self.delete_data(&req),
            (Method::Delete, "/account") => self.delete_account(),
            (Method::Post, "/disconnect") => self.disconnect(req).await,
            _ => error_response(&ApiError::new(404, error_codes::NOT_FOUND, "not found")),
        }
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        // Clients only listen. Answer a stray ping; ignore anything else.
        if let WebSocketIncomingMessage::String(s) = message
            && s == "ping"
        {
            let _ = ws.send_with_str("pong");
        }
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, code: usize, _reason: String, _was_clean: bool) -> Result<()> {
        // Complete the close handshake. 1005/1006/1015 cannot be sent back.
        let code = match code as u16 {
            1005 | 1006 | 1015 => 1000,
            c => c,
        };
        let _ = ws.close(Some(code), Some("closing"));
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, error: worker::Error) -> Result<()> {
        worker::console_warn!("sync room socket error: {error}");
        Ok(())
    }
}
