// lib.rs -- re-exports for integration tests and external consumers.
// The binary entry point lives in main.rs; this crate facade exposes
// only the modules needed by `tests/integration.rs`.
//
// Clippy lints that fire on pub re-exports but are already handled in
// the binary crate are suppressed here.
#![allow(
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::new_without_default,
    clippy::too_long_first_doc_paragraph,
    clippy::implicit_hasher
)]

pub mod db;
pub mod models;
pub mod repository;
pub mod theme;
pub mod util;

#[cfg(test)]
pub mod test_utils;
