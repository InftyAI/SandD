// Allow the non-snake-case/unsafe-heavy shapes a C ABI requires.
#![allow(clippy::missing_safety_doc)]

//! A C ABI over the controller, for hosts that are not CPython.
//!
//! THIRD CONSUMER, same crate: `main.rs` is the binary, `python.rs` is the pyo3
//! extension, and this is the C surface a cgo host links against. All three drive the
//! SAME `SandboxServer` and `DaemonRegistry` — nothing here reimplements the protocol,
//! the registry or token verification, it only re-exposes them through pointers and
//! byte buffers instead of Python objects.
//!
//! Behind the `ffi` feature so a plain `cargo build` of the binary links no extra
//! symbols. Unlike the `python` feature this pulls in NO external dependency — a C ABI
//! needs only `std` — so enabling it cannot fail to link the way `extension-module`
//! deliberately does.
//!
//! ## Shape, and why it mirrors python.rs rather than improving on it
//!
//! `Session::read` (python.rs) is a BLOCKING PULL with a timeout, not a callback. That
//! choice is what makes a non-Python host cheap: the host never has to be called INTO,
//! so there are no function pointers crossing the boundary, no host runtime pinned per
//! callback, and no reentrancy to reason about. A Go caller runs `read` in one goroutine
//! and `write` from another, exactly as Python runs it on one thread and writes from
//! another. Keeping the same shape here is deliberate — a callback-style API would be
//! more "natural" C and much worse over cgo.
//!
//! ## Ownership rules the host must honour
//!
//! Every `*mut c_char` this module RETURNS is heap-allocated by Rust and must be freed
//! with `sandd_string_free`. Every `*const c_char` the host PASSES IN is borrowed for
//! the duration of the call and copied if retained. Handles (`SanddServer`,
//! `SanddSession`) are opaque and freed with their own `*_free`. Freeing a handle twice,
//! or using one after free, is undefined behaviour — the Go wrapper is responsible for
//! making that unrepresentable.
//!
//! Errors are reported as a NEGATIVE return code with a message retrievable via
//! `sandd_last_error`, which is thread-local: a message set on one thread is invisible to
//! another, so the host must fetch it on the same thread that saw the failure.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use sandd_protocol::Message;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot, Mutex};
use uuid::Uuid;

use crate::auth::TokenVerifier;
use crate::registry::DaemonRegistry;
use crate::server::SandboxServer;

// ── error reporting ──────────────────────────────────────────────────────────

thread_local! {
    /// Last error on THIS thread. Thread-local rather than a global so two host
    /// threads failing concurrently cannot overwrite each other's message — with a
    /// shared slot the reported cause would depend on scheduling.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: impl Into<String>) {
    // A NUL inside the message would truncate it at the boundary; replace rather than
    // drop the error, since a mangled message still beats a silent failure.
    let cleaned = msg.into().replace('\0', "?");
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = CString::new(cleaned).ok();
    });
}

/// The last error on the CALLING thread, or NULL if there is none.
///
/// The returned pointer is owned by Rust and valid until the next failing call on this
/// thread. The host must copy it, not retain it, and must NOT free it.
#[no_mangle]
pub extern "C" fn sandd_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| match &*slot.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Frees a string this library returned. NULL is accepted and ignored.
#[no_mangle]
pub unsafe extern "C" fn sandd_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Return codes. Negative is failure, and every failure sets `sandd_last_error`.
pub const SANDD_OK: c_int = 0;
pub const SANDD_ERR: c_int = -1;
/// The daemon id is not in the registry — it never connected, or was reaped/disconnected.
/// Distinct from SANDD_ERR because the host maps it to a 404-shaped outcome rather than
/// an internal error.
pub const SANDD_ERR_NO_DAEMON: c_int = -2;
/// `sandd_session_read` timed out with no data. NOT an error: an idle terminal produces
/// nothing for long stretches, so the host loops on this rather than tearing down.
pub const SANDD_TIMEOUT: c_int = -3;
/// The session's output channel closed — the daemon went away or the session ended.
pub const SANDD_CLOSED: c_int = -4;

