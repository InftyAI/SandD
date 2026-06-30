use crate::snapshot::object_store::ObjectStore;
use crate::snapshot::tree::{get_mode, set_mode, set_mtime, EntryType, Tree, TreeEntry};
use crate::snapshot::types::{Snapshot, SnapshotId, SnapshotInfo};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::fs;
use uuid::Uuid;

pub struct SnapshotManager {
    store: ObjectStore,
    snapshots_dir: PathBuf,
    tags_dir: PathBuf,
}

impl SnapshotManager {
    pub fn new(root: PathBuf) -> Result<Self> {
        let store = ObjectStore::new(root.clone());
        let snapshots_dir = root.join("snapshots");
        let tags_dir = root.join("refs").join("tags");

        std::fs::create_dir_all(&snapshots_dir).with_context(|| {
            format!(
                "Failed to create snapshots directory: {}",
                snapshots_dir.display()
            )
        })?;

        std::fs::create_dir_all(&tags_dir)
            .with_context(|| format!("Failed to create tags directory: {}", tags_dir.display()))?;

        Ok(Self {
            store,
            snapshots_dir,
            tags_dir,
        })
    }

    /// Create a snapshot of workspace
    pub async fn create_snapshot(
        &self,
        workspace: &Path,
        message: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<SnapshotId> {
        let mut tags = tags.unwrap_or_default();

        // Deduplicate tags in input
        tags.sort();
        tags.dedup();

        // Validate tag names (security: prevent path traversal)
        for tag in &tags {
            Self::validate_tag_name(tag)?;
        }

        // Create snapshot metadata (store tags in snapshot file)
        let created_at = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("System time error: {}", e))?
            .as_secs();

        // Check tag existence upfront (still has TOCTOU, but add_tag uses atomic write)
        for tag in &tags {
            let tag_file = self.tags_dir.join(tag);
            if tag_file.exists() {
                anyhow::bail!("Tag '{}' already exists", tag);
            }
        }

        let snapshot_id = Uuid::new_v4().to_string();

        // Build tree recursively
        let (tree_hash, file_count, total_size) = self.build_tree(workspace).await?;

        let snapshot = Snapshot {
            id: snapshot_id.clone(),
            created_at,
            tree: tree_hash,
            message: message.unwrap_or_else(|| format!("Snapshot {}", snapshot_id)),
            tags: tags.clone(), // Store in snapshot for fast access
            workspace: workspace.to_path_buf(),
            file_count,
            total_size,
        };

        // Save snapshot
        let snapshot_file = self.snapshots_dir.join(format!("{}.json", snapshot_id));
        let json = serde_json::to_string_pretty(&snapshot)?;

        // Atomic write
        let temp_file = snapshot_file.with_extension("tmp");
        fs::write(&temp_file, json).await?;
        fs::rename(temp_file, snapshot_file).await?;

        // Create tag refs (Git-style: refs/tags/<tag> contains snapshot ID for O(1) filtering)
        for tag in &tags {
            self.add_tag(&snapshot_id, tag).await?;
        }

        Ok(snapshot_id)
    }

