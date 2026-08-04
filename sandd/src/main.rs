mod executor;
// Use shared protocol crate
mod session;
pub mod snapshot;

use anyhow::{Context, Result};
use clap::Parser;
use executor::CommandExecutor;
use futures_util::{SinkExt, StreamExt};
use sandd_protocol::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::System;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{debug, error, info, warn};

/// Address of the SOCKS5 proxy tailscaled exposes in tunnel mode (see
/// setup_tunnel). In --tun=userspace-networking there is no kernel route to the
/// tailnet, so the daemon dials the controller THROUGH this proxy to reach mesh
/// peers; using it with remote DNS also lets MagicDNS names resolve inside
/// tailscaled. Localhost-only: reachable solely by this container's daemon.
const TUNNEL_SOCKS_PROXY: &str = "127.0.0.1:1055";

/// Why the serve loop returned, so main() knows whether to reconnect or exit.
#[derive(Debug, PartialEq, Eq)]
enum ServeOutcome {
    /// The connection dropped (server closed, socket error). main() reconnects.
    Disconnected,
    /// We received SIGTERM/SIGINT and closed the connection cleanly. main() exits
    /// the process instead of reconnecting.
    Shutdown,
}

/// Resolve when the process is asked to terminate: SIGTERM (what `kubectl delete
/// pod` / `docker stop` send) or SIGINT (Ctrl-C). On non-unix, only Ctrl-C.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // If we can't install a handler there is nothing sensible to do but keep
        // running; a failed handler must not take the daemon down.
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to install SIGTERM handler: {}", e);
                // Fall back to only Ctrl-C.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = sigterm.recv() => info!("Received SIGTERM, shutting down"),
            _ = tokio::signal::ctrl_c() => info!("Received SIGINT, shutting down"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("Received Ctrl-C, shutting down");
    }
}

#[derive(Parser, Debug)]
#[command(name = "sandd")]
#[command(
    about = "SandD - A lightweight sandbox daemon that provides secure, isolated execution environments for agents."
)]
struct Args {
    /// Server URL (e.g., ws://localhost:8765/ws)
    #[arg(short, long, env = "SERVER_URL")]
    server_url: String,

    /// Daemon ID (unique identifier)
    #[arg(short, long, env = "DAEMON_ID")]
    daemon_id: Option<String>,

    /// Reconnection interval in seconds
    #[arg(short, long, default_value = "5")]
    reconnect_interval: u64,

    /// Heartbeat interval in seconds
    #[arg(long, default_value = "10")]
    heartbeat_interval: u64,

    /// Labels in key=value format (e.g., --label env=prod --label region=us-west)
    #[arg(short, long = "label", value_name = "KEY=VALUE")]
    labels: Vec<String>,

    /// Enable tunnel mode (requires Tailscale)
    #[arg(long)]
    tunnel: bool,

    /// Tunnel auth key (required if --tunnel is set)
    #[arg(long)]
    tunnel_authkey: Option<String>,

    /// Tunnel control server URL (required if --tunnel is set)
    #[arg(long)]
    tunnel_server: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    // Generate daemon ID if not provided
    let daemon_id = args
        .daemon_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Parse labels from key=value format
    let mut labels = HashMap::new();
    for label in &args.labels {
        if let Some((key, value)) = label.split_once('=') {
            labels.insert(key.to_string(), value.to_string());
        } else {
            warn!("Invalid label format (expected key=value): {}", label);
        }
    }

    info!("Starting sandbox daemon: {}", daemon_id);
    if !labels.is_empty() {
        info!("Labels: {:?}", labels);
    }

    if args.tunnel {
        info!("Tunnel mode enabled");
    }

