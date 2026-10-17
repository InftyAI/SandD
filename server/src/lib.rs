// Allow dead code and unused imports for MVP
#![allow(dead_code)]
#![allow(non_local_definitions)]

//! The SandD controller: the registry of connected daemons and the WebSocket server
//! they dial into.
//!
//! Driven from Python: `src/python.rs` holds the pyo3 bindings maturin builds into
//! `sandd._core`, behind the `python` feature so `cargo test` needs no interpreter.

pub mod registry;
pub mod server;

#[cfg(feature = "python")]
mod python;
