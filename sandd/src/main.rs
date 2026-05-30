// Shell functionality is disabled in MVP, allow dead code warnings
#![allow(dead_code)]
#![allow(unused_imports)]

mod executor;
mod protocol;
mod shell;

use anyhow::Result;
use clap::Parser;
use executor::CommandExecutor;
use futures_util::{SinkExt, StreamExt};
use protocol::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::System;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{debug, error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "sandd")]
#[command(about = "SandD - Sandbox Daemon for remote command execution")]
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
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    info!("Starting sandbox daemon: {}", daemon_id);

    // Main connection loop with reconnection
    loop {
        match connect_and_serve(&args.server_url, &daemon_id, args.heartbeat_interval).await {
            Ok(_) => info!("Connection closed gracefully"),
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
) -> Result<()> {
    info!("Connecting to server at {}", server_url);

    // Connect with subprotocol using client builder
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = server_url.into_client_request()?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static("sandd.v1"),
    );

    let (ws_stream, response) = match tokio_tungstenite::connect_async(request).await {
        Ok(result) => result,
        Err(e) => {
            error!("WebSocket connection error details: {:?}", e);
            return Err(anyhow::anyhow!("Failed to connect to server: {}", e));
        }
    };

    // Check negotiated protocol
    if let Some(protocol) = response.headers().get("sec-websocket-protocol") {
        info!("Negotiated protocol: {:?}", protocol);
    } else {
        warn!("Server did not negotiate protocol");
    }

    info!("WebSocket connection established");

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Gather system metadata
    let metadata = protocol::DaemonMetadata {
        hostname: System::host_name().unwrap_or_else(|| "unknown".to_string()),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        labels: HashMap::new(),
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
                    return Ok(());
                }
            }
            _ => {
                warn!("Unexpected message, continuing anyway");
            }
        }
    }

    // Initialize executors
    let executor = Arc::new(CommandExecutor::new());

    // Spawn heartbeat task
    let ws_tx_clone = Arc::new(tokio::sync::Mutex::new(ws_tx));
    let ws_tx_heartbeat = ws_tx_clone.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval));
        loop {
            interval.tick().await;
            let heartbeat = Message::Heartbeat;
            if let Ok(json) = serde_json::to_string(&heartbeat) {
                let mut tx = ws_tx_heartbeat.lock().await;
                if tx.send(WsMessage::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    // Message processing loop
    while let Some(msg) = ws_rx.next().await {
        let msg = match msg {
            Ok(WsMessage::Text(text)) => text,
            Ok(WsMessage::Close(_)) => {
                info!("Server closed connection");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
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
        if let Err(e) = handle_message(message, ws_tx_clone.clone(), executor.clone()).await {
            error!("Error handling message: {}", e);
        }
    }

    heartbeat_handle.abort();
    info!("Disconnected from agent");

    Ok(())
}

async fn handle_message<T>(
    message: Message,
    ws_tx: Arc<tokio::sync::Mutex<T>>,
    executor: Arc<CommandExecutor>,
) -> Result<()>
where
    T: SinkExt<WsMessage> + Unpin,
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
            // Check for in-tree commands (sandd_* prefix)
            if let Some(intree_cmd) = command.strip_prefix("sandd_") {
                debug!("Handling in-tree command: {}", intree_cmd);

                let start = std::time::Instant::now();
                let result = handle_intree_command(intree_cmd).await;
                let duration_ms = start.elapsed().as_millis() as u64;

                let response = match result {
                    Ok(output) => Message::CommandOutput {
                        request_id,
                        stdout: output,
                        stderr: String::new(),
                        exit_code: 0,
                        duration_ms,
                    },
                    Err(e) => Message::CommandOutput {
                        request_id,
                        stdout: String::new(),
                        stderr: format!("In-tree command error: {}", e),
                        exit_code: 1,
                        duration_ms,
                    },
                };

                let json = serde_json::to_string(&response)?;
                let mut tx = ws_tx.lock().await;
                tx.send(WsMessage::Text(json)).await?
            } else {
                // Normal shell execution
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
        }

        Message::StartShell {
            request_id,
            rows: _,
            cols: _,
            term: _,
        } => {
            debug!(
                "Starting shell session: {} (not implemented in MVP)",
                request_id
            );

            // TODO: Shell functionality disabled for MVP due to PtySystem Sync issues
            let response = Message::ShellStarted {
                request_id,
                success: false,
                error: Some("Shell functionality not implemented in MVP".to_string()),
            };

            let json = serde_json::to_string(&response)?;
            let mut tx = ws_tx.lock().await;
            tx.send(WsMessage::Text(json))
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        Message::ShellInput {
            request_id: _,
            data: _,
        } => {
            debug!("Shell input (not implemented)");
            // TODO: Shell functionality disabled for MVP
        }

        Message::ShellResize {
            request_id: _,
            rows: _,
            cols: _,
        } => {
            debug!("Shell resize (not implemented)");
            // TODO: Shell functionality disabled for MVP
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

        _ => {
            debug!("Received unhandled message type");
        }
    }

    Ok(())
}

async fn handle_intree_command(cmd: &str) -> Result<String> {
    match cmd {
        _ => Err(anyhow::anyhow!("Unknown in-tree command: {}", cmd)),
    }
}
