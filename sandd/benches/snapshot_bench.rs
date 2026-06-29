use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sandd::snapshot::SnapshotManager;
use tempfile::TempDir;
use tokio::runtime::Runtime;

// Helper to create a workspace with N files
async fn create_test_workspace(dir: &std::path::Path, num_files: usize, file_size: usize) {
    tokio::fs::create_dir_all(dir).await.unwrap();

    for i in 0..num_files {
        let file = dir.join(format!("file_{:05}.txt", i));
        let content = vec![b'A' + (i % 26) as u8; file_size];
        tokio::fs::write(&file, content).await.unwrap();
    }
}

/// Benchmark: Small files (typical source code)
///
/// **Purpose:** Baseline performance for common use case
/// **Tests:** Many small files (~100 bytes, unique content)
/// **Expected:** Shows syscall overhead dominates (not I/O)
/// **Detects:** Per-file overhead bottleneck - typical for codebases
fn bench_small_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("small_files");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    for num_files in [100, 1000, 5000].iter() {
        group.throughput(Throughput::Elements(*num_files as u64));

        // Benchmark: Snapshot creation
        group.bench_with_input(
            BenchmarkId::new("snapshot", num_files),
            num_files,
            |b, &num_files| {
                let rt = Runtime::new().unwrap();

                b.to_async(&rt).iter(|| async {
                    let temp_dir = TempDir::new().unwrap();
                    let workspace = temp_dir.path().join("workspace");
                    let store_dir = temp_dir.path().join("store");

                    create_test_workspace(&workspace, num_files, 100).await;

                    let manager = SnapshotManager::new(store_dir).unwrap();
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );

        // Benchmark: Restore
        group.bench_with_input(
            BenchmarkId::new("restore", num_files),
            num_files,
            |b, &num_files| {
                let rt = Runtime::new().unwrap();

                // Setup: create snapshot once
                let temp_dir = TempDir::new().unwrap();
                let workspace = temp_dir.path().join("workspace");
                let store_dir = temp_dir.path().join("store");

                rt.block_on(async {
                    create_test_workspace(&workspace, num_files, 100).await;
                });

                let manager = SnapshotManager::new(store_dir).unwrap();
                let snapshot_id = rt.block_on(async {
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap()
                });

                // Benchmark: restore only
                b.to_async(&rt).iter(|| async {
                    let restore_dir = temp_dir
                        .path()
                        .join(format!("restore_{}", uuid::Uuid::new_v4()));
                    manager
                        .restore_snapshot(&snapshot_id, &restore_dir)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Large files
///
/// **Purpose:** I/O throughput measurement
/// **Tests:** Binary/media files (1MB, 10MB, 100MB)
/// **Expected:** Should show MB/sec throughput (I/O bound)
/// **Detects:** Buffer size issues, streaming efficiency
fn bench_large_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_files");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    // Files of different sizes: 1MB, 10MB, 100MB
    for file_size in [1024 * 1024, 10 * 1024 * 1024, 100 * 1024 * 1024].iter() {
        let size_mb = file_size / (1024 * 1024);
        group.throughput(Throughput::Bytes(*file_size as u64));

        // Benchmark: Snapshot creation
        group.bench_with_input(
            BenchmarkId::new("snapshot", format!("{}MB", size_mb)),
            file_size,
            |b, &file_size| {
                let rt = Runtime::new().unwrap();

                b.to_async(&rt).iter(|| async {
                    let temp_dir = TempDir::new().unwrap();
                    let workspace = temp_dir.path().join("workspace");
                    let store_dir = temp_dir.path().join("store");

                    create_test_workspace(&workspace, 1, file_size).await;

                    let manager = SnapshotManager::new(store_dir).unwrap();
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );

        // Benchmark: Restore
        group.bench_with_input(
            BenchmarkId::new("restore", format!("{}MB", size_mb)),
            file_size,
            |b, &file_size| {
                let rt = Runtime::new().unwrap();

                // Setup: create snapshot once
                let temp_dir = TempDir::new().unwrap();
                let workspace = temp_dir.path().join("workspace");
                let store_dir = temp_dir.path().join("store");

                rt.block_on(async {
                    create_test_workspace(&workspace, 1, file_size).await;
                });

                let manager = SnapshotManager::new(store_dir).unwrap();
                let snapshot_id = rt.block_on(async {
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap()
                });

                // Benchmark: restore only
                b.to_async(&rt).iter(|| async {
                    let restore_dir = temp_dir
                        .path()
                        .join(format!("restore_{}", uuid::Uuid::new_v4()));
                    manager
                        .restore_snapshot(&snapshot_id, &restore_dir)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Scalability with number of files
///
/// **Purpose:** Test if performance scales linearly
/// **Tests:** 100 → 50K files
/// **Expected:** Constant files/sec (linear scaling)
/// **Detects:** Non-linear scaling issues
fn bench_file_count_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_count_scaling");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(300));

    // Test scalability: 100, 500, 1K, 5K, 10K, 50K, 100K files
    for num_files in [100, 500, 1000, 5000, 10000, 50000, 100000].iter() {
        group.throughput(Throughput::Elements(*num_files as u64));

        // Benchmark: Snapshot creation
        group.bench_with_input(
            BenchmarkId::new("snapshot", format!("{}files", num_files)),
            num_files,
            |b, &num_files| {
                let rt = Runtime::new().unwrap();

                b.to_async(&rt).iter(|| async {
                    let temp_dir = TempDir::new().unwrap();
                    let workspace = temp_dir.path().join("workspace");
                    let store_dir = temp_dir.path().join("store");

                    create_test_workspace(&workspace, num_files, 100).await;

                    let manager = SnapshotManager::new(store_dir).unwrap();
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );

        // Benchmark: Restore
        group.bench_with_input(
            BenchmarkId::new("restore", format!("{}files", num_files)),
            num_files,
            |b, &num_files| {
                let rt = Runtime::new().unwrap();

                // Setup: create snapshot once
                let temp_dir = TempDir::new().unwrap();
                let workspace = temp_dir.path().join("workspace");
                let store_dir = temp_dir.path().join("store");

                rt.block_on(async {
                    create_test_workspace(&workspace, num_files, 100).await;
                });

                let manager = SnapshotManager::new(store_dir).unwrap();
                let snapshot_id = rt.block_on(async {
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap()
                });

                // Benchmark: restore only
                b.to_async(&rt).iter(|| async {
                    let restore_dir = temp_dir
                        .path()
                        .join(format!("restore_{}", uuid::Uuid::new_v4()));
                    manager
                        .restore_snapshot(&snapshot_id, &restore_dir)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Nested directory structure scaling
///
/// **Purpose:** Test flat vs nested directory structures
/// **Tests:** Different directory depths with same total files
/// **Expected:** Similar performance (tree structure handles both)
/// **Detects:** Directory traversal bottlenecks
fn bench_directory_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("directory_depth");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    // Test with different directory structures
    for (depth, files_per_dir) in [(1, 1000), (5, 200), (10, 100)].iter() {
        let total_files = depth * files_per_dir;
        group.throughput(Throughput::Elements(total_files as u64));

        // Benchmark: Snapshot creation
        group.bench_with_input(
            BenchmarkId::new("snapshot", format!("depth{}_total{}", depth, total_files)),
            &(depth, files_per_dir),
            |b, &(depth, files_per_dir)| {
                let rt = Runtime::new().unwrap();

                b.to_async(&rt).iter(|| async {
                    let temp_dir = TempDir::new().unwrap();
                    let workspace = temp_dir.path().join("workspace");
                    let store_dir = temp_dir.path().join("store");

                    // Create nested directory structure
                    tokio::fs::create_dir_all(&workspace).await.unwrap();
                    for d in 0..*depth {
                        let dir = workspace.join(format!("dir_{}", d));
                        tokio::fs::create_dir_all(&dir).await.unwrap();
                        for f in 0..*files_per_dir {
                            let file = dir.join(format!("file_{}.txt", f));
                            let content = vec![b'A'; 100];
                            tokio::fs::write(&file, content).await.unwrap();
                        }
                    }

                    let manager = SnapshotManager::new(store_dir).unwrap();
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );

        // Benchmark: Restore
        group.bench_with_input(
            BenchmarkId::new("restore", format!("depth{}_total{}", depth, total_files)),
            &(depth, files_per_dir),
            |b, &(depth, files_per_dir)| {
                let rt = Runtime::new().unwrap();

                // Setup: create snapshot once
                let temp_dir = TempDir::new().unwrap();
                let workspace = temp_dir.path().join("workspace");
                let store_dir = temp_dir.path().join("store");

                rt.block_on(async {
                    tokio::fs::create_dir_all(&workspace).await.unwrap();
                    for d in 0..*depth {
                        let dir = workspace.join(format!("dir_{}", d));
                        tokio::fs::create_dir_all(&dir).await.unwrap();
                        for f in 0..*files_per_dir {
                            let file = dir.join(format!("file_{}.txt", f));
                            let content = vec![b'A'; 100];
                            tokio::fs::write(&file, content).await.unwrap();
                        }
                    }
                });

                let manager = SnapshotManager::new(store_dir).unwrap();
                let snapshot_id = rt.block_on(async {
                    manager
                        .create_snapshot(&workspace, None, None)
                        .await
                        .unwrap()
                });

                // Benchmark: restore only
                b.to_async(&rt).iter(|| async {
                    let restore_dir = temp_dir
                        .path()
                        .join(format!("restore_{}", uuid::Uuid::new_v4()));
                    manager
                        .restore_snapshot(&snapshot_id, &restore_dir)
                        .await
                        .unwrap();

                    black_box(())
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_small_files,
    bench_large_files,
    bench_file_count_scaling,
    bench_directory_depth
);
criterion_main!(benches);
