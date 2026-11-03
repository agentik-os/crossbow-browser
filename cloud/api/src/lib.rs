//! Crossbow API Worker (`crossbow-api`).
//!
//! `logic` is pure and tested natively. Everything else only builds for wasm32:
//! the HTTP front door (`entry`), auth and devices on D1 (`auth`, `db`), browser
//! sign-in with Clerk (`clerk`), sync routing (`sync`), the per-account
//! `SyncRoom` Durable Object (`room`), sign-in mail (`mail`) and the `/v1/ai/*`
//! proxy (`ai_proxy`).

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod ai_proxy;
#[cfg(target_arch = "wasm32")]
mod auth;
#[cfg(target_arch = "wasm32")]
mod clerk;
#[cfg(target_arch = "wasm32")]
mod db;
#[cfg(target_arch = "wasm32")]
mod entry;
#[cfg(target_arch = "wasm32")]
mod mail;
#[cfg(target_arch = "wasm32")]
mod room;
#[cfg(target_arch = "wasm32")]
mod rt;
#[cfg(target_arch = "wasm32")]
mod sync;

#[cfg(target_arch = "wasm32")]
pub use room::SyncRoom;
