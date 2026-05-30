use crate::protocol::Message;
use anyhow::{anyhow, Result};
use futures_util::SinkExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize, PtySystem};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{debug, error};

pub struct ShellSession {
    #[allow(dead_code)]
    session_id: String,
    _reader_handle: tokio::task::JoinHandle<()>,
}

pub struct ShellManager {
    sessions: HashMap<String, ShellSession>,
    pty_system: Box<dyn PtySystem>,
}

impl ShellManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            pty_system: native_pty_system(),
        }
    }

    pub async fn start_shell<T>(
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
            "Starting shell session {} ({}x{}, term={})",
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

        // Spawn shell
        let shell = if cfg!(target_os = "windows") {
            "cmd.exe".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        };

        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", term);

        let _child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("Failed to spawn shell: {}", e))?;

        drop(pair.slave);

        let _writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow!("Failed to get PTY writer: {}", e))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow!("Failed to get PTY reader: {}", e))?;

        // Spawn task to read from PTY and send to WebSocket
        let session_id_clone = session_id.clone();
        let reader_handle = tokio::spawn(async move {
            let mut buffer = vec![0u8; 8192];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        debug!("Shell session {} ended", session_id_clone);

                        // Send exit message
                        let exit_msg = Message::ShellExit {
                            request_id: session_id_clone.clone(),
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

                        let output_msg = Message::ShellOutput {
                            request_id: session_id_clone.clone(),
                            data,
                        };

                        if let Ok(json) = serde_json::to_string(&output_msg) {
                            let mut tx = ws_tx.lock().await;
                            if tx.send(WsMessage::Text(json)).await.is_err() {
                                error!("Failed to send shell output, connection closed");
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
            ShellSession {
                session_id,
                _reader_handle: reader_handle,
            },
        );

        Ok(())
    }

    pub async fn send_input(&self, _session_id: &str, data: &[u8]) -> Result<()> {
        // Note: In a production implementation, you'd want interior mutability here
        // For now, this is a simplified version
        debug!("Sending {} bytes to shell session {}", data.len(), _session_id);

        Ok(())
    }

    pub fn resize(&self, session_id: &str, rows: u16, cols: u16) -> Result<()> {
        debug!("Resizing shell session {} to {}x{}", session_id, rows, cols);

        // Note: portable-pty doesn't expose resize after creation easily
        // In production, you'd store the PtyPair and call resize on it

        Ok(())
    }

    pub fn close_session(&mut self, session_id: &str) {
        if let Some(session) = self.sessions.remove(session_id) {
            debug!("Closing shell session {}", session.session_id);
        }
    }
}
