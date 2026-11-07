//! Jail filesystem snapshotting for faster repeated builds.
//!
//! Two storage modes:
//!
//! 1. **Full copy** (the original) — [`Snapshot::create`] walks the output
//!    dir and writes a plain duplicate under `snapshot_dir`. Simple,
//!    zero coupling to other snapshots.
//! 2. **Content-addressed incremental** —
//!    [`Snapshot::create_incremental`] writes each unique file body into
//!    a shared *object pool* keyed by BLAKE3, plus a `manifest.json`
//!    under `snapshot_dir`. Later snapshots of the same bytes dedupe to
//!    zero extra disk. [`Snapshot::restore_incremental`] *copies* each
//!    blob back into a target dir — reflink (`FICLONE`) when the
//!    filesystem supports it, a plain copy otherwise. Pool blobs are
//!    shared across snapshots (and tenants), so they are never
//!    hard-linked into a restored tree and never chmod-ed: a write in
//!    a restored tree must not be able to reach the pool.
//!
//! Both modes are complementary — the control plane picks one at write
//! time based on `AGENTJAIL_SNAPSHOT_POOL_DIR`.

use crate::error::{JailError, Result};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, OsStr};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Freeze every process in a cgroup and wait for quiescence.
///
/// Writing `1` to `cgroup.freeze` only *requests* the transition — the
/// kernel reports `frozen 1` in `cgroup.events` once every task is
/// actually stopped. Without polling that file, a snapshot captured
/// immediately after the write can include torn writes from tasks
/// that were in flight. We poll for up to 50 ms (sub-ms is typical).
///
/// **An `Err` always means "not frozen":** on timeout the freeze is
/// rolled back (`cgroup.freeze = 0`) before returning, so callers that
/// skip the thaw on error don't leave the jail stuck.
pub fn freeze_cgroup(cgroup_path: &Path) -> Result<()> {
    crate::cgroup::freeze_path(cgroup_path, crate::cgroup::FREEZE_DEADLINE)
}

/// Unfreeze every process in a cgroup. Idempotent — thawing an already
/// thawed cgroup is safe. Does not wait: resume is instant from the
/// caller's perspective.
pub fn thaw_cgroup(cgroup_path: &Path) -> Result<()> {
    crate::cgroup::thaw_path(cgroup_path)
}

/// Capture a [`Snapshot`] of a jail's output directory, freezing the
/// jail's cgroup for the duration of the copy.
///
/// - `Some(cgroup)`: the cgroup is frozen before the copy and thawed
///   afterwards, even if the copy fails. If the freeze fails (cgroup v1,
///   no `cgroup.freeze`, timeout) this returns the error and copies
///   nothing: a running jail could otherwise rewrite the tree mid-copy.
///   A cgroup that no longer exists means the jail has exited (a
///   cgroup can only be removed once empty), so the copy goes ahead.
/// - `None`: no live jail; same as [`Snapshot::create`]. Passing `None`
///   for a live jail is an explicit opt-in to a hot copy (the walk still
///   never follows symlinks).
pub fn snapshot_frozen(
    cgroup_path: Option<&Path>,
    output_dir: &Path,
    snapshot_dir: &Path,
) -> Result<Snapshot> {
    let Some(p) = cgroup_path else {
        return Snapshot::create(output_dir, snapshot_dir);
    };
    if let Err(e) = freeze_cgroup(p) {
        if !p.exists() {
            return Snapshot::create(output_dir, snapshot_dir);
        }
        return Err(e);
    }
    let out = Snapshot::create(output_dir, snapshot_dir);
    // A failed thaw leaves the jail frozen; report it over the copy.
    thaw_cgroup(p)?;
    out
}

/// A snapshot of a jail's output directory.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Path to the snapshot directory.
    path: PathBuf,
    /// Original output directory this was taken from.
    source: PathBuf,
}

impl Snapshot {
    /// Create a snapshot from an output directory.
    pub fn create(output_dir: &Path, snapshot_dir: &Path) -> Result<Self> {
        if !output_dir.exists() {
            return Err(JailError::PathNotFound(output_dir.to_path_buf()));
        }

        // Create snapshot directory
        fs::create_dir_all(snapshot_dir).map_err(JailError::Snapshot)?;

        // Copy contents
        copy_dir_recursive(output_dir, snapshot_dir)?;

        Ok(Self {
            path: snapshot_dir.to_path_buf(),
            source: output_dir.to_path_buf(),
        })
    }

    /// Restore snapshot to the original output directory.
    pub fn restore(&self) -> Result<()> {
        self.restore_to(&self.source)
    }

