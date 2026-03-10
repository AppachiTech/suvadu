//! Suvadu — total recall for your terminal.
//!
//! A high-performance, database-backed shell history tool with rich TUI search,
//! AI agent tracking, session management, risk assessment, and secret redaction.
//!
//! # Architecture
//!
//! - **`cli`** — Command-line argument parsing via `clap` derive macros.
//! - **`commands/`** — Handlers for each CLI subcommand.
//! - **`repository/`** — Data access layer wrapping `SQLite` via `rusqlite`.
//! - **`db`** — Schema management, migrations, and database initialization.
//! - **`models`** — Domain types (`Entry`, `Session`, `Tag`, `Alias`, etc.).
//! - **`search/`** — Interactive TUI search with fuzzy matching.
//! - **`config`** — TOML configuration with mtime-cached loading.
//! - **`hooks`** — Shell hook script generation (zsh/bash).
//! - **`integrations`** — IDE and AI tool integrations (Claude Code, Cursor).
//! - **`risk`** — Command risk assessment for agent activity.
//! - **`redact`** — Secret detection and redaction before storage.
//! - **`util`** — Shared helpers (terminal guards, formatting, path utilities).

// This crate facade exposes only the modules needed by `tests/integration.rs`.
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