    // Main connection loop with reconnection.
    loop {
        // In tunnel mode, (re)establish the mesh on EVERY iteration before dialing
        // the controller. setup_tunnel is idempotent (see its body): first pass it
        // starts tailscaled + joins; on a reconnect it re-runs `tailscale up`, which
        // re-registers the node if headscale reaped it. Without this, a reaped daemon
        // loops forever dialing the controller through a dead tunnel and never rejoins
        // (container stays Running, node stays gone from headscale). On failure, log
        // and fall through to the backoff sleep rather than crash — a transient mesh
        // failure must not kill a long-lived daemon.
        if args.tunnel {
            if let Err(e) = setup_tunnel(&args).await {
                error!("Failed to (re)establish tunnel: {}; retrying", e);
                warn!("Reconnecting in {} seconds...", args.reconnect_interval);
                tokio::time::sleep(Duration::from_secs(args.reconnect_interval)).await;
                continue;
            }
        }

        match connect_and_serve(
            &args.server_url,
            &daemon_id,
            args.heartbeat_interval,
            labels.clone(),
            args.tunnel,
        )
        .await
        {
            // A clean shutdown (SIGTERM/SIGINT) must NOT reconnect — exit the
            // process so the pod terminates promptly and the controller sees the
            // Close frame we just sent.
            Ok(ServeOutcome::Shutdown) => {
                info!("Shutdown complete");
                return Ok(());
            }
            // The specific reason (server Close, socket error, stream end,
            // registration failure) is already logged at the break site inside
            // serve(); avoid claiming "gracefully" here since Disconnected also
            // covers error paths. main() only needs to know: reconnect.
            Ok(ServeOutcome::Disconnected) => info!("Connection closed, reconnecting"),
            Err(e) => error!("Connection error: {}", e),
        }

        warn!("Reconnecting in {} seconds...", args.reconnect_interval);
        tokio::time::sleep(Duration::from_secs(args.reconnect_interval)).await;
    }
}

async fn connect_and_serve(
    server_url: &str,
    daemon_id: &str,
    heartbeat_interval: u64,
    labels: HashMap<String, String>,
    tunnel: bool,
) -> Result<ServeOutcome> {
    info!("Connecting to server at {}", server_url);

    // Connect with subprotocol using client builder
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = server_url.into_client_request()?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static("sandd.v1"),
    );

    // Two transports, ONE serve loop (generic over the stream):
    //   - tunnel mode: the tailnet has no kernel route in userspace-networking, so
    //     open the TCP hop THROUGH tailscaled's SOCKS5 proxy, then run the
    //     WebSocket over that socket. The target host is passed to the proxy as a
    //     name (not pre-resolved locally), so MagicDNS names resolve inside
    //     tailscaled ("socks5h" semantics).
    //   - direct mode: unchanged — dial the server straight with connect_async.
    if tunnel {
        let uri = request.uri().clone();
        let host = uri
            .host()
            .ok_or_else(|| anyhow::anyhow!("tunnel: server URL has no host: {}", server_url))?
            .to_string();
        // ws:// -> 80, wss:// -> 443 if unspecified; Nebula sets an explicit :8765.
        let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
            Some("wss") => 443,
            _ => 80,
        });

        info!(
            "Dialing {}:{} via tailscaled SOCKS5 proxy {}",
            host, port, TUNNEL_SOCKS_PROXY
        );
        // (host, port) with a non-IP host becomes a SOCKS "domain" target, so
        // tailscaled does the DNS — this is what makes MagicDNS names work.
        let socks = tokio_socks::tcp::Socks5Stream::connect(TUNNEL_SOCKS_PROXY, (host.as_str(), port))
            .await
            .with_context(|| {
                format!(
                    "tunnel: failed to reach {}:{} through SOCKS5 proxy {} (is tailscaled up and joined?)",
                    host, port, TUNNEL_SOCKS_PROXY
                )
            })?;

        let (ws_stream, response) = tokio_tungstenite::client_async_tls(request, socks)
            .await
            .context("tunnel: WebSocket handshake over SOCKS5 failed")?;
        log_negotiated_protocol(&response);
        return serve(ws_stream, daemon_id, heartbeat_interval, labels, shutdown_signal()).await;
    }

    let (ws_stream, response) = match tokio_tungstenite::connect_async(request).await {
        Ok(result) => result,
        Err(e) => {
            error!("WebSocket connection error details: {:?}", e);
            return Err(anyhow::anyhow!("Failed to connect to server: {}", e));
        }
    };
    log_negotiated_protocol(&response);
    serve(ws_stream, daemon_id, heartbeat_interval, labels, shutdown_signal()).await
}

/// Log the WebSocket subprotocol the server negotiated (shared by both transports).
fn log_negotiated_protocol(
    response: &tokio_tungstenite::tungstenite::handshake::client::Response,
) {
    if let Some(protocol) = response.headers().get("sec-websocket-protocol") {
        info!("Negotiated protocol: {:?}", protocol);
    } else {
        warn!("Server did not negotiate protocol");
    }
}

