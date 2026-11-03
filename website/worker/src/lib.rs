//! Separate public landing Worker. It never uses the browser's account API.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod entry;
