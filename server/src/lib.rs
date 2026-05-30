// Allow dead code and unused imports for MVP
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(non_local_definitions)]

mod protocol;
mod registry;
mod server;

use pyo3::prelude::*;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyValueError};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tracing_subscriber;
use uuid::Uuid;

use protocol::Message;
use registry::{CommandResult, DaemonRegistry};
use server::SandboxServer;

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
    #[pyo3(signature = (host="0.0.0.0".to_string(), port=8765))]
    fn new(host: String, port: u16) -> PyResult<Self> {
        // Initialize logging
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(tracing::Level::INFO.into()),
            )
            .try_init();

        let runtime = Runtime::new()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to create runtime: {}", e)))?;

        let bind_addr = format!("{}:{}", host, port);
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
    fn execute_command(
        &self,
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

        conn.register_command(request_id.clone(), tx);

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

        self.runtime.block_on(async {
            // Wait for result with timeout
            match tokio::time::timeout(Duration::from_secs(timeout + 5), rx).await {
                Ok(Ok(result)) => Ok(PyCommandResult {
                    stdout: result.stdout,
                    stderr: result.stderr,
                    exit_code: result.exit_code,
                    duration_ms: result.duration_ms,
                }),
                Ok(Err(_)) => Err(PyRuntimeError::new_err("Command channel closed")),
                Err(_) => Err(PyTimeoutError::new_err("Command execution timed out")),
            }
        })
    }

    /// Start an interactive shell session
    #[pyo3(signature = (daemon_id, rows=24, cols=80, term="xterm-256color".to_string()))]
    fn start_shell(
        &self,
        daemon_id: String,
        rows: u16,
        cols: u16,
        term: String,
    ) -> PyResult<ShellSession> {
        let conn = self
            .registry
            .get(&daemon_id)
            .ok_or_else(|| PyValueError::new_err(format!("Daemon {} not found", daemon_id)))?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        conn.register_shell_session(request_id.clone(), tx);

        let msg = Message::StartShell {
            request_id: request_id.clone(),
            rows,
            cols,
            term,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to start shell: {}", e)))?;

        Ok(ShellSession {
            session_id: request_id,
            daemon_id,
            registry: self.registry.clone(),
            runtime_handle: self.runtime.handle().clone(),
            output_rx: Arc::new(tokio::sync::Mutex::new(rx)),
        })
    }

    /// Upload a file to a daemon
    fn upload_file(
        &self,
        daemon_id: String,
        remote_path: String,
        data: Vec<u8>,
    ) -> PyResult<()> {
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

    /// List all connected daemons
    fn list_daemons(&self) -> PyResult<Vec<String>> {
        Ok(self.registry.list_all())
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
}

/// Shell session handle
#[pyclass]
pub struct ShellSession {
    session_id: String,
    daemon_id: String,
    registry: Arc<DaemonRegistry>,
    runtime_handle: tokio::runtime::Handle,
    output_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>,
}

#[pymethods]
impl ShellSession {
    /// Write data to the shell
    fn write(&self, data: Vec<u8>) -> PyResult<()> {
        let conn = self
            .registry
            .get(&self.daemon_id)
            .ok_or_else(|| PyRuntimeError::new_err("Daemon disconnected"))?;

        let msg = Message::ShellInput {
            request_id: self.session_id.clone(),
            data,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to write: {}", e)))
    }

    /// Read output from the shell (non-blocking)
    #[pyo3(signature = (timeout=1.0))]
    fn read(&self, timeout: f64) -> PyResult<Option<Vec<u8>>> {
        self.runtime_handle.block_on(async {
            let mut rx = self.output_rx.lock().await;
            match tokio::time::timeout(
                Duration::from_secs_f64(timeout),
                rx.recv()
            ).await {
                Ok(Some(data)) => Ok(Some(data)),
                Ok(None) => Ok(None),
                Err(_) => Ok(None), // Timeout
            }
        })
    }

    /// Resize the shell
    fn resize(&self, rows: u16, cols: u16) -> PyResult<()> {
        let conn = self
            .registry
            .get(&self.daemon_id)
            .ok_or_else(|| PyRuntimeError::new_err("Daemon disconnected"))?;

        let msg = Message::ShellResize {
            request_id: self.session_id.clone(),
            rows,
            cols,
        };

        conn.send_message(msg)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to resize: {}", e)))
    }

    /// Get session ID
    #[getter]
    fn session_id(&self) -> String {
        self.session_id.clone()
    }
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

/// Python module
#[pymodule]
fn _core(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<Server>()?;
    m.add_class::<ShellSession>()?;
    m.add_class::<PyCommandResult>()?;
    m.add_class::<PyStats>()?;
    Ok(())
}
