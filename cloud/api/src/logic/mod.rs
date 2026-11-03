//! Pure logic for the API Worker. Nothing in here touches the Workers runtime,
//! so every rule (validation, hashing, LWW, limits, token format) is unit tested
//! natively with `cargo test`.

pub mod account;
pub mod clerk;
pub mod epoch;
pub mod error;
pub mod limits;
pub mod lww;
pub mod pull;
pub mod route;
pub mod tokens;
pub mod validate;

/// One minute, in milliseconds.
pub const MINUTE_MS: u64 = 60 * 1000;
/// One hour, in milliseconds.
pub const HOUR_MS: u64 = 60 * MINUTE_MS;
/// One day, in milliseconds.
pub const DAY_MS: u64 = 24 * HOUR_MS;
