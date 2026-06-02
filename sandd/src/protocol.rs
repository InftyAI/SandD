// Re-export the protocol from server crate for consistency
// In production, you'd want a shared protocol crate

use serde::{Deserialize, Serialize};

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
    StartSession {
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
