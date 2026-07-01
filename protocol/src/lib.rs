// Shared protocol between daemon and server

use serde::{Deserialize, Serialize};

/// Snapshot metadata (shared between daemon and server)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub created_at: u64, // Unix timestamp in seconds
    pub message: String,
    pub tags: Vec<String>,
    pub file_count: usize,
    pub total_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Register {
        daemon_id: String,
        metadata: DaemonMetadata,
    },
    RegisterAck {
        success: bool,
        message: String,
    },
    Heartbeat,
    Pong,
    ExecuteCommand {
        request_id: String,
        command: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    CommandOutput {
        request_id: String,
        stdout: String,
        stderr: String,
        exit_code: i32,
        duration_ms: u64,
    },
    CommandError {
        request_id: String,
        error: String,
    },
    NewSession {
        session_id: String,
        rows: u16,
        cols: u16,
        #[serde(default = "default_term")]
        term: String,
    },
    SessionStarted {
        session_id: String,
        success: bool,
        error: Option<String>,
    },
    SessionInput {
        session_id: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    SessionOutput {
        session_id: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    SessionResize {
        session_id: String,
        rows: u16,
        cols: u16,
    },
    SessionClose {
        session_id: String,
    },
    SessionExit {
        session_id: String,
        exit_code: i32,
    },
    FileUploadStart {
        request_id: String,
        path: String,
        total_size: u64,
        #[serde(default)]
        mode: Option<u32>,
    },
    FileUploadChunk {
        request_id: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
        offset: u64,
    },
    FileUploadComplete {
        request_id: String,
        success: bool,
        error: Option<String>,
    },
    FileDownloadStart {
        request_id: String,
        path: String,
    },
    FileDownloadChunk {
        request_id: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
        offset: u64,
        is_last: bool,
    },
    FileDownloadError {
        request_id: String,
        error: String,
    },
    // Snapshot operations
    CreateSnapshot {
        request_id: String,
        workspace: String,
        message: Option<String>,
        tags: Option<Vec<String>>,
    },
    SnapshotCreated {
        request_id: String,
        snapshot_id: String,
        file_count: usize,
        total_size: u64,
    },
    RestoreSnapshot {
        request_id: String,
        snapshot_id: String,
        destination: String,
    },
    SnapshotRestored {
        request_id: String,
        file_count: usize,
    },
    ListSnapshots {
        request_id: String,
        tags: Option<Vec<String>>,
    },
    SnapshotList {
        request_id: String,
        snapshots: Vec<SnapshotInfo>,
    },
    FindSnapshotByTag {
        request_id: String,
        tag: String,
    },
    GetSnapshot {
        request_id: String,
        snapshot_id: String,
    },
    SnapshotDetails {
        request_id: String,
        snapshot: Option<SnapshotInfo>,
    },
    DeleteSnapshot {
        request_id: String,
        snapshot_id: String,
    },
    SnapshotDeleted {
        request_id: String,
    },
    SnapshotError {
        request_id: String,
        error: String,
    },
    Error {
        message: String,
        #[serde(default)]
        recoverable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonMetadata {
    pub hostname: String,
    pub platform: String,
    pub arch: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
}

fn default_timeout() -> u64 {
    300
}

fn default_term() -> String {
    "xterm-256color".to_string()
}

mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(v);
        s.serialize_str(&base64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        use base64::Engine;
        let base64 = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(base64.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = Message::ExecuteCommand {
            request_id: "test-123".to_string(),
            command: "ls -la".to_string(),
            timeout_secs: 30,
            env: std::collections::HashMap::new(),
            cwd: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::ExecuteCommand { request_id, .. } => {
                assert_eq!(request_id, "test-123");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_register_message() {
        let mut labels = std::collections::HashMap::new();
        labels.insert("env".to_string(), "prod".to_string());

        let msg = Message::Register {
            daemon_id: "daemon-1".to_string(),
            metadata: DaemonMetadata {
                hostname: "test-host".to_string(),
                platform: "linux".to_string(),
                arch: "x86_64".to_string(),
                version: "0.1.0".to_string(),
                labels,
            },
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::Register {
                daemon_id,
                metadata,
            } => {
                assert_eq!(daemon_id, "daemon-1");
                assert_eq!(metadata.hostname, "test-host");
                assert_eq!(metadata.platform, "linux");
                assert_eq!(metadata.labels.get("env").unwrap(), "prod");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_heartbeat_message() {
        let msg = Message::Heartbeat;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::Heartbeat => {}
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_command_output_message() {
        let msg = Message::CommandOutput {
            request_id: "req-1".to_string(),
            stdout: "output".to_string(),
            stderr: "error".to_string(),
            exit_code: 0,
            duration_ms: 123,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::CommandOutput {
                request_id,
                stdout,
                stderr,
                exit_code,
                duration_ms,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(stdout, "output");
                assert_eq!(stderr, "error");
                assert_eq!(exit_code, 0);
                assert_eq!(duration_ms, 123);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_session_messages() {
        let start = Message::NewSession {
            session_id: "session-1".to_string(),
            rows: 24,
            cols: 80,
            term: "xterm-256color".to_string(),
        };

        let json = serde_json::to_string(&start).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::NewSession {
                session_id,
                rows,
                cols,
                term,
            } => {
                assert_eq!(session_id, "session-1");
                assert_eq!(rows, 24);
                assert_eq!(cols, 80);
                assert_eq!(term, "xterm-256color");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_session_input_with_binary_data() {
        let data = vec![0x01, 0x02, 0x03, 0xFF];
        let msg = Message::SessionInput {
            session_id: "session-1".to_string(),
            data: data.clone(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::SessionInput {
                data: parsed_data, ..
            } => {
                assert_eq!(parsed_data, data);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_file_upload_messages() {
        let start = Message::FileUploadStart {
            request_id: "upload-1".to_string(),
            path: "/tmp/test.txt".to_string(),
            total_size: 1024,
            mode: Some(0o644),
        };

        let json = serde_json::to_string(&start).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::FileUploadStart {
                request_id,
                path,
                total_size,
                mode,
            } => {
                assert_eq!(request_id, "upload-1");
                assert_eq!(path, "/tmp/test.txt");
                assert_eq!(total_size, 1024);
                assert_eq!(mode, Some(0o644));
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_file_upload_chunk() {
        let data = b"test file content".to_vec();
        let msg = Message::FileUploadChunk {
            request_id: "upload-1".to_string(),
            data: data.clone(),
            offset: 0,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::FileUploadChunk {
                data: parsed_data,
                offset,
                ..
            } => {
                assert_eq!(parsed_data, data);
                assert_eq!(offset, 0);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_error_message() {
        let msg = Message::Error {
            message: "Something went wrong".to_string(),
            recoverable: true,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::Error {
                message,
                recoverable,
            } => {
                assert_eq!(message, "Something went wrong");
                assert_eq!(recoverable, true);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_command_with_env_vars() {
        let mut env = std::collections::HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("HOME".to_string(), "/home/user".to_string());

        let msg = Message::ExecuteCommand {
            request_id: "cmd-1".to_string(),
            command: "echo $PATH".to_string(),
            timeout_secs: 60,
            env,
            cwd: Some("/tmp".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::ExecuteCommand { env, cwd, .. } => {
                assert_eq!(env.get("PATH"), Some(&"/usr/bin".to_string()));
                assert_eq!(env.get("HOME"), Some(&"/home/user".to_string()));
                assert_eq!(cwd, Some("/tmp".to_string()));
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_default_timeout() {
        let timeout = default_timeout();
        assert_eq!(timeout, 300);
    }

    #[test]
    fn test_default_term() {
        let term = default_term();
        assert_eq!(term, "xterm-256color");
    }

    #[test]
    fn test_daemon_metadata() {
        let mut labels = std::collections::HashMap::new();
        labels.insert("region".to_string(), "us-west".to_string());
        labels.insert("env".to_string(), "staging".to_string());

        let metadata = DaemonMetadata {
            hostname: "worker-01".to_string(),
            platform: "linux".to_string(),
            arch: "aarch64".to_string(),
            version: "1.0.0".to_string(),
            labels,
        };

        assert_eq!(metadata.hostname, "worker-01");
        assert_eq!(metadata.platform, "linux");
        assert_eq!(metadata.arch, "aarch64");
        assert_eq!(metadata.labels.len(), 2);
    }

    #[test]
    fn test_empty_labels() {
        let metadata = DaemonMetadata {
            hostname: "test".to_string(),
            platform: "darwin".to_string(),
            arch: "x86_64".to_string(),
            version: "0.1.0".to_string(),
            labels: std::collections::HashMap::new(),
        };

        assert!(metadata.labels.is_empty());
    }

    #[test]
    fn test_base64_encoding() {
        // Test that binary data is properly encoded
        let data = vec![0, 1, 2, 255, 254, 253];
        let msg = Message::SessionOutput {
            session_id: "test".to_string(),
            data: data.clone(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        // Should not contain raw binary, should be base64
        assert!(!json.contains("\0"));
        assert!(!json.contains("\u{00FF}"));

        let parsed: Message = serde_json::from_str(&json).unwrap();
        match parsed {
            Message::SessionOutput {
                data: parsed_data, ..
            } => {
                assert_eq!(parsed_data, data);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_session_resize() {
        let msg = Message::SessionResize {
            session_id: "session-1".to_string(),
            rows: 50,
            cols: 120,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::SessionResize { rows, cols, .. } => {
                assert_eq!(rows, 50);
                assert_eq!(cols, 120);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_file_download_error() {
        let msg = Message::FileDownloadError {
            request_id: "download-1".to_string(),
            error: "File not found".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();

        match parsed {
            Message::FileDownloadError { error, .. } => {
                assert_eq!(error, "File not found");
            }
            _ => panic!("Wrong message type"),
        }
    }
}
