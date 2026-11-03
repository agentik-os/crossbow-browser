//! Content blocking: the lists that come with Crossbow ([`bundled`]: EasyList
//! and EasyPrivacy) and filter lists the user adds from files, kept for each
//! profile on this Mac and never synced. Crossbow downloads no list.
//!
//! [`filters`] reads a subset of the Adblock Plus syntax; a rule outside it
//! is counted and skipped, never approximated. [`matcher`] compiles a list
//! once per text, shared by every profile that added the same file, and
//! decides a request without locks or I/O. [`lists`] keeps each profile's
//! copies and their manifest, which also names the bundled lists it turned
//! off.

pub mod bundled;
pub mod exceptions;
pub mod filters;
pub mod hiding;
pub mod lists;
pub mod matcher;

#[cfg(test)]
#[path = "content_blocking/fixture_tests.rs"]
mod fixture_tests;