    /// Restore snapshot to a specific directory.
    pub fn restore_to(&self, target: &Path) -> Result<()> {
        if !self.path.exists() {
            return Err(JailError::PathNotFound(self.path.clone()));
        }

        // Clear target directory
        if target.exists() {
            clear_dir(target)?;
        } else {
            fs::create_dir_all(target).map_err(JailError::Snapshot)?;
        }

        // Copy snapshot contents
        copy_dir_recursive(&self.path, target)?;

        Ok(())
    }

    /// Load an existing snapshot from disk.
    pub fn load(snapshot_dir: &Path, original_source: &Path) -> Result<Self> {
        if !snapshot_dir.exists() {
            return Err(JailError::PathNotFound(snapshot_dir.to_path_buf()));
        }

        Ok(Self {
            path: snapshot_dir.to_path_buf(),
            source: original_source.to_path_buf(),
        })
    }

    /// Delete the snapshot.
    pub fn delete(self) -> Result<()> {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).map_err(JailError::Snapshot)?;
        }
        Ok(())
    }

    /// Get snapshot path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get snapshot size in bytes.
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        dir_size(&self.path).unwrap_or(0)
    }
}

/// Copy directory recursively. Only directories and regular files are
/// copied; symlinks, sockets, FIFOs and device nodes are skipped.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    copy_dir_with(src, dst, &mut |f, d| copy_file_to(f, d).map(|_| ()))
}

/// Result of [`copy_file_to`] for one regular file.
pub(crate) struct CopiedFile {
    /// Logical size in bytes.
    pub bytes: u64,
    /// `true` when the data was reflinked (`FICLONE`), `false` when it
    /// was byte-copied.
    pub reflinked: bool,
}

/// Copy one open regular file to `dst`: replace `dst` without following
/// anything there, reflink or copy the data, and set the mode with
/// setuid/setgid stripped.
pub(crate) fn copy_file_to(src: &fs::File, dst: &Path) -> Result<CopiedFile> {
    let meta = src.metadata().map_err(JailError::Snapshot)?;
    let dst_file = create_replacing_nofollow(dst).map_err(JailError::Snapshot)?;
    let reflinked = clone_or_copy(src, &dst_file).map_err(JailError::Snapshot)?;
    dst_file
        .set_permissions(fs::Permissions::from_mode(safe_mode(meta.mode())))
        .map_err(JailError::Snapshot)?;
    Ok(CopiedFile {
        bytes: meta.len(),
        reflinked,
    })
}

/// Copy the tree at `src` into `dst`, calling `copy_file` with each
/// regular file (already open) and its destination path. Shared by
/// [`Snapshot`] (plain copy) and live forking (COW copy via FICLONE).
/// The source is walked with [`walk_nofollow`], so a running jail that
/// swaps entries for symlinks can't redirect the copy outside `src`.
pub(crate) fn copy_dir_with<F>(src: &Path, dst: &Path, copy_file: &mut F) -> Result<()>
where
    F: FnMut(&fs::File, &Path) -> Result<()>,
{
    fs::create_dir_all(dst).map_err(JailError::Snapshot)?;
    walk_nofollow(
        src,
        &mut |rel| fs::create_dir_all(dst.join(rel)).map_err(JailError::Snapshot),
        &mut |f| match f.open().map_err(JailError::Snapshot)? {
            Some(file) => copy_file(&file, &dst.join(f.rel)),
            None => Ok(()),
        },
    )
}

/// A regular file met by [`walk_nofollow`], addressed by its parent
/// directory's fd and its name rather than by path.
pub(crate) struct FileAt<'a> {
    dir: BorrowedFd<'a>,
    name: &'a CStr,
    /// Path relative to the walk's root.
    pub rel: &'a Path,
}

impl FileAt<'_> {
    /// Open read-only without following a symlink. `Ok(None)` when the
    /// entry is no longer a regular file (it may have been swapped since
    /// it was listed). `O_NONBLOCK` keeps a FIFO that raced in from
    /// blocking the open.
    pub(crate) fn open(&self) -> std::io::Result<Option<fs::File>> {
        use rustix::fs::{Mode, OFlags, openat};
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = match openat(self.dir, self.name, flags, Mode::empty()) {
            Ok(fd) => fd,
            // ELOOP: a symlink. ENXIO: a socket. ENOENT: gone.
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NXIO | rustix::io::Errno::NOENT) => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let f = fs::File::from(fd);
        Ok(f.metadata()?.file_type().is_file().then_some(f))
    }

    /// `lstat` of the name as it is now (never follows a symlink).
    fn metadata(&self) -> std::io::Result<fs::Metadata> {
        use rustix::fs::{Mode, OFlags, openat};
        let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd: OwnedFd = openat(self.dir, self.name, flags, Mode::empty())?;
        fs::File::from(fd).metadata()
    }
}