/// Smallest read buffer a host should pass to `sandd_session_read`. The channel yields
/// whole chunks and a short buffer silently discards the tail of one, so this is a
/// correctness floor, not a tuning knob.
pub const SANDD_READ_BUF_MIN: usize = 64 * 1024;

/// Borrows a C string as `&str`, or sets an error and returns None.
unsafe fn as_str<'a>(p: *const c_char, what: &str) -> Option<&'a str> {
    if p.is_null() {
        set_error(format!("{what} is NULL"));
        return None;
    }
    match CStr::from_ptr(p).to_str() {
        Ok(s) => Some(s),
        Err(_) => {
            set_error(format!("{what} is not valid UTF-8"));
            None
        }
    }
}

/// Moves a Rust string out to the host as an owned `*mut c_char`.
fn out_string(s: String) -> *mut c_char {
    match CString::new(s.replace('\0', "?")) {
        Ok(c) => c.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

// ── server handle ────────────────────────────────────────────────────────────

/// Opaque server handle. Owns the tokio runtime the WebSocket server runs on, so the
/// runtime outlives every session derived from it.
pub struct SanddServer {
    runtime: Runtime,
    registry: Arc<DaemonRegistry>,
}

/// Starts a controller listening for daemon dial-ins on `bind_addr` (e.g.
/// "0.0.0.0:8765").
///
/// Passing NULL for `public_key_pem`/`controller_id` starts with authentication
/// DISABLED, which mirrors `SandboxServer::new` and must be used only in tests: an
/// unauthenticated controller admits any caller that speaks the protocol. With a key,
/// `controller_id` is the ONLY `aud` admitted, and `issuer`/`kid` must match the minter.
/// `kid` may be empty to accept any key id.
///
/// Returns NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn sandd_server_start(
    bind_addr: *const c_char,
    public_key_pem: *const c_char,
    controller_id: *const c_char,
    issuer: *const c_char,
    kid: *const c_char,
) -> *mut SanddServer {
    let Some(bind) = as_str(bind_addr, "bind_addr") else {
        return ptr::null_mut();
    };

    // Auth is on iff BOTH a key and an audience are supplied. Half-configured is
    // rejected rather than silently downgraded: a caller that meant to enable auth and
    // passed only one of the two would otherwise get a wide-open controller that looks
    // healthy.
    let auth = match (
        as_str(public_key_pem, "public_key_pem"),
        as_str(controller_id, "controller_id"),
    ) {
        (Some(pem), Some(id)) => Some((pem, id)),
        (None, None) => {
            // as_str already set an error for each NULL; clear it, both-NULL is legal.
            LAST_ERROR.with(|s| *s.borrow_mut() = None);
            None
        }
        _ => {
            set_error(
                "public_key_pem and controller_id must be supplied together \
                 (both NULL disables authentication)",
            );
            return ptr::null_mut();
        }
    };

    let runtime = match Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            set_error(format!("failed to create tokio runtime: {e}"));
            return ptr::null_mut();
        }
    };

    let server = match auth {
        Some((pem, id)) => {
            let iss = as_str(issuer, "issuer").unwrap_or("nebula");
            let k = as_str(kid, "kid").unwrap_or("");
            match TokenVerifier::new(pem, id, iss, k) {
                Ok(v) => SandboxServer::with_auth(bind.to_string(), v),
                Err(e) => {
                    set_error(format!("failed to build token verifier: {e}"));
                    return ptr::null_mut();
                }
            }
        }
        None => SandboxServer::new(bind.to_string()),
    };

    let registry = server.registry();
    // The server task is spawned and deliberately not awaited: this call returns a
    // handle, it does not block the host thread. A bind failure therefore surfaces in
    // the server's own log rather than here — the host should treat "no daemon ever
    // connects" as the symptom, the same way the binary does.
    runtime.spawn(async move {
        if let Err(e) = server.start().await {
            eprintln!("sandd server error: {e}");
        }
    });

    Box::into_raw(Box::new(SanddServer { runtime, registry }))
}

