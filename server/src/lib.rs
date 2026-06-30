// Allow dead code and unused imports for MVP
#![allow(dead_code)]
#![allow(non_local_definitions)]

mod protocol;
mod registry;
mod server;

use anyhow::Context;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tracing_subscriber;
use uuid::Uuid;

use protocol::Message;
use registry::DaemonRegistry;
use server::SandboxServer;

/// Tunnel configuration
#[pyclass]
#[derive(Clone)]
pub struct TunnelConfig {
    #[pyo3(get, set)]
    pub authkey: String,
    #[pyo3(get, set)]
    pub server: String,
}

#[pymethods]
impl TunnelConfig {
    #[new]
    fn new(authkey: String, server: String) -> Self {
        Self { authkey, server }
    }

    fn __repr__(&self) -> String {
        format!("TunnelConfig(server={})", self.server)
    }
}

/// Python wrapper for the Rust server
#[pyclass]
pub struct Server {
    runtime: Runtime,
    registry: Arc<DaemonRegistry>,
    _server_handle: Option<tokio::task::JoinHandle<()>>,
}

#[pymethods]
impl Server {
    #[new]
    #[pyo3(signature = (
        host="0.0.0.0".to_string(),
        port=8765,
        verbose=true,
        connect="direct".to_string(),
        tunnel_config=None
    ))]
    fn new(
        py: Python,
        host: String,
        port: u16,
        verbose: bool,
        connect: String,
        tunnel_config: Option<Py<TunnelConfig>>,
    ) -> PyResult<Self> {
        // Validate connect parameter
        if connect != "direct" && connect != "tunnel" {
            return Err(PyValueError::new_err(format!(
                "connect must be 'direct' or 'tunnel', got '{}'",
                connect
            )));
        }

        // Validate tunnel parameters
        if connect == "tunnel" && tunnel_config.is_none() {
            return Err(PyValueError::new_err(
                "tunnel mode requires tunnel_config parameter",
            ));
        }

        // Initialize logging: INFO by default, unless verbose=False
        // RUST_LOG env var can override (e.g., RUST_LOG=debug)
        if verbose {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::Level::INFO.into()),
                )
                .try_init();
        }

        let runtime = Runtime::new()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to create runtime: {}", e)))?;

        // Handle tunnel mode
        let bind_addr = if connect == "tunnel" {
            let config_py = tunnel_config.unwrap();
            let config = config_py.borrow(py).clone();

            // Setup tunnel
            runtime.block_on(async {
                setup_tunnel_controller(&config)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("Tunnel setup failed: {}", e)))
            })?;

            // Get mesh IP (for logging only)
            let mesh_ip = runtime.block_on(async {
                get_mesh_ip()
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to get mesh IP: {}", e)))
            })?;

            tracing::info!(
                "Controller mesh IP: {} (binding to 0.0.0.0:{})",
                mesh_ip,
                port
            );

            // Bind to 0.0.0.0 instead of mesh IP
            // Tailscale will route traffic to this port through the mesh
            format!("0.0.0.0:{}", port)
        } else {
            format!("{}:{}", host, port)
        };

        let server = SandboxServer::new(bind_addr);
        let registry = server.registry();

        // Start server in background
        let server_handle = runtime.spawn(async move {
            if let Err(e) = server.start().await {
                eprintln!("Server error: {}", e);
            }
        });

        // Give server time to start
        std::thread::sleep(Duration::from_millis(100));

        Ok(Self {
            runtime,
            registry,
            _server_handle: Some(server_handle),
        })
    }

    /// Execute a command on a daemon
    #[pyo3(signature = (daemon_id, command, timeout=300, env=None, cwd=None))]
    fn exec(
        &self,
        py: Python,
        daemon_id: String,
        command: String,
        timeout: u64,
        env: Option<HashMap<String, String>>,
        cwd: Option<String>,
    ) -> PyResult<PyCommandResult> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        // Send command to daemon
        let msg = Message::ExecuteCommand {
            request_id: request_id.clone(),
            command,
            timeout_secs: timeout,
            env: env.unwrap_or_default(),
            cwd,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send command: {}", e)))?;

        // Release GIL while waiting for result to allow Python thread concurrency
        // Re-acquire GIL to return result or raise timeout error
        py.allow_threads(|| {
            self.runtime.block_on(async {
                // Wait for result with timeout
                match tokio::time::timeout(Duration::from_secs(timeout), rx).await {
                    Ok(Ok(Message::CommandOutput {
                        stdout,
                        stderr,
                        exit_code,
                        duration_ms,
                        ..
                    })) => Ok(PyCommandResult {
                        stdout,
                        stderr,
                        exit_code,
                        duration_ms,
                    }),
                    Ok(Ok(Message::CommandError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("Command error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("Command channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("Command execution timed out")),
                }
            })
        })
    }

    /// Create a new interactive session
    #[pyo3(signature = (daemon_id, rows=24, cols=80, term="xterm-256color".to_string()))]
    fn new_session(
        &self,
        daemon_id: String,
        rows: u16,
        cols: u16,
        term: String,
    ) -> PyResult<Session> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let session_id = Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let msg = Message::NewSession {
            session_id: session_id.clone(),
            rows,
            cols,
            term,
        };

        conn.register_session(session_id.clone(), tx);

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to start session: {}", e)))?;

        Ok(Session {
            session_id,
            daemon_id,
            registry: self.registry.clone(),
            runtime_handle: self.runtime.handle().clone(),
            output_rx: Arc::new(tokio::sync::Mutex::new(rx)),
        })
    }

    /// Upload a file to a daemon
    fn upload_file(&self, daemon_id: String, remote_path: String, data: Vec<u8>) -> PyResult<()> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        const CHUNK_SIZE: usize = 64 * 1024; // 64KB chunks

        self.runtime.block_on(async {
            // Send start message
            let start_msg = Message::FileUploadStart {
                request_id: request_id.clone(),
                path: remote_path,
                total_size: data.len() as u64,
                mode: None,
            };
            conn.send_message(start_msg)
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to start upload: {}", e)))?;

            // Send chunks
            for (offset, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
                let chunk_msg = Message::FileUploadChunk {
                    request_id: request_id.clone(),
                    data: chunk.to_vec(),
                    offset: (offset * CHUNK_SIZE) as u64,
                };
                conn.send_message(chunk_msg)
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to send chunk: {}", e)))?;
            }

            Ok(())
        })
    }

    /// Download a file from a daemon
    fn download_file(&self, daemon_id: String, remote_path: String) -> PyResult<Vec<u8>> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();

        self.runtime.block_on(async {
            conn.start_file_transfer(request_id.clone(), remote_path.clone(), 0);

            let msg = Message::FileDownloadStart {
                request_id: request_id.clone(),
                path: remote_path,
            };

            conn.send_message(msg)
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to start download: {}", e)))?;

            // Wait for transfer to complete (with timeout)
            tokio::time::sleep(Duration::from_secs(5)).await;

            conn.complete_file_transfer(&request_id)
                .ok_or_else(|| PyRuntimeError::new_err("File transfer did not complete"))
        })
    }

    /// List all connected daemons, optionally filtered by labels
    #[pyo3(signature = (labels=None))]
    fn list_daemons(&self, labels: Option<HashMap<String, String>>) -> PyResult<Vec<PyDaemonInfo>> {
        let daemon_ids = self.registry.list_all(labels.as_ref());
        let mut result = Vec::with_capacity(daemon_ids.len());

        for daemon_id in daemon_ids {
            if let Some(conn) = self.registry.get(&daemon_id) {
                result.push(PyDaemonInfo {
                    id: conn.id.clone(),
                    version: conn.metadata.version.clone(),
                    labels: conn.metadata.labels.clone(),
                    is_busy: conn.is_busy(),
                });
            }
        }

        Ok(result)
    }

    /// Get daemon count
    fn daemon_count(&self) -> PyResult<usize> {
        Ok(self.registry.count())
    }

    /// Get server statistics
    fn get_stats(&self) -> PyResult<PyStats> {
        let stats = self.registry.get_stats();
        Ok(PyStats {
            total_daemons: stats.total_daemons,
            by_platform: stats.by_platform,
            oldest_connection_secs: stats.oldest_connection_secs,
        })
    }

    /// Get daemon by ID (returns None if not found)
    fn get_daemon(&self, daemon_id: String) -> PyResult<Option<PyDaemonInfo>> {
        Ok(self.registry.get(&daemon_id).map(|conn| PyDaemonInfo {
            id: conn.id.clone(),
            version: conn.metadata.version.clone(),
            labels: conn.metadata.labels.clone(),
            is_busy: conn.is_busy(),
        }))
    }

    /// Create snapshot on daemon
    #[pyo3(signature = (daemon_id, workspace, message=None, tags=None))]
    fn create_snapshot(
        &self,
        py: Python,
        daemon_id: String,
        workspace: String,
        message: Option<String>,
        tags: Option<Vec<String>>,
    ) -> PyResult<String> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        let msg = Message::CreateSnapshot {
            request_id: request_id.clone(),
            workspace,
            message,
            tags,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send snapshot request: {}", e)))?;

        py.allow_threads(|| {
            self.runtime.block_on(async {
                match tokio::time::timeout(Duration::from_secs(300), rx).await {
                    Ok(Ok(Message::SnapshotCreated { snapshot_id, .. })) => Ok(snapshot_id),
                    Ok(Ok(Message::SnapshotError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("Snapshot error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("Snapshot channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("Snapshot creation timed out")),
                }
            })
        })
    }

    /// Restore snapshot on daemon
    fn restore_snapshot(
        &self,
        py: Python,
        daemon_id: String,
        snapshot_id: String,
        destination: String,
    ) -> PyResult<usize> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        let msg = Message::RestoreSnapshot {
            request_id: request_id.clone(),
            snapshot_id,
            destination,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send restore request: {}", e)))?;

        py.allow_threads(|| {
            self.runtime.block_on(async {
                match tokio::time::timeout(Duration::from_secs(300), rx).await {
                    Ok(Ok(Message::SnapshotRestored { file_count, .. })) => Ok(file_count),
                    Ok(Ok(Message::SnapshotError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("Restore error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("Restore channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("Restore timed out")),
                }
            })
        })
    }

    /// List snapshots on daemon
    #[pyo3(signature = (daemon_id, tags=None))]
    fn list_snapshots(
        &self,
        py: Python,
        daemon_id: String,
        tags: Option<Vec<String>>,
    ) -> PyResult<Vec<PyObject>> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        let msg = Message::ListSnapshots {
            request_id: request_id.clone(),
            tags,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send list request: {}", e)))?;

        py.allow_threads(|| {
            self.runtime.block_on(async {
                match tokio::time::timeout(Duration::from_secs(60), rx).await {
                    Ok(Ok(Message::SnapshotList { snapshots, .. })) => {
                        Python::with_gil(|py| {
                            snapshots.into_iter()
                                .map(|s| pythonize::pythonize(py, &s).map_err(|e| PyRuntimeError::new_err(e.to_string())))
                                .collect()
                        })
                    }
                    Ok(Ok(Message::SnapshotError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("List error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("List channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("List timed out")),
                }
            })
        })
    }

    /// Find snapshot by tag
    fn find_snapshot_by_tag(
        &self,
        py: Python,
        daemon_id: String,
        tag: String,
    ) -> PyResult<Option<PyObject>> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        let msg = Message::FindSnapshotByTag {
            request_id: request_id.clone(),
            tag,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send find request: {}", e)))?;

        py.allow_threads(|| {
            self.runtime.block_on(async {
                match tokio::time::timeout(Duration::from_secs(60), rx).await {
                    Ok(Ok(Message::SnapshotDetails { snapshot: None, .. })) => Ok(None),
                    Ok(Ok(Message::SnapshotDetails { snapshot: Some(snapshot), .. })) => {
                        Python::with_gil(|py| {
                            pythonize::pythonize(py, &snapshot)
                                .map(Some)
                                .map_err(|e| PyRuntimeError::new_err(e.to_string()))
                        })
                    }
                    Ok(Ok(Message::SnapshotError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("Find error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("Find channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("Find timed out")),
                }
            })
        })
    }

    /// Get snapshot details (returns None if not found)
    fn get_snapshot(
        &self,
        py: Python,
        daemon_id: String,
        snapshot_id: String,
    ) -> PyResult<Option<PyObject>> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        let msg = Message::GetSnapshot {
            request_id: request_id.clone(),
            snapshot_id,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send get request: {}", e)))?;

        py.allow_threads(|| {
            self.runtime.block_on(async {
                match tokio::time::timeout(Duration::from_secs(60), rx).await {
                    Ok(Ok(Message::SnapshotDetails { snapshot: Some(snapshot), .. })) => {
                        Python::with_gil(|py| {
                            pythonize::pythonize(py, &snapshot)
                                .map(Some)
                                .map_err(|e| PyRuntimeError::new_err(e.to_string()))
                        })
                    }
                    Ok(Ok(Message::SnapshotDetails { snapshot: None, .. })) => {
                        Ok(None)
                    }
                    Ok(Ok(Message::SnapshotError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("Get error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("Get channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("Get timed out")),
                }
            })
        })
    }

    /// Delete snapshot
    fn delete_snapshot(
        &self,
        py: Python,
        daemon_id: String,
        snapshot_id: String,
    ) -> PyResult<()> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        conn.register_request(request_id.clone(), tx);

        let msg = Message::DeleteSnapshot {
            request_id: request_id.clone(),
            snapshot_id,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to send delete request: {}", e)))?;

        py.allow_threads(|| {
            self.runtime.block_on(async {
                match tokio::time::timeout(Duration::from_secs(60), rx).await {
                    Ok(Ok(Message::SnapshotDeleted { .. })) => Ok(()),
                    Ok(Ok(Message::SnapshotError { error, .. })) => {
                        Err(PyRuntimeError::new_err(format!("Delete error: {}", error)))
                    }
                    Ok(Ok(_)) => Err(PyRuntimeError::new_err("Unexpected response type")),
                    Ok(Err(_)) => Err(PyRuntimeError::new_err("Delete channel closed")),
                    Err(_) => Err(PyTimeoutError::new_err("Delete timed out")),
                }
            })
        })
    }
}

/// Session handle
#[pyclass(name = "Session")]
pub struct Session {
    session_id: String,
    daemon_id: String,
    registry: Arc<DaemonRegistry>,
    runtime_handle: tokio::runtime::Handle,
    output_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>,
}

#[pymethods]
impl Session {
    /// Write data to the session
    fn write(&self, data: Vec<u8>) -> PyResult<()> {
        let conn = self
            .registry
            .get(&self.daemon_id)
            .ok_or_else(|| PyRuntimeError::new_err("Daemon disconnected"))?;

        let msg = Message::SessionInput {
            session_id: self.session_id.clone(),
            data,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to write: {}", e)))
    }

    /// Read output from the session (non-blocking)
    #[pyo3(signature = (timeout=1.0))]
    fn read(&self, timeout: f64) -> PyResult<Option<Py<PyBytes>>> {
        self.runtime_handle.block_on(async {
            let mut rx = self.output_rx.lock().await;
            match tokio::time::timeout(Duration::from_secs_f64(timeout), rx.recv()).await {
                Ok(Some(data)) => Python::with_gil(|py| Ok(Some(PyBytes::new(py, &data).into()))),
                Ok(None) => Ok(None),
                Err(_) => Ok(None), // Timeout
            }
        })
    }

    /// Resize the session
    fn resize(&self, rows: u16, cols: u16) -> PyResult<()> {
        let conn = self
            .registry
            .get(&self.daemon_id)
            .ok_or_else(|| PyRuntimeError::new_err("Daemon disconnected"))?;

        let msg = Message::SessionResize {
            session_id: self.session_id.clone(),
            rows,
            cols,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to resize: {}", e)))
    }

    /// Close the session
    fn close(&self) -> PyResult<()> {
        let conn = self
            .registry
            .get(&self.daemon_id)
            .ok_or_else(|| PyRuntimeError::new_err("Daemon disconnected"))?;

        let msg = Message::SessionClose {
            session_id: self.session_id.clone(),
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to close session: {}", e)))
    }

    /// Get session ID
    #[getter]
    fn session_id(&self) -> String {
        self.session_id.clone()
    }
}

/// Daemon information
#[pyclass]
#[derive(Clone)]
pub struct PyDaemonInfo {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub version: String,
    #[pyo3(get)]
    pub labels: HashMap<String, String>,
    #[pyo3(get)]
    pub is_busy: bool,
}

/// Command execution result
#[pyclass]
#[derive(Clone)]
pub struct PyCommandResult {
    #[pyo3(get)]
    pub stdout: String,
    #[pyo3(get)]
    pub stderr: String,
    #[pyo3(get)]
    pub exit_code: i32,
    #[pyo3(get)]
    pub duration_ms: u64,
}

#[pymethods]
impl PyCommandResult {
    fn __repr__(&self) -> String {
        format!(
            "CommandResult(exit_code={}, duration_ms={}, stdout={} bytes, stderr={} bytes)",
            self.exit_code,
            self.duration_ms,
            self.stdout.len(),
            self.stderr.len()
        )
    }
}

/// Server statistics
#[pyclass]
#[derive(Clone)]
pub struct PyStats {
    #[pyo3(get)]
    pub total_daemons: usize,
    #[pyo3(get)]
    pub by_platform: HashMap<String, usize>,
    #[pyo3(get)]
    pub oldest_connection_secs: u64,
}

/// Setup tunnel for controller
async fn setup_tunnel_controller(config: &TunnelConfig) -> anyhow::Result<()> {
    use std::process::Command;

    // Check if tailscale is installed by trying to run it
    let tailscale_check = Command::new("tailscale").arg("version").output();

    if tailscale_check.is_err() {
        return Err(anyhow::anyhow!(
            "Tailscale not found. Install it first:\n  \
            curl -fsSL https://tailscale.com/install.sh | sh"
        ));
    }

    tracing::info!("Starting tailscaled...");

    // Start tailscaled in background (if not already running)
    let _tailscaled = Command::new("tailscaled")
        .arg("--tun=userspace-networking")
        .arg("--state=/var/lib/tailscale/tailscaled.state")
        .spawn()
        .context("Failed to start tailscaled")?;

    // Give tailscaled time to start
    tokio::time::sleep(Duration::from_secs(2)).await;

    tracing::info!("Joining mesh network...");

    // Join mesh
    let output = Command::new("tailscale")
        .arg("up")
        .arg(format!("--authkey={}", config.authkey))
        .arg(format!("--login-server={}", config.server))
        .arg("--accept-routes")
        .output()?;

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
                    tracing::info!("✓ Controller joined mesh network with IP: {}", ip);
                    return Ok(());
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Err(anyhow::anyhow!("Timeout waiting for mesh IP assignment"))
}

/// Get mesh IP address
async fn get_mesh_ip() -> anyhow::Result<String> {
    use std::process::Command;

    let output = Command::new("tailscale").arg("ip").arg("-4").output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("Failed to get mesh IP"));
    }

    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        return Err(anyhow::anyhow!("No mesh IP assigned"));
    }

    Ok(ip)
}

/// Python module
#[pymodule]
fn _core(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<Server>()?;
    m.add_class::<Session>()?;
    m.add_class::<TunnelConfig>()?;
    m.add_class::<PyCommandResult>()?;
    m.add_class::<PyDaemonInfo>()?;
    m.add_class::<PyStats>()?;
    Ok(())
}
