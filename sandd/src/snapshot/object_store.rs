use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncReadExt;

pub struct ObjectStore {
    root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Store a blob (arbitrary bytes), return its hash
    pub async fn put_blob(&self, content: &[u8]) -> Result<String> {
        let hash = blake3::hash(content);
        let hash_hex = hash.to_hex().to_string();

        // Check if already exists (deduplication!)
        let object_path = self.hash_to_path(&hash_hex)?;
        if object_path.exists() {
            return Ok(hash_hex);
        }

        // Store object: objects/ab/cdef123...
        fs::create_dir_all(object_path.parent().unwrap()).await?;

        // Atomic write (temp + rename, like Git)
        let temp_path = object_path.with_extension("tmp");
        fs::write(&temp_path, content).await?;
        fs::rename(temp_path, object_path).await?;

        Ok(hash_hex)
    }

    /// Store a file by path, return its hash
    /// Uses streaming to handle large files without loading entire file into memory
    pub async fn put_file(&self, path: &Path) -> Result<String> {
        // Stream file in chunks to compute hash
        let mut file = fs::File::open(path)
            .await
            .with_context(|| format!("Failed to open file: {}", path.display()))?;

        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; 4 * 1024 * 1024]; // 4MB buffer

        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        let hash = hasher.finalize();
        let hash_hex = hash.to_hex().to_string();

        // Check if already exists (deduplication!)
        let object_path = self.hash_to_path(&hash_hex)?;
        if object_path.exists() {
            return Ok(hash_hex);
        }

        // Copy file to object store
        fs::create_dir_all(object_path.parent().unwrap()).await?;

        // Atomic write: temp file + rename
        let temp_path = object_path.with_extension("tmp");
        fs::copy(path, &temp_path)
            .await
            .with_context(|| format!("Failed to copy file to object store"))?;
        fs::rename(temp_path, object_path).await?;

        Ok(hash_hex)
    }

    /// Get blob content by hash
    pub async fn get_blob(&self, hash: &str) -> Result<Vec<u8>> {
        let object_path = self.hash_to_path(hash)?;
        fs::read(&object_path)
            .await
            .with_context(|| format!("Object {} not found", hash))
    }

    /// Copy object to file
    pub async fn copy_file(&self, hash: &str, dest: &Path) -> Result<()> {
        let object_path = self.hash_to_path(hash)?;

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::copy(&object_path, dest)
            .await
            .with_context(|| format!("Failed to copy object {} to {}", hash, dest.display()))?;
        Ok(())
    }

    /// Check if object exists
    pub fn exists(&self, hash: &str) -> bool {
        self.hash_to_path(hash).map(|p| p.exists()).unwrap_or(false)
    }

    /// Convert hash to filesystem path
    /// Hash: abc123def456... → objects/ab/c123def456...
    ///
    /// # Safety
    /// Validates hash to prevent panic and path traversal
    fn hash_to_path(&self, hash: &str) -> Result<PathBuf> {
        // Need at least 3 chars to slice safely (hash[..2] and hash[2..])
        if hash.len() < 3 {
            anyhow::bail!("Invalid hash: too short (need at least 3 chars)");
        }

        // Validate hex characters only (prevent path traversal like "../")
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("Invalid hash: must contain only hex characters (0-9, a-f)");
        }

        Ok(self.root
            .join("objects")
            .join(&hash[..2])      // First 2 chars as subdir
            .join(&hash[2..]))     // Rest as filename
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_put_and_get_blob() {
        let temp_dir = TempDir::new().unwrap();
        let store = ObjectStore::new(temp_dir.path().to_path_buf());

        let content = b"Hello, world!";
        let hash = store.put_blob(content).await.unwrap();

        let retrieved = store.get_blob(&hash).await.unwrap();
        assert_eq!(content, retrieved.as_slice());
    }

    #[tokio::test]
    async fn test_deduplication() {
        use std::time::Duration;

        let temp_dir = TempDir::new().unwrap();
        let store = ObjectStore::new(temp_dir.path().to_path_buf());

        let content = b"Same content";

        // First write
        let hash1 = store.put_blob(content).await.unwrap();

        // Get blob file path and timestamp
        let blob_path = store.hash_to_path(&hash1).unwrap();
        let first_modified = std::fs::metadata(&blob_path)
            .unwrap()
            .modified()
            .unwrap();

        // Wait to ensure timestamp would differ if file was rewritten
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Second write (same content)
        let hash2 = store.put_blob(content).await.unwrap();

        // Verify deduplication
        assert_eq!(hash1, hash2, "Same content should produce same hash");
        assert!(store.exists(&hash1), "Blob should exist");

        // Verify file wasn't rewritten (timestamp unchanged)
        let second_modified = std::fs::metadata(&blob_path)
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(
            first_modified, second_modified,
            "Blob file should not be rewritten - timestamp should be unchanged"
        );
    }

    #[tokio::test]
    async fn test_hash_sharding() {
        let temp_dir = TempDir::new().unwrap();
        let store = ObjectStore::new(temp_dir.path().to_path_buf());

        let hash = "abc123def456789";
        let path = store.hash_to_path(hash).unwrap();

        // Should create subdirectory based on first 2 chars
        // Use path components instead of string matching (cross-platform)
        let components: Vec<_> = path.components().collect();

        // Path should be: <root>/objects/ab/c123def456789
        assert!(components.len() >= 3);
        assert_eq!(components[components.len() - 2].as_os_str(), "ab");
        assert_eq!(components[components.len() - 1].as_os_str(), "c123def456789");
    }

    #[test]
    fn test_hash_validation() {
        let temp_dir = TempDir::new().unwrap();
        let store = ObjectStore::new(temp_dir.path().to_path_buf());

        // Valid hash (hex only, 3+ chars)
        assert!(store.hash_to_path("abc123").is_ok());
        assert!(store.hash_to_path("def456789abcdef").is_ok());

        // Too short (< 3 chars) - should fail
        assert!(store.hash_to_path("ab").is_err());
        assert!(store.hash_to_path("a").is_err());
        assert!(store.hash_to_path("").is_err());

        // Path traversal attempts - should fail (non-hex characters)
        assert!(store.hash_to_path("../etc/passwd").is_err());
        assert!(store.hash_to_path("..").is_err());
        assert!(store.hash_to_path("ab/../cd").is_err());
        assert!(store.hash_to_path("abc/123").is_err());
    }

    #[tokio::test]
    async fn test_large_file_streaming() {
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let store = ObjectStore::new(temp_dir.path().to_path_buf());

        // Create a "large" test file (10MB)
        let large_file = temp_dir.path().join("large.bin");
        let mut file = tokio::fs::File::create(&large_file).await.unwrap();

        // Write 10MB of data in chunks (simulates large file)
        let chunk = vec![0xAB; 1024 * 1024]; // 1MB chunk
        for _ in 0..10 {
            file.write_all(&chunk).await.unwrap();
        }
        file.flush().await.unwrap();
        drop(file);

        // Store the large file (should stream, not load all into memory)
        let hash = store.put_file(&large_file).await.unwrap();

        // Verify we can retrieve it
        assert!(store.exists(&hash));

        // Verify hash is consistent
        let hash2 = store.put_file(&large_file).await.unwrap();
        assert_eq!(hash, hash2, "Same file should produce same hash");
    }
}