/// Stops the controller and frees the handle, dropping every daemon socket it holds.
/// NULL is accepted and ignored. Sessions derived from this server must be freed FIRST:
/// they hold a runtime handle that becomes dangling once the runtime is dropped.
#[no_mangle]
pub unsafe extern "C" fn sandd_server_free(srv: *mut SanddServer) {
    if !srv.is_null() {
        drop(Box::from_raw(srv));
    }
}

/// Number of daemons currently registered. Returns a negative code on a NULL handle.
#[no_mangle]
pub unsafe extern "C" fn sandd_server_daemon_count(srv: *const SanddServer) -> c_int {
    if srv.is_null() {
        set_error("server handle is NULL");
        return SANDD_ERR;
    }
    (*srv).registry.count() as c_int
}

/// Registry statistics as a JSON object, owned by the caller (free with
/// `sandd_string_free`). Returns NULL on failure.
///
/// Built with `serde_json::json!` rather than by deriving `Serialize` on
/// `RegistryStats`: that type is shared with the binary and the pyo3 layer, and a derive
/// there would make this module's wire format a property of the registry rather than of
/// this boundary. Field names match the `/stats` HTTP route so a host can consume either
/// interchangeably.
#[no_mangle]
pub unsafe extern "C" fn sandd_server_stats_json(srv: *const SanddServer) -> *mut c_char {
    if srv.is_null() {
        set_error("server handle is NULL");
        return ptr::null_mut();
    }
    let stats = (*srv).registry.get_stats();
    let daemons: serde_json::Map<String, serde_json::Value> = stats
        .daemons
        .into_iter()
        .map(|(id, d)| {
            (
                id,
                serde_json::json!({
                    "hostname": d.hostname,
                    "platform": d.platform,
                    "arch": d.arch,
                    "version": d.version,
                    "labels": d.labels,
                    "is_busy": d.is_busy,
                    "connected_secs": d.connected_secs,
                    "seconds_since_heartbeat": d.seconds_since_heartbeat,
                }),
            )
        })
        .collect();

    out_string(
        serde_json::json!({
            "total_daemons": stats.total_daemons,
            "by_platform": stats.by_platform,
            "oldest_connection_secs": stats.oldest_connection_secs,
            "daemons": daemons,
        })
        .to_string(),
    )
}

// ── one-shot exec ────────────────────────────────────────────────────────────

/// Runs `command` on `daemon_id` and blocks until it completes or `timeout_secs`
/// elapses.
///
/// One-shot only: the result is complete stdout/stderr after the fact, so this backs
/// `kubectl exec -- ls` but NOT an interactive `-it` shell. Use the session API for that.
///
/// On success returns SANDD_OK and writes a JSON object
/// `{stdout, stderr, exit_code, duration_ms}` to `*out_json`, which the caller frees with
/// `sandd_string_free`. `*out_json` is untouched on failure.
///
/// A timeout is reported as SANDD_ERR, not SANDD_TIMEOUT: unlike an idle session read,
/// the command may have RUN — the caller cannot know — so this must not look retryable.
#[no_mangle]
pub unsafe extern "C" fn sandd_exec(
    srv: *const SanddServer,
    daemon_id: *const c_char,
    command: *const c_char,
    timeout_secs: u64,
    out_json: *mut *mut c_char,
) -> c_int {
    if srv.is_null() || out_json.is_null() {
        set_error("server handle or out_json is NULL");
        return SANDD_ERR;
    }
    let Some(id) = as_str(daemon_id, "daemon_id") else {
        return SANDD_ERR;
    };
    let Some(cmd) = as_str(command, "command") else {
        return SANDD_ERR;
    };

    let srv = &*srv;
    let Some(conn) = srv.registry.get(id) else {
        set_error(format!("daemon {id} not found"));
        return SANDD_ERR_NO_DAEMON;
    };

    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    // Registered BEFORE the send, so a daemon that answers immediately cannot have its
    // response arrive with no channel waiting for it.
    conn.register_request(request_id.clone(), tx);

    if let Err(e) = conn.send_message(Message::ExecuteCommand {
        request_id,
        command: cmd.to_string(),
        timeout_secs,
        env: Default::default(),
        cwd: None,
    }) {
        set_error(format!("failed to send command: {e}"));
        return SANDD_ERR;
    }

    srv.runtime.block_on(async {
        match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(Message::CommandOutput {
                stdout,
                stderr,
                exit_code,
                duration_ms,
                ..
            })) => {
                *out_json = out_string(
                    serde_json::json!({
                        "stdout": stdout,
                        "stderr": stderr,
                        "exit_code": exit_code,
                        "duration_ms": duration_ms,
                    })
                    .to_string(),
                );
                SANDD_OK
            }
            Ok(Ok(Message::CommandError { error, .. })) => {
                set_error(format!("command error: {error}"));
                SANDD_ERR
            }
            Ok(Ok(_)) => {
                set_error("unexpected response type for exec");
                SANDD_ERR
            }
            Ok(Err(_)) => {
                set_error("command channel closed (daemon disconnected)");
                SANDD_ERR
            }
            Err(_) => {
                set_error("command execution timed out");
                SANDD_ERR
            }
        }
    })
}

