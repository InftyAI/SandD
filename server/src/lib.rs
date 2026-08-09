// Allow dead code and unused imports for MVP
#![allow(dead_code)]
#![allow(non_local_definitions)]

//! The SandD controller: the registry of connected daemons, the WebSocket server
//! they dial into, and the token verification that admits them.
//!
//! THREE CONSUMERS, one crate:
//!   - `src/main.rs` — the `sandd-controller` binary, which is what Nebula runs
//!     (one Deployment per workload). It needs nothing but the modules below.
//!   - `src/python.rs` — the pyo3 bindings maturin builds into `sandd._core`, for
//!     driving a controller from Python. Behind the `python` feature so the binary
//!     does not link CPython; see that module's docs.
//!   - `src/ffi.rs` — a C ABI for hosts that are not CPython (Nebula's Go manager links
//!     it via cgo). Behind the `ffi` feature. Same registry, same protocol, same token
//!     verification as the other two: it re-exposes them, it does not reimplement them.
//!
//! The modules are `pub` rather than private because `main.rs` is a SEPARATE crate
//! that reaches them through this library — a private `mod` would be invisible to
//! it, and giving the binary its own module tree would compile the server twice and
//! let the two copies drift.

pub mod auth;
pub mod registry;
pub mod server;

#[cfg(feature = "python")]
mod python;

// `pub` unlike `python`: the exported symbols must reach the cdylib's symbol table for a
// C host to link them, which a private module would not do.
#[cfg(feature = "ffi")]
pub mod ffi;
