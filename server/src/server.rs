use crate::auth::{bearer_token, AuthError, TokenVerifier};
use crate::registry::{DaemonConnection, DaemonRegistry};
use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use sandd_protocol::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// What `/ws` needs on every upgrade: the registry to place the daemon in, and the
/// verifier to admit it. Grouped because axum's `State` is a single extractor.
///
/// `verifier` is None when auth is DISABLED (see SandboxServer::new): the switch is the
/// presence of the verifier, not a separate boolean, so there is no way to be "auth on
/// but no key" or "key present but not enforced".
#[derive(Clone)]
struct AppState {
    registry: Arc<DaemonRegistry>,
    verifier: Option<Arc<TokenVerifier>>,
}

// Lets handlers that only need the registry keep extracting
// `State<Arc<DaemonRegistry>>` (as /stats does) instead of reaching through AppState.
impl axum::extract::FromRef<AppState> for Arc<DaemonRegistry> {
    fn from_ref(state: &AppState) -> Self {
        state.registry.clone()
    }
}

pub struct SandboxServer {
    registry: Arc<DaemonRegistry>,
    bind_addr: String,
    verifier: Option<Arc<TokenVerifier>>,
}

impl SandboxServer {
    /// Builds a server with authentication DISABLED — every daemon that speaks
    /// `sandd.v1` is admitted. This is the standalone/local-dev shape (a laptop, the
    /// existing e2e stacks) where the controller is not reachable by untrusted callers.
    ///
    /// Under Nebula, use `with_auth`: the controller runs in the workload's namespace and
    /// its Service is reachable by anything that can route to it.
    pub fn new(bind_addr: String) -> Self {
        Self {
            registry: Arc::new(DaemonRegistry::new()),
            bind_addr,
            verifier: None,
        }
    }

    /// Builds a server that REQUIRES a valid daemon token on every `/ws` upgrade.
    ///
    /// Taking the verifier by value (rather than a flag plus optional key) is what makes
    /// "auth enabled but unusable" unrepresentable: the caller cannot enable auth without
    /// having already constructed a verifier from a real key.
    pub fn with_auth(bind_addr: String, verifier: TokenVerifier) -> Self {
        Self {
            registry: Arc::new(DaemonRegistry::new()),
            bind_addr,
            verifier: Some(Arc::new(verifier)),
        }
    }

    pub fn registry(&self) -> Arc<DaemonRegistry> {
        self.registry.clone()
    }

    pub async fn start(self) -> Result<()> {
        let registry = self.registry.clone();

        // Start heartbeat monitor
        let monitor_registry = registry.clone();
        tokio::spawn(async move {
            heartbeat_monitor(monitor_registry).await;
        });

        // State that /ws needs; /stats and /health only read the registry and get it
        // from the same struct.
        let state = AppState {
            registry,
            verifier: self.verifier,
        };

        // Say which mode we are in at startup, unmissably. An operator who believes auth
        // is on when it is not has no other signal — an unauthenticated controller looks
        // identical to a healthy one until someone connects to it.
        if state.verifier.is_some() {
            info!("daemon authentication ENABLED (every /ws upgrade requires a valid token)");
        } else {
            warn!(
                "daemon authentication DISABLED: any client speaking sandd.v1 will be \
                 admitted. Do not run this way where the controller is reachable by \
                 untrusted callers."
            );
        }

        // Build web server
        let app = Router::new()
            .route("/ws", get(websocket_handler))
            .route("/stats", get(stats_handler))
            .route("/health", get(health_handler))
            .with_state(state);

        info!("Starting sandbox server on {}", self.bind_addr);

        let listener = tokio::net::TcpListener::bind(&self.bind_addr)
            .await
            .context("Failed to bind server")?;

        axum::serve(listener, app).await.context("Server error")?;

        Ok(())
    }
}