// ── interactive sessions ─────────────────────────────────────────────────────

/// Opaque handle to one interactive PTY session.
///
/// Holds a runtime HANDLE, not the runtime: the session borrows the server's runtime, so
/// the server must outlive every session opened against it.
pub struct SanddSession {
    session_id: String,
    daemon_id: String,
    registry: Arc<DaemonRegistry>,
    runtime: tokio::runtime::Handle,
    output_rx: Arc<Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

/// Opens an interactive session on `daemon_id` with the given terminal geometry.
/// `term` may be NULL for "xterm-256color". Returns NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn sandd_session_open(
    srv: *const SanddServer,
    daemon_id: *const c_char,
    rows: u16,
    cols: u16,
    term: *const c_char,
) -> *mut SanddSession {
    if srv.is_null() {
        set_error("server handle is NULL");
        return ptr::null_mut();
    }
    let Some(id) = as_str(daemon_id, "daemon_id") else {
        return ptr::null_mut();
    };
    let term = if term.is_null() {
        "xterm-256color"
    } else {
        match as_str(term, "term") {
            Some(t) => t,
            None => return ptr::null_mut(),
        }
    };

    let srv = &*srv;
    let Some(conn) = srv.registry.get(id) else {
        set_error(format!("daemon {id} not found"));
        return ptr::null_mut();
    };

    let session_id = Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::unbounded_channel();
    // Registered before the send, for the same reason as exec: output can arrive
    // before this function returns.
    conn.register_session(session_id.clone(), tx);

    if let Err(e) = conn.send_message(Message::NewSession {
        session_id: session_id.clone(),
        rows,
        cols,
        term: term.to_string(),
    }) {
        conn.close_session(&session_id);
        set_error(format!("failed to start session: {e}"));
        return ptr::null_mut();
    }

    Box::into_raw(Box::new(SanddSession {
        session_id,
        daemon_id: id.to_string(),
        registry: srv.registry.clone(),
        runtime: srv.runtime.handle().clone(),
        output_rx: Arc::new(Mutex::new(rx)),
    }))
}

/// Writes `len` bytes of stdin to the session. Returns SANDD_OK or a negative code.
#[no_mangle]
pub unsafe extern "C" fn sandd_session_write(
    sess: *const SanddSession,
    data: *const u8,
    len: usize,
) -> c_int {
    if sess.is_null() || (data.is_null() && len > 0) {
        set_error("session handle or data is NULL");
        return SANDD_ERR;
    }
    let sess = &*sess;
    let Some(conn) = sess.registry.get(&sess.daemon_id) else {
        set_error("daemon disconnected");
        return SANDD_ERR_NO_DAEMON;
    };
    let buf = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    match conn.send_message(Message::SessionInput {
        session_id: sess.session_id.clone(),
        data: buf,
    }) {
        Ok(()) => SANDD_OK,
        Err(e) => {
            set_error(format!("failed to write to session: {e}"));
            SANDD_ERR
        }
    }
}