/// Walk the tree under `root` without following a symlink at any depth.
///
/// Every directory is opened with `openat(parent, name, O_DIRECTORY |
/// O_NOFOLLOW)` and every entry is classified with `fstatat(...,
/// AT_SYMLINK_NOFOLLOW)`, so a running jail that swaps a directory for a
/// symlink (to `/`, `/proc/<pid>`, ...) mid-walk makes that entry fail
/// to open or be skipped; it can't make the walk read outside `root`.
/// `on_dir` gets each subdirectory's relative path before its contents;
/// `on_file` gets each regular file. Symlinks, sockets, FIFOs and
/// devices are skipped: opening a socket fails, a FIFO blocks until a
/// writer appears and a char device can stream forever, so a jail could
/// otherwise make every snapshot fail or hang.
pub(crate) fn walk_nofollow(
    root: &Path,
    on_dir: &mut dyn FnMut(&Path) -> Result<()>,
    on_file: &mut dyn FnMut(&FileAt<'_>) -> Result<()>,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags};
    let top = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| JailError::Snapshot(e.into()))?;
    let mut rel = PathBuf::new();
    walk_dir(top, &mut rel, on_dir, on_file)
}

fn walk_dir(
    dir: OwnedFd,
    rel: &mut PathBuf,
    on_dir: &mut dyn FnMut(&Path) -> Result<()>,
    on_file: &mut dyn FnMut(&FileAt<'_>) -> Result<()>,
) -> Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
    let io = |e: rustix::io::Errno| JailError::Snapshot(e.into());
    for entry in rustix::fs::Dir::read_from(&dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let st = match statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => continue, // removed since listed
            Err(e) => return Err(io(e)),
        };
        rel.push(OsStr::from_bytes(name.to_bytes()));
        match FileType::from_raw_mode(st.st_mode) {
            FileType::Directory => {
                let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
                match openat(&dir, name, flags, Mode::empty()) {
                    Ok(sub) => {
                        on_dir(rel)?;
                        walk_dir(sub, rel, on_dir, on_file)?;
                    }
                    // Swapped for a symlink or a file, or removed, since
                    // the stat: skip it.
                    Err(
                        rustix::io::Errno::LOOP
                        | rustix::io::Errno::NOTDIR
                        | rustix::io::Errno::NOENT,
                    ) => {}
                    Err(e) => return Err(io(e)),
                }
            }
            FileType::RegularFile => {
                use std::os::fd::AsFd;
                on_file(&FileAt {
                    dir: dir.as_fd(),
                    name,
                    rel: rel.as_path(),
                })?;
            }
            _ => {}
        }
        rel.pop();
    }
    Ok(())
}

/// Open `p` read-only **without following symlinks** and confirm it's a
/// regular file. `Ok(None)` means "not a regular file, skip it".
///
/// `O_NONBLOCK` keeps a FIFO that raced in from blocking the open; it
/// has no effect on regular-file reads.
fn open_regular_nofollow(p: &Path) -> std::io::Result<Option<fs::File>> {
    let f = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(p)
    {
        Ok(f) => f,
        // ELOOP: it's a symlink. ENXIO: it's a socket.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENXIO)) => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    if !f.metadata()?.file_type().is_file() {
        return Ok(None);
    }
    Ok(Some(f))
}

/// Create `p` exclusively (fails if anything already exists there,
/// including a dangling symlink) with owner-only perms; the caller sets
/// the final mode via `fchmod` on the returned handle.
fn create_new_nofollow(p: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(p)
}

/// Like [`create_new_nofollow`], but first unlinks a non-directory that
/// already sits at `p` (e.g. when a snapshot dir is reused). Unlinking —
/// rather than truncating through the existing name — means a symlink
/// or a hard link left there can never redirect the write.
fn create_replacing_nofollow(p: &Path) -> std::io::Result<fs::File> {
    match create_new_nofollow(p) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(p)?.is_dir() {
                return Err(e);
            }
            fs::remove_file(p)?;
            create_new_nofollow(p)
        }
        other => other,
    }
}

/// `FICLONE` ioctl number — `_IOW(0x94, 9, int)`; identical on x86-64
/// and aarch64. Kept as `u32` and cast at the call site because
/// `ioctl`'s request type differs between glibc and musl.
const FICLONE: u32 = 0x4004_9409;

