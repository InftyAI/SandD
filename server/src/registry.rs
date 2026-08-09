use sandd_protocol::{DaemonMetadata, Message};
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
    /// Channel to send requests to daemon (Python → handle_websocket → Daemon)
    /// Handles all message types: ExecuteCommand, CreateSnapshot, etc.
    /// This is the bridge from Python API to the WebSocket handler.
    /// Multiple Python threads can send concurrently (lock-free).
    request_tx: mpsc::UnboundedSender<Message>,

    // ═══════════════════════════════════════════════════════════════════
    // Incoming: Daemon → Python (Request/Response Pattern)
    // ═══════════════════════════════════════════════════════════════════
    /// Maps request_id → response channel for ALL request/response operations
    /// Handles: ExecuteCommand, CreateSnapshot, ListSnapshots, FindSnapshotByTag,
    /// GetSnapshot, DeleteSnapshot, RestoreSnapshot, and future operations
    /// Pattern: Request/Response (each request gets exactly one response Message)
    pending_requests: Arc<DashMap<String, oneshot::Sender<Message>>>,

    // ═══════════════════════════════════════════════════════════════════
    // Incoming: Daemon → Python (Streaming Pattern)
    // ═══════════════════════════════════════════════════════════════════
    /// Maps session_id → output channel for interactive sessions
    /// Session output arrives incrementally from daemon, gets forwarded to Python.
    /// Pattern: Streaming (continuous flow of data chunks)
    sessions: Arc<DashMap<String, mpsc::UnboundedSender<Vec<u8>>>>,

    // ═══════════════════════════════════════════════════════════════════
    // Incoming: Daemon → Python (Chunked Buffering Pattern)
    // ═══════════════════════════════════════════════════════════════════
    /// Maps request_id → accumulated file chunks for download operations
    /// File arrives in chunks from daemon, we buffer them until complete.
    /// Pattern: Chunked (collect pieces, return whole on completion)
    file_transfers: Arc<DashMap<String, FileTransfer>>,
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
        request_tx: mpsc::UnboundedSender<Message>,
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
            request_tx,
            pending_requests: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
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
        self.request_tx
            .send(msg)
            .map_err(|_| anyhow!("Daemon channel closed"))?;
        Ok(())
    }

    pub fn register_request(&self, request_id: String, tx: oneshot::Sender<Message>) {
        self.pending_requests.insert(request_id, tx);
    }

    pub fn complete_request(&self, request_id: &str, response: Message) {
        if let Some((_, tx)) = self.pending_requests.remove(request_id) {
            let _ = tx.send(response);
        }
    }

    pub fn is_busy(&self) -> bool {
        !self.pending_requests.is_empty()
    }

    pub fn register_session(&self, session_id: String, tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.sessions.insert(session_id, tx);
    }

    pub fn send_session_output(&self, session_id: &str, data: Vec<u8>) {
        if let Some(tx) = self.sessions.get(session_id) {
            let _ = tx.send(data);
        }
    }

    pub fn close_session(&self, session_id: &str) {
        self.sessions.remove(session_id);
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

    pub fn list_all(
        &self,
        labels: Option<&std::collections::HashMap<String, String>>,
    ) -> Vec<String> {
        self.connections
            .iter()
            .filter(|entry| {
                match labels {
                    Some(filter_labels) if !filter_labels.is_empty() => {
                        // Check if daemon has ALL specified labels (AND logic)
                        filter_labels.iter().all(|(key, value)| {
                            entry
                                .value()
                                .metadata
                                .labels
                                .get(key)
                                .map(|v| v == value)
                                .unwrap_or(false)
                        })
                    }
                    _ => true, // No filter, include all
                }
            })
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
            daemons: std::collections::HashMap::new(),
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

            stats.daemons.insert(
                conn.id.clone(),
                DaemonInfo {
                    hostname: conn.metadata.hostname.clone(),
                    platform: conn.metadata.platform.clone(),
                    arch: conn.metadata.arch.clone(),
                    version: conn.metadata.version.clone(),
                    labels: conn.metadata.labels.clone(),
                    is_busy: conn.is_busy(),
                    connected_secs: age,
                    seconds_since_heartbeat: conn.seconds_since_heartbeat(),
                },
            );
        }

        stats
    }
}