/// Reads up to `cap` bytes of session output into `out`, blocking at most
/// `timeout_ms`.
///
/// Returns the byte count (>= 0) on success, SANDD_TIMEOUT if nothing arrived, or
/// SANDD_CLOSED once the session has ended. SANDD_TIMEOUT is EXPECTED and not an error —
/// an idle terminal produces nothing for long stretches — so a host loop should keep
/// polling on it and tear down only on SANDD_CLOSED.
///
/// This is a blocking PULL by design: the host is never called into, so no function
/// pointer crosses the boundary and no host thread is pinned. Reading from one host
/// thread while writing from another is safe.
///
/// Output longer than `cap` is truncated and the remainder DISCARDED — the channel
/// yields whole chunks, so a short buffer loses the tail of one rather than resuming it on
/// the next call. Hosts should pass at least `SANDD_READ_BUF_MIN`.
#[no_mangle]
pub unsafe extern "C" fn sandd_session_read(
    sess: *const SanddSession,
    out: *mut u8,
    cap: usize,
    timeout_ms: u64,
) -> c_int {
    if sess.is_null() || out.is_null() {
        set_error("session handle or out buffer is NULL");
        return SANDD_ERR;
    }
    let sess = &*sess;
    sess.runtime.block_on(async {
        let mut rx = sess.output_rx.lock().await;
        match tokio::time::timeout(Duration::from_millis(timeout_ms), rx.recv()).await {
            Ok(Some(data)) => {
                let n = data.len().min(cap);
                ptr::copy_nonoverlapping(data.as_ptr(), out, n);
                n as c_int
            }
            Ok(None) => SANDD_CLOSED,
            Err(_) => SANDD_TIMEOUT,
        }
    })
}

/// Tells the daemon the terminal was resized.
#[no_mangle]
pub unsafe extern "C" fn sandd_session_resize(
    sess: *const SanddSession,
    rows: u16,
    cols: u16,
) -> c_int {
    if sess.is_null() {
        set_error("session handle is NULL");
        return SANDD_ERR;
    }
    let sess = &*sess;
    let Some(conn) = sess.registry.get(&sess.daemon_id) else {
        set_error("daemon disconnected");
        return SANDD_ERR_NO_DAEMON;
    };
    match conn.send_message(Message::SessionResize {
        session_id: sess.session_id.clone(),
        rows,
        cols,
    }) {
        Ok(()) => SANDD_OK,
        Err(e) => {
            set_error(format!("failed to resize session: {e}"));
            SANDD_ERR
        }
    }
}

/// Closes the session and frees the handle. NULL is accepted and ignored.
///
/// Best-effort on the wire: a daemon that has already disconnected cannot be told, and
/// that is not an error — the local state is released either way, so this cannot leak on
/// the path that matters.
#[no_mangle]
pub unsafe extern "C" fn sandd_session_free(sess: *mut SanddSession) {
    if sess.is_null() {
        return;
    }
    let sess = Box::from_raw(sess);
    if let Some(conn) = sess.registry.get(&sess.daemon_id) {
        let _ = conn.send_message(Message::SessionClose {
            session_id: sess.session_id.clone(),
        });
        conn.close_session(&sess.session_id);
    }
}

/// The session id, owned by the caller (free with `sandd_string_free`). Useful for
/// correlating host-side logs with the controller's.
#[no_mangle]
pub unsafe extern "C" fn sandd_session_id(sess: *const SanddSession) -> *mut c_char {
    if sess.is_null() {
        set_error("session handle is NULL");
        return ptr::null_mut();
    }
    out_string((*sess).session_id.clone())
}
