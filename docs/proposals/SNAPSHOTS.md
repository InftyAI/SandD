# SandD Snapshot System

## Overview

A Git-inspired snapshot system for capturing and restoring workspace state in agent sandboxes. This is a **pure snapshot system** (not version control) - focused on state capture/restore rather than tracking changes over time.

## Key Features

- **Hierarchical trees**: Efficient for large projects (100k+ files)
- **Content-addressable storage**: Automatic deduplication via BLAKE3 hashing
- **Cross-platform**: Works on Linux, macOS, Windows without special privileges
- **Tag-based filtering**: Organize snapshots with multiple tags
- **Independent snapshots**: No parent chains, each snapshot stands alone

---

## Similar Systems

This design takes inspiration from:

- **VM Snapshots** (VMware/VirtualBox): State capture/restore
- **ZFS/Btrfs Snapshots**: Filesystem-level snapshots
- **Docker Layers**: Image layers with content addressing
- **Time Machine**: Point-in-time backups

We use Git's storage model (hierarchical trees, content-addressable) but with snapshot semantics (no version control features).

---

## Architecture Overview

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     SandD Daemon                         │
│  ┌────────────────────────────────────────────────────┐ │
│  │            Snapshot Manager (Public API)           │ │
│  └────────────────┬───────────────────────────────────┘ │
│                   │                                      │
│  ┌────────────────┴───────────────────────────────────┐ │
│  │              Object Store (CAS)                    │ │
│  │  - Store blobs by content hash                     │ │
│  │  - Retrieve blobs by hash                          │ │
│  │  - Automatic deduplication                         │ │
│  └────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
                          ↓
         ┌────────────────────────────────┐
         │     Filesystem Storage         │
         │  .snapshots/                   │
         │  ├── objects/                  │
         │  │   ├── ab/                   │
         │  │   │   └── cdef123...        │
         │  │   └── 12/                   │
         │  │       └── 3456...           │
         │  ├── snapshots/                │
         │  │   ├── snap-uuid-1.json      │
         │  │   └── snap-uuid-2.json      │
         │  └── refs/                     │
         │      └── tags/                 │
         │          ├── v1.0.0            │
         │          └── stable            │
         └────────────────────────────────┘
```

**Note:** `ab/` and `12/` are subdirectories named after the first 2 characters of content hashes. This is explained in detail below.

### Storage Model (Git-Inspired)

**Content-Addressable Storage:**
- Files stored by BLAKE3 hash (64 hex characters, e.g., `abc123def456...`)
- Automatic deduplication (same content = same hash = stored once)
- Immutable objects (never modified after creation)

**Hash-Based Directory Sharding:**

To keep directories fast (many filesystems slow down with >10k files per directory), we split objects into subdirectories based on the **first 2 characters** of their hash:

```
Hash:     abc123def456789...  (64 chars)
          ↑↑ ↑↑↑↑↑↑↑↑↑↑↑↑↑
          │  └─ Filename
          └─ Subdirectory name

Stored as: objects/ab/c123def456789...
                   ↑↑  ↑↑↑↑↑↑↑↑↑↑↑↑↑
                   │   └─ Rest of hash (62 chars)
                   └─ First 2 chars (256 possible: 00-ff)
```

**Why this works:**
- BLAKE3 hashes are uniformly distributed (cryptographic property)
- First 2 hex chars = 256 possible subdirectories (16² = 00, 01, ..., fe, ff)
- 10,000 objects = ~39 objects per subdirectory (10000/256)
- Industry standard pattern (used by Git, Docker, IPFS)

**Example:**

```
File: main.rs
Content: "fn main() {}"
Hash: ab7c3ef21a9b4d5e6f8a1c2d3e4f5a6b...
Stored at: objects/ab/7c3ef21a9b4d5e6f8a1c2d3e4f5a6b...
                   ↑↑  ↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑↑
                   │   Remaining 62 characters
                   First 2 characters
```

**Tree Structure:**
```
workspace/
├── src/
│   ├── main.rs    → Hash: ab7c3ef2...
│   └── lib.rs     → Hash: cd8e9f1a...
└── Cargo.toml     → Hash: 12a4b6c8...

Becomes:

objects/
├── ab/
│   └── 7c3ef2...  ← Blob: main.rs content
├── cd/
│   └── 8e9f1a...  ← Blob: lib.rs content
├── 12/
│   └── a4b6c8...  ← Blob: Cargo.toml content
├── ef/
│   └── aabbcc...  ← Tree: src/ directory structure (JSON)
└── 99/
    └── 887766...  ← Tree: root directory structure (JSON)

snapshots/snap-uuid.json → points to root tree (998877...)

refs/tags/v1.0.0 → contains: snap-uuid  (plain text file)
refs/tags/stable → contains: snap-uuid
```

**Tag System (Git-Style):**

Tags are stored as plain text files in `refs/tags/`:

```
refs/tags/
├── v1.0.0      ← Contains: "snap-abc-123-def"
├── stable      ← Contains: "snap-xyz-789"
└── pre-deploy  ← Contains: "snap-uvw-456"
```

**Tag properties:**
- **Immutable**: Once created, a tag cannot be changed (like Git tags)
- **O(1) lookup**: Finding snapshots by tag requires reading one small file
- **Stored in both places**: Tag names are in snapshot JSON *and* tag ref files
  - Snapshot file: Contains list of tags for fast delete
  - Tag refs: Enable O(1) tag → snapshot lookup
- **Automatically cleaned up**: Deleting a snapshot removes its tag refs

---

## Core API

```rust
pub struct SnapshotManager {
    store: ObjectStore,
    snapshots_dir: PathBuf,
}

