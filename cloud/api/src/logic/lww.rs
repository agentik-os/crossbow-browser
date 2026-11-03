//! Last-writer-wins on the HLC string and the push planner used by `SyncRoom`.
//!
//! The planner is pure: it reads the stored `(hlc, deleted)` of an entity through
//! a closure and returns the rows to write. `SyncRoom` runs it and applies the
//! plan with synchronous SQL in the same event, so a push is atomic.

use std::collections::HashMap;

use crossbow_proto::{Change, Encoding, Hlc, StaleChange};

use super::limits::{live_delta, within_quota};

/// Strictly newer HLC wins. Equal clocks are stale, so retries are idempotent.
pub fn incoming_wins(stored_hlc: Option<&str>, incoming_hlc: &str) -> bool {
    match stored_hlc {
        None => true,
        Some(stored) => incoming_hlc > stored,
    }
}

/// Stored state of one entity, as far as LWW needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredVersion {
    pub hlc: String,
    pub deleted: bool,
}

/// One accepted change and the sequence number it gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedWrite {
    pub seq: u64,
    pub change: Change,
}

#[derive(Clone, Debug)]
pub struct PushPlan {
    pub writes: Vec<PlannedWrite>,
    pub stale: Vec<StaleChange>,
    /// Highest sequence number after the push.
    pub cursor: u64,
    /// Live (non-tombstone) rows after the push.
    pub live: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaExceeded;

/// Tombstones never carry a payload.
pub fn normalize(mut change: Change) -> Change {
    if change.deleted {
        change.payload.clear();
    }
    change
}

/// Plan a push. `changes` must already be validated. Within one push, later
/// changes see the effect of earlier ones (duplicates resolve by LWW too).
pub fn plan_push(
    changes: Vec<Change>,
    mut lookup: impl FnMut(&str, &str) -> Option<StoredVersion>,
    seq_before: u64,
    live_before: u64,
) -> Result<PushPlan, QuotaExceeded> {
    let mut overlay: HashMap<(String, String), StoredVersion> = HashMap::new();
    let mut writes = Vec::new();
    let mut stale = Vec::new();
    let mut seq = seq_before;
    let mut delta: i64 = 0;

    for change in changes {
        let change = normalize(change);
        let key = (change.kind.clone(), change.id.clone());
        let stored = match overlay.get(&key) {
            Some(v) => Some(v.clone()),
            None => lookup(&change.kind, &change.id),
        };
        let incoming = change.hlc.encode();
        if incoming_wins(stored.as_ref().map(|s| s.hlc.as_str()), &incoming) {
            delta += live_delta(stored.as_ref().map(|s| s.deleted), change.deleted);
            seq += 1;
            overlay.insert(key, StoredVersion { hlc: incoming, deleted: change.deleted });
            writes.push(PlannedWrite { seq, change });
        } else {
            let server_hlc = stored.map(|s| s.hlc).unwrap_or_default();
            stale.push(StaleChange {
                kind: change.kind,
                id: change.id,
                // Stored HLCs were written from parsed clocks, so they parse.
                server_hlc: Hlc::parse(&server_hlc).unwrap_or_else(|_| change.hlc.clone()),
            });
        }
    }

    if !within_quota(live_before, delta) {
        return Err(QuotaExceeded);
    }
    let live = (live_before as i64 + delta).max(0) as u64;
    Ok(PushPlan { writes, stale, cursor: seq, live })
}

/// Text form of an encoding, as stored in the `enc` column.
pub fn encoding_str(enc: Encoding) -> &'static str {
    match enc {
        Encoding::Json => "json",
        Encoding::Xchacha20V1 => "xchacha20_v1",
    }
}

pub fn encoding_from_str(s: &str) -> Encoding {
    match s {
        "xchacha20_v1" => Encoding::Xchacha20V1,
        _ => Encoding::Json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::limits::MAX_LIVE_ROWS;

    fn change(id: &str, wall: u64, deleted: bool) -> Change {
        Change {
            kind: "item".into(),
            id: id.into(),
            hlc: Hlc::new(wall, 0, "n1"),
            deleted,
            enc: Encoding::Json,
            payload: if deleted { "ignored".into() } else { format!("{{\"v\":{wall}}}") },
        }
    }

    #[test]
    fn lww_strictly_newer_wins() {
        let a = Hlc::new(10, 0, "a").encode();
        let b = Hlc::new(10, 1, "a").encode();
        assert!(incoming_wins(None, &a));
        assert!(incoming_wins(Some(&a), &b));
        assert!(!incoming_wins(Some(&b), &a));
        assert!(!incoming_wins(Some(&a), &a), "equal clocks are stale");
    }

    #[test]
    fn plan_assigns_sequences_and_reports_stale() {
        let stored = StoredVersion { hlc: Hlc::new(50, 0, "n1").encode(), deleted: false };
        let plan = plan_push(
            vec![change("new", 10, false), change("old", 40, false), change("old2", 60, false)],
            |_, id| match id {
                "old" | "old2" => Some(stored.clone()),
                _ => None,
            },
            7,
            3,
        )
        .unwrap();
        assert_eq!(plan.writes.iter().map(|w| w.seq).collect::<Vec<_>>(), vec![8, 9]);
        assert_eq!(plan.writes[0].change.id, "new");
        assert_eq!(plan.writes[1].change.id, "old2");
        assert_eq!(plan.stale.len(), 1);
        assert_eq!(plan.stale[0].id, "old");
        assert_eq!(plan.stale[0].server_hlc, Hlc::new(50, 0, "n1"));
        assert_eq!(plan.cursor, 9);
        assert_eq!(plan.live, 4); // one new live row; "old2" replaced a live row
    }

    #[test]
    fn duplicates_inside_one_push_resolve_in_order() {
        let plan =
            plan_push(vec![change("x", 20, false), change("x", 10, false)], |_, _| None, 0, 0)
                .unwrap();
        assert_eq!(plan.writes.len(), 1);
        assert_eq!(plan.stale[0].server_hlc, Hlc::new(20, 0, "n1"));
        assert_eq!(plan.cursor, 1);
    }

    #[test]
    fn tombstones_clear_payload_and_free_quota() {
        let stored = StoredVersion { hlc: Hlc::new(1, 0, "n1").encode(), deleted: false };
        let plan = plan_push(vec![change("x", 5, true)], |_, _| Some(stored.clone()), 1, 1).unwrap();
        assert_eq!(plan.writes[0].change.payload, "");
        assert!(plan.writes[0].change.deleted);
        assert_eq!(plan.live, 0);
    }

    #[test]
    fn quota_rejects_growth_past_the_limit() {
        let r = plan_push(vec![change("x", 5, false)], |_, _| None, 0, MAX_LIVE_ROWS);
        assert!(matches!(r, Err(QuotaExceeded)));
        // Updates and deletes still work at the limit.
        let stored = StoredVersion { hlc: Hlc::new(1, 0, "n1").encode(), deleted: false };
        assert!(plan_push(vec![change("x", 5, false)], |_, _| Some(stored.clone()), 0, MAX_LIVE_ROWS).is_ok());
    }

    #[test]
    fn encoding_text_roundtrip() {
        for e in [Encoding::Json, Encoding::Xchacha20V1] {
            assert_eq!(encoding_from_str(encoding_str(e)), e);
            // Same spelling as the wire (serde snake_case).
            assert_eq!(serde_json::to_value(e).unwrap(), encoding_str(e));
        }
    }
}
