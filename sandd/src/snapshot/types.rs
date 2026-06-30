use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type SnapshotId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub created_at: u64,  // Unix timestamp in seconds
    pub tree: String,
    pub message: String,
    pub tags: Vec<String>,
    pub workspace: PathBuf,
    pub file_count: usize,
    pub total_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub id: SnapshotId,
    pub created_at: u64,  // Unix timestamp in seconds
    pub message: String,
    pub tags: Vec<String>,
    pub file_count: usize,
    pub total_size: u64,
}

impl From<Snapshot> for SnapshotInfo {
    fn from(snapshot: Snapshot) -> Self {
        Self {
            id: snapshot.id,
            created_at: snapshot.created_at,
            message: snapshot.message,
            tags: snapshot.tags,
            file_count: snapshot.file_count,
            total_size: snapshot.total_size,
        }
    }
}
