//! Sync epochs. Deleting an account's sync data starts a new epoch, and a push
//! is stored only when it names the account's current one, so a device still
//! holding the deleted data can't upload it again.

use super::error::ApiError;

/// A push names no epoch (a client from before epochs) or the epoch of its
/// last pull. Either must be the account's current epoch, absent meaning 0.
pub fn check_push(current: u64, claimed: Option<u64>) -> Result<(), ApiError> {
    if claimed.unwrap_or(0) == current { Ok(()) } else { Err(ApiError::sync_epoch_changed()) }
}

/// The room's counters after deleting every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deletion {
    /// One past the last sequence number, so every device's cursor is behind
    /// it: their next pull, prompted by the broadcast, reads the new epoch.
    pub seq: u64,
    pub sync_epoch: u64,
}

pub fn plan_delete(seq_before: u64, epoch_before: u64) -> Deletion {
    Deletion { seq: seq_before + 1, sync_epoch: epoch_before + 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_proto::error_codes;

    #[test]
    fn a_push_must_name_the_current_sync_epoch() {
        // Clients from before sync epochs send none, which means epoch 0.
        assert!(check_push(0, None).is_ok());
        assert!(check_push(0, Some(0)).is_ok());
        assert!(check_push(3, Some(3)).is_ok());
        // After a delete, an older client and a device still on the old data are refused.
        for (current, claimed) in [(1, None), (1, Some(0)), (3, Some(2)), (3, Some(4))] {
            let refusal = check_push(current, claimed).unwrap_err();
            assert_eq!((refusal.status, refusal.code), (409, error_codes::SYNC_EPOCH_CHANGED), "{current} {claimed:?}");
        }
    }

    #[test]
    fn a_delete_starts_a_new_epoch_past_every_cursor() {
        let first = plan_delete(41, 0);
        assert_eq!((first.seq, first.sync_epoch), (42, 1));
        // A second delete is fine: it leaves the same empty store, one epoch on.
        let again = plan_delete(first.seq, first.sync_epoch);
        assert_eq!((again.seq, again.sync_epoch), (43, 2));
    }
}