/// Run the daemon session over an established WebSocket stream. Generic over the
/// transport so the direct (connect_async) and tunnel (SOCKS5) paths share one
/// implementation.
///
/// `shutdown` is the future that, once resolved, triggers a graceful close: in
/// production it is `shutdown_signal()` (SIGTERM/SIGINT); tests inject a future
/// they control so the shutdown path can be exercised without raising a real,
/// process-wide signal mid-connection.
async fn serve<S, F>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    daemon_id: &str,
    heartbeat_interval: u64,
    labels: HashMap<String, String>,
    shutdown: F,
) -> Result<ServeOutcome>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    F: std::future::Future<Output = ()>,
{
    info!("WebSocket connection established");

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Gather system metadata
    let metadata = sandd_protocol::DaemonMetadata {
        hostname: System::host_name().unwrap_or_else(|| "unknown".to_string()),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        labels,
    };

    // Send registration
    let register_msg = Message::Register {
        daemon_id: daemon_id.to_string(),
        metadata,
    };
    let register_json = serde_json::to_string(&register_msg)?;
    ws_tx.send(WsMessage::Text(register_json)).await?;

    info!("Registration sent, waiting for ack...");

    // Wait for registration ack
    if let Some(Ok(WsMessage::Text(text))) = ws_rx.next().await {
        let msg: Message = serde_json::from_str(&text)?;
        match msg {
            Message::RegisterAck { success, message } => {
                if success {
                    info!("Registration successful: {}", message);
                } else {
                    error!("Registration failed: {}", message);
                    return Ok(ServeOutcome::Disconnected);
                }
            }
            _ => {
                warn!("Unexpected message, continuing anyway");
            }
        }
    }

    // Initialize executors
    let executor = Arc::new(CommandExecutor::new());
    let session_manager = Arc::new(tokio::sync::Mutex::new(session::SessionManager::new()));

    // Initialize sandd root (default: ~/.sandd)
    let sandd_root = std::env::var("SANDD_ROOT").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{}/.sandd", home)
    });
    let snapshot_manager = Arc::new(
        snapshot::SnapshotManager::new(std::path::PathBuf::from(&sandd_root))
            .context("Failed to initialize snapshot manager")?,
    );

    // Spawn heartbeat task. A failed heartbeat send is our ONLY reliable signal
    // that the connection is dead: over a DERP-relayed mesh, a controller that
    // vanishes (e.g. pod restart) often produces no TCP FIN/RST on the daemon side,
    // so `ws_rx.next()` in the loop below blocks forever and never surfaces the
    // drop. The heartbeat write, by contrast, fails. So on send failure we trip
    // `dead_tx`, which the select! polls to break the serve loop and let main()
    // reconnect — without this the daemon wedges half-open until the pod is deleted.
    let ws_tx_clone = Arc::new(tokio::sync::Mutex::new(ws_tx));
    let ws_tx_heartbeat = ws_tx_clone.clone();
    let (dead_tx, dead_rx) = tokio::sync::oneshot::channel::<()>();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval));
        let mut dead_tx = Some(dead_tx);
        loop {
            interval.tick().await;
            let heartbeat = Message::Heartbeat;
            if let Ok(json) = serde_json::to_string(&heartbeat) {
                let mut tx = ws_tx_heartbeat.lock().await;
                if tx.send(WsMessage::Text(json)).await.is_err() {
                    // Signal the serve loop that the connection is dead so it
                    // reconnects instead of blocking forever on a half-open read.
                    if let Some(d) = dead_tx.take() {
                        let _ = d.send(());
                    }
                    break;
                }
            }
        }
    });

    // Message processing loop. Also races a shutdown signal so that on
    // SIGTERM/SIGINT we send a WebSocket Close frame BEFORE exiting: the
    // controller removes a daemon the moment it sees that Close (server.rs
    // handle_websocket), so a graceful pod deletion deregisters immediately
    // instead of waiting out the ~90s heartbeat-timeout reaper.
    //
    // Pin the shutdown future once so it can be polled across loop iterations
    // without being moved (it may be `!Unpin`).
    tokio::pin!(shutdown);
    tokio::pin!(dead_rx);
    let outcome = loop {
        tokio::select! {
            // Poll shutdown FIRST. With `biased`, tokio checks branches top to
            // bottom and only reaches a later branch when earlier ones are
            // Pending; if the message branch were first and inbound messages were
            // continuously ready (buffered / high throughput), the shutdown branch
            // could starve and delay the Close frame + process exit. Shutdown is
            // usually Pending, so in normal operation this falls straight through
            // to draining messages; it can't starve them because shutdown fires
            // once and breaks the loop.
            biased;

            _ = &mut shutdown => {
                // Best-effort clean close so the controller deregisters us now.
                let mut tx = ws_tx_clone.lock().await;
                if let Err(e) = tx.send(WsMessage::Close(None)).await {
                    warn!("Failed to send Close frame on shutdown: {}", e);
                }
                break ServeOutcome::Shutdown;
            }

            // Heartbeat send failed => connection is dead. Reconnect. (The Err
            // arm — heartbeat task gone without signalling — is treated the same:
            // no live heartbeat means no live connection.)
            _ = &mut dead_rx => {
                warn!("Heartbeat send failed, connection is dead; reconnecting");
                break ServeOutcome::Disconnected;
            }

            msg = ws_rx.next() => {
                let msg = match msg {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(_))) => {
                        info!("Server closed connection");
                        break ServeOutcome::Disconnected;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break ServeOutcome::Disconnected;
                    }
                    None => {
                        // Stream ended.
                        break ServeOutcome::Disconnected;
                    }
                    _ => continue,
                };

                let message: Message = match serde_json::from_str(&msg) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Failed to parse message: {}", e);
                        continue;
                    }
                };

                // Handle message inline
                if let Err(e) = handle_message(
                    message,
                    ws_tx_clone.clone(),
                    executor.clone(),
                    session_manager.clone(),
                    snapshot_manager.clone(),
                )
                .await
                {
                    error!("Error handling message: {}", e);
                }
            }
        }
    };

    heartbeat_handle.abort();
    info!("Disconnected from agent");

    Ok(outcome)
}

