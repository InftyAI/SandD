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

pub struct SandboxServer {
    registry: Arc<DaemonRegistry>,
    bind_addr: String,
}

impl SandboxServer {
    pub fn new(bind_addr: String) -> Self {
        Self {
            registry: Arc::new(DaemonRegistry::new()),
            bind_addr,
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

        // Build web server
        let app = Router::new()
            .route("/ws", get(websocket_handler))
            .route("/stats", get(stats_handler))
            .route("/health", get(health_handler))
            .with_state(registry);

        info!("Starting sandbox server on {}", self.bind_addr);

        let listener = tokio::net::TcpListener::bind(&self.bind_addr)
            .await
            .context("Failed to bind server")?;

        axum::serve(listener, app).await.context("Server error")?;

        Ok(())
    }
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(registry): State<Arc<DaemonRegistry>>,
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

    if has_protocol {
        info!("Client negotiated protocol: {}", SUPPORTED_PROTOCOL);
        ws.protocols([SUPPORTED_PROTOCOL])
            .on_upgrade(move |socket| handle_websocket(socket, registry))
            .into_response()
    } else {
        error!("Client did not specify required protocol: sandd.v1");
        (
            StatusCode::BAD_REQUEST,
            "Missing required Sec-WebSocket-Protocol: sandd.v1",
        )
            .into_response()
    }
}

async fn handle_websocket(ws: WebSocket, registry: Arc<DaemonRegistry>) {
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

                handle_daemon_message(message, &mut daemon_id, &registry, &mut ws_tx, &request_tx).await;
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

async fn handle_daemon_message(
    message: Message,
    daemon_id: &mut Option<String>,
    registry: &Arc<DaemonRegistry>,
    ws_tx: &mut futures_util::stream::SplitSink<WebSocket, axum::extract::ws::Message>,
    request_tx: &mpsc::UnboundedSender<Message>,
) {
    use futures_util::SinkExt;

    match message {
        Message::Register {
            daemon_id: id,
            metadata,
        } => {
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

async fn stats_handler(State(registry): State<Arc<DaemonRegistry>>) -> impl IntoResponse {
    let stats = registry.get_stats();
    axum::Json(serde_json::json!({
        "total_daemons": stats.total_daemons,
        "by_platform": stats.by_platform,
        "oldest_connection_secs": stats.oldest_connection_secs,
    }))
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
}