/// Fill `dst` (a fresh, empty file) with the contents of `src`.
///
/// Tries a reflink first: on btrfs / XFS / bcachefs the destination
/// shares data blocks copy-on-write, so it's O(1) and a later write to
/// `dst` allocates new blocks — `src` is never affected. Falls back to a
/// byte copy (`copy_file_range` under the hood) everywhere else. Either
/// way `dst` is a distinct inode. Returns `true` when it reflinked.
fn clone_or_copy(src: &fs::File, dst: &fs::File) -> std::io::Result<bool> {
    // SAFETY: both fds are valid for the duration of the call; FICLONE
    // takes the source fd as its integer argument.
    let ret = unsafe {
        libc::ioctl(
            dst.as_raw_fd(),
            FICLONE as libc::c_ulong as _,
            src.as_raw_fd(),
        )
    };
    if ret == 0 {
        return Ok(true);
    }
    let mut src = src;
    let mut dst = dst;
    std::io::copy(&mut src, &mut dst)?;
    Ok(false)
}

/// Mode bits we're willing to recreate: permission + sticky bits only,
/// with setuid / setgid stripped. Snapshots are portable artifacts — if
/// a workspace authored a setuid binary and the snapshot is later
/// restored to a mount without NOSUID, the bit would be live.
fn safe_mode(mode: u32) -> u32 {
    mode & 0o1777
}

/// Clear directory contents without removing the directory itself.
/// Symlinks are removed (not followed) to prevent directory traversal attacks.
fn clear_dir(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(JailError::Snapshot)? {
        let entry = entry.map_err(JailError::Snapshot)?;
        let ft = entry.file_type().map_err(JailError::Snapshot)?;
        let path = entry.path();

        if ft.is_symlink() {
            // Remove the symlink itself — never follow it.
            fs::remove_file(&path).map_err(JailError::Snapshot)?;
        } else if ft.is_dir() {
            fs::remove_dir_all(&path).map_err(JailError::Snapshot)?;
        } else {
            fs::remove_file(&path).map_err(JailError::Snapshot)?;
        }
    }
    Ok(())
}

// ---------------- content-addressed incremental ----------------

/// One entry in an incremental manifest.
///
/// `hash` is a hex-encoded BLAKE3-256 of the file's bytes. BLAKE3 was
/// chosen over SHA-256 for its SIMD throughput; its digest is the same
/// 32-byte width so blob paths in the pool keep the `{prefix}/{hash}`
/// layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Path relative to the snapshot's output root, using `/` separators.
    pub path: String,
    /// Unix mode bits. We preserve the executable bit and general perms
    /// on restore; symlinks and non-regular files are skipped at capture.
    pub mode: u32,
    /// Hex-encoded BLAKE3-256 of the file's bytes.
    pub hash: String,
    /// File size in bytes (cached so a restore can allocate).
    pub size: u64,
    /// Modification time as nanoseconds since the Unix epoch.
    /// Informational only: a jail can set any mtime it likes with
    /// `utimensat`, so it is **not** trusted by the fast path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ns: Option<u128>,
    /// Inode-change time (`st_ctime`) as nanoseconds since the Unix
    /// epoch, captured *before* the bytes were hashed. Together with
    /// `ino` and `size` this keys the fast path in
    /// [`Snapshot::create_incremental_with_hint`]. Unlike mtime, ctime
    /// can't be set from userspace — any write, chmod, rename or
    /// truncate bumps it. `None` when the file was modified while being
    /// hashed, or was changed too recently for its ctime to be trusted
    /// (see `RACY_WINDOW_NS`); such entries are always re-hashed next
    /// time. Absent on older manifests, which therefore fall back to
    /// full hashing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctime_ns: Option<u128>,
    /// Inode number at capture time (fast-path key, see `ctime_ns`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ino: Option<u64>,
}

/// Manifest format version for BLAKE3-hashed entries. Readers must
/// refuse higher majors than they understand.
pub const MANIFEST_VERSION: u32 = 2;

/// Manifest format written under `snapshot_dir/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// `1` = legacy SHA-256 entries; `2` = BLAKE3 entries (+ optional
    /// `ctime_ns`/`ino` fast-path keys). Readers refuse majors higher
    /// than they understand.
    pub version: u32,
    /// Files, sorted by path for deterministic output.
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Total logical size of all referenced blobs.
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    /// Hex-encoded blob hashes referenced by this manifest. Useful for GC.
    pub fn referenced_blobs(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.hash.as_str())
    }
}

const MANIFEST_NAME: &str = "manifest.json";

/// Lock file at the pool root that serialises GC against writers.
const POOL_LOCK_NAME: &str = ".lock";

/// Advisory `flock(2)` on `{pool}/.lock`.
///
/// Snapshot creation and restore hold it **shared** for their whole
/// duration (blob writes *and* the manifest write); GC holds it
/// **exclusive**. `flock` locks belong to the open file description, so
/// this serialises threads of one process as well as separate processes
/// sharing a pool. The lock is released when the guard drops.
struct PoolLock {
    _file: fs::File,
}

