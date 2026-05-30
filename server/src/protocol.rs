use serde::{Deserialize, Serialize};

/// Protocol messages exchanged between agent and daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    // Connection management
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

    // Command execution (simple mode)
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

    // Interactive shell (PTY mode)
    StartShell {
        request_id: String,
        rows: u16,
        cols: u16,
        #[serde(default = "default_term")]
        term: String,
    },
    ShellStarted {
        request_id: String,
        success: bool,
        error: Option<String>,
    },
    ShellInput {
        request_id: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    ShellOutput {
        request_id: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    ShellResize {
        request_id: String,
        rows: u16,
        cols: u16,
    },
    ShellExit {
        request_id: String,
        exit_code: i32,
    },

    // File transfer
    FileUploadStart {
        request_id: String,
        path: String,
        total_size: u64,
        #[serde(default)]
        mode: Option<u32>, // Unix file permissions
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

    // Error handling
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
    300 // 5 minutes
}

fn default_term() -> String {
    "xterm-256color".to_string()
}

// Base64 encoding for binary data in JSON
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
}