/// Authenticate an upgrade request, returning the daemon id the token authorizes.
///
/// `Ok(None)` means auth is disabled and the connection is unrestricted — the caller
/// then imposes no `sub` binding, so `Register` may claim any id (the pre-auth
/// behaviour, preserved for standalone use).
///
/// Split out of the handler so it is unit-testable: `WebSocketUpgrade` cannot be
/// constructed without a real request, but this takes only the headers.
fn authenticate(
    verifier: Option<&Arc<TokenVerifier>>,
    headers: &HeaderMap,
) -> Result<Option<String>, AuthError> {
    let Some(verifier) = verifier else {
        return Ok(None);
    };
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let token = bearer_token(header)?;
    let claims = verifier.verify(token)?;
    Ok(Some(claims.sub))
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Check for WebSocket subprotocol
    const SUPPORTED_PROTOCOL: &str = "sandd.v1";

    let has_protocol = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|protocols| {
            // Client can send multiple protocols: "sandd.v1, sandd.v2"
            protocols.split(',').any(|p| p.trim() == SUPPORTED_PROTOCOL)
        })
        .unwrap_or(false);

    if !has_protocol {
        error!("Client did not specify required protocol: sandd.v1");
        return (
            StatusCode::BAD_REQUEST,
            "Missing required Sec-WebSocket-Protocol: sandd.v1",
        )
            .into_response();
    }

    // Authenticate BEFORE upgrading. An unauthenticated caller never gets a socket, so it
    // cannot hold server resources or reach the message loop at all — and the rejection
    // is a plain HTTP 401 it can actually understand, rather than a close frame after a
    // successful handshake.
    let authorized_daemon = match authenticate(state.verifier.as_ref(), &headers) {
        Ok(id) => id,
        Err(e) => {
            // Detail to the LOG (the operator needs it); the body stays generic so a
            // prober cannot learn which part of its forgery to fix.
            warn!("rejecting /ws upgrade: {}", e.detail());
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    };
    if let Some(ref id) = authorized_daemon {
        info!("authenticated daemon {} at upgrade", id);
    }

    info!("Client negotiated protocol: {}", SUPPORTED_PROTOCOL);
    let registry = state.registry.clone();
    ws.protocols([SUPPORTED_PROTOCOL])
        .on_upgrade(move |socket| handle_websocket(socket, registry, authorized_daemon))
        .into_response()
}

/// `authorized_daemon` is the `sub` from the verified token, or None when auth is
/// disabled. When present it IS the id this connection registers as, whatever the Register
/// message claims — see `registration_id`.
async fn handle_websocket(
    ws: WebSocket,
    registry: Arc<DaemonRegistry>,
    authorized_daemon: Option<String>,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Create channel for outgoing requests (Python → Daemon)
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut daemon_id: Option<String> = None;

    loop {
        tokio::select! {
            // Receive from daemon
            Some(ws_msg) = ws_rx.next() => {
                let ws_msg = match ws_msg {
                    Ok(msg) => msg,
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                };

                let text = match ws_msg {
                    axum::extract::ws::Message::Text(text) => text,
                    axum::extract::ws::Message::Close(_) => {
                        debug!("WebSocket closed by client");
                        break;
                    }
                    _ => continue,
                };

                let message: Message = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Failed to parse message: {}", e);
                        continue;
                    }
                };

                handle_daemon_message(message, &mut daemon_id, &registry, &mut ws_tx, &request_tx, authorized_daemon.as_deref()).await;
            }

            // Receive requests from Python (via channel)
            Some(request) = request_rx.recv() => {
                let json = match serde_json::to_string(&request) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize request: {}", e);
                        continue;
                    }
                };

                if let Err(e) = ws_tx.send(axum::extract::ws::Message::Text(json)).await {
                    error!("Failed to send request to daemon: {}", e);
                    break;
                }
            }

            else => break,
        }
    }

    // Clean up on disconnect
    if let Some(id) = daemon_id {
        registry.remove(&id);
    }
}

