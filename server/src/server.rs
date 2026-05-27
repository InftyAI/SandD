use crate::protocol::Message;
use crate::registry::{CommandResult, DaemonConnection, DaemonRegistry};
use anyhow::{anyhow, Context, Result};
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
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
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

    // Create channel for outgoing commands
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();

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

                handle_daemon_message(message, &mut daemon_id, &registry, &mut ws_tx, &cmd_tx).await;
            }

            // Receive commands from Python (via channel)
            Some(cmd) = cmd_rx.recv() => {
                let json = match serde_json::to_string(&cmd) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize command: {}", e);
                        continue;
                    }
                };

                if let Err(e) = ws_tx.send(axum::extract::ws::Message::Text(json)).await {
                    error!("Failed to send command to daemon: {}", e);
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

async fn handle_daemon_message(
    message: Message,
    daemon_id: &mut Option<String>,
    registry: &Arc<DaemonRegistry>,
    ws_tx: &mut futures_util::stream::SplitSink<WebSocket, axum::extract::ws::Message>,
    cmd_tx: &mpsc::UnboundedSender<Message>,
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
                "Daemon {} registered: {} {} {}",
                id, metadata.hostname, metadata.platform, metadata.arch
            );

            // Create and register connection with channel
            let new_conn = DaemonConnection::new(id.clone(), metadata, cmd_tx.clone());
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
                if let Some(conn) = registry.get(id) {
                    conn.update_heartbeat();
                    debug!("Heartbeat from daemon {}", id);
                }
            }
        }

        Message::CommandOutput {
            command_id,
            stdout,
            stderr,
            exit_code,
            duration_ms,
        } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    debug!("Command {} completed on daemon {}", command_id, id);
                    conn.complete_command(
                        &command_id,
                        CommandResult {
                            stdout,
                            stderr,
                            exit_code,
                            duration_ms,
                        },
                    );
                }
            }
        }

        Message::ShellOutput { session_id, data } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    conn.send_shell_output(&session_id, data);
                }
            }
        }

        Message::ShellExit {
            session_id,
            exit_code,
        } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    debug!(
                        "Shell session {} exited with code {} on daemon {}",
                        session_id, exit_code, id
                    );
                    conn.close_shell_session(&session_id);
                }
            }
        }

        Message::FileDownloadChunk {
            transfer_id,
            data,
            is_last,
            ..
        } => {
            if let Some(ref id) = daemon_id {
                if let Some(conn) = registry.get(id) {
                    conn.add_file_chunk(&transfer_id, data);
                    if is_last {
                        debug!("File transfer {} completed on daemon {}", transfer_id, id);
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
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;

        let removed = registry.cleanup_stale(90); // 90 second timeout
        if removed > 0 {
            warn!("Cleaned up {} stale daemon connections", removed);
        }

        info!("Active daemons: {} ", registry.count());
    }
}
