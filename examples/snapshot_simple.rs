use anyhow::Result;
use sandd::snapshot::SnapshotManager;
use tempfile::TempDir;
use tokio::fs;

#[tokio::main]
async fn main() -> Result<()> {
    println!("SandD Snapshot System Demo\n");

    // Setup: create temporary workspace and snapshot storage
    let temp_dir = TempDir::new()?;
    let workspace = temp_dir.path().join("workspace");
    let snapshot_store = temp_dir.path().join("snapshots");

    fs::create_dir_all(&workspace).await?;

    let manager = SnapshotManager::new(snapshot_store)?;

    // 1. Create initial workspace state
    println!("1. Creating initial workspace...");
    fs::write(workspace.join("README.md"), "# My Project\n").await?;
    fs::create_dir_all(workspace.join("src")).await?;
    fs::write(workspace.join("src/main.rs"), "fn main() {}\n").await?;
    fs::write(workspace.join("src/lib.rs"), "pub fn hello() {}\n").await?;

    // 2. Create snapshot with tags
    println!("2. Creating snapshot 'init'...");
    let snap1_id = manager
        .create_snapshot(
            &workspace,
            Some("Initial project setup".to_string()),
            Some(vec!["init".to_string(), "stable".to_string()]),
        )
        .await?;
    println!("   Created: {}", snap1_id);

    // 3. Modify workspace
    println!("\n3. Modifying workspace...");
    fs::write(workspace.join("src/main.rs"), "fn main() {\n    println!(\"Hello!\");\n}\n").await?;
    fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"demo\"\n").await?;

    // 4. Create another snapshot
    println!("4. Creating snapshot 'feature-work'...");
    let snap2_id = manager
        .create_snapshot(
            &workspace,
            Some("Added hello world".to_string()),
            Some(vec!["feature".to_string()]),
        )
        .await?;
    println!("   Created: {}", snap2_id);

    // 5. List all snapshots
    println!("\n5. Listing all snapshots:");
    let all_snapshots = manager.list_snapshots(None).await?;
    for snap in &all_snapshots {
        println!("   {} - {} (tags: {:?})", snap.id, snap.message, snap.tags);
        println!("      Files: {}, Size: {} bytes", snap.file_count, snap.total_size);
    }

    // 6. Filter by tag
    println!("\n6. Finding snapshots with 'init' tag:");
    let init_snapshots = manager.find_by_tag("init").await?;
    for snap in &init_snapshots {
        println!("   {} - {}", snap.id, snap.message);
    }

    println!("\n7. Finding snapshots with 'feature' tag:");
    let feature_snapshots = manager.find_by_tag("feature").await?;
    for snap in &feature_snapshots {
        println!("   {} - {}", snap.id, snap.message);
    }

    // 8. Restore first snapshot to new location
    println!("\n8. Restoring '{}' to new location...", snap1_id);
    let restore_dir = temp_dir.path().join("restored");
    manager.restore_snapshot(&snap1_id, &restore_dir).await?;

    // Verify restored files
    let readme = fs::read_to_string(restore_dir.join("README.md")).await?;
    let main_rs = fs::read_to_string(restore_dir.join("src/main.rs")).await?;
    println!("   Restored README.md: {}", readme.trim());
    println!("   Restored src/main.rs: {}", main_rs.trim());

    // 9. Get snapshot details
    println!("\n9. Getting snapshot details:");
    let snap = manager.get_snapshot(&snap2_id).await?;
    println!("   ID: {}", snap.id);
    println!("   Message: {}", snap.message);
    println!("   Tags: {:?}", snap.tags);
    println!("   Created: {:?}", snap.created_at);
    println!("   Files: {}", snap.file_count);
    println!("   Total size: {} bytes", snap.total_size);

    println!("\n✅ Demo complete!");
    Ok(())
}