/// Record a heartbeat. Returns whether the daemon was registered; `false` means it must
/// send `Register` again before it is reachable (it was reaped, or never registered on
/// this connection) and is the only way a heartbeat can fail.
///
/// A daemon can be reaped while its socket is still healthy: mesh churn (DERP peer
/// reconfig, netmap propagation) stalls heartbeats past heartbeat_monitor's threshold
/// WITHOUT breaking TCP. `Register` is sent once per connection, and the daemon cannot
/// detect the eviction — its heartbeat writes keep succeeding, so it never trips its
/// own dead-connection signal. Left alone it stays invisible (no exec, no logs) until
/// the socket truly breaks, which its own heartbeats keep preventing.
///
/// The daemon owns its metadata, so recovery is to fail the heartbeat and let it send
/// `Register` again, rather than reconstructing the entry server-side from a copy the
/// server would have to hold for every connection.
///
/// Daemons predating `HeartbeatAck` effectively ignore it (it fails to deserialize) and
/// recover only when the connection eventually drops. They cannot be upgraded in place —
/// the binary is fetched from /releases/latest/ at instance boot — so this takes full effect
/// on instances provisioned after the daemon ships.
///
/// Split out of handle_daemon_message so it is unit-testable: the parent needs a
/// SplitSink<WebSocket, _> that cannot be constructed without a real socket.
fn handle_heartbeat(id: &str, registry: &Arc<DaemonRegistry>) -> bool {
    match registry.get(id) {
        Some(conn) => {
            conn.update_heartbeat();
            debug!("Heartbeat from daemon {}", id);
            true
        }
        // warn, not debug: reaching here means the reaper fired on a live socket.
        // Recovering silently would hide the churn that caused it.
        None => {
            warn!(
                "Heartbeat from unregistered daemon {} (reaped while connected); \
                 asking it to register again",
                id
            );
            false
        }
    }
}

/// The id a connection registers under: the token's `sub` when authenticated, otherwise
/// the id the daemon claimed. `None` means the registration must be refused.
///
/// The TOKEN is authoritative, and a claimed id is IGNORED when one is present — not
/// compared and rejected on mismatch, which is what this used to do. Two reasons:
///
///   - There was nothing to disagree about. `sub` already IS the identity, so a second
///     copy in the Register payload can only be right or wrong, and being wrong failed
///     silently: a daemon whose id was not configured invented a UUID, was refused, and
///     never entered the registry — indistinguishable downstream from "no daemon
///     connected yet". Deriving the id deletes that failure mode rather than reporting it.
///   - It is not a weakening. What prevents impersonation is that registering as another
///     daemon needs a token whose `sub` IS that daemon, and minting one needs the
///     manager's private key. Taking the id from the verified token enforces that
///     directly; matching a self-reported copy only enforced it indirectly.
///
/// `authorized == None` means auth is disabled and the claimed id is used as-is — the
/// standalone/local-dev path, where there is no token to derive an identity from.
///
/// Split out for the same reason as handle_heartbeat: the parent needs a
/// SplitSink<WebSocket, _> that cannot be built without a real socket.
fn registration_id(authorized: Option<&str>, claimed: &str) -> Option<String> {
    match authorized {
        Some(sub) => Some(sub.to_string()),
        // An empty id is the one thing an unauthenticated caller cannot register under:
        // the registry entry would be keyed by something nothing can address.
        None if claimed.is_empty() => None,
        None => Some(claimed.to_string()),
    }
}

