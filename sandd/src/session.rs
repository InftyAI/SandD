use crate::protocol::Message;
use anyhow::{anyhow, Result};
use futures_util::SinkExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize, PtySystem};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{debug, error};

pub struct SessionHandle {
    session_id: String,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    _reader_handle: tokio::task::JoinHandle<()>,
}

pub struct SessionManager {
    sessions: HashMap<String, SessionHandle>,
    pty_system: Box<dyn PtySystem>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            pty_system: native_pty_system(),
        }
    }

    pub async fn start_session<T>(
        &mut self,
        session_id: String,
        rows: u16,
        cols: u16,
        term: &str,
        ws_tx: Arc<Mutex<T>>,
    ) -> Result<()>
    where
        T: SinkExt<WsMessage> + Unpin + Send + 'static,
        T::Error: std::error::Error + Send + Sync + 'static,
    {
        debug!(
            "Starting session {} ({}x{}, term={})",
            session_id, rows, cols, term
        );

        let pty_size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = self
            .pty_system
            .openpty(pty_size)
            .map_err(|e| anyhow!("Failed to open PTY: {}", e))?;

        // Spawn session - try bash first, fall back to $SHELL, then /bin/sh
        let session = if cfg!(target_os = "windows") {
            "cmd.exe".to_string()
        } else {
            // Prefer bash (most compatible), then user's shell, then POSIX sh
            if std::path::Path::new("/bin/bash").exists() {
                "/bin/bash".to_string()
            } else {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
            }
        };

        let mut cmd = CommandBuilder::new(&session);
        cmd.env("TERM", term);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow!("Failed to get PTY reader: {}", e))?;

        // Take the writer before spawning
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow!("Failed to get PTY writer: {}", e))?;

        let _child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("Failed to spawn session: {}", e))?;

        // Store writer and master separately
        let writer = Arc::new(Mutex::new(writer));
        let master = Arc::new(Mutex::new(pair.master));

        // Spawn task to read from PTY and send to WebSocket
        let session_id_clone = session_id.clone();
        let reader_handle = tokio::spawn(async move {
            let mut buffer = vec![0u8; 8192];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        debug!("Session {} ended", session_id_clone);

                        // Send exit message
                        let exit_msg = Message::SessionExit {
                            session_id: session_id_clone.clone(),
                            exit_code: 0,
                        };

                        if let Ok(json) = serde_json::to_string(&exit_msg) {
                            let mut tx = ws_tx.lock().await;
                            let _ = tx.send(WsMessage::Text(json)).await;
                        }

                        break;
                    }
                    Ok(n) => {
                        let data = buffer[..n].to_vec();

                        let output_msg = Message::SessionOutput {
                            session_id: session_id_clone.clone(),
                            data,
                        };

                        if let Ok(json) = serde_json::to_string(&output_msg) {
                            let mut tx = ws_tx.lock().await;
                            if tx.send(WsMessage::Text(json)).await.is_err() {
                                error!("Failed to send session output, connection closed");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Error reading from PTY: {}", e);
                        break;
                    }
                }
            }
        });

        self.sessions.insert(
            session_id.clone(),
            SessionHandle {
                session_id,
                writer,
                master,
                _reader_handle: reader_handle,
            },
        );

        Ok(())
    }

    pub async fn send_input(&self, session_id: &str, data: &[u8]) -> Result<()> {
        debug!("Sending {} bytes to session {}", data.len(), session_id);

        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("Session not found: {}", session_id))?;

        let mut writer = session.writer.lock().await;

        writer
            .write_all(data)
            .map_err(|e| anyhow!("Failed to write to PTY: {}", e))?;

        writer
            .flush()
            .map_err(|e| anyhow!("Failed to flush PTY writer: {}", e))?;

        Ok(())
    }

    pub async fn resize(&self, session_id: &str, rows: u16, cols: u16) -> Result<()> {
        debug!("Resizing session {} to {}x{}", session_id, rows, cols);

        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("Session not found: {}", session_id))?;

        let master = session.master.lock().await;
        let new_size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        master
            .resize(new_size)
            .map_err(|e| anyhow!("Failed to resize PTY: {}", e))?;

        Ok(())
    }

    pub fn close_session(&mut self, session_id: &str) {
        if let Some(session) = self.sessions.remove(session_id) {
            debug!("Closing session {}", session.session_id);
        }
    }
}