impl SnapshotManager {
    /// Initialize snapshot manager
    pub fn new(root: PathBuf) -> Result<Self>;

    /// Create a snapshot of workspace
    pub async fn create_snapshot(
        &self,
        workspace: &Path,
        message: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<String>;  // Returns snapshot ID

    /// Restore snapshot to destination
    pub async fn restore_snapshot(
        &self,
        snapshot_id: &str,
        destination: &Path,
    ) -> Result<()>;

    /// List all snapshots (optionally filtered by tags)
    /// Filter by tags uses O(1) tag ref lookup
    pub async fn list_snapshots(
        &self,
        tags: Option<Vec<String>>,  // OR filter: any matching tag
    ) -> Result<Vec<SnapshotInfo>>;

    /// Find snapshot by tag (O(1) lookup via tag ref)
    /// Returns single snapshot since tags are immutable
    pub async fn find_snapshot_by_tag(&self, tag: &str) -> Result<Option<SnapshotInfo>>;

    /// Get snapshot by ID
    pub async fn get_snapshot(&self, id: &str) -> Result<Snapshot>;

    /// Delete snapshot (also removes tag refs)
    pub async fn delete_snapshot(&self, id: &str) -> Result<()>;
}
```

---

## Protocol Integration

Snapshot operations are exposed via WebSocket protocol messages. All operations include a `request_id` for matching requests with responses.

**Message types:**

```rust
pub enum Message {
    // Create snapshot
    CreateSnapshot {
        request_id: String,
        workspace: String,            // Path to workspace directory
        message: Option<String>,      // Optional description
        tags: Option<Vec<String>>,    // Optional tags (must be unique)
    },
    SnapshotCreated {
        request_id: String,
        snapshot_id: String,          // UUID of created snapshot
        file_count: usize,            // Number of files captured
        total_size: u64,              // Total size in bytes
    },

    // Restore snapshot
    RestoreSnapshot {
        request_id: String,
        snapshot_id: String,          // Snapshot ID
        destination: String,          // Path to restore to
    },
    SnapshotRestored {
        request_id: String,
        file_count: usize,            // Number of files restored
    },

    // List snapshots (with optional tag filter)
    ListSnapshots {
        request_id: String,
        tags: Option<Vec<String>>,    // OR filter: snapshots with any of these tags
    },
    SnapshotList {
        request_id: String,
        snapshots: Vec<SnapshotInfo>, // Sorted by creation time (newest first)
    },

    // Find snapshot by tag (O(1) lookup)
    FindSnapshotByTag {
        request_id: String,
        tag: String,                  // Tag name (immutable)
    },
    // Get snapshot details
    GetSnapshot {
        request_id: String,
        snapshot_id: String,
    },
    SnapshotDetails {
        request_id: String,
        snapshot: Option<SnapshotInfo>, // Snapshot metadata, None if doesn't exist
    },

    // Delete snapshot (also removes tag refs)
    DeleteSnapshot {
        request_id: String,
        snapshot_id: String,
    },
    SnapshotDeleted {
        request_id: String,
    },

    // Error response
    SnapshotError {
        request_id: String,
        error: String,                // Error message (e.g., "Tag 'v1.0.0' already exists")
    },
}
```

**Error cases:**
- `CreateSnapshot` with existing tag → `SnapshotError`
- `RestoreSnapshot`/`GetSnapshot`/`DeleteSnapshot` with non-existent ID → `SnapshotError`
- File I/O errors → `SnapshotError`

---


## Example Usage

```rust
use sandd::snapshot::SnapshotManager;

#[tokio::main]
async fn main() -> Result<()> {
    let manager = SnapshotManager::new(
        PathBuf::from("/var/sandd/snapshots")
    )?;

    // Create snapshot with optional message and tags
    let snapshot_id = manager.create_snapshot(
        Path::new("/workspace/agent-123"),
        Some("Before task execution".to_string()),
        Some(vec!["pre-task".to_string()]),
    ).await?;

    println!("Created snapshot: {}", snapshot_id);

    // List all snapshots
    let snapshots = manager.list_snapshots(None).await?;
    for snap in snapshots {
        println!("{}: {} (tags: {:?})", snap.id, snap.message, snap.tags);
    }

    // Find snapshot by tag (O(1) lookup)
    if let Some(snapshot) = manager.find_snapshot_by_tag("pre-task").await? {
        println!("Found: {}", snapshot.id);
    }

    // Filter snapshots by tags
    let filtered = manager.list_snapshots(
        Some(vec!["pre-task".to_string(), "important".to_string()])
    ).await?;  // Returns snapshots with "pre-task" OR "important"

    // Get specific snapshot details
    let snapshot = manager.get_snapshot(&snapshot_id).await?;
    println!("Files: {}, Size: {} bytes", snapshot.file_count, snapshot.total_size);

    // Restore if needed
    manager.restore_snapshot(
        &snapshot_id,
        Path::new("/tmp/restored"),
    ).await?;

    Ok(())
}
```

---

## Alternatives Considered

| Alternative | Why Not? |
|-------------|----------|
| Docker volumes | Requires Docker, container-only |
| BTRFS/ZFS | Requires specific filesystem + root |
| overlayfs | Requires root, Linux only |
| fuse-overlayfs | 3-4x I/O overhead, requires /dev/fuse |
| rsync | No built-in versioning, manual management |

**Decision:** Git model is proven, cross-platform, and works everywhere.