impl PoolLock {
    fn acquire(pool: &Path, exclusive: bool) -> Result<Self> {
        fs::create_dir_all(pool).map_err(JailError::Snapshot)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(pool.join(POOL_LOCK_NAME))
            .map_err(JailError::Snapshot)?;
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        loop {
            // SAFETY: valid fd owned by `file`.
            if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
                return Ok(Self { _file: file });
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(JailError::Snapshot(err));
            }
        }
    }
}

impl Snapshot {
    /// Create an incremental snapshot: each regular file's bytes go into
    /// a content-addressed `objects_pool` (by BLAKE3), and a manifest of
    /// `(path, hash, mode, size)` lives in `snapshot_dir`. Files whose
    /// hash is already in the pool are free.
    ///
    /// The pool layout is `{pool}/{hash[0..2]}/{hash}`. Pool writes are
    /// idempotent; stale writes from an interrupted previous snapshot
    /// are safely overwritten. The pool's shared lock is held until the
    /// manifest is on disk, so a concurrent [`gc_objects_pool_locked`]
    /// either sees the manifest or runs entirely before the first blob
    /// was written.
    pub fn create_incremental(
        output_dir: &Path,
        snapshot_dir: &Path,
        objects_pool: &Path,
    ) -> Result<Self> {
        Self::create_incremental_with_hint(output_dir, snapshot_dir, objects_pool, None)
    }

    /// Same as [`Self::create_incremental`], but with a fast-path hint:
    /// when `prior` matches a file's `(relative-path, size, ctime_ns,
    /// ino)` and the prior blob is still present in `objects_pool`, we
    /// skip the BLAKE3 hash and the temp-file write entirely — the prior
    /// hash is reused verbatim.
    ///
    /// mtime is deliberately *not* part of the key: a jail can forge it
    /// (`touch -d`), so `(size, mtime)` would let it make a modified file
    /// look unchanged. ctime is kernel-maintained and bumped by every
    /// content or metadata change.
    ///
    /// Typical cost for an unchanged 10 GB / 100 k-file tree drops from
    /// *full read + hash + write-to-tmp per file* to *one `stat` per
    /// file*.
    ///
    /// Pass `prior` as the manifest returned from the most recent
    /// successful snapshot of the same `output_dir`. Load via
    /// [`load_manifest`] if you only have the on-disk form.
    pub fn create_incremental_with_hint(
        output_dir: &Path,
        snapshot_dir: &Path,
        objects_pool: &Path,
        prior: Option<&Manifest>,
    ) -> Result<Self> {
        if !output_dir.exists() {
            return Err(JailError::PathNotFound(output_dir.to_path_buf()));
        }
        fs::create_dir_all(snapshot_dir).map_err(JailError::Snapshot)?;
        let _lock = PoolLock::acquire(objects_pool, false)?;

        let prior_map = prior.map(build_prior_lookup).unwrap_or_default();
        let ctx = WalkCtx {
            pool: objects_pool,
            prior: &prior_map,
            started_ns: now_ns(),
        };

        let mut entries: Vec<ManifestEntry> = Vec::new();
        walk_and_hash(&ctx, output_dir, &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        let manifest = Manifest {
            version: MANIFEST_VERSION,
            entries,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| JailError::Snapshot(std::io::Error::other(e)))?;
        // Write-then-rename so a concurrent reader (e.g. a GC collecting
        // references) never parses a half-written manifest.
        let tmp = snapshot_dir.join(format!(".{MANIFEST_NAME}.tmp-{}", uniq()));
        fs::write(&tmp, manifest_bytes).map_err(JailError::Snapshot)?;
        if let Err(e) = fs::rename(&tmp, snapshot_dir.join(MANIFEST_NAME)) {
            let _ = fs::remove_file(&tmp);
            return Err(JailError::Snapshot(e));
        }

        Ok(Self {
            path: snapshot_dir.to_path_buf(),
            source: output_dir.to_path_buf(),
        })
    }

    /// Restore a content-addressed snapshot into `target_dir`.
    ///
    /// Every file is a fresh inode: reflinked from the pool blob when the
    /// filesystem supports `FICLONE` (O(1), copy-on-write), otherwise
    /// byte-copied. Blobs are never hard-linked and never chmod-ed —
    /// they're shared by every snapshot (of every tenant) that has the
    /// same bytes, so a hard link would let a write or `chmod` inside one
    /// restored tree corrupt all of them.
    ///
    /// Holds the pool's shared lock so GC can't delete blobs mid-restore.
    pub fn restore_incremental(
        snapshot_dir: &Path,
        objects_pool: &Path,
        target_dir: &Path,
    ) -> Result<()> {
        let manifest = load_manifest(snapshot_dir)?;
        let _lock = PoolLock::acquire(objects_pool, false)?;

        if target_dir.exists() {
            clear_dir(target_dir)?;
        } else {
            fs::create_dir_all(target_dir).map_err(JailError::Snapshot)?;
        }

        for entry in manifest.entries {
            // Reject manifest entries whose path escapes `target_dir`.
            //
            // `Path::join` with an absolute right-hand side *replaces*
            // the left, so a malicious manifest entry `path:
            // "/etc/shadow"` (or `"../../etc/shadow"`) would land the
            // blob anywhere the parent process can write. Accept only
            // paths made of normal components. Same for the hash, which
            // becomes a path under the pool.
            validate_relative_path(&entry.path)?;
            validate_hash(&entry.hash)?;

            let blob = blob_path(objects_pool, &entry.hash);
            let blob_file = match open_regular_nofollow(&blob) {
                Ok(Some(f)) => f,
                Ok(None) | Err(_) => {
                    return Err(JailError::Snapshot(std::io::Error::other(format!(
                        "blob {} missing from pool",
                        entry.hash,
                    ))));
                }
            };
            let dst = target_dir.join(&entry.path);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(JailError::Snapshot)?;
            }
            let dst_file = create_new_nofollow(&dst).map_err(JailError::Snapshot)?;
            clone_or_copy(&blob_file, &dst_file).map_err(JailError::Snapshot)?;
            // fchmod on our own fresh inode — never on the blob.
            dst_file
                .set_permissions(fs::Permissions::from_mode(safe_mode(entry.mode)))
                .map_err(JailError::Snapshot)?;
        }
        Ok(())
    }
}

