//! What a test fixes on its own thread so that a file it writes is the same
//! on every run: the ids `new_id` hands out, the time `now_ms` reads and the
//! vault's random bytes. Only this crate's tests compile it, and nothing
//! changes until a test starts a [`Script`].

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

thread_local! {
    static IDS: RefCell<Option<VecDeque<String>>> = const { RefCell::new(None) };
    static NOW_MS: Cell<Option<u64>> = const { Cell::new(None) };
    static RANDOM: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Until it drops, `new_id` hands out the scripted ids in order and panics
/// past the last, `now_ms` reads the scripted time, and the vault's random
/// bytes are a fixed stream from the seed.
pub(crate) struct Script(());

impl Script {
    pub(crate) fn start(ids: &[&str], now_ms: u64, seed: u64) -> Script {
        IDS.with(|queued| {
            let mut queued = queued.borrow_mut();
            assert!(queued.is_none(), "one script at a time");
            *queued = Some(ids.iter().map(|id| id.to_string()).collect());
        });
        NOW_MS.set(Some(now_ms));
        RANDOM.set(Some(seed));
        Script(())
    }

    /// The scripted ids not handed out yet.
    pub(crate) fn ids_left(&self) -> usize {
        IDS.with(|queued| queued.borrow().as_ref().map_or(0, VecDeque::len))
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        IDS.with(|queued| queued.borrow_mut().take());
        NOW_MS.set(None);
        RANDOM.set(None);
    }
}

pub(crate) fn id() -> Option<String> {
    IDS.with(|queued| queued.borrow_mut().as_mut().map(|queued| queued.pop_front().expect("the test used more ids than it scripted")))
}

pub(crate) fn now_ms() -> Option<u64> {
    NOW_MS.get()
}

/// Fill `bytes` from the scripted stream (SplitMix64), if a script is running.
pub(crate) fn fill(bytes: &mut [u8]) -> bool {
    let Some(mut state) = RANDOM.get() else { return false };
    for chunk in bytes.chunks_mut(8) {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        chunk.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes()[..chunk.len()]);
    }
    RANDOM.set(Some(state));
    true
}
