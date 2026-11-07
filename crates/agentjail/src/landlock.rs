//! Landlock filesystem sandboxing.
//!
//! Landlock provides kernel-level filesystem access control without
//! requiring root privileges. Available since Linux 5.13.
//!
//! The jail applies its ruleset from the jail's init process *after*
//! `pivot_root` and the procfs mount (a landlocked process may no longer
//! change its mount topology), so every rule names an in-jail path.

use crate::error::{JailError, Result};
use landlock::{
    ABI, Access as _, AccessFs, BitFlags, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus,
};
use std::path::Path;

/// Highest ABI whose rights the jail handles. The `landlock` crate
/// downgrades to whatever the running kernel supports.
const ABI_HANDLED: ABI = ABI::V5;

/// Read + execute: system binaries and libraries.
pub fn read_exec() -> BitFlags<AccessFs> {
    AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute
}

/// Read only: `/etc`, `/proc`, a read-only source.
pub fn read_only() -> BitFlags<AccessFs> {
    AccessFs::ReadFile | AccessFs::ReadDir
}

/// Read + write (no execute): `/tmp`.
pub fn read_write() -> BitFlags<AccessFs> {
    AccessFs::ReadFile
        | AccessFs::ReadDir
        | AccessFs::WriteFile
        | AccessFs::Truncate
        | AccessFs::RemoveFile
        | AccessFs::RemoveDir
        | AccessFs::MakeReg
        | AccessFs::MakeDir
        | AccessFs::MakeSym
        | AccessFs::MakeFifo
        | AccessFs::MakeSock
        | AccessFs::Refer
}

/// Device nodes under `/dev`: read, write and device ioctls, but no
/// node creation.
pub fn devices() -> BitFlags<AccessFs> {
    AccessFs::ReadFile
        | AccessFs::ReadDir
        | AccessFs::WriteFile
        | AccessFs::Truncate
        | AccessFs::IoctlDev
}

/// Landlock ABI version supported by the running kernel, or 0 when
/// Landlock is unavailable (pre-5.13 kernel, or disabled at boot).
pub fn abi_version() -> i32 {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: landlock_create_ruleset(NULL, 0, VERSION) only returns the
    // ABI version (or -1); it takes no pointer we own.
    let v = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if v < 0 { 0 } else { v as i32 }
}

/// Check if landlock is available on this kernel.
pub fn is_available() -> bool {
    abi_version() >= 1
}

/// Restrict the calling process to `rules` (in-jail paths). Paths that
/// don't exist (e.g. `/lib64` on aarch64) are skipped — a missing rule
/// only ever removes access. Returns an error when the kernel did not
/// enforce the ruleset at all, so `landlock = true` can never silently
/// degrade to "no Landlock".
///
/// Requires `PR_SET_NO_NEW_PRIVS` (or `CAP_SYS_ADMIN`) to already be set.
pub fn restrict(rules: &[(&Path, BitFlags<AccessFs>)]) -> Result<()> {
    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI_HANDLED))
        .map_err(JailError::Landlock)?
        .create()
        .map_err(JailError::Landlock)?;

    for (path, access) in rules {
        let Ok(fd) = PathFd::new(path) else {
            continue;
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, *access))
            .map_err(JailError::Landlock)?;
    }

    let status = ruleset.restrict_self().map_err(JailError::Landlock)?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(JailError::Unsupported(
            "landlock ruleset was not enforced by the kernel".into(),
        ));
    }
    Ok(())
}