/// Reject any manifest entry path that could escape the target dir.
///
/// Manifest files are cheap to forge once an attacker can write one
/// — so a strict check-at-restore is the right trust boundary even
/// though `create_incremental` only emits relative paths today.
fn validate_relative_path(p: &str) -> Result<()> {
    use std::path::Component;
    let path = Path::new(p);
    if p.is_empty() {
        return Err(JailError::Snapshot(std::io::Error::other(
            "manifest entry has empty path",
        )));
    }
    if path.is_absolute() {
        return Err(JailError::Snapshot(std::io::Error::other(format!(
            "manifest entry path is absolute: {p}",
        ))));
    }
    for c in path.components() {
        match c {
            Component::Normal(_) => {}
            _ => {
                return Err(JailError::Snapshot(std::io::Error::other(format!(
                    "manifest entry path contains non-normal component: {p}",
                ))));
            }
        }
    }
    Ok(())
}

/// Blob names are 32-byte digests in lowercase hex. Anything else in a
/// manifest (e.g. `../../etc/shadow`) would turn the restore into an
/// arbitrary-file read.
fn validate_hash(h: &str) -> Result<()> {
    if h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(JailError::Snapshot(std::io::Error::other(format!(
            "manifest entry has malformed hash: {h:?}",
        ))))
    }
}

/// Load and return a manifest from a snapshot dir created with
/// [`Snapshot::create_incremental`]. Errors when the file is missing —
/// useful for tools distinguishing full vs incremental snapshots.
pub fn load_manifest(snapshot_dir: &Path) -> Result<Manifest> {
    let bytes = fs::read(snapshot_dir.join(MANIFEST_NAME)).map_err(JailError::Snapshot)?;
    serde_json::from_slice(&bytes).map_err(|e| JailError::Snapshot(std::io::Error::other(e)))
}

fn blob_path(pool: &Path, sha: &str) -> PathBuf {
    let prefix = if sha.len() >= 2 { &sha[..2] } else { sha };
    pool.join(prefix).join(sha)
}

/// Fast-path identity of a file: everything that must be unchanged for
/// a prior hash to be reused. All of it comes from one `stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatKey {
    size: u64,
    ctime_ns: u128,
    ino: u64,
    dev: u64,
    mtime_ns: u128,
}

impl StatKey {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            size: m.size(),
            ctime_ns: ts_ns(m.ctime(), m.ctime_nsec()),
            ino: m.ino(),
            dev: m.dev(),
            mtime_ns: ts_ns(m.mtime(), m.mtime_nsec()),
        }
    }
}