    /// Build tree recursively, return (tree_hash, file_count, total_size)
    fn build_tree<'a>(
        &'a self,
        dir: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(String, usize, u64)>> + 'a>>
    {
        Box::pin(async move {
            let mut entries = Vec::new();
            let mut file_count = 0usize;
            let mut total_size = 0u64;

            let mut read_dir = fs::read_dir(dir).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                let path = entry.path();
                // Use symlink_metadata to NOT follow symlinks
                let metadata = fs::symlink_metadata(&path).await?;
                let name = entry.file_name().to_string_lossy().to_string();

                let (entry_type, hash, size, _sub_count) = if metadata.is_symlink() {
                    // Check symlink FIRST (before is_file/is_dir which would follow the link)
                    // Store symlink target as blob object
                    let target = fs::read_link(&path).await?;
                    let target_bytes = target.to_string_lossy().as_bytes().to_vec();
                    let hash = self.store.put_blob(&target_bytes).await?;
                    file_count += 1;

                    // Determine symlink type (for Windows symlink restoration)
                    let target_is_dir = if target.is_absolute() {
                        target.is_dir()
                    } else {
                        // Relative path - resolve relative to parent
                        path.parent()
                            .map(|p| p.join(&target).is_dir())
                            .unwrap_or(false)
                    };

                    let symlink_type = if target_is_dir {
                        EntryType::SymlinkDir
                    } else {
                        EntryType::Symlink
                    };

                    (symlink_type, hash, 0, 0)
                } else if metadata.is_file() {
                    // Store file as blob object
                    let hash = self.store.put_file(&path).await?;
                    let size = metadata.len();
                    total_size += size;
                    file_count += 1;
                    (EntryType::Blob, hash, size, 0)
                } else if metadata.is_dir() {
                    // Recursively build tree object for subdirectory
                    let (hash, sub_count, sub_size) = self.build_tree(&path).await?;
                    total_size += sub_size;
                    file_count += sub_count;
                    (EntryType::Tree, hash, 0, sub_count)
                } else {
                    // Skip unsupported file types (pipes, sockets, devices, etc.)
                    tracing::warn!(
                        "Skipping unsupported file type: {} (not a regular file, directory, or symlink)",
                        path.display()
                    );
                    continue;
                };

                entries.push(TreeEntry {
                    name,
                    mode: get_mode(&metadata),
                    entry_type,
                    hash,
                    size,
                    modified: metadata.modified()?,
                });
            }

            // Create and store tree object (JSON)
            let tree = Tree { entries };
            let tree_json = serde_json::to_vec(&tree)?;
            let tree_hash = self.store.put_blob(&tree_json).await?;

            Ok((tree_hash, file_count, total_size))
        })
    }

    /// Restore snapshot to destination
    pub async fn restore_snapshot(&self, snapshot_id: &str, dest: &Path) -> Result<()> {
        // Load snapshot
        let snapshot_file = self.snapshots_dir.join(format!("{}.json", snapshot_id));
        let json = fs::read_to_string(snapshot_file)
            .await
            .with_context(|| format!("Snapshot {} not found", snapshot_id))?;
        let snapshot: Snapshot = serde_json::from_str(&json)?;

        // Restore tree recursively
        self.restore_tree(&snapshot.tree, dest).await?;

        Ok(())
    }

    /// Restore tree recursively (always clean - deletes extras after successful restore)
    fn restore_tree<'a>(
        &'a self,
        tree_hash: &'a str,
        dest: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            fs::create_dir_all(dest).await?;

            // Load tree object - tells us what SHOULD exist
            let tree_json = self.store.get_blob(tree_hash).await?;
            let tree: Tree = serde_json::from_slice(&tree_json)?;

            // Build set of expected names in this directory (owned strings to avoid borrow issues)
            let expected_names: std::collections::HashSet<String> = tree
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect();

            // Phase 1: Restore each entry from snapshot
            // Do this FIRST - if restore fails, extras remain untouched (safer)
            for entry in tree.entries {
                let entry_path = dest.join(&entry.name);

                match entry.entry_type {
                    EntryType::Blob => {
                        // Check if file already exists with same content
                        let should_copy = if entry_path.exists() {
                            // Compare metadata first (fast check)
                            if let Ok(metadata) = fs::metadata(&entry_path).await {
                                if metadata.len() == entry.size
                                    && metadata.modified().ok() == Some(entry.modified)
                                {
                                    // Size and mtime match - likely unchanged, skip copy
                                    false
                                } else {
                                    // Metadata differs - need to verify with hash
                                    let file_hash = self.store.put_file(&entry_path).await?;
                                    file_hash != entry.hash
                                }
                            } else {
                                true
                            }
                        } else {
                            true
                        };

                        if should_copy {
                            // Restore file from blob object
                            self.store.copy_file(&entry.hash, &entry_path).await?;
                        }

                        // Always update metadata (cheap operation)
                        set_mode(&entry_path, entry.mode)?;
                        set_mtime(&entry_path, entry.modified)?;
                    }
                    EntryType::Tree => {
                        // Recursively restore subdirectory from tree object
                        self.restore_tree(&entry.hash, &entry_path).await?;
                    }
                    EntryType::Symlink | EntryType::SymlinkDir => {
                        // Restore symlink from blob object (target path)
                        let target_bytes = self.store.get_blob(&entry.hash).await?;
                        let target = PathBuf::from(String::from_utf8(target_bytes)?);

                        // Remove existing file/symlink if present (for idempotent restore)
                        if entry_path.exists() || entry_path.is_symlink() {
                            // Use remove_file for both files and symlinks
                            let _ = fs::remove_file(&entry_path).await;
                        }

                        #[cfg(unix)]
                        tokio::fs::symlink(target, &entry_path).await?;

                        #[cfg(windows)]
                        {
                            // Use EntryType to determine symlink type (stored at snapshot time)
                            // (can't check target.is_dir() since target may not exist yet or be relative)
                            if entry.entry_type == EntryType::SymlinkDir {
                                tokio::fs::symlink_dir(target, &entry_path).await?;
                            } else {
                                tokio::fs::symlink_file(target, &entry_path).await?;
                            }
                        }
                    }
                }
            }

            // Phase 2: Clean this directory - delete extras (only after successful restore)
            // Cleanup failures are warned but don't fail the operation
            let mut read_dir = fs::read_dir(dest).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                let name = entry.file_name();
                let name_str = name.to_string_lossy().to_string();

                if !expected_names.contains(&name_str) {
                    let path = entry.path();

                    // Not in snapshot - delete it
                    if path.is_dir() {
                        if let Err(e) = fs::remove_dir_all(&path).await {
                            tracing::warn!("Failed to delete directory {}: {}", path.display(), e);
                        }
                    } else {
                        if let Err(e) = fs::remove_file(&path).await {
                            tracing::warn!("Failed to delete file {}: {}", path.display(), e);
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// List all snapshots (optionally filtered by tags)
    pub async fn list_snapshots(&self, tags: Option<Vec<String>>) -> Result<Vec<SnapshotInfo>> {
        let snapshot_ids = if let Some(tags) = tags {
            // Validate tag names (security)
            for tag in &tags {
                Self::validate_tag_name(tag)?;
            }

            // Fast path: O(k) tag lookup where k = number of filter tags
            let mut ids = Vec::new();
            for tag in tags {
                // Error if tag doesn't exist (don't silently skip)
                match self.get_snapshot_by_tag(&tag).await? {
                    Some(id) => ids.push(id),
                    None => anyhow::bail!("Tag '{}' does not exist", tag),
                }
            }
            ids
        } else {
            // No filter: load all snapshots
            let mut ids = Vec::new();
            let mut entries = fs::read_dir(&self.snapshots_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Some(stem) = path.file_stem() {
                        ids.push(stem.to_string_lossy().to_string());
                    }
                }
            }
            ids
        };

        // Load snapshot metadata
        let mut snapshots = Vec::new();
        for id in snapshot_ids {
            // Propagate errors (don't silently ignore corrupted snapshot files)
            let snapshot = self.get_snapshot(&id).await?;
            snapshots.push(snapshot.into());
        }

        // Sort by creation time (newest first), then deduplicate by ID
        // Note: when filtering by tags, multiple tags may point to same snapshot
        snapshots.sort_by(|a: &SnapshotInfo, b: &SnapshotInfo| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        // Deduplicate by ID (keep first occurrence = newest due to sort)
        snapshots.dedup_by(|a, b| a.id == b.id);

        Ok(snapshots)
    }

    /// Find snapshot by tag (O(1) lookup via tag ref)
    /// Returns single snapshot since tags are immutable
    pub async fn find_snapshot_by_tag(&self, tag: &str) -> Result<Option<SnapshotInfo>> {
        // Validate tag name (security)
        Self::validate_tag_name(tag)?;

        if let Some(id) = self.get_snapshot_by_tag(tag).await? {
            let snapshot = self.get_snapshot(&id).await?;
            Ok(Some(snapshot.into()))
        } else {
            Ok(None)
        }
    }

    /// Get snapshot by ID
    pub async fn get_snapshot(&self, id: &str) -> Result<SnapshotInfo> {
        let snapshot_file = self.snapshots_dir.join(format!("{}.json", id));
        let json = fs::read_to_string(snapshot_file)
            .await
            .with_context(|| format!("Snapshot {} not found", id))?;
        let snapshot: Snapshot = serde_json::from_str(&json)?;
        Ok(snapshot.into())
    }

    /// Delete snapshot and its tag refs
    pub async fn delete_snapshot(&self, id: &str) -> Result<()> {
        // Read snapshot to get tags (O(1) - no scanning!)
        let snapshot = self.get_snapshot(id).await?;

        // Remove tag refs
        for tag in &snapshot.tags {
            self.remove_tag(tag)
                .await
                .with_context(|| format!("Failed to remove tag ref '{}'", tag))?;
        }

        // Remove snapshot file
        let snapshot_file = self.snapshots_dir.join(format!("{}.json", id));
        fs::remove_file(snapshot_file)
            .await
            .with_context(|| format!("Failed to delete snapshot {}", id))?;
        Ok(())
    }

    /// Validate tag name (prevent path traversal and invalid names)
    fn validate_tag_name(tag: &str) -> Result<()> {
        // Reject empty tags
        if tag.is_empty() {
            anyhow::bail!("Tag name cannot be empty");
        }

        // Reject absolute paths
        if std::path::Path::new(tag).is_absolute() {
            anyhow::bail!("Tag '{}' is an absolute path", tag);
        }

        // Reject path traversal (../, ..\, etc.)
        if tag.contains("..") {
            anyhow::bail!("Tag '{}' contains path traversal sequence", tag);
        }

        // Reject path separators (prevent subdirectories)
        if tag.contains('/') || tag.contains('\\') {
            anyhow::bail!("Tag '{}' contains path separators", tag);
        }

        // Reject special names
        if tag == "." || tag == ".." {
            anyhow::bail!("Tag '{}' is a reserved name", tag);
        }

        // Reject control characters and other dangerous chars
        if tag.chars().any(|c| c.is_control() || c == '\0') {
            anyhow::bail!("Tag '{}' contains invalid characters", tag);
        }

        Ok(())
    }

    /// Add a tag to a snapshot (creates tag ref)
    /// Assumes tag doesn't exist and is validated (checked by caller)
    async fn add_tag(&self, snapshot_id: &str, tag: &str) -> Result<()> {
        let tag_file = self.tags_dir.join(tag);

        // Atomic write with temp file + rename
        let temp_file = tag_file.with_extension("tmp");
        fs::write(&temp_file, snapshot_id).await?;
        fs::rename(temp_file, tag_file).await?;

        Ok(())
    }

    /// Remove a tag (delete tag ref file)
    async fn remove_tag(&self, tag: &str) -> Result<()> {
        // Note: tag is from snapshot.tags which was already validated at creation
        let tag_file = self.tags_dir.join(tag);
        if tag_file.exists() {
            fs::remove_file(tag_file).await?;
        }
        Ok(())
    }

    /// Get snapshot ID for a tag (O(1) - just read tag file)
    /// Returns single snapshot ID since tags are immutable (one tag → one snapshot)
    async fn get_snapshot_by_tag(&self, tag: &str) -> Result<Option<String>> {
        let tag_file = self.tags_dir.join(tag);

        if !tag_file.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&tag_file).await?;
        Ok(Some(content.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_and_restore_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        // Create test workspace
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file1.txt"), "Hello")
            .await
            .unwrap();
        fs::create_dir_all(workspace.join("subdir")).await.unwrap();
        fs::write(workspace.join("subdir/file2.txt"), "World")
            .await
            .unwrap();

        // Create snapshot
        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Test snapshot".to_string()), None)
            .await
            .unwrap();

        // Restore snapshot
        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        // Verify restored files
        let content1 = fs::read_to_string(restore_dir.join("file1.txt"))
            .await
            .unwrap();
        assert_eq!(content1, "Hello");

        let content2 = fs::read_to_string(restore_dir.join("subdir/file2.txt"))
            .await
            .unwrap();
        assert_eq!(content2, "World");
    }

    #[tokio::test]
    async fn test_timestamp_preservation() {
        use std::time::Duration;

        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        // Create test file
        fs::create_dir_all(&workspace).await.unwrap();
        let test_file = workspace.join("test.txt");
        fs::write(&test_file, "content").await.unwrap();

        // Get original timestamp
        let original_metadata = fs::metadata(&test_file).await.unwrap();
        let original_mtime = original_metadata.modified().unwrap();

        // Wait a bit to ensure timestamps would differ
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create snapshot
        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Test".to_string()), None)
            .await
            .unwrap();

        // Wait again
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Restore snapshot
        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        // Check restored file has original timestamp
        let restored_metadata = fs::metadata(restore_dir.join("test.txt")).await.unwrap();
        let restored_mtime = restored_metadata.modified().unwrap();

        // Timestamps should match (within 1 second for filesystem precision)
        let diff = if restored_mtime > original_mtime {
            restored_mtime.duration_since(original_mtime).unwrap()
        } else {
            original_mtime.duration_since(restored_mtime).unwrap()
        };

        assert!(
            diff < Duration::from_secs(1),
            "Timestamp should be preserved. Original: {:?}, Restored: {:?}",
            original_mtime,
            restored_mtime
        );
    }

    #[tokio::test]
    async fn test_list_snapshots() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("test.txt"), "content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();

        // Create multiple snapshots
        let _id1 = manager
            .create_snapshot(
                &workspace,
                Some("First".to_string()),
                Some(vec!["tag1".to_string()]),
            )
            .await
            .unwrap();

        // sleep for a while to ensure different timestamps
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let _id2 = manager
            .create_snapshot(
                &workspace,
                Some("Second".to_string()),
                Some(vec!["tag2".to_string()]),
            )
            .await
            .unwrap();

        // List all snapshots
        let snapshots = manager.list_snapshots(None).await.unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].message, "Second"); // Newest first
        assert_eq!(snapshots[1].message, "First");

        // Filter by tag
        let tag1_snapshots = manager
            .list_snapshots(Some(vec!["tag1".to_string()]))
            .await
            .unwrap();
        assert_eq!(tag1_snapshots.len(), 1);
        assert_eq!(tag1_snapshots[0].message, "First");

        // Find by tag (returns single snapshot since tags are immutable)
        let tag2_snapshot = manager.find_snapshot_by_tag("tag2").await.unwrap();
        assert!(tag2_snapshot.is_some());
        assert_eq!(tag2_snapshot.unwrap().message, "Second");
    }

    #[tokio::test]
    async fn test_binary_files() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        fs::create_dir_all(&workspace).await.unwrap();
        let binary_data = vec![0x00, 0xFF, 0xAB, 0xCD, 0x12, 0x34];
        fs::write(workspace.join("binary.dat"), &binary_data)
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Binary test".to_string()), None)
            .await
            .unwrap();

        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        let restored = fs::read(restore_dir.join("binary.dat")).await.unwrap();
        assert_eq!(restored, binary_data);
    }

    #[tokio::test]
    async fn test_empty_directories() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::create_dir_all(workspace.join("empty")).await.unwrap();
        fs::create_dir_all(workspace.join("nested/empty/dirs"))
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Empty dirs test".to_string()), None)
            .await
            .unwrap();

        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        assert!(restore_dir.join("empty").is_dir());
        assert!(restore_dir.join("nested/empty/dirs").is_dir());
    }

    #[tokio::test]
    async fn test_deduplication() {
        use std::time::Duration;

        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();

        let manager = SnapshotManager::new(store_dir.clone()).unwrap();

        // Create first file with content
        let content = "Same content in multiple files";
        fs::write(workspace.join("file1.txt"), content)
            .await
            .unwrap();

        // Create first snapshot
        manager
            .create_snapshot(&workspace, Some("First".to_string()), None)
            .await
            .unwrap();

        // Get blob creation time
        let objects_dir = store_dir.join("objects");
        let mut first_blob_path = None;
        for entry in walkdir::WalkDir::new(&objects_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let data = std::fs::read(entry.path()).unwrap();
                if data == content.as_bytes() {
                    first_blob_path = Some(entry.path().to_path_buf());
                    break;
                }
            }
        }

        let first_blob_path = first_blob_path.expect("Should find content blob");
        let first_created = std::fs::metadata(&first_blob_path)
            .unwrap()
            .modified()
            .unwrap();

        // Wait to ensure timestamp would differ if recreated
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Add more files with same content
        fs::write(workspace.join("file2.txt"), content)
            .await
            .unwrap();
        fs::write(workspace.join("file3.txt"), content)
            .await
            .unwrap();

        // Create second snapshot
        manager
            .create_snapshot(&workspace, Some("Second".to_string()), None)
            .await
            .unwrap();

        // Verify blob wasn't recreated (timestamp unchanged)
        let second_created = std::fs::metadata(&first_blob_path)
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(
            first_created, second_created,
            "Blob should not be recreated - timestamp should be unchanged"
        );

        // Count unique content blobs
        let mut content_blob_count = 0;
        for entry in walkdir::WalkDir::new(&objects_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let data = std::fs::read(entry.path()).unwrap();
                if data == content.as_bytes() {
                    content_blob_count += 1;
                }
            }
        }

        assert_eq!(
            content_blob_count, 1,
            "Content should be stored exactly once"
        );
    }

    #[tokio::test]
    async fn test_special_filenames() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file with spaces.txt"), "Spaces")
            .await
            .unwrap();
        fs::write(workspace.join("file-with-dashes.txt"), "Dashes")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Special names".to_string()), None)
            .await
            .unwrap();

        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(restore_dir.join("file with spaces.txt"))
                .await
                .unwrap(),
            "Spaces"
        );
        assert_eq!(
            fs::read_to_string(restore_dir.join("file-with-dashes.txt"))
                .await
                .unwrap(),
            "Dashes"
        );
    }

    #[tokio::test]
    async fn test_restore_skip_unchanged() {
        use std::time::Duration;

        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir.clone()).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Test".to_string()), None)
            .await
            .unwrap();

        // First restore
        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        // Get file timestamp after first restore
        let first_timestamp = std::fs::metadata(restore_dir.join("file.txt"))
            .unwrap()
            .modified()
            .unwrap();

        // Wait to ensure timestamp would differ if file was rewritten
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Second restore to same location
        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        // File should NOT be rewritten (timestamp unchanged)
        let second_timestamp = std::fs::metadata(restore_dir.join("file.txt"))
            .unwrap()
            .modified()
            .unwrap();

        // Timestamps should match (file was not recopied)
        assert_eq!(
            first_timestamp, second_timestamp,
            "File should not be recopied if unchanged"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_symlinks() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("target.txt"), "Target")
            .await
            .unwrap();
        symlink(workspace.join("target.txt"), workspace.join("link.txt")).unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Symlink test".to_string()), None)
            .await
            .unwrap();

        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        let link_target = fs::read_link(restore_dir.join("link.txt")).await.unwrap();
        assert!(link_target.to_string_lossy().contains("target.txt"));
        assert_eq!(
            fs::read_to_string(restore_dir.join("target.txt"))
                .await
                .unwrap(),
            "Target"
        );
    }

    #[tokio::test]
    async fn test_get_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "Content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(
                &workspace,
                Some("Test message".to_string()),
                Some(vec!["tag1".to_string(), "tag2".to_string()]),
            )
            .await
            .unwrap();

        // Get snapshot by ID
        let snapshot = manager.get_snapshot(&snapshot_id).await.unwrap();

        assert_eq!(snapshot.id, snapshot_id);
        assert_eq!(snapshot.message, "Test message");
        assert_eq!(snapshot.tags, vec!["tag1", "tag2"]);
        assert_eq!(snapshot.file_count, 1);

        // Try getting non-existent snapshot
        let result = manager.get_snapshot("non-existent-id").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "Content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();

        // Create two snapshots
        let snap1_id = manager
            .create_snapshot(&workspace, Some("Snapshot 1".to_string()), None)
            .await
            .unwrap();

        let snap2_id = manager
            .create_snapshot(&workspace, Some("Snapshot 2".to_string()), None)
            .await
            .unwrap();

        // List should have 2 snapshots
        let snapshots = manager.list_snapshots(None).await.unwrap();
        assert_eq!(snapshots.len(), 2);

        // Delete first snapshot
        manager.delete_snapshot(&snap1_id).await.unwrap();

        // List should now have 1 snapshot
        let snapshots = manager.list_snapshots(None).await.unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, snap2_id);

        // Getting deleted snapshot should fail
        let result = manager.get_snapshot(&snap1_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tag_immutability() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "Content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();

        // Create first snapshot with tag
        let _snap1 = manager
            .create_snapshot(
                &workspace,
                Some("First".to_string()),
                Some(vec!["v1.0.0".to_string()]),
            )
            .await
            .unwrap();

        // Try to create second snapshot with same tag - should fail
        let result = manager
            .create_snapshot(
                &workspace,
                Some("Second".to_string()),
                Some(vec!["v1.0.0".to_string()]),
            )
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_delete_snapshot_with_tags() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "Content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir.clone()).unwrap();

        // Create snapshot with tags
        let snap_id = manager
            .create_snapshot(
                &workspace,
                Some("Test".to_string()),
                Some(vec!["v1.0.0".to_string(), "stable".to_string()]),
            )
            .await
            .unwrap();

        // Verify tag files exist
        let tag_file1 = store_dir.join("refs/tags/v1.0.0");
        let tag_file2 = store_dir.join("refs/tags/stable");
        assert!(tag_file1.exists());
        assert!(tag_file2.exists());

        // Delete snapshot
        manager.delete_snapshot(&snap_id).await.unwrap();

        // Tag files should be deleted
        assert!(!tag_file1.exists());
        assert!(!tag_file2.exists());

        // Can now reuse the tags
        let result = manager
            .create_snapshot(
                &workspace,
                Some("New".to_string()),
                Some(vec!["v1.0.0".to_string()]),
            )
            .await;
        assert!(result.is_ok());

        // Deleting non-existent snapshot should fail
        let result = manager.delete_snapshot("non-existent-id").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_snapshots_deduplication() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "Content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();

        // Create snapshot with multiple tags
        let _snap_id = manager
            .create_snapshot(
                &workspace,
                Some("Test".to_string()),
                Some(vec![
                    "v1.0.0".to_string(),
                    "stable".to_string(),
                    "latest".to_string(),
                ]),
            )
            .await
            .unwrap();

        // Filter by multiple tags pointing to same snapshot
        let snapshots = manager
            .list_snapshots(Some(vec![
                "v1.0.0".to_string(),
                "stable".to_string(),
                "latest".to_string(),
            ]))
            .await
            .unwrap();

        // Should return only 1 snapshot (deduplicated)
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].message, "Test");
    }

    #[test]
    fn test_validate_tag_name() {
        // Valid tags
        let valid = vec!["v1.0.0", "stable", "release-2024", "tag_name", "TAG123"];
        for tag in valid {
            assert!(
                SnapshotManager::validate_tag_name(tag).is_ok(),
                "Should accept valid tag: {}",
                tag
            );
        }

        // Invalid tags
        let invalid = vec![
            ("../etc/passwd", "path traversal"),
            ("..\\windows\\system32", "Windows path traversal"),
            ("/etc/passwd", "absolute path (Unix)"),
            ("C:\\windows\\system32", "absolute path (Windows)"),
            ("subdir/tag", "path separator (/)"),
            ("subdir\\tag", "path separator (\\)"),
            (".", "special name (.)"),
            ("..", "special name (..)"),
            ("", "empty string"),
            ("tag\0null", "null byte"),
            ("tag\nline", "control char (newline)"),
        ];

        for (tag, reason) in invalid {
            assert!(
                SnapshotManager::validate_tag_name(tag).is_err(),
                "Should reject {}: {}",
                reason,
                tag
            );
        }
    }

    #[tokio::test]
    async fn test_duplicate_tags_in_input() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");

        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file.txt"), "Content")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();

        // Create snapshot with duplicate tags in input
        let result = manager
            .create_snapshot(
                &workspace,
                Some("Test".to_string()),
                Some(vec![
                    "v1.0.0".to_string(),
                    "v1.0.0".to_string(), // Duplicate
                    "stable".to_string(),
                    "v1.0.0".to_string(), // Another duplicate
                ]),
            )
            .await;

        // Should succeed (duplicates removed)
        assert!(result.is_ok());

        let snap_id = result.unwrap();
        let snapshot = manager.get_snapshot(&snap_id).await.unwrap();

        // Should have deduplicated tags
        assert_eq!(snapshot.tags, vec!["stable", "v1.0.0"]);
    }

    #[tokio::test]
    async fn test_list_nonexistent_tag() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");

        let manager = SnapshotManager::new(store_dir).unwrap();

        // Try to list with non-existent tag
        let result = manager
            .list_snapshots(Some(vec!["nonexistent".to_string()]))
            .await;

        // Should error instead of returning empty list
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_restore_always_clean() {
        let temp_dir = TempDir::new().unwrap();
        let store_dir = temp_dir.path().join("store");
        let workspace = temp_dir.path().join("workspace");
        let restore_dir = temp_dir.path().join("restored");

        // Create snapshot with specific files
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("file1.txt"), "content1")
            .await
            .unwrap();
        fs::create_dir_all(workspace.join("dir1")).await.unwrap();
        fs::write(workspace.join("dir1/file2.txt"), "content2")
            .await
            .unwrap();

        let manager = SnapshotManager::new(store_dir).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Clean test".to_string()), None)
            .await
            .unwrap();

        // Restore to directory with extra files
        fs::create_dir_all(&restore_dir).await.unwrap();
        fs::write(restore_dir.join("extra_file.txt"), "should be deleted")
            .await
            .unwrap();
        fs::create_dir_all(restore_dir.join("extra_dir"))
            .await
            .unwrap();
        fs::write(restore_dir.join("extra_dir/nested.txt"), "also deleted")
            .await
            .unwrap();
        fs::create_dir_all(restore_dir.join("dir1")).await.unwrap();
        fs::write(restore_dir.join("dir1/extra_in_dir.txt"), "delete me")
            .await
            .unwrap();

        // Restore snapshot (should clean extras)
        manager
            .restore_snapshot(&snapshot_id, &restore_dir)
            .await
            .unwrap();

        // Verify exact match - only snapshot files exist
        assert!(restore_dir.join("file1.txt").exists());
        assert!(restore_dir.join("dir1/file2.txt").exists());

        // Verify extras are deleted
        assert!(!restore_dir.join("extra_file.txt").exists());
        assert!(!restore_dir.join("extra_dir").exists());
        assert!(!restore_dir.join("dir1/extra_in_dir.txt").exists());

        // Verify content is correct
        let content1 = fs::read_to_string(restore_dir.join("file1.txt"))
            .await
            .unwrap();
        assert_eq!(content1, "content1");
        let content2 = fs::read_to_string(restore_dir.join("dir1/file2.txt"))
            .await
            .unwrap();
        assert_eq!(content2, "content2");
    }
}
