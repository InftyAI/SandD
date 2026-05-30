use crate::protocol::{DaemonMetadata, Message};
use anyhow::{anyhow, Result};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Represents a connected daemon with its metadata and command channel
pub struct DaemonConnection {
    pub id: String,
    pub metadata: DaemonMetadata,
    pub last_heartbeat: AtomicU64,
    pub connected_at: u64,

    // ═══════════════════════════════════════════════════════════════════
    // Outgoing: Python → Daemon
    // ═══════════════════════════════════════════════════════════════════
    /// Channel to send commands to daemon (Python → handle_websocket → Daemon)
    /// This is the bridge from Python API to the WebSocket handler.
    /// Multiple Python threads can send concurrently (lock-free).
    command_tx: mpsc::UnboundedSender<Message>,

    // ═══════════════════════════════════════════════════════════════════
    // Incoming: Daemon → Python (Request/Response Pattern)
    // ═══════════════════════════════════════════════════════════════════
    /// Maps command_id → response channel for execute_command() calls
    /// When Python sends a command, it registers a oneshot channel here and waits.
    /// When daemon responds with CommandOutput, we look up and send result back.
    /// Pattern: Request/Response (each command gets exactly one response)
    pending_commands: Arc<DashMap<String, oneshot::Sender<CommandResult>>>,

    // ═══════════════════════════════════════════════════════════════════
    // Incoming: Daemon → Python (Streaming Pattern)
    // ═══════════════════════════════════════════════════════════════════
    /// Maps session_id → output channel for interactive shell sessions
    /// Shell output arrives incrementally from daemon, gets forwarded to Python.
    /// Pattern: Streaming (continuous flow of data chunks)
    shell_sessions: Arc<DashMap<String, mpsc::UnboundedSender<Vec<u8>>>>,

    // ═══════════════════════════════════════════════════════════════════
    // Incoming: Daemon → Python (Chunked Buffering Pattern)
    // ═══════════════════════════════════════════════════════════════════
    /// Maps transfer_id → accumulated file chunks for download operations
    /// File arrives in chunks from daemon, we buffer them until complete.
    /// Pattern: Chunked (collect pieces, return whole on completion)
    file_transfers: Arc<DashMap<String, FileTransfer>>,
}

#[derive(Debug, Clone)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct FileTransfer {
    pub path: String,
    pub chunks: Vec<Vec<u8>>,
    pub total_size: u64,
    pub received_size: u64,
}

impl DaemonConnection {
    pub fn new(
        id: String,
        metadata: DaemonMetadata,
        command_tx: mpsc::UnboundedSender<Message>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            id,
            metadata,
            last_heartbeat: AtomicU64::new(now),
            connected_at: now,
            command_tx,
            pending_commands: Arc::new(DashMap::new()),
            shell_sessions: Arc::new(DashMap::new()),
            file_transfers: Arc::new(DashMap::new()),
        }
    }

    pub fn update_heartbeat(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.last_heartbeat.store(now, Ordering::Relaxed);
    }

    pub fn seconds_since_heartbeat(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let last = self.last_heartbeat.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }

    pub fn send_message(&self, msg: Message) -> Result<()> {
        self.command_tx
            .send(msg)
            .map_err(|_| anyhow!("Daemon channel closed"))?;
        Ok(())
    }

    pub fn register_command(&self, command_id: String, tx: oneshot::Sender<CommandResult>) {
        self.pending_commands.insert(command_id, tx);
    }

    pub fn complete_command(&self, command_id: &str, result: CommandResult) {
        if let Some((_, tx)) = self.pending_commands.remove(command_id) {
            let _ = tx.send(result);
        }
    }

    pub fn register_shell_session(&self, session_id: String, tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.shell_sessions.insert(session_id, tx);
    }

    pub fn send_shell_output(&self, session_id: &str, data: Vec<u8>) {
        if let Some(tx) = self.shell_sessions.get(session_id) {
            let _ = tx.send(data);
        }
    }

    pub fn close_shell_session(&self, session_id: &str) {
        self.shell_sessions.remove(session_id);
    }

    pub fn start_file_transfer(&self, transfer_id: String, path: String, total_size: u64) {
        self.file_transfers.insert(
            transfer_id,
            FileTransfer {
                path,
                chunks: Vec::new(),
                total_size,
                received_size: 0,
            },
        );
    }

    pub fn add_file_chunk(&self, transfer_id: &str, data: Vec<u8>) {
        if let Some(mut transfer) = self.file_transfers.get_mut(transfer_id) {
            transfer.received_size += data.len() as u64;
            transfer.chunks.push(data);
        }
    }

    pub fn complete_file_transfer(&self, transfer_id: &str) -> Option<Vec<u8>> {
        self.file_transfers
            .remove(transfer_id)
            .map(|(_, transfer)| transfer.chunks.into_iter().flatten().collect())
    }
}

/// Central registry for all daemon connections
pub struct DaemonRegistry {
    connections: Arc<DashMap<String, Arc<DaemonConnection>>>,
}

impl DaemonRegistry {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
        }
    }

    pub fn register(&self, conn: DaemonConnection) -> Arc<DaemonConnection> {
        let id = conn.id.clone();
        let arc_conn = Arc::new(conn);

        if let Some(_old) = self.connections.insert(id.clone(), arc_conn.clone()) {
            warn!("Daemon {} reconnected, replacing old connection", id);
        } else {
            info!("Daemon {} registered", id);
        }

        arc_conn
    }

    pub fn get(&self, daemon_id: &str) -> Option<Arc<DaemonConnection>> {
        self.connections.get(daemon_id).map(|entry| entry.clone())
    }

    pub fn remove(&self, daemon_id: &str) {
        if self.connections.remove(daemon_id).is_some() {
            info!("Daemon {} disconnected", daemon_id);
        }
    }

    pub fn list_all(&self) -> Vec<String> {
        self.connections
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    pub fn count(&self) -> usize {
        self.connections.len()
    }

    /// Clean up daemons that haven't sent heartbeat in a while
    pub fn cleanup_stale(&self, timeout_secs: u64) -> usize {
        let mut removed = 0;
        self.connections.retain(|id, conn| {
            let since_heartbeat = conn.seconds_since_heartbeat();
            if since_heartbeat > timeout_secs {
                warn!(
                    "Removing stale daemon {} (no heartbeat for {}s)",
                    id, since_heartbeat
                );
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
    }

    pub fn get_stats(&self) -> RegistryStats {
        let mut stats = RegistryStats {
            total_daemons: self.count(),
            by_platform: std::collections::HashMap::new(),
            oldest_connection_secs: 0,
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        for entry in self.connections.iter() {
            let conn = entry.value();
            *stats
                .by_platform
                .entry(conn.metadata.platform.clone())
                .or_insert(0) += 1;

            let age = now.saturating_sub(conn.connected_at);
            if age > stats.oldest_connection_secs {
                stats.oldest_connection_secs = age;
            }
        }

        stats
    }
}

#[derive(Debug, Clone)]
pub struct RegistryStats {
    pub total_daemons: usize,
    pub by_platform: std::collections::HashMap<String, usize>,
    pub oldest_connection_secs: u64,
}

impl Default for DaemonRegistry {
    fn default() -> Self {
        Self::new()
    }
}