fn ts_ns(sec: i64, nsec: i64) -> u128 {
    (sec.max(0) as u128).saturating_mul(1_000_000_000) + nsec.max(0) as u128
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Files whose ctime falls within this window of the walk's start are
/// "racily clean" (git's term): the kernel stamps ctime from a coarse
/// clock, so a write landing in the same tick as our stat could leave
/// ctime unchanged. We still hash such files, but don't record a ctime
/// for them, so the next snapshot re-hashes instead of trusting it.
const RACY_WINDOW_NS: u128 = 2_000_000_000;

/// How many times to re-hash a file that keeps changing under us before
/// giving up on a stable read (the entry is then recorded without a
/// fast-path key).
const MAX_HASH_ATTEMPTS: usize = 3;

/// Lookup from prior manifest: `relpath → (size, ctime_ns, ino, hash)`.
///
/// Only entries carrying both `ctime_ns` and `ino` participate — older
/// manifests fall back to full hashing.
type PriorLookup<'a> = std::collections::HashMap<&'a str, (u64, u128, u64, &'a str)>;

fn build_prior_lookup(prior: &Manifest) -> PriorLookup<'_> {
    prior
        .entries
        .iter()
        .filter_map(|e| match (e.ctime_ns, e.ino) {
            (Some(c), Some(i)) if validate_hash(&e.hash).is_ok() => {
                Some((e.path.as_str(), (e.size, c, i, e.hash.as_str())))
            }
            _ => None,
        })
        .collect()
}

struct WalkCtx<'a> {
    pool: &'a Path,
    prior: &'a PriorLookup<'a>,
    started_ns: u128,
}

fn walk_and_hash(ctx: &WalkCtx<'_>, root: &Path, out: &mut Vec<ManifestEntry>) -> Result<()> {
    walk_nofollow(root, &mut |_| Ok(()), &mut |f| {
        // Forward slashes so manifests round-trip cross-platform.
        let rel = f
            .rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if let Some(e) = hash_entry(ctx, f, rel)? {
            out.push(e);
        }
        Ok(())
    })
}

/// Produce the manifest entry for one regular file, or `None` if it
/// vanished / stopped being a regular file since it was listed.
fn hash_entry(ctx: &WalkCtx<'_>, src: &FileAt<'_>, rel: String) -> Result<Option<ManifestEntry>> {
    for _ in 0..MAX_HASH_ATTEMPTS {
        let Some(mut file) = src.open().map_err(JailError::Snapshot)? else {
            return Ok(None);
        };
        // Stat BEFORE reading: whatever we record must describe a state
        // at or before the bytes we hash, never after.
        let pre_meta = file.metadata().map_err(JailError::Snapshot)?;
        let pre = StatKey::of(&pre_meta);
        let mode = pre_meta.mode();

        // Fast path: unchanged (size, ctime, inode) and the blob is still
        // in the pool (GC can't remove it while we hold the shared lock).
        if let Some((p_size, p_ctime, p_ino, p_hash)) = ctx.prior.get(rel.as_str())
            && pre.size == *p_size
            && pre.ctime_ns == *p_ctime
            && pre.ino == *p_ino
            && blob_path(ctx.pool, p_hash).is_file()
        {
            return Ok(Some(ManifestEntry {
                path: rel,
                mode,
                hash: (*p_hash).to_string(),
                size: pre.size,
                mtime_ns: Some(pre.mtime_ns),
                ctime_ns: Some(pre.ctime_ns),
                ino: Some(pre.ino),
            }));
        }

        let (hash, size) = hash_into_pool(&mut file, ctx.pool)?;

        // Re-stat after: both the fd (in-place modification) and the
        // path (file replaced by rename). Any difference means the bytes
        // we hashed may be a torn mix — try again.
        let post_fd = file.metadata().map(|m| StatKey::of(&m)).ok();
        let post_path = src.metadata().map(|m| StatKey::of(&m)).ok();
        let stable = post_fd == Some(pre) && post_path == Some(pre) && size == pre.size;
        if stable {
            let trusted = pre.ctime_ns.saturating_add(RACY_WINDOW_NS) < ctx.started_ns;
            return Ok(Some(ManifestEntry {
                path: rel,
                mode,
                hash,
                size,
                mtime_ns: Some(pre.mtime_ns),
                ctime_ns: trusted.then_some(pre.ctime_ns),
                ino: trusted.then_some(pre.ino),
            }));
        }
    }

    // Kept changing on every attempt: record the last read but without a
    // fast-path key so the next snapshot can't reuse it blindly.
    let Some(mut file) = src.open().map_err(JailError::Snapshot)? else {
        return Ok(None);
    };
    let mode = file.metadata().map_err(JailError::Snapshot)?.mode();
    let (hash, size) = hash_into_pool(&mut file, ctx.pool)?;
    Ok(Some(ManifestEntry {
        path: rel,
        mode,
        hash,
        size,
        mtime_ns: None,
        ctime_ns: None,
        ino: None,
    }))
}

