//! Recovery codes and key bytes in tests, held as [`Hidden`]: compared by
//! value, printed only as `Hidden(..)`. A failed assert, or a message that
//! formats one, never writes a code or key to the test output. The app's
//! `test_codes` source check keeps tests from revealing one inside an
//! assert or a message. The app shares this file; each crate converts its
//! own codes and keys (crossbow-core in sync/tests.rs, the app in
//! test_codes.rs).

use std::fmt;

#[derive(Clone)]
pub(crate) struct Hidden<T>(T);

impl<T> Hidden<T> {
    pub(crate) const fn new(value: T) -> Hidden<T> {
        Hidden(value)
    }

    /// The value itself, to type, parse or derive from: as a call's
    /// argument, never inside an assert or a message.
    pub(crate) fn reveal(&self) -> &T {
        &self.0
    }

    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> Hidden<U> {
        Hidden(f(self.0))
    }
}

impl<T: PartialEq<U>, U> PartialEq<Hidden<U>> for Hidden<T> {
    fn eq(&self, other: &Hidden<U>) -> bool {
        self.0 == other.0
    }
}

impl<T> fmt::Debug for Hidden<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hidden(..)")
    }
}

impl<T> fmt::Display for Hidden<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hidden(..)")
    }
}

/// The message of the panic `f` raises. The panic hook stays quiet for this
/// thread only, so the message is never printed and a panic on another
/// thread still reports.
#[track_caller]
pub(crate) fn failure(f: impl FnOnce()) -> String {
    static SWAP: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _swapping = SWAP.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let quiet = std::thread::current().id();
    let previous = std::sync::Arc::new(std::panic::take_hook());
    let others = previous.clone();
    std::panic::set_hook(Box::new(move |info| if std::thread::current().id() != quiet { others(info) }));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    drop(std::panic::take_hook());
    match std::sync::Arc::try_unwrap(previous) {
        Ok(hook) => std::panic::set_hook(hook),
        Err(hook) => std::panic::set_hook(Box::new(move |info| hook(info))),
    }
    let payload = caught.expect_err("the assert must fail");
    payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string())).unwrap_or_default()
}
