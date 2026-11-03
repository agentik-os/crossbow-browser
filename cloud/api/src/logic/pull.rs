//! Pull query parsing and paging.

use crossbow_proto::{MAX_PULL_CHANGES, ServerChange};

use super::error::ApiError;

/// Upper bound on payload bytes in one pull response. A page stops early (with
/// `more = true`) once it would exceed this, so a full page of 64 KiB payloads
/// cannot exhaust Worker memory. Always returns at least one row.
pub const MAX_PULL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullQuery {
    pub since: u64,
    pub limit: usize,
}

/// Parse `since` and `limit` from a query string (without the leading `?`).
/// Missing `since` = 0; missing `limit` = `MAX_PULL_CHANGES`; limit is clamped
/// to `1..=MAX_PULL_CHANGES`.
pub fn parse_pull_query(query: Option<&str>) -> Result<PullQuery, ApiError> {
    let mut since = 0u64;
    let mut limit = MAX_PULL_CHANGES;
    for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "since" => {
                since = v.parse().map_err(|_| ApiError::bad_request("since must be an unsigned integer"))?
            }
            "limit" => {
                let n: u64 =
                    v.parse().map_err(|_| ApiError::bad_request("limit must be an unsigned integer"))?;
                limit = n.clamp(1, MAX_PULL_CHANGES as u64) as usize;
            }
            _ => {}
        }
    }
    Ok(PullQuery { since, limit })
}

/// Cut a page from rows fetched with `LIMIT limit + 1`, ordered by `seq`.
/// Returns `(changes, cursor, more)`.
pub fn page(mut rows: Vec<ServerChange>, q: PullQuery) -> (Vec<ServerChange>, u64, bool) {
    let mut more = rows.len() > q.limit;
    rows.truncate(q.limit);
    let mut bytes = 0usize;
    let mut keep = 0usize;
    for row in &rows {
        bytes += row.change.payload.len();
        if keep > 0 && bytes > MAX_PULL_BYTES {
            break;
        }
        keep += 1;
    }
    if keep < rows.len() {
        rows.truncate(keep);
        more = true;
    }
    let cursor = rows.last().map(|r| r.seq).unwrap_or(q.since);
    (rows, cursor, more)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_proto::{Change, Encoding, Hlc};

    fn row(seq: u64, payload_len: usize) -> ServerChange {
        ServerChange {
            seq,
            device_id: "dev_a".into(),
            change: Change {
                kind: "item".into(),
                id: format!("i{seq}"),
                hlc: Hlc::new(seq, 0, "n"),
                deleted: false,
                enc: Encoding::Json,
                payload: "x".repeat(payload_len),
            },
        }
    }

    #[test]
    fn query_parsing() {
        assert_eq!(parse_pull_query(None).unwrap(), PullQuery { since: 0, limit: MAX_PULL_CHANGES });
        assert_eq!(parse_pull_query(Some("since=12&limit=5")).unwrap(), PullQuery { since: 12, limit: 5 });
        assert_eq!(parse_pull_query(Some("limit=0")).unwrap().limit, 1);
        assert_eq!(parse_pull_query(Some("limit=999999")).unwrap().limit, MAX_PULL_CHANGES);
        assert_eq!(parse_pull_query(Some("since=-1")).unwrap_err().status, 400);
        assert_eq!(parse_pull_query(Some("since=abc")).unwrap_err().code, "bad_request");
    }

    #[test]
    fn paging_sets_more_and_cursor() {
        let q = PullQuery { since: 3, limit: 2 };
        let (rows, cursor, more) = page(vec![row(4, 1), row(5, 1), row(6, 1)], q);
        assert_eq!(rows.len(), 2);
        assert_eq!(cursor, 5);
        assert!(more);
        let (rows, cursor, more) = page(vec![row(4, 1)], q);
        assert_eq!((rows.len(), cursor, more), (1, 4, false));
        let (rows, cursor, more) = page(vec![], q);
        assert_eq!((rows.len(), cursor, more), (0, 3, false));
    }

    #[test]
    fn paging_respects_byte_budget_but_returns_one_row() {
        let q = PullQuery { since: 0, limit: 10 };
        let big = MAX_PULL_BYTES / 2 + 1;
        let (rows, cursor, more) = page(vec![row(1, big), row(2, big), row(3, 1)], q);
        assert_eq!((rows.len(), cursor, more), (1, 1, true));
        let (rows, _, _) = page(vec![row(1, MAX_PULL_BYTES * 2)], q);
        assert_eq!(rows.len(), 1);
    }
}