/// Per-daemon detail carried in [`RegistryStats::daemons`].
///
/// This exists so ONE `/stats` request answers "which daemons are live, and how
/// stale is each" — the question you actually have when a provisioned instance
/// never shows up. `total_daemons` alone tells you a daemon is missing but not
/// WHICH, and `seconds_since_heartbeat` is what distinguishes "connected and
/// healthy" from "connected but about to be reaped by cleanup_stale".
///
/// Purely derived from `DaemonConnection` at call time — no new state is kept,
/// so this cannot drift from the registry.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub hostname: String,
    pub platform: String,
    pub arch: String,
    pub version: String,
    pub labels: std::collections::HashMap<String, String>,
    pub is_busy: bool,
    /// Seconds since this daemon connected.
    pub connected_secs: u64,
    /// Seconds since its last heartbeat. Compare against the heartbeat timeout
    /// (see `cleanup_stale`) to see how close it is to being dropped.
    pub seconds_since_heartbeat: u64,
}

#[derive(Debug, Clone)]
pub struct RegistryStats {
    /// Size of `daemons`, kept as a plain count: it is exposed to Python as
    /// `PyStats.total_daemons` and is the cheap answer to "how many".
    pub total_daemons: usize,
    pub by_platform: std::collections::HashMap<String, usize>,
    pub oldest_connection_secs: u64,
    /// Keyed by daemon id, so a caller that knows the id can look it up directly
    /// instead of scanning a list.
    pub daemons: std::collections::HashMap<String, DaemonInfo>,
}

