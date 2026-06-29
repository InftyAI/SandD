use serde::{Deserialize, Serialize};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,            // File/directory name (not full path)
    pub mode: u32,               // Unix permissions (e.g., 0o755)
    pub entry_type: EntryType,
    pub hash: String,            // Content hash (BLAKE3)
    pub size: u64,               // Size in bytes
    #[serde(with = "system_time_format")]
    pub modified: SystemTime,    // Last modified time
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    Blob,        // File content (blob object)
    Tree,        // Subdirectory (tree object)
    Symlink,     // Symlink to file (blob object storing target path)
    SymlinkDir,  // Symlink to directory (blob object storing target path)
}

// Helper module for SystemTime serialization
mod system_time_format {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::time::{SystemTime, UNIX_EPOCH};

    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let duration = time
            .duration_since(UNIX_EPOCH)
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_u64(duration.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }
}

#[cfg(unix)]
pub fn get_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
pub fn get_mode(_metadata: &std::fs::Metadata) -> u32 {
    0o644 // Default for non-Unix systems
}

#[cfg(unix)]
pub fn set_mode(path: &std::path::Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn set_mode(_path: &std::path::Path, _mode: u32) -> anyhow::Result<()> {
    Ok(()) // No-op on non-Unix systems
}

pub fn set_mtime(path: &std::path::Path, mtime: SystemTime) -> anyhow::Result<()> {
    use filetime::{FileTime, set_file_mtime};

    let filetime = FileTime::from_system_time(mtime);
    set_file_mtime(path, filetime)?;
    Ok(())
}
