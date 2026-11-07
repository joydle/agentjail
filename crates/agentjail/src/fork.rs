//! Live forking: COW-clone a running jail's output via `FICLONE`. Falls
//! back to regular copy when reflinks aren't supported (ext4 without
//! `reflink`, tmpfs, cross-filesystem). The running jail is frozen for
//! the duration of the clone (sub-millisecond) so the copy is consistent.

use crate::error::{JailError, Result};
use crate::snapshot::{copy_dir_with, copy_file_to};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// Method used for filesystem cloning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneMethod {
    /// Reflink COW clone via `FICLONE` ioctl. Instant — no extra disk
    /// space until writes diverge.
    Reflink,
    /// Regular file copy. Full disk usage immediately.
    Copy,
    /// Some files were reflinked, others fell back to regular copy.
    Mixed,
}

/// Metadata about a completed fork operation.
#[derive(Debug, Clone)]
pub struct ForkInfo {
    /// Wall-clock time for the filesystem clone.
    pub clone_duration: Duration,
    /// Clone strategy that was actually used.
    pub clone_method: CloneMethod,
    /// Total regular files cloned.
    pub files_cloned: u64,
    /// Files that used COW (reflink).
    pub files_cow: u64,
    /// Total logical bytes cloned.
    pub bytes_cloned: u64,
    /// Whether the source jail was frozen during the clone.
    pub was_frozen: bool,
}

/// Clone a directory tree using copy-on-write when possible.
///
/// Same walk and per-file path as snapshots ([`copy_dir_with`],
/// [`copy_file_to`]): the source tree is walked by directory fd without
/// following symlinks (the jail may still be running and swapping
/// entries), the destination is replaced without following anything
/// already there, data is reflinked with `FICLONE` or byte-copied when
/// that's unsupported, and setuid/setgid bits are stripped. Symlinks,
/// FIFOs, sockets and devices are skipped.
pub(crate) fn cow_clone(src: &Path, dst: &Path) -> Result<ForkInfo> {
    let start = Instant::now();

    if !src.exists() {
        return Err(JailError::PathNotFound(src.to_path_buf()));
    }

    fs::create_dir_all(dst).map_err(JailError::Io)?;

    let mut total_files: u64 = 0;
    let mut cow_files: u64 = 0;
    let mut total_bytes: u64 = 0;

    copy_dir_with(src, dst, &mut |src_file, dst_path| {
        let f = copy_file_to(src_file, dst_path)?;
        total_files += 1;
        total_bytes += f.bytes;
        cow_files += u64::from(f.reflinked);
        Ok(())
    })?;

    let clone_method = match (cow_files, total_files) {
        (_, 0) => CloneMethod::Copy,
        (cow, total) if cow == total => CloneMethod::Reflink,
        (0, _) => CloneMethod::Copy,
        _ => CloneMethod::Mixed,
    };

    Ok(ForkInfo {
        clone_duration: start.elapsed(),
        clone_method,
        files_cloned: total_files,
        files_cow: cow_files,
        bytes_cloned: total_bytes,
        was_frozen: false, // caller sets this after the fact
    })
}