async fn handle_message<T>(
    message: Message,
    ws_tx: Arc<tokio::sync::Mutex<T>>,
    executor: Arc<CommandExecutor>,
    session_manager: Arc<tokio::sync::Mutex<session::SessionManager>>,
    snapshot_manager: Arc<snapshot::SnapshotManager>,
) -> Result<()>
where
    T: SinkExt<WsMessage> + Unpin + Send + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    match message {
        Message::ExecuteCommand {
            request_id,
            command,
            timeout_secs,
            env,
            cwd,
        } => {
            // Execute command directly via shell
            debug!("Executing command: {}", command);
            let result = executor.execute(&command, timeout_secs, env, cwd).await;

            let response = match result {
                Ok(output) => Message::CommandOutput {
                    request_id,
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit_code: output.exit_code,
                    duration_ms: output.duration_ms,
                },
                Err(e) => Message::CommandError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::NewSession {
            session_id,
            rows,
            cols,
            term,
        } => {
            debug!("Starting session: {}", session_id);

            let mut manager = session_manager.lock().await;
            let result = manager
                .new_session(session_id.clone(), rows, cols, &term, ws_tx.clone())
                .await;

            let response = match result {
                Ok(()) => Message::SessionStarted {
                    session_id,
                    success: true,
                    error: None,
                },
                Err(e) => Message::SessionStarted {
                    session_id,
                    success: false,
                    error: Some(e.to_string()),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::SessionInput { session_id, data } => {
            debug!(
                "Session input: {} bytes for session {}",
                data.len(),
                session_id
            );
            let manager = session_manager.lock().await;
            if let Err(e) = manager.send_input(&session_id, &data).await {
                error!("Failed to send input to session {}: {}", session_id, e);
            }
        }

        Message::SessionResize {
            session_id,
            rows,
            cols,
        } => {
            debug!("Session resize: {} to {}x{}", session_id, rows, cols);
            let manager = session_manager.lock().await;
            if let Err(e) = manager.resize(&session_id, rows, cols).await {
                error!("Failed to resize session {}: {}", session_id, e);
            }
        }

        Message::SessionClose { session_id } => {
            debug!("Closing session: {}", session_id);
            let mut manager = session_manager.lock().await;
            manager.close_session(&session_id);
        }

        Message::FileUploadStart {
            request_id: _,
            path,
            total_size,
            mode: _,
        } => {
            debug!("Starting file upload: {} ({} bytes)", path, total_size);
            // File upload will be handled by subsequent chunks
            // For now, just acknowledge
        }

        Message::FileUploadChunk {
            request_id: _,
            data,
            offset,
        } => {
            // In a full implementation, write chunks to file
            debug!(
                "Received file chunk: {} bytes at offset {}",
                data.len(),
                offset
            );
        }

        Message::FileDownloadStart { request_id, path } => {
            debug!("Starting file download: {}", path);

            // Read file and send chunks
            match tokio::fs::read(&path).await {
                Ok(data) => {
                    const CHUNK_SIZE: usize = 64 * 1024;
                    for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
                        let is_last = (i + 1) * CHUNK_SIZE >= data.len();
                        let response = Message::FileDownloadChunk {
                            request_id: request_id.clone(),
                            data: chunk.to_vec(),
                            offset: (i * CHUNK_SIZE) as u64,
                            is_last,
                        };

                        let json = serde_json::to_string(&response)?;
                        let mut tx = ws_tx.lock().await;
                        tx.send(WsMessage::Text(json)).await?;
                    }
                }
                Err(e) => {
                    let response = Message::FileDownloadError {
                        request_id,
                        error: e.to_string(),
                    };
                    let json = serde_json::to_string(&response)?;
                    let mut tx = ws_tx.lock().await;
                    tx.send(WsMessage::Text(json)).await?;
                }
            }
        }

        Message::CreateSnapshot {
            request_id,
            workspace,
            message,
            tags,
        } => {
            debug!("Creating snapshot of {}", workspace);
            let result = snapshot_manager
                .create_snapshot(std::path::Path::new(&workspace), message, tags)
                .await;

            let response = match result {
                Ok(snapshot_id) => {
                    let snapshot = snapshot_manager.get_snapshot(&snapshot_id).await?;
                    Message::SnapshotCreated {
                        request_id,
                        snapshot_id,
                        file_count: snapshot.file_count,
                        total_size: snapshot.total_size,
                    }
                }
                Err(e) => Message::SnapshotError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::RestoreSnapshot {
            request_id,
            snapshot_id,
            destination,
        } => {
            debug!("Restoring snapshot {} to {}", snapshot_id, destination);
            let result = snapshot_manager
                .restore_snapshot(&snapshot_id, std::path::Path::new(&destination))
                .await;

            let response = match result {
                Ok(()) => {
                    let snapshot = snapshot_manager.get_snapshot(&snapshot_id).await?;
                    Message::SnapshotRestored {
                        request_id,
                        file_count: snapshot.file_count,
                    }
                }
                Err(e) => Message::SnapshotError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::ListSnapshots { request_id, tags } => {
            debug!("Listing snapshots");
            let result = snapshot_manager.list_snapshots(tags).await;

            let response = match result {
                Ok(snapshots) => Message::SnapshotList {
                    request_id,
                    snapshots,
                },
                Err(e) => Message::SnapshotError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::FindSnapshotByTag { request_id, tag } => {
            debug!("Finding snapshot by tag: {}", tag);
            let result = snapshot_manager.find_snapshot_by_tag(&tag).await;

            let response = match result {
                Ok(snapshot) => Message::SnapshotDetails {
                    request_id,
                    snapshot,
                },
                Err(e) => Message::SnapshotError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::GetSnapshot {
            request_id,
            snapshot_id,
        } => {
            debug!("Getting snapshot: {}", snapshot_id);
            let result = snapshot_manager.get_snapshot(&snapshot_id).await;

            let response = match result {
                Ok(snapshot_info) => Message::SnapshotDetails {
                    request_id,
                    snapshot: Some(snapshot_info),
                },
                Err(e) => Message::SnapshotError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::DeleteSnapshot {
            request_id,
            snapshot_id,
        } => {
            debug!("Deleting snapshot: {}", snapshot_id);
            let result = snapshot_manager.delete_snapshot(&snapshot_id).await;

            let response = match result {
                Ok(()) => Message::SnapshotDeleted { request_id },
                Err(e) => Message::SnapshotError {
                    request_id,
                    error: e.to_string(),
                },
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        _ => {
            debug!("Received unhandled message type");
        }
    }

    Ok(())
}

async fn setup_tunnel(args: &Args) -> Result<()> {
    use std::process::Command;

    // Validate required arguments
    let authkey = args
        .tunnel_authkey
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--tunnel requires --tunnel-authkey"))?;

    let server = args
        .tunnel_server
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--tunnel requires --tunnel-server"))?;

    // Check if tailscale is installed by trying to run it
    let tailscale_check = Command::new("tailscale").arg("version").output();

    if tailscale_check.is_err() {
        return Err(anyhow::anyhow!(
            "Tailscale not found. Install it first:\n  \
            curl -fsSL https://raw.githubusercontent.com/InftyAI/SandD/main/hack/scripts/install.sh | sudo bash -s -- --tunnel"
        ));
    }

    // setup_tunnel is IDEMPOTENT: main()'s reconnect loop calls it before every
    // connect attempt so a dropped/reaped node re-establishes the mesh (not just the
    // WebSocket). tailscaled is a long-lived singleton — spawning a second one would
    // collide on the SOCKS5 port and the state lock — so we only start it if it isn't
    // already up. `tailscale up`, by contrast, ALWAYS re-runs: it is idempotent when
    // already connected (cheap no-op) and is exactly what re-activates the node with
    // headscale after an ephemeral reap. This is the fix for "container Running but
    // node gone from headscale": before, tailscale up ran once at startup only, so a
    // reaped daemon looped forever dialing the controller through a dead tunnel and
    // never re-registered.
    let tailscaled_running = Command::new("tailscale")
        .arg("status")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if tailscaled_running {
        info!("tailscaled already running; re-joining mesh");
    } else {
        info!("Starting tailscaled...");
        // --socks5-server is what makes tunnel mode actually work: with
        // --tun=userspace-networking there is no TUN device and thus no kernel route
        // to the tailnet (100.64.0.0/10), so a plain socket to the controller's mesh
        // address always fails. The SOCKS5 proxy is the entry point INTO tailscaled's
        // userspace network stack; connect_and_serve dials the controller through it
        // (see TUNNEL_SOCKS_PROXY) so the WebSocket rides the mesh. Bound to localhost
        // so only this container's daemon can use it.
        Command::new("tailscaled")
            .arg("--tun=userspace-networking")
            .arg(format!("--socks5-server={}", TUNNEL_SOCKS_PROXY))
            .arg("--state=/var/lib/tailscale/tailscaled.state")
            .spawn()
            .context("Failed to start tailscaled")?;

        // Give tailscaled time to start
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    info!("Joining mesh network...");

    // Join mesh. Always run (even when tailscaled was already up): if the node was
    // reaped by headscale this re-registers it; if it's still a valid member this is
    // an idempotent no-op.
    let output = Command::new("tailscale")
        .arg("up")
        .arg(format!("--authkey={}", authkey))
        .arg(format!("--login-server={}", server))
        .arg("--accept-routes")
        .output()
        .context("Failed to join mesh network")?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Failed to join mesh: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Wait for IP assignment
    for _ in 0..30 {
        let ip_output = Command::new("tailscale").arg("ip").arg("-4").output();

        if let Ok(output) = ip_output {
            if output.status.success() {
                let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !ip.is_empty() {
                    info!("✓ Joined mesh network with IP: {}", ip);
                    return Ok(());
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Err(anyhow::anyhow!("Timeout waiting for mesh IP assignment"))
}

#[cfg(test)]
mod shutdown_tests {
    //! Tests for the graceful-shutdown path added to `serve`: on a shutdown
    //! signal the daemon must send a WebSocket Close frame and return
    //! `ServeOutcome::Shutdown` (so `main` exits instead of reconnecting). The
    //! Close is what lets the controller deregister the daemon immediately
    //! rather than waiting out its ~90s heartbeat-timeout reaper.
    //!
    //! `serve` takes the shutdown future as a parameter precisely so these tests
    //! can trigger it deterministically, without raising a real, process-wide
    //! SIGTERM in the middle of a test run.

    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
    use tokio_tungstenite::{accept_async, WebSocketStream};

    /// Stand up an in-process WebSocket server on localhost and connect a client
    /// to it. Returns (client_stream_for_serve, accepted_server_stream). The
    /// server side lets a test act as the controller: ack registration, then
    /// observe what the daemon sends (e.g. the Close frame on shutdown).
    async fn ws_pair() -> (
        WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        WebSocketStream<TcpStream>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept concurrently with the client dial so neither side blocks.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            accept_async(stream).await.unwrap()
        });

        let url = format!("ws://{}/ws", addr);
        let (client, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
        let server = server.await.unwrap();
        (client, server)
    }

    /// Play the controller: read the daemon's Register and reply RegisterAck so
    /// `serve` proceeds past registration into its main loop.
    async fn ack_registration(server: &mut WebSocketStream<TcpStream>) {
        let reg = server.next().await.unwrap().unwrap();
        let text = reg.into_text().unwrap();
        let msg: Message = serde_json::from_str(&text).unwrap();
        assert!(
            matches!(msg, Message::Register { .. }),
            "expected Register first, got: {:?}",
            msg
        );
        let ack = Message::RegisterAck {
            success: true,
            message: "ok".to_string(),
        };
        server
            .send(WsMessage::Text(serde_json::to_string(&ack).unwrap()))
            .await
            .unwrap();
    }

    /// The core regression: when the shutdown future fires, `serve` returns
    /// `Shutdown` AND the peer receives a Close frame.
    ///
    /// `serve`'s future is not `Send` (SessionManager is !Sync), so it can't be
    /// `tokio::spawn`ed; instead we run it concurrently with the controller side
    /// via `join!` on the current task.
    #[tokio::test]
    async fn shutdown_sends_close_and_returns_shutdown() {
        let (client, mut server) = ws_pair().await;

        // Trigger shutdown via a oneshot fired AFTER registration is acked, so
        // the ordering is deterministic (no wall-clock sleep racing the
        // handshake). `serve` awaits the receiver; mapping away the RecvError
        // matches its `Future<Output = ()>` bound and means a dropped sender also
        // resolves shutdown rather than hanging.
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown = async {
            let _ = shutdown_rx.await;
        };

        let daemon = serve(client, "test-daemon", 3600, HashMap::new(), shutdown);

        let controller = async {
            ack_registration(&mut server).await;
            // Registration is complete and serve() is now in its select! loop;
            // fire shutdown deterministically.
            shutdown_tx.send(()).unwrap();
            // The controller side should observe a Close frame. Tolerate any
            // pre-close traffic (e.g. a heartbeat), though the 3600s interval
            // makes that unlikely in-test.
            let mut saw_close = false;
            while let Some(frame) = server.next().await {
                match frame {
                    Ok(WsMessage::Close(_)) => {
                        saw_close = true;
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            saw_close
        };

        let (outcome, saw_close) = tokio::join!(daemon, controller);
        assert!(saw_close, "daemon did not send a Close frame on shutdown");
        assert_eq!(outcome.unwrap(), ServeOutcome::Shutdown);
    }

    /// The counterpart: if the controller closes the connection, `serve` returns
    /// `Disconnected` (so `main` reconnects) — NOT `Shutdown`. Guards against the
    /// shutdown branch swallowing ordinary disconnects.
    #[tokio::test]
    async fn server_close_returns_disconnected() {
        let (client, mut server) = ws_pair().await;

        // A shutdown future that never resolves: only the server-close path can
        // end this session.
        let shutdown = std::future::pending::<()>();

        let daemon = serve(client, "test-daemon", 3600, HashMap::new(), shutdown);

        let controller = async {
            ack_registration(&mut server).await;
            // Controller closes the connection.
            server.close(None).await.unwrap();
        };

        let (outcome, ()) = tokio::join!(daemon, controller);
        assert_eq!(outcome.unwrap(), ServeOutcome::Disconnected);
    }

    /// `shutdown_signal()` must resolve when the process receives SIGTERM (what
    /// `kubectl delete pod` / `docker stop` send). Uses a real self-signal; unix
    /// only. This asserts the wiring, not the WebSocket behavior above.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_signal_resolves_on_sigterm() {
        // Raise SIGTERM to our own process after a short delay, then confirm the
        // helper's future completes rather than hanging.
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            // SAFETY: raising a signal to our own PID is sound; kill(2) with a
            // valid signal number has no memory-safety implications.
            unsafe {
                libc::raise(libc::SIGTERM);
            }
        });

        tokio::time::timeout(Duration::from_secs(5), shutdown_signal())
            .await
            .expect("shutdown_signal did not resolve on SIGTERM");
    }
}