/// Stream an open file through BLAKE3 into a temp file in the pool, then
/// atomically rename to the final blob path if it doesn't already exist.
/// Blobs are made read-only (0444) so nothing can write through them by
/// accident.
fn hash_into_pool(f: &mut fs::File, pool: &Path) -> Result<(String, u64)> {
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(0))
        .map_err(JailError::Snapshot)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size: u64 = 0;
    let tmp = pool.join(format!(".tmp-{}", uniq()));
    let mut tmp_writer = create_new_nofollow(&tmp).map_err(JailError::Snapshot)?;

    let res = (|| -> std::io::Result<()> {
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            tmp_writer.write_all(&buf[..n])?;
            size += n as u64;
        }
        tmp_writer.flush()?;
        tmp_writer.set_permissions(fs::Permissions::from_mode(0o444))
    })();
    drop(tmp_writer);
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(JailError::Snapshot(e));
    }

    let hash = hex::encode(hasher.finalize().as_bytes());
    let final_path = blob_path(pool, &hash);
    if let Some(parent) = final_path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        let _ = fs::remove_file(&tmp);
        return Err(JailError::Snapshot(e));
    }
    if final_path.exists() {
        // Already in pool — drop the temp and skip.
        let _ = fs::remove_file(&tmp);
    } else {
        // Rename within a filesystem is atomic. A concurrent writer for
        // the same hash may race us; last-write-wins is fine because the
        // contents are identical by construction.
        if let Err(e) = fs::rename(&tmp, &final_path) {
            let _ = fs::remove_file(&tmp);
            return Err(JailError::Snapshot(e));
        }
    }
    Ok((hash, size))
}

fn uniq() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", std::process::id(), c)
}

/// Garbage-collect unreferenced blobs from `objects_pool`.
///
/// `referenced` is the union of every manifest's `referenced_blobs()`.
/// Anything on disk under the pool that isn't in that set is removed.
/// Returns `(blobs_deleted, bytes_freed)`.
///
/// Takes the pool's exclusive lock, so it never runs concurrently with
/// a snapshot or restore. **But** `referenced` was computed by the
/// caller *before* the lock was taken, so a snapshot that finished in
/// between is invisible to it and would lose its blobs. Prefer
/// [`gc_objects_pool_locked`], which collects references under the
/// lock.
pub fn gc_objects_pool(
    objects_pool: &Path,
    referenced: &std::collections::HashSet<String>,
) -> Result<(usize, u64)> {
    let referenced = referenced.clone();
    gc_objects_pool_locked(objects_pool, move || Ok(referenced))
}

/// Race-free GC: takes the pool's exclusive lock, *then* calls
/// `collect_referenced` (typically: read every `manifest.json` that
/// points at this pool) and sweeps everything not in the returned set,
/// plus orphaned `.tmp-*` files from crashed writers.
///
/// Because [`Snapshot::create_incremental`] holds the shared lock until
/// its manifest has been atomically renamed into place, every snapshot
/// is either fully visible to `collect_referenced` or hasn't written a
/// single blob yet. If `collect_referenced` fails, nothing is deleted.
pub fn gc_objects_pool_locked<F>(objects_pool: &Path, collect_referenced: F) -> Result<(usize, u64)>
where
    F: FnOnce() -> Result<std::collections::HashSet<String>>,
{
    if !objects_pool.exists() {
        return Ok((0, 0));
    }
    let _lock = PoolLock::acquire(objects_pool, true)?;
    let referenced = collect_referenced()?;

    let mut deleted = 0usize;
    let mut freed = 0u64;
    let mut remove = |p: &Path, sz: u64| {
        if fs::remove_file(p).is_ok() {
            deleted += 1;
            freed += sz;
        }
    };
    for shard in fs::read_dir(objects_pool).map_err(JailError::Snapshot)? {
        let shard = shard.map_err(JailError::Snapshot)?;
        let shard_path = shard.path();
        let name = shard.file_name().to_string_lossy().into_owned();
        let ft = shard.file_type().map_err(JailError::Snapshot)?;
        if !ft.is_dir() {
            // Root-level temp files are where `hash_into_pool` stages
            // blobs; with the exclusive lock held none are in flight.
            if ft.is_file() && name.starts_with(".tmp-") {
                let sz = shard.metadata().map(|m| m.len()).unwrap_or(0);
                remove(&shard_path, sz);
            }
            continue;
        }
        for blob in fs::read_dir(&shard_path).map_err(JailError::Snapshot)? {
            let blob = blob.map_err(JailError::Snapshot)?;
            let name = blob.file_name().to_string_lossy().into_owned();
            if name.starts_with(".tmp-") || !referenced.contains(&name) {
                let sz = blob.metadata().map(|m| m.len()).unwrap_or(0);
                remove(&blob.path(), sz);
            }
        }
        // Best-effort empty-shard cleanup.
        let _ = fs::remove_dir(&shard_path);
    }
    Ok((deleted, freed))
}

/// Calculate directory size recursively (does not follow symlinks).
fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut size = 0;

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let ft = entry.file_type()?;

        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            size += dir_size(&entry.path())?;
        } else {
            size += entry.metadata()?.len();
        }
    }

    Ok(size)
}
