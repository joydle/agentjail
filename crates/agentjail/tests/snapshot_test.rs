//! Tests for jail filesystem snapshotting.
//!
//! Run with: cargo test --test snapshot_test

use agentjail::{Snapshot, gc_objects_pool, load_manifest, snapshot_frozen};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[test]
fn test_snapshot_create_restore() {
    let output = PathBuf::from("/tmp/snapshot-test-output");
    let snapshot = PathBuf::from("/tmp/snapshot-test-snap");

    // Setup
    let _ = fs::remove_dir_all(&output);
    let _ = fs::remove_dir_all(&snapshot);
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("file.txt"), "hello").unwrap();
    fs::create_dir_all(output.join("subdir")).unwrap();
    fs::write(output.join("subdir/nested.txt"), "world").unwrap();

    // Create snapshot
    let snap = Snapshot::create(&output, &snapshot).unwrap();
    assert!(snap.size_bytes() > 0);

    // Modify output
    fs::write(output.join("file.txt"), "modified").unwrap();
    fs::remove_file(output.join("subdir/nested.txt")).unwrap();

    // Restore
    snap.restore().unwrap();

    // Verify
    assert_eq!(fs::read_to_string(output.join("file.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(output.join("subdir/nested.txt")).unwrap(),
        "world"
    );

    // Cleanup
    snap.delete().unwrap();
    let _ = fs::remove_dir_all(&output);
}

#[test]
fn test_snapshot_skips_symlinks() {
    let src = PathBuf::from("/tmp/snapshot-symlink-src");
    let dst = PathBuf::from("/tmp/snapshot-symlink-dst");
    let secret = PathBuf::from("/tmp/snapshot-symlink-secret");

    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dst);
    let _ = fs::remove_dir_all(&secret);

    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // Create a secret file outside the snapshot dir
    fs::write(&secret, "TOP_SECRET_DATA").unwrap();

    // Create a regular file and a symlink pointing to the secret
    fs::write(src.join("legit.txt"), "normal").unwrap();
    std::os::unix::fs::symlink(&secret, src.join("sneaky_link")).unwrap();

    // Copy should skip the symlink
    let snap = Snapshot::create(&src, &dst).unwrap();

    // legit.txt must be copied
    assert_eq!(fs::read_to_string(dst.join("legit.txt")).unwrap(), "normal");
    // symlink must NOT be copied (not followed, not recreated)
    assert!(
        !dst.join("sneaky_link").exists(),
        "Symlink should not be copied into snapshot"
    );

    // Cleanup
    snap.delete().unwrap();
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_file(&secret);
}

#[test]
fn test_snapshot_frozen_without_cgroup_equivalent_to_create() {
    let output   = PathBuf::from("/tmp/snapshot-frozen-none-output");
    let snapshot = PathBuf::from("/tmp/snapshot-frozen-none-snap");
    let _ = fs::remove_dir_all(&output);
    let _ = fs::remove_dir_all(&snapshot);
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("a.txt"), "A").unwrap();

    // cgroup_path = None → behaves like Snapshot::create.
    let snap = snapshot_frozen(None, &output, &snapshot).unwrap();
    assert!(snap.path().exists());
    assert_eq!(fs::read_to_string(snap.path().join("a.txt")).unwrap(), "A");

    snap.delete().unwrap();
    let _ = fs::remove_dir_all(&output);
}

#[test]
fn test_snapshot_frozen_with_missing_cgroup_falls_back_to_plain_copy() {
    // Simulate a system where the given cgroup path doesn't exist.
    // `freeze_cgroup` should error, `snapshot_frozen` should warn and
    // proceed with a plain copy rather than failing the whole request.
    let output    = PathBuf::from("/tmp/snapshot-frozen-missing-output");
    let snapshot  = PathBuf::from("/tmp/snapshot-frozen-missing-snap");
    let bogus_cg  = PathBuf::from("/tmp/__does_not_exist_cgroup__");
    let _ = fs::remove_dir_all(&output);
    let _ = fs::remove_dir_all(&snapshot);
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("b.txt"), "B").unwrap();

    let snap = snapshot_frozen(Some(&bogus_cg), &output, &snapshot)
        .expect("snapshot must succeed even when freeze is impossible");
    assert_eq!(fs::read_to_string(snap.path().join("b.txt")).unwrap(), "B");

    snap.delete().unwrap();
    let _ = fs::remove_dir_all(&output);
}

#[test]
fn incremental_roundtrip_and_dedupes() {
    let output    = PathBuf::from("/tmp/snap-incr-out");
    let snap_a    = PathBuf::from("/tmp/snap-incr-a");
    let snap_b    = PathBuf::from("/tmp/snap-incr-b");
    let restore   = PathBuf::from("/tmp/snap-incr-restore");
    let pool      = PathBuf::from("/tmp/snap-incr-pool");

    for p in [&output, &snap_a, &snap_b, &restore, &pool] {
        let _ = fs::remove_dir_all(p);
    }
    fs::create_dir_all(&output).unwrap();
    fs::create_dir_all(output.join("nested")).unwrap();
    fs::write(output.join("same.txt"), "HELLO").unwrap();
    fs::write(output.join("nested/other.txt"), "WORLD").unwrap();

    // First snapshot.
    let s_a = Snapshot::create_incremental(&output, &snap_a, &pool).unwrap();
    assert!(s_a.path().join("manifest.json").exists());
    let m_a = load_manifest(&snap_a).unwrap();
    assert_eq!(m_a.entries.len(), 2);
    // Files are path-sorted: nested/other.txt, same.txt
    assert_eq!(m_a.entries[0].path, "nested/other.txt");
    assert_eq!(m_a.entries[1].path, "same.txt");
    assert!(m_a.size_bytes() > 0);

    // Dedupe: second snapshot of identical bytes reuses pool blobs.
    let pool_size_before = dir_bytes(&pool);
    let _s_b = Snapshot::create_incremental(&output, &snap_b, &pool).unwrap();
    let pool_size_after = dir_bytes(&pool);
    assert_eq!(
        pool_size_before, pool_size_after,
        "duplicate snapshot should not grow the pool"
    );

    // Restore into a fresh dir.
    Snapshot::restore_incremental(&snap_a, &pool, &restore).unwrap();
    assert_eq!(
        fs::read_to_string(restore.join("same.txt")).unwrap(),
        "HELLO"
    );
    assert_eq!(
        fs::read_to_string(restore.join("nested/other.txt")).unwrap(),
        "WORLD"
    );

    // GC with both manifests referenced keeps everything.
    let refs: HashSet<String> = m_a.referenced_blobs().map(str::to_string).collect();
    let (deleted, _) = gc_objects_pool(&pool, &refs).unwrap();
    assert_eq!(deleted, 0);

    // After dropping every reference, GC sweeps the pool clean.
    let empty: HashSet<String> = HashSet::new();
    let (deleted, freed) = gc_objects_pool(&pool, &empty).unwrap();
    assert!(deleted >= 2);
    assert!(freed > 0);

    for p in [&output, &snap_a, &snap_b, &restore, &pool] {
        let _ = fs::remove_dir_all(p);
    }
}

/// Approximate byte-sum of everything under a dir (regular files only).
fn dir_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let rd = match fs::read_dir(&cur) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let ft = match e.file_type() {
                Ok(f) => f,
                Err(_) => continue,
            };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}