async fn handle_daemon_message(
    message: Message,
    daemon_id: &mut Option<String>,
    registry: &Arc<DaemonRegistry>,
    ws_tx: &mut futures_util::stream::SplitSink<WebSocket, axum::extract::ws::Message>,
    request_tx: &mpsc::UnboundedSender<Message>,
    authorized_daemon: Option<&str>,
) {
    use futures_util::SinkExt;

    match message {
        Message::Register {
            daemon_id: claimed,
            metadata,
        } => {
            // The token's `sub` wins over anything the payload claims; see registration_id.
            let Some(id) = registration_id(authorized_daemon, &claimed) else {
                warn!("refusing registration: no daemon id, and no token to derive one from");
                let nack = Message::RegisterAck {
                    success: false,
                    message: "no daemon id".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&nack) {
                    let _ = ws_tx.send(axum::extract::ws::Message::Text(json)).await;
                }
                // daemon_id stays None, so the disconnect path removes nothing and this
                // connection never appears in the registry.
                return;
            };

            // Logged when they differ, at info: it is not a failure (the token decides),
            // but it is the one clue that a daemon is misconfigured, and silence here is
            // what made the previous behaviour hard to diagnose.
            if !claimed.is_empty() && claimed != id {
                info!(
                    "daemon claimed id {:?} but its token authorizes {:?}; using the token's",
                    claimed, id
                );
            }
            info!("Daemon {} attempting to register", id);

            *daemon_id = Some(id.clone());

            info!(
                "Daemon {} registered: hostname={} platform={} arch={}",
                id, metadata.hostname, metadata.platform, metadata.arch
            );

            // Create and register connection with channel
            let new_conn = DaemonConnection::new(id.clone(), metadata, request_tx.clone());
            registry.register(new_conn);

            // Send ack
            let ack = Message::RegisterAck {
                success: true,
                message: "Successfully registered".to_string(),
            };
            let ack_json = serde_json::to_string(&ack).unwrap();
            if let Err(e) = ws_tx.send(axum::extract::ws::Message::Text(ack_json)).await {
                error!("Failed to send registration ack: {}", e);
            }
        }

        Message::Heartbeat => {
            if let Some(ref id) = daemon_id {
                // Ack either way. On failure it tells the daemon to register again (only
                // the daemon holds its metadata, so it re-sends Register rather than the
                // server rebuilding the entry from a copy). On success it proves this
                // controller is still PROCESSING messages — the daemon's own liveness
                // check only sees whether its write reached a socket buffer, so a hung
                // controller is indistinguishable from a healthy one without this.
                let registered = handle_heartbeat(id, registry);
                let ack = Message::HeartbeatAck {
                    success: registered,
                    reason: if registered {
                        String::new()
                    } else {
                        "daemon is not registered".to_string()
                    },
                };
                match serde_json::to_string(&ack) {
                    Ok(json) => {
                        if let Err(e) = ws_tx.send(axum::extract::ws::Message::Text(json)).await {
                            error!("Failed to ack heartbeat from daemon {}: {}", id, e);
                        }
                    }
                    Err(e) => error!("Failed to serialize heartbeat ack: {}", e),
                }
            }
        }

        // All response messages for request/response pattern
        response @ (Message::CommandOutput { .. }
        | Message::CommandError { .. }
        | Message::SnapshotCreated { .. }
        | Message::SnapshotRestored { .. }
        | Message::SnapshotList { .. }
        | Message::SnapshotDetails { .. }
        | Message::SnapshotDeleted { .. }
        | Message::SnapshotError { .. }) => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    // Helper to extract request_id without moving
                    let request_id = match &response {
                        Message::CommandOutput { request_id, .. }
                        | Message::CommandError { request_id, .. }
                        | Message::SnapshotCreated { request_id, .. }
                        | Message::SnapshotRestored { request_id, .. }
                        | Message::SnapshotList { request_id, .. }
                        | Message::SnapshotDetails { request_id, .. }
                        | Message::SnapshotDeleted { request_id, .. }
                        | Message::SnapshotError { request_id, .. } => request_id.clone(),
                        _ => unreachable!(),
                    };
                    debug!("Request {} completed on daemon {}", request_id, id);
                    conn.complete_request(&request_id, response);
                }
            }
        }

        Message::SessionOutput { session_id, data } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    conn.send_session_output(&session_id, data);
                }
            }
        }

        Message::SessionExit {
            session_id,
            exit_code,
        } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    debug!(
                        "Session {} exited with code {} on daemon {}",
                        session_id, exit_code, id
                    );
                    conn.close_session(&session_id);
                }
            }
        }

        Message::FileDownloadChunk {
            request_id,
            data,
            is_last,
            ..
        } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    conn.add_file_chunk(&request_id, data);
                    if is_last {
                        debug!("File transfer {} completed on daemon {}", request_id, id);
                    }
                }
            }
        }

        _ => {
            debug!("Received unhandled message type");
        }
    }
}

