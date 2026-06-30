#!/usr/bin/env python3
"""
Snapshot Management Example

Demonstrates how to:
1. Create snapshots with tags
2. List and filter snapshots
3. Find snapshots by tag
4. Restore snapshots
5. Delete snapshots

Usage:
    python examples/snapshot_example.py
"""

import sys
import tempfile
from pathlib import Path

from sandd import Server


def main():
    # Create a temporary workspace
    with tempfile.TemporaryDirectory() as workspace:
        workspace_path = Path(workspace)
        print(f"📁 Using workspace: {workspace_path}\n")

        # Create some test files
        (workspace_path / "file1.txt").write_text("Hello World")
        (workspace_path / "file2.txt").write_text("Python Snapshot Example")
        (workspace_path / "subdir").mkdir()
        (workspace_path / "subdir" / "file3.txt").write_text("Nested file")

        # Start server
        server = Server(host="0.0.0.0", port=8765)
        print("✅ Server started on 0.0.0.0:8765")
        print("⏳ Waiting for daemon to connect...\n")

        # Wait for at least one daemon to connect
        import time
        while server.daemon_count() == 0:
            time.sleep(0.5)

        daemons = server.list_daemons()
        daemon_id = daemons[0].id
        print(f"✅ Connected to daemon: {daemon_id}\n")

        # ========== 1. Create Snapshots ==========
        print("=" * 60)
        print("1️⃣  Creating Snapshots")
        print("=" * 60)

        snapshot_id1 = server.create_snapshot(
            daemon_id=daemon_id,
            workspace=str(workspace_path),
            message="Initial snapshot with 3 files",
            tags=["v1.0", "initial"],
        )
        print(f"✅ Created snapshot 1: {snapshot_id1}")
        print(f"   Tags: v1.0, initial\n")

        # Modify workspace
        (workspace_path / "file2.txt").write_text("Modified content")
        (workspace_path / "file4.txt").write_text("New file")

        snapshot_id2 = server.create_snapshot(
            daemon_id=daemon_id,
            workspace=str(workspace_path),
            message="After modifications",
            tags=["v1.1"],
        )
        print(f"✅ Created snapshot 2: {snapshot_id2}")
        print(f"   Tags: v1.1\n")

        # ========== 2. List All Snapshots ==========
        print("=" * 60)
        print("2️⃣  Listing All Snapshots")
        print("=" * 60)

        snapshots = server.list_snapshots(daemon_id=daemon_id)
        for i, snap in enumerate(snapshots, 1):
            print(f"\nSnapshot {i}:")
            print(f"  ID:         {snap.id}")
            print(f"  Message:    {snap.message}")
            print(f"  Tags:       {', '.join(snap.tags)}")
            print(f"  Files:      {snap.file_count}")
            print(f"  Size:       {snap.total_size} bytes")
            print(f"  Created:    {snap.created_at}")

        # ========== 3. Filter by Tags ==========
        print("\n" + "=" * 60)
        print("3️⃣  Filtering Snapshots by Tag")
        print("=" * 60)

        v1_snapshots = server.list_snapshots(daemon_id=daemon_id, tags=["v1.0"])
        print(f"\n📌 Snapshots with tag 'v1.0': {len(v1_snapshots)}")
        for snap in v1_snapshots:
            print(f"  - {snap.message} ({snap.id})")

        # ========== 4. Find by Tag ==========
        print("\n" + "=" * 60)
        print("4️⃣  Finding Snapshot by Tag")
        print("=" * 60)

        initial_snapshot = server.find_snapshot_by_tag(daemon_id=daemon_id, tag="initial")
        if initial_snapshot:
            print(f"\n✅ Found snapshot with tag 'initial':")
            print(f"   ID:      {initial_snapshot.id}")
            print(f"   Message: {initial_snapshot.message}")
        else:
            print("❌ No snapshot found with tag 'initial'")

        # ========== 5. Get Snapshot Details ==========
        print("\n" + "=" * 60)
        print("5️⃣  Getting Snapshot Details")
        print("=" * 60)

        snapshot = server.get_snapshot(daemon_id=daemon_id, snapshot_id=snapshot_id1)
        if snapshot:
            print(f"\n✅ Snapshot details:")
            print(f"   ID:         {snapshot.id}")
            print(f"   Message:    {snapshot.message}")
            print(f"   Tags:       {snapshot.tags}")
            print(f"   File count: {snapshot.file_count}")
            print(f"   Total size: {snapshot.total_size} bytes")
        else:
            print("❌ Snapshot not found")

        # ========== 6. Restore Snapshot ==========
        print("\n" + "=" * 60)
        print("6️⃣  Restoring Snapshot")
        print("=" * 60)

        restore_path = workspace_path / "restored"
        restore_path.mkdir()

        file_count = server.restore_snapshot(
            daemon_id=daemon_id,
            snapshot_id=snapshot_id1,
            destination=str(restore_path),
        )
        print(f"\n✅ Restored {file_count} files to: {restore_path}")

        # Verify restored files
        restored_files = list(restore_path.rglob("*"))
        print(f"   Files in restored directory:")
        for f in restored_files:
            if f.is_file():
                print(f"   - {f.relative_to(restore_path)}")

        # ========== 7. Delete Snapshot ==========
        print("\n" + "=" * 60)
        print("7️⃣  Deleting Snapshot")
        print("=" * 60)

        server.delete_snapshot(daemon_id=daemon_id, snapshot_id=snapshot_id2)
        print(f"\n✅ Deleted snapshot: {snapshot_id2}")

        # Verify deletion
        remaining = server.list_snapshots(daemon_id=daemon_id)
        print(f"   Remaining snapshots: {len(remaining)}")

        print("\n" + "=" * 60)
        print("✅ Snapshot example completed successfully!")
        print("=" * 60)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\n👋 Interrupted by user")
        sys.exit(0)
    except Exception as e:
        print(f"\n❌ Error: {e}", file=sys.stderr)
        sys.exit(1)
