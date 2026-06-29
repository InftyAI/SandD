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
}

impl SnapshotManager {
    pub fn new(root: PathBuf) -> Result<Self> {
        let store = ObjectStore::new(root.clone());
        let snapshots_dir = root.join("snapshots");

        std::fs::create_dir_all(&snapshots_dir).with_context(|| {
            format!(
                "Failed to create snapshots directory: {}",
                snapshots_dir.display()
            )
        })?;

        Ok(Self {
            store,
            snapshots_dir,
        })
    }

    /// Create a snapshot of workspace
    pub async fn create_snapshot(
        &self,
        workspace: &Path,
        message: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<SnapshotId> {
        let snapshot_id = Uuid::new_v4().to_string();

        // Build tree recursively
        let (tree_hash, file_count, total_size) = self.build_tree(workspace).await?;

        // Create snapshot metadata
        let snapshot = Snapshot {
            id: snapshot_id.clone(),
            created_at: SystemTime::now(),
            tree: tree_hash,
            message: message.unwrap_or_else(|| format!("Snapshot {}", snapshot_id)),
            tags: tags.unwrap_or_default(),
            workspace_path: workspace.to_path_buf(),
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
                let metadata = entry.metadata().await?;
                let name = entry.file_name().to_string_lossy().to_string();

                let (entry_type, hash, size, _sub_count) = if metadata.is_file() {
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
                } else if metadata.is_symlink() {
                    // Store symlink target as blob object
                    let target = fs::read_link(&path).await?;
                    let target_bytes = target.to_string_lossy().as_bytes().to_vec();
                    let hash = self.store.put_blob(&target_bytes).await?;
                    file_count += 1;
                    (EntryType::Symlink, hash, 0, 0)
                } else {
                    continue; // Skip other types
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

    /// Restore tree recursively
    fn restore_tree<'a>(
        &'a self,
        tree_hash: &'a str,
        dest: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            fs::create_dir_all(dest).await?;

            // Load tree object
            let tree_json = self.store.get_blob(tree_hash).await?;
            let tree: Tree = serde_json::from_slice(&tree_json)?;

            // Restore each entry
            for entry in tree.entries {
                let entry_path = dest.join(&entry.name);

                match entry.entry_type {
                    EntryType::Blob => {
                        // Check if file already exists with same content
                        let should_copy = if entry_path.exists() {
                            // Compare metadata first (fast check)
                            if let Ok(metadata) = fs::metadata(&entry_path).await {
                                if metadata.len() == entry.size
                                    && metadata.modified().ok() == Some(entry.modified) {
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
                    EntryType::Symlink => {
                        // Restore symlink from blob object (target path)
                        let target_bytes = self.store.get_blob(&entry.hash).await?;
                        let target = PathBuf::from(String::from_utf8(target_bytes)?);

                        #[cfg(unix)]
                        tokio::fs::symlink(target, entry_path).await?;

                        #[cfg(windows)]
                        {
                            if target.is_dir() {
                                tokio::fs::symlink_dir(target, entry_path).await?;
                            } else {
                                tokio::fs::symlink_file(target, entry_path).await?;
                            }
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// List all snapshots (optionally filtered by tags)
    pub async fn list_snapshots(
        &self,
        filter_tags: Option<Vec<String>>,
    ) -> Result<Vec<SnapshotInfo>> {
        let mut snapshots = Vec::new();

        let mut entries = fs::read_dir(&self.snapshots_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let json = fs::read_to_string(&path).await?;
            let snapshot: Snapshot = serde_json::from_str(&json)?;

            // Filter by tags if specified
            if let Some(ref filter) = filter_tags {
                if !filter.iter().any(|tag| snapshot.tags.contains(tag)) {
                    continue;
                }
            }

            snapshots.push(snapshot.into());
        }

        // Sort by creation time (newest first)
        snapshots.sort_by(|a: &SnapshotInfo, b: &SnapshotInfo| b.created_at.cmp(&a.created_at));

        Ok(snapshots)
    }

    /// Find snapshots by tag
    pub async fn find_by_tag(&self, tag: &str) -> Result<Vec<SnapshotInfo>> {
        self.list_snapshots(Some(vec![tag.to_string()])).await
    }

    /// Get snapshot by ID
    pub async fn get_snapshot(&self, id: &str) -> Result<Snapshot> {
        let snapshot_file = self.snapshots_dir.join(format!("{}.json", id));
        let json = fs::read_to_string(snapshot_file)
            .await
            .with_context(|| format!("Snapshot {} not found", id))?;
        let snapshot: Snapshot = serde_json::from_str(&json)?;
        Ok(snapshot)
    }

    /// Delete snapshot
    /// TODO: This will remove the snapshot metadata file, but the underlying objects in the object
    /// store will remain.
    pub async fn delete_snapshot(&self, id: &str) -> Result<()> {
        let snapshot_file = self.snapshots_dir.join(format!("{}.json", id));
        fs::remove_file(snapshot_file)
            .await
            .with_context(|| format!("Failed to delete snapshot {}", id))?;
        Ok(())
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
            .create_snapshot(&workspace, Some("First".to_string()), Some(vec!["tag1".to_string()]))
            .await
            .unwrap();

        let _id2 = manager
            .create_snapshot(&workspace, Some("Second".to_string()), Some(vec!["tag2".to_string()]))
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

        // Find by tag
        let tag2_snapshots = manager.find_by_tag("tag2").await.unwrap();
        assert_eq!(tag2_snapshots.len(), 1);
        assert_eq!(tag2_snapshots[0].message, "Second");
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
        fs::write(workspace.join("file1.txt"), content).await.unwrap();

        // Create first snapshot
        manager
            .create_snapshot(&workspace, Some("First".to_string()), None)
            .await
            .unwrap();

        // Get blob creation time
        let objects_dir = store_dir.join("objects");
        let mut first_blob_path = None;
        for entry in walkdir::WalkDir::new(&objects_dir).into_iter().filter_map(|e| e.ok()) {
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
        fs::write(workspace.join("file2.txt"), content).await.unwrap();
        fs::write(workspace.join("file3.txt"), content).await.unwrap();

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
        for entry in walkdir::WalkDir::new(&objects_dir).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                let data = std::fs::read(entry.path()).unwrap();
                if data == content.as_bytes() {
                    content_blob_count += 1;
                }
            }
        }

        assert_eq!(content_blob_count, 1, "Content should be stored exactly once");
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
        fs::write(workspace.join("file.txt"), "content").await.unwrap();

        let manager = SnapshotManager::new(store_dir.clone()).unwrap();
        let snapshot_id = manager
            .create_snapshot(&workspace, Some("Test".to_string()), None)
            .await
            .unwrap();

        // First restore
        manager.restore_snapshot(&snapshot_id, &restore_dir).await.unwrap();

        // Get file timestamp after first restore
        let first_timestamp = std::fs::metadata(restore_dir.join("file.txt"))
            .unwrap()
            .modified()
            .unwrap();

        // Wait to ensure timestamp would differ if file was rewritten
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Second restore to same location
        manager.restore_snapshot(&snapshot_id, &restore_dir).await.unwrap();

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
        assert_eq!(snapshot.workspace_path, workspace);

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

        // Deleting non-existent snapshot should fail
        let result = manager.delete_snapshot("non-existent-id").await;
        assert!(result.is_err());
    }
}