impl Default for DaemonRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    fn create_test_metadata(hostname: &str, platform: &str) -> DaemonMetadata {
        DaemonMetadata {
            hostname: hostname.to_string(),
            platform: platform.to_string(),
            arch: "x86_64".to_string(),
            version: "0.1.0".to_string(),
            labels: HashMap::new(),
        }
    }

    fn create_test_metadata_with_labels(
        hostname: &str,
        platform: &str,
        labels: HashMap<String, String>,
    ) -> DaemonMetadata {
        DaemonMetadata {
            hostname: hostname.to_string(),
            platform: platform.to_string(),
            arch: "x86_64".to_string(),
            version: "0.1.0".to_string(),
            labels,
        }
    }

    #[test]
    fn test_registry_new() {
        let registry = DaemonRegistry::new();
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_register_single_daemon() {
        let registry = DaemonRegistry::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);

        registry.register(conn);

        assert_eq!(registry.count(), 1);
        assert!(registry.get("daemon-1").is_some());
    }

    #[test]
    fn test_register_multiple_daemons() {
        let registry = DaemonRegistry::new();

        for i in 0..5 {
            let (tx, _rx) = mpsc::unbounded_channel();
            let metadata = create_test_metadata(&format!("host{}", i), "linux");
            let conn = DaemonConnection::new(format!("daemon-{}", i), metadata, tx);
            registry.register(conn);
        }

        assert_eq!(registry.count(), 5);
    }

    #[test]
    fn test_register_replaces_existing() {
        let registry = DaemonRegistry::new();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();

        let metadata1 = create_test_metadata("host1", "linux");
        let conn1 = DaemonConnection::new("daemon-1".to_string(), metadata1, tx1);
        registry.register(conn1);

        assert_eq!(registry.count(), 1);

        // Register again with same ID
        let metadata2 = create_test_metadata("host2", "darwin");
        let conn2 = DaemonConnection::new("daemon-1".to_string(), metadata2, tx2);
        registry.register(conn2);

        // Should still be 1 daemon
        assert_eq!(registry.count(), 1);

        // Should have new metadata
        let conn = registry.get("daemon-1").unwrap();
        assert_eq!(conn.metadata.hostname, "host2");
    }

    #[test]
    fn test_get_existing_daemon() {
        let registry = DaemonRegistry::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);
        registry.register(conn);

        let retrieved = registry.get("daemon-1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, "daemon-1");
    }

    #[test]
    fn test_get_nonexistent_daemon() {
        let registry = DaemonRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_remove_daemon() {
        let registry = DaemonRegistry::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);
        registry.register(conn);

        assert_eq!(registry.count(), 1);

        registry.remove("daemon-1");

        assert_eq!(registry.count(), 0);
        assert!(registry.get("daemon-1").is_none());
    }

    #[test]
    fn test_remove_nonexistent_daemon() {
        let registry = DaemonRegistry::new();
        // Should not panic
        registry.remove("nonexistent");
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_list_all_no_filter() {
        let registry = DaemonRegistry::new();

        for i in 0..3 {
            let (tx, _rx) = mpsc::unbounded_channel();
            let metadata = create_test_metadata(&format!("host{}", i), "linux");
            let conn = DaemonConnection::new(format!("daemon-{}", i), metadata, tx);
            registry.register(conn);
        }

        let daemons = registry.list_all(None);
        assert_eq!(daemons.len(), 3);
    }

    #[test]
    fn test_list_all_with_label_filter() {
        let registry = DaemonRegistry::new();

        // Daemon with env=prod, region=us-west
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let mut labels1 = HashMap::new();
        labels1.insert("env".to_string(), "prod".to_string());
        labels1.insert("region".to_string(), "us-west".to_string());
        let metadata1 = create_test_metadata_with_labels("host1", "linux", labels1);
        let conn1 = DaemonConnection::new("daemon-1".to_string(), metadata1, tx1);
        registry.register(conn1);

        // Daemon with env=dev, region=us-east
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let mut labels2 = HashMap::new();
        labels2.insert("env".to_string(), "dev".to_string());
        labels2.insert("region".to_string(), "us-east".to_string());
        let metadata2 = create_test_metadata_with_labels("host2", "linux", labels2);
        let conn2 = DaemonConnection::new("daemon-2".to_string(), metadata2, tx2);
        registry.register(conn2);

        // Daemon with no labels
        let (tx3, _rx3) = mpsc::unbounded_channel();
        let metadata3 = create_test_metadata("host3", "linux");
        let conn3 = DaemonConnection::new("daemon-3".to_string(), metadata3, tx3);
        registry.register(conn3);

        // Filter by single label: env=prod
        let mut filter = HashMap::new();
        filter.insert("env".to_string(), "prod".to_string());
        let prod_daemons = registry.list_all(Some(&filter));
        assert_eq!(prod_daemons.len(), 1);
        assert_eq!(prod_daemons[0], "daemon-1");

        // Filter by single label: env=dev
        let mut filter = HashMap::new();
        filter.insert("env".to_string(), "dev".to_string());
        let dev_daemons = registry.list_all(Some(&filter));
        assert_eq!(dev_daemons.len(), 1);
        assert_eq!(dev_daemons[0], "daemon-2");

        // Filter by multiple labels: env=prod AND region=us-west (match)
        let mut filter = HashMap::new();
        filter.insert("env".to_string(), "prod".to_string());
        filter.insert("region".to_string(), "us-west".to_string());
        let multi_match = registry.list_all(Some(&filter));
        assert_eq!(multi_match.len(), 1);
        assert_eq!(multi_match[0], "daemon-1");

        // Filter by multiple labels: env=prod AND region=us-east (no match)
        let mut filter = HashMap::new();
        filter.insert("env".to_string(), "prod".to_string());
        filter.insert("region".to_string(), "us-east".to_string());
        let no_match = registry.list_all(Some(&filter));
        assert_eq!(no_match.len(), 0);

        // Filter by nonexistent label
        let mut filter = HashMap::new();
        filter.insert("env".to_string(), "staging".to_string());
        let none_daemons = registry.list_all(Some(&filter));
        assert_eq!(none_daemons.len(), 0);

        // Empty filter returns all
        let empty_filter = HashMap::new();
        let all_daemons = registry.list_all(Some(&empty_filter));
        assert_eq!(all_daemons.len(), 3);

        // No filter returns all
        let all_daemons = registry.list_all(None);
        assert_eq!(all_daemons.len(), 3);
    }

    #[test]
    fn test_get_stats_empty() {
        let registry = DaemonRegistry::new();
        let stats = registry.get_stats();

        assert_eq!(stats.total_daemons, 0);
        assert_eq!(stats.by_platform.len(), 0);
        assert_eq!(stats.oldest_connection_secs, 0);
    }

    #[test]
    fn test_get_stats_with_daemons() {
        let registry = DaemonRegistry::new();

        // Add Linux daemon
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let metadata1 = create_test_metadata("host1", "linux");
        let conn1 = DaemonConnection::new("daemon-1".to_string(), metadata1, tx1);
        registry.register(conn1);

        // Add another Linux daemon
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let metadata2 = create_test_metadata("host2", "linux");
        let conn2 = DaemonConnection::new("daemon-2".to_string(), metadata2, tx2);
        registry.register(conn2);

        // Add Darwin daemon
        let (tx3, _rx3) = mpsc::unbounded_channel();
        let metadata3 = create_test_metadata("host3", "darwin");
        let conn3 = DaemonConnection::new("daemon-3".to_string(), metadata3, tx3);
        registry.register(conn3);

        let stats = registry.get_stats();

        assert_eq!(stats.total_daemons, 3);
        assert_eq!(stats.by_platform.len(), 2);
        assert_eq!(stats.by_platform.get("linux"), Some(&2));
        assert_eq!(stats.by_platform.get("darwin"), Some(&1));
    }

    #[test]
    fn test_get_stats_daemons_detail() {
        let registry = DaemonRegistry::new();

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut labels = HashMap::new();
        labels.insert("pod".to_string(), "sandbox-1".to_string());
        let metadata = create_test_metadata_with_labels("host1", "linux", labels);
        registry.register(DaemonConnection::new("daemon-1".to_string(), metadata, tx));

        let stats = registry.get_stats();

        // total_daemons stays the size of the map, so a caller can trust either.
        assert_eq!(stats.total_daemons, stats.daemons.len());

        // Keyed by daemon id — the id is NOT repeated inside the value.
        let info = stats.daemons.get("daemon-1").expect("daemon-1 in stats");
        assert_eq!(info.hostname, "host1");
        assert_eq!(info.platform, "linux");
        assert_eq!(info.arch, "x86_64");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(
            info.labels.get("pod").map(String::as_str),
            Some("sandbox-1")
        );
        assert!(!info.is_busy);
        // Just registered, so it is fresh on both clocks.
        assert!(info.seconds_since_heartbeat <= 1);
        assert!(info.connected_secs <= 1);
    }

    #[tokio::test]
    async fn test_get_stats_daemons_reflects_reap() {
        let registry = DaemonRegistry::new();

        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let arc_conn =
            registry.register(DaemonConnection::new("daemon-1".to_string(), metadata, tx));

        // A daemon that has gone quiet still appears, with a large staleness — this
        // is the state that distinguishes "connected then wedged" from "never came up".
        arc_conn
            .last_heartbeat
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let stats = registry.get_stats();
        assert!(stats.daemons["daemon-1"].seconds_since_heartbeat > 1_000);

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert_eq!(registry.cleanup_stale(0), 1);

        // Once reaped it is gone from the map, and the count follows.
        let stats = registry.get_stats();
        assert!(!stats.daemons.contains_key("daemon-1"));
        assert_eq!(stats.total_daemons, 0);
        assert_eq!(stats.daemons.len(), 0);
    }

    #[test]
    fn test_cleanup_stale_none() {
        let registry = DaemonRegistry::new();

        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);
        registry.register(conn);

        // Cleanup with very long timeout - nothing should be removed
        let removed = registry.cleanup_stale(3600);
        assert_eq!(removed, 0);
        assert_eq!(registry.count(), 1);
    }

    #[tokio::test]
    async fn test_cleanup_stale_old_connection() {
        let registry = DaemonRegistry::new();

        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);
        let arc_conn = registry.register(conn);

        // Manually set old heartbeat
        arc_conn
            .last_heartbeat
            .store(0, std::sync::atomic::Ordering::Relaxed);

        // Wait a moment
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Cleanup with 0 second timeout - should remove
        let removed = registry.cleanup_stale(0);
        assert_eq!(removed, 1);
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_daemon_connection_heartbeat() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);

        let initial_heartbeat = conn
            .last_heartbeat
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(initial_heartbeat > 0);

        // Update heartbeat
        std::thread::sleep(std::time::Duration::from_millis(100));
        conn.update_heartbeat();

        let new_heartbeat = conn
            .last_heartbeat
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(new_heartbeat >= initial_heartbeat);
    }

    #[test]
    fn test_daemon_connection_seconds_since_heartbeat() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let metadata = create_test_metadata("host1", "linux");
        let conn = DaemonConnection::new("daemon-1".to_string(), metadata, tx);

        let since = conn.seconds_since_heartbeat();
        assert_eq!(since, 0);

        // Set old heartbeat
        conn.last_heartbeat
            .store(0, std::sync::atomic::Ordering::Relaxed);

        let since = conn.seconds_since_heartbeat();
        assert!(since > 0);
    }
}