async fn health_handler() -> impl IntoResponse {
    "OK"
}

/// The `/stats` response body. Split out from the handler so a test can assert the
/// WIRE SHAPE — the field names an operator's `curl | jq` depends on — without
/// standing up a server or reading an HTTP body.
fn stats_body(stats: crate::registry::RegistryStats) -> serde_json::Value {
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

    serde_json::json!({
        "total_daemons": stats.total_daemons,
        "by_platform": stats.by_platform,
        "oldest_connection_secs": stats.oldest_connection_secs,
        "daemons": daemons,
    })
}

/// GET /stats — the only externally reachable view of the registry.
///
/// `total_daemons` stays a COUNT (it is also `PyStats.total_daemons`); the
/// per-daemon detail is a sibling `daemons` map keyed by daemon id, so adding it
/// breaks no existing consumer. With both, one curl tells you not just that a
/// daemon is missing but which ones are present and how stale each is — the
/// difference between "the daemon never connected" and "it connected and went
/// quiet", which is otherwise only visible in controller logs.
async fn stats_handler(State(registry): State<Arc<DaemonRegistry>>) -> impl IntoResponse {
    axum::Json(stats_body(registry.get_stats()))
}

async fn heartbeat_monitor(registry: Arc<DaemonRegistry>) {
    // Tick every 5s so an ungraceful death (instance hard-killed, network yanked —
    // no Close frame, so the immediate remove() on disconnect never fires) is noticed
    // within ~5s of crossing the threshold, not up to a full tick later.
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;

        // 30s threshold against a 5s daemon heartbeat interval = ~6 missed beats before
        // reaping. That margin is deliberate: mesh churn (DERP peer reconfig, netmap
        // propagation) can stall heartbeats for tens of seconds WITHOUT the daemon being
        // dead. Reaping a daemon whose socket is still open no longer orphans it: its
        // next heartbeat re-registers it (see handle_heartbeat), so a false reap costs
        // one heartbeat interval of invisibility rather than lasting until the socket
        // breaks. Detection is ~30-35s vs the old ~90-120s; clean disconnects are still
        // removed instantly on Close (see the remove() on the disconnect path above).
        let removed = registry.cleanup_stale(30);
        if removed > 0 {
            warn!("Cleaned up {} stale daemon connections", removed);
        }

        info!("Active daemons: {} ", registry.count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandd_protocol::DaemonMetadata;
    use std::collections::HashMap;

    fn test_metadata() -> DaemonMetadata {
        let mut labels = HashMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        DaemonMetadata {
            hostname: "gpu-box".to_string(),
            platform: "linux".to_string(),
            arch: "x86_64".to_string(),
            version: "0.1.0".to_string(),
            labels,
        }
    }

    fn registered(id: &str) -> (Arc<DaemonRegistry>, mpsc::UnboundedSender<Message>) {
        let registry = Arc::new(DaemonRegistry::new());
        let (tx, _rx) = mpsc::unbounded_channel();
        registry.register(DaemonConnection::new(
            id.to_string(),
            test_metadata(),
            tx.clone(),
        ));
        (registry, tx)
    }

    // A daemon reaped while its socket stayed open must be ASKED to register again.
    // Before the fix the beat was dropped and the daemon stayed invisible forever:
    // Register is sent once per connection and the daemon cannot detect the eviction,
    // so nothing ever prompted it.
    #[test]
    fn heartbeat_from_reaped_daemon_requires_register() {
        let (registry, _tx) = registered("daemon-1");

        // Reaper evicts it (heartbeats stalled past the threshold), socket still open.
        registry.remove("daemon-1");
        assert_eq!(registry.count(), 0);

        assert!(!handle_heartbeat("daemon-1", &registry));
        // The server does NOT fabricate an entry — the daemon owns its metadata and
        // re-sends it, so the restored entry is faithful rather than a stale copy.
        assert_eq!(registry.count(), 0);
    }

    // A daemon that never registered on this connection is treated the same: ask it to
    // register. No special case needed.
    #[test]
    fn heartbeat_from_unknown_daemon_requires_register() {
        let registry = Arc::new(DaemonRegistry::new());

        assert!(!handle_heartbeat("daemon-1", &registry));
    }

    // The normal path: a registered daemon's heartbeat refreshes its timestamp so the
    // reaper leaves it alone.
    #[test]
    fn heartbeat_refreshes_existing_daemon() {
        let (registry, _tx) = registered("daemon-1");

        assert!(handle_heartbeat("daemon-1", &registry));
        assert_eq!(registry.count(), 1);
        assert_eq!(
            registry.get("daemon-1").unwrap().seconds_since_heartbeat(),
            0
        );
    }

    // The daemon's re-registration must land on the LIVE connection. Re-registering is
    // an ordinary Register, so it replaces the entry — which is correct here because it
    // arrives on the socket the daemon is actually using.
    #[test]
    fn reregister_routes_to_the_registering_connection() {
        let (registry, mut stale_rx) = {
            let registry = Arc::new(DaemonRegistry::new());
            let (tx, rx) = mpsc::unbounded_channel();
            registry.register(DaemonConnection::new(
                "daemon-1".to_string(),
                test_metadata(),
                tx,
            ));
            (registry, rx)
        };

        // The daemon re-registers, carrying its own connection's channel.
        let (new_tx, mut new_rx) = mpsc::unbounded_channel();
        registry.register(DaemonConnection::new(
            "daemon-1".to_string(),
            test_metadata(),
            new_tx,
        ));

        assert_eq!(registry.count(), 1);
        registry
            .get("daemon-1")
            .unwrap()
            .send_message(Message::Heartbeat)
            .unwrap();
        assert!(
            new_rx.try_recv().is_ok(),
            "must route to the re-registered connection"
        );
        assert!(
            stale_rx.try_recv().is_err(),
            "must not route to the old connection"
        );
    }

    // A LIVE daemon evicted by a DYING one must recover. This is the stale-remove race:
    // handle_websocket's cleanup is `registry.remove(&id)`, keyed on the id with no
    // check of WHICH connection is stored there, so when two connections for one daemon
    // overlap — a half-open socket that has not errored yet, plus the reconnect the
    // daemon made over a working path — the old task's cleanup deletes the NEW task's
    // entry. Reproduced here in the order the tasks actually interleave.
    //
    // The race window is still open (a ptr-identity-aware remove would close it); what
    // this pins down is that it is no longer PERMANENT. Before HeartbeatAck the live
    // daemon stayed invisible for the life of its connection — Register is sent once, it
    // cannot observe the eviction (its writes still succeed), and its own heartbeats keep
    // the socket from breaking. Now its next heartbeat is rejected and it re-registers,
    // so the damage is bounded to one heartbeat interval.
    #[test]
    fn daemon_evicted_by_a_dying_connection_recovers_on_its_next_heartbeat() {
        // The daemon's original connection.
        let (registry, _stale_tx) = registered("daemon-1");

        // Its mesh path half-dies. TCP does not fail fast, so the old task is still
        // parked in ws_rx.next() while the daemon reconnects over a working path and
        // registers again — an ordinary Register, which overwrites the entry.
        let (live_tx, mut live_rx) = mpsc::unbounded_channel();
        registry.register(DaemonConnection::new(
            "daemon-1".to_string(),
            test_metadata(),
            live_tx.clone(),
        ));
        assert_eq!(registry.count(), 1);

        // The old socket finally errors and its task runs the cleanup on the way out.
        // Keyed only by id, it evicts the LIVE connection that replaced it.
        registry.remove("daemon-1");
        assert_eq!(
            registry.count(),
            0,
            "the dying connection's cleanup evicted the live entry (the race being modeled)"
        );

        // The live daemon is now invisible even though its socket is fine — the exact
        // state that used to persist forever. Its next heartbeat is rejected...
        assert!(
            !handle_heartbeat("daemon-1", &registry),
            "an evicted daemon's heartbeat must be rejected so it knows to re-register"
        );

        // ...so it re-registers on that same healthy socket, carrying its own channel.
        registry.register(DaemonConnection::new(
            "daemon-1".to_string(),
            test_metadata(),
            live_tx,
        ));

        // Recovered: visible again, and reachable on the connection it is actually using.
        assert_eq!(registry.count(), 1);
        assert!(handle_heartbeat("daemon-1", &registry));
        registry
            .get("daemon-1")
            .unwrap()
            .send_message(Message::Heartbeat)
            .unwrap();
        assert!(
            live_rx.try_recv().is_ok(),
            "work must route to the recovered daemon's live connection"
        );
    }

    // The `sub` binding. A token names ONE daemon, and that is the id the connection
    // registers under — so no daemon can take over another's registry entry and receive
    // its exec/logs traffic.
    //
    // Note what this does NOT rest on: keeping the id secret. A JWT payload is base64,
    // not encrypted, so the bearer can always read its own `sub`. What stops
    // impersonation is that registering as another daemon requires a token whose sub IS
    // that daemon, and minting one requires the manager's private key.
    #[test]
    fn the_token_decides_the_registered_id() {
        // Agreement is the ordinary case.
        assert_eq!(
            registration_id(Some("daemon-1"), "daemon-1").as_deref(),
            Some("daemon-1")
        );

        // The attack: a valid token for daemon-1 used to claim daemon-2. Registration
        // proceeds, but under daemon-1 — so daemon-2's traffic is never redirected.
        assert_eq!(
            registration_id(Some("daemon-1"), "daemon-2").as_deref(),
            Some("daemon-1"),
            "a claimed id must never override the token's sub"
        );

        // Near-misses resolve to the token's id too, with no trimming or case folding
        // that could make a lookalike collide with the real entry.
        for claimed in [
            "daemon-10",
            "daemon-1 ",
            " daemon-1",
            "Daemon-1",
            "daemon-1\n",
            "",
        ] {
            assert_eq!(
                registration_id(Some("daemon-1"), claimed).as_deref(),
                Some("daemon-1"),
                "claiming {:?} must still register as daemon-1",
                claimed
            );
        }
    }

    // A daemon that was never told an id is the case this replaced: it used to invent a
    // UUID and be refused, leaving nothing in the registry and no clue why. With the id
    // derived from the token, an unconfigured daemon now registers correctly.
    #[test]
    fn an_unconfigured_daemon_still_registers_under_its_token() {
        // What the daemon sends when SANDD_DAEMON_ID is unset: a random UUID.
        let uuid = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        assert_eq!(
            registration_id(Some("team-ml-trainer"), uuid).as_deref(),
            Some("team-ml-trainer")
        );
    }

    // Auth disabled: the claimed id is used as-is, preserving standalone/local-dev
    // behaviour. An empty id is refused, since nothing could address that entry.
    #[test]
    fn the_claimed_id_is_used_when_auth_is_disabled() {
        assert_eq!(
            registration_id(None, "any-daemon").as_deref(),
            Some("any-daemon")
        );
        assert_eq!(registration_id(None, ""), None);
    }

    // Auth disabled: no Authorization header needed, and no id is bound.
    #[test]
    fn authenticate_admits_everyone_when_disabled() {
        let headers = HeaderMap::new();

        assert_eq!(authenticate(None, &headers), Ok(None));
    }

    // Auth enabled with no credential presented. The rejection must be MissingToken, so
    // the log distinguishes "daemon predates auth / lost its token" from "forgery".
    #[test]
    fn authenticate_requires_a_header_when_enabled() {
        let verifier = Arc::new(test_verifier());
        let headers = HeaderMap::new();

        assert_eq!(
            authenticate(Some(&verifier), &headers),
            Err(AuthError::MissingToken)
        );
    }

    // A garbage bearer token is refused rather than panicking. This runs BEFORE the
    // upgrade, on an unauthenticated path, so it is the most exposed code in the server.
    #[test]
    fn authenticate_rejects_a_bogus_token() {
        let verifier = Arc::new(test_verifier());
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer not-a-real-jwt".parse().unwrap(),
        );

        assert_eq!(
            authenticate(Some(&verifier), &headers),
            Err(AuthError::InvalidToken)
        );
    }

    // A non-UTF8 header value must be treated as absent, not unwrapped.
    #[test]
    fn authenticate_survives_a_non_utf8_header() {
        let verifier = Arc::new(test_verifier());
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );

        assert_eq!(
            authenticate(Some(&verifier), &headers),
            Err(AuthError::MissingToken)
        );
    }

    // Auth is enabled by CONSTRUCTION: with_auth requires an already-built verifier, so
    // "enabled but no usable key" cannot be represented. new() is explicitly the
    // unauthenticated shape.
    #[test]
    fn auth_mode_follows_the_constructor() {
        assert!(SandboxServer::new("127.0.0.1:0".to_string())
            .verifier
            .is_none());
        assert!(
            SandboxServer::with_auth("127.0.0.1:0".to_string(), test_verifier())
                .verifier
                .is_some()
        );
    }

    /// A verifier over a throwaway key. Only used for paths that must fail before any
    /// signature check, so the key never needs to match a minted token.
    fn test_verifier() -> TokenVerifier {
        // Generated with `openssl genpkey -algorithm ed25519 | openssl pkey -pubout`.
        const PUBLIC_PEM: &str =
            "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n-----END PUBLIC KEY-----\n";
        TokenVerifier::new(PUBLIC_PEM, "sandd-test", "nebula", "kid-1").unwrap()
    }

    // /stats is scraped by hand (curl | jq) when a provisioned instance's daemon never
    // shows up, so the FIELD NAMES are the contract — renaming one silently breaks the
    // reader. The pre-existing three keys are asserted alongside the new `daemons` map
    // because they are what any current consumer already reads.
    #[test]
    fn stats_body_exposes_each_daemon_keyed_by_id() {
        let (registry, _tx) = registered("daemon-1");

        let body = stats_body(registry.get_stats());

        assert_eq!(body["total_daemons"], 1);
        assert_eq!(body["by_platform"]["linux"], 1);
        assert!(body["oldest_connection_secs"].is_u64());

        let d = &body["daemons"]["daemon-1"];
        assert_eq!(d["hostname"], "gpu-box");
        assert_eq!(d["platform"], "linux");
        assert_eq!(d["arch"], "x86_64");
        assert_eq!(d["version"], "0.1.0");
        assert_eq!(d["labels"]["env"], "prod");
        assert_eq!(d["is_busy"], false);
        // Present and numeric: this is the field that says how close the daemon is to
        // being reaped, so an absent/null value would make the payload useless.
        assert!(d["seconds_since_heartbeat"].is_u64());
        assert!(d["connected_secs"].is_u64());
    }

    // An empty registry must still emit `daemons` as an OBJECT, not null or a missing
    // key — otherwise `jq '.daemons | keys'` errors exactly when there are no daemons,
    // which is the case you are most often debugging.
    #[test]
    fn stats_body_has_empty_daemons_object_when_none_connected() {
        let registry = Arc::new(DaemonRegistry::new());

        let body = stats_body(registry.get_stats());

        assert_eq!(body["total_daemons"], 0);
        assert!(
            body["daemons"].is_object(),
            "daemons must be an object even when empty, got {}",
            body["daemons"]
        );
        assert_eq!(body["daemons"].as_object().unwrap().len(), 0);
    }
}
