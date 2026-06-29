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
         │     Filesystem Storage          │
         │  .snapshots/                    │
         │  ├── objects/                   │
         │  │   ├── ab/                    │
         │  │   │   └── cdef123...         │
         │  │   └── 12/                    │
         │  │       └── 3456...            │
         │  ├── snapshots/                 │
         │  │   ├── snap-uuid-1.json       │
         │  │   └── snap-uuid-2.json       │
         │  └── HEAD                       │
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
```

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
        message: String,
        tags: Vec<String>,
    ) -> Result<String>;  // Returns snapshot ID

    /// Restore snapshot to destination
    pub async fn restore_snapshot(
        &self,
        snapshot_id: &str,
        destination: &Path,
    ) -> Result<()>;

    /// List all snapshots
    pub async fn list_snapshots(&self) -> Result<Vec<Snapshot>>;

    /// Delete snapshot and orphaned objects
    pub async fn delete_snapshot(&self, id: &str) -> Result<()>;

    /// Garbage collect unreferenced objects
    pub async fn gc(&self) -> Result<GcStats>;
}
```

---

## Protocol Integration

**Note:** See [Protocol Specification](PROTOCOL.md) for complete message format details.

**New message types:**

```rust
pub enum Request {
    CreateSnapshot {
        daemon_id: String,
        workspace_path: String,
        message: String,
        tags: Vec<String>,
    },

    RestoreSnapshot {
        daemon_id: String,
        snapshot_id: String,
        destination: String,
    },

    ListSnapshots { daemon_id: String },
    DeleteSnapshot { daemon_id: String, snapshot_id: String },
    GarbageCollect { daemon_id: String },
}

pub enum Response {
    SnapshotCreated {
        snapshot_id: String,
        file_count: usize,
        total_size: u64,
        duration_ms: u64,
    },

    SnapshotRestored { file_count: usize, duration_ms: u64 },
    Snapshots { snapshots: Vec<SnapshotInfo> },
    SnapshotDeleted { freed_bytes: u64 },
    GarbageCollected { objects_deleted: usize, bytes_freed: u64 },
}
```

---


## Example Usage

```rust
use sandd::snapshot::SnapshotManager;

#[tokio::main]
async fn main() -> Result<()> {
    let manager = SnapshotManager::new(
        PathBuf::from("/var/sandd/snapshots")
    )?;

    // Create snapshot
    let snapshot_id = manager.create_snapshot(
        Path::new("/workspace/agent-123"),
        "Before task execution".to_string(),
        vec!["pre-task".to_string()],
    ).await?;

    println!("Created snapshot: {}", snapshot_id);

    // List snapshots
    let snapshots = manager.list_snapshots().await?;
    for snap in snapshots {
        println!("{}: {} (tags: {:?})", snap.id, snap.message, snap.tags);
    }

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
