use anyhow::Result;
use sandd::snapshot::SnapshotManager;
use std::time::Instant;
use tempfile::TempDir;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Real Project Snapshot Test\n");

    let temp_dir = TempDir::new()?;
    let snapshot_store = temp_dir.path().join("snapshots");

    // Use command line argument or default to kubernetes
    let repo_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://github.com/kubernetes/kubernetes".to_string());

    let workspace = temp_dir.path().join("project");

    println!("Cloning repository: {}", repo_url);
    let clone_start = Instant::now();

    let output = std::process::Command::new("git")
        .args(&[
            "clone",
            "--depth",
            "1",
            &repo_url,
            workspace.to_str().unwrap(),
        ])
        .output()?;

    if !output.status.success() {
        eprintln!(
            "Git clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(anyhow::anyhow!("Failed to clone repository"));
    }

    let clone_elapsed = clone_start.elapsed();
    println!("✅ Clone complete: {:.2}s\n", clone_elapsed.as_secs_f64());

    // Count files
    let file_count = walkdir::WalkDir::new(&workspace)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count();

    println!("Project statistics:");
    println!("  Files: {}", file_count);

    // Create first snapshot
    let manager = SnapshotManager::new(snapshot_store.clone())?;

    println!("\nCreating first snapshot...");
    let snap1_start = Instant::now();
    let snapshot1_id = manager
        .create_snapshot(
            &workspace,
            Some(format!("Initial snapshot of {}", repo_url)),
            Some(vec!["initial".to_string()]),
        )
        .await?;
    let snap1_elapsed = snap1_start.elapsed();

    println!("✅ Snapshot 1 created: {}", snapshot1_id);
    println!("   Time: {:.2}s", snap1_elapsed.as_secs_f64());
    println!(
        "   Throughput: {:.0} files/sec\n",
        file_count as f64 / snap1_elapsed.as_secs_f64()
    );

    // Modify workspace - add/change a few files
    println!("Modifying workspace...");
    tokio::fs::write(workspace.join("NEW_FILE.txt"), "This is a new file\n").await?;
    tokio::fs::write(workspace.join("MODIFIED.txt"), "Modified content\n").await?;

    // Find and modify an existing file (if README exists)
    if workspace.join("README.md").exists() {
        let readme = tokio::fs::read_to_string(workspace.join("README.md")).await?;
        tokio::fs::write(workspace.join("README.md"), format!("{}\n\n# Modified", readme)).await?;
        println!("  Modified README.md");
    }
    println!("  Added NEW_FILE.txt and MODIFIED.txt\n");

    // Create second snapshot
    println!("Creating second snapshot...");
    let snap2_start = Instant::now();
    let snapshot2_id = manager
        .create_snapshot(
            &workspace,
            Some(format!("Modified snapshot of {}", repo_url)),
            Some(vec!["modified".to_string()]),
        )
        .await?;
    let snap2_elapsed = snap2_start.elapsed();

    println!("✅ Snapshot 2 created: {}", snapshot2_id);
    println!("   Time: {:.2}s", snap2_elapsed.as_secs_f64());

    let snapshot2 = manager.get_snapshot(&snapshot2_id).await?;
    println!(
        "   Throughput: {:.0} files/sec",
        snapshot2.file_count as f64 / snap2_elapsed.as_secs_f64()
    );
    println!(
        "   Speedup vs first snapshot: {:.2}x\n",
        snap1_elapsed.as_secs_f64() / snap2_elapsed.as_secs_f64()
    );

    // Get snapshot details
    let snapshot1 = manager.get_snapshot(&snapshot1_id).await?;
    println!("Snapshot 1 details:");
    println!("  Files: {}", snapshot1.file_count);
    println!(
        "  Total size: {} bytes ({:.2} MB)",
        snapshot1.total_size,
        snapshot1.total_size as f64 / 1_048_576.0
    );

    println!("\nSnapshot 2 details:");
    println!("  Files: {}", snapshot2.file_count);
    println!(
        "  Total size: {} bytes ({:.2} MB)",
        snapshot2.total_size,
        snapshot2.total_size as f64 / 1_048_576.0
    );

    // Check storage efficiency (deduplication)
    let objects_dir = snapshot_store.join("objects");
    let mut object_count = 0;
    let mut object_size = 0u64;

    for entry in walkdir::WalkDir::new(&objects_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            object_count += 1;
            if let Ok(metadata) = entry.metadata() {
                object_size += metadata.len();
            }
        }
    }

    println!("\nStorage statistics:");
    println!("  Objects stored: {}", object_count);
    println!(
        "  Storage size: {} bytes ({:.2} MB)",
        object_size,
        object_size as f64 / 1_048_576.0
    );
    println!(
        "  Deduplication ratio: {:.2}x (both snapshots share storage)",
        (snapshot1.total_size + snapshot2.total_size) as f64 / object_size as f64
    );

    // Restore first snapshot
    println!("\nRestoring snapshot 1...");
    let restore1_dir = temp_dir.path().join("restored1");
    let restore1_start = Instant::now();
    manager.restore_snapshot(&snapshot1_id, &restore1_dir).await?;
    let restore1_elapsed = restore1_start.elapsed();

    println!("✅ Restore 1 complete: {:.2}s", restore1_elapsed.as_secs_f64());
    println!(
        "   Throughput: {:.0} files/sec",
        snapshot1.file_count as f64 / restore1_elapsed.as_secs_f64()
    );

    // Verify restored1 doesn't have new files
    assert!(!restore1_dir.join("NEW_FILE.txt").exists());
    println!("   ✓ Verified: no NEW_FILE.txt in snapshot 1");

    // Restore second snapshot
    println!("\nRestoring snapshot 2...");
    let restore2_dir = temp_dir.path().join("restored2");
    let restore2_start = Instant::now();
    manager.restore_snapshot(&snapshot2_id, &restore2_dir).await?;
    let restore2_elapsed = restore2_start.elapsed();

    println!("✅ Restore 2 complete: {:.2}s", restore2_elapsed.as_secs_f64());
    println!(
        "   Throughput: {:.0} files/sec",
        snapshot2.file_count as f64 / restore2_elapsed.as_secs_f64()
    );

    // Verify restored2 has new files
    assert!(restore2_dir.join("NEW_FILE.txt").exists());
    println!("   ✓ Verified: NEW_FILE.txt exists in snapshot 2\n");

    // Performance summary
    println!("Performance Summary:");
    println!(
        "  Snapshot 1: {:.2}s ({:.0} files/sec)",
        snap1_elapsed.as_secs_f64(),
        snapshot1.file_count as f64 / snap1_elapsed.as_secs_f64()
    );
    println!(
        "  Snapshot 2: {:.2}s ({:.0} files/sec) - {:.2}x faster due to deduplication",
        snap2_elapsed.as_secs_f64(),
        snapshot2.file_count as f64 / snap2_elapsed.as_secs_f64(),
        snap1_elapsed.as_secs_f64() / snap2_elapsed.as_secs_f64()
    );
    println!(
        "  Restore 1:  {:.2}s ({:.0} files/sec)",
        restore1_elapsed.as_secs_f64(),
        snapshot1.file_count as f64 / restore1_elapsed.as_secs_f64()
    );
    println!(
        "  Restore 2:  {:.2}s ({:.0} files/sec)",
        restore2_elapsed.as_secs_f64(),
        snapshot2.file_count as f64 / restore2_elapsed.as_secs_f64()
    );

    Ok(())
}
