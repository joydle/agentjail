//! Landlock filesystem sandboxing.
//!
//! Landlock provides kernel-level filesystem access control without
//! requiring root privileges. Available since Linux 5.13.

use crate::config::Access;
use crate::error::{JailError, Result};
use landlock::{
    ABI, Access as LandlockAccess, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr,
};
use std::path::Path;

/// Apply landlock rules for the given paths.
pub fn apply_rules(rules: &[(impl AsRef<Path>, Access)]) -> Result<()> {
    let abi = ABI::V5;

    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(JailError::Landlock)?
        .create()
        .map_err(JailError::Landlock)?;

    for (path, access) in rules {
        let path = path.as_ref();
        if !path.exists() {
            eprintln!("landlock: skipping non-existent path: {}", path.display());
            continue;
        }

        let fs_access = access_to_landlock(*access);

        let path_fd = match PathFd::new(path) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("landlock: cannot open {}: {}", path.display(), e);
                continue;
            }
        };

        ruleset = ruleset
            .add_rule(PathBeneath::new(path_fd, fs_access))
            .map_err(JailError::Landlock)?;
    }

    ruleset.restrict_self().map_err(JailError::Landlock)?;

    Ok(())
}

fn access_to_landlock(access: Access) -> landlock::BitFlags<AccessFs> {
    match access {
        Access::ReadOnly => AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute,
        Access::WriteOnly => {
            AccessFs::WriteFile
                | AccessFs::RemoveFile
                | AccessFs::RemoveDir
                | AccessFs::MakeReg
                | AccessFs::MakeDir
                | AccessFs::MakeSym
        }
        Access::ReadWrite => {
            AccessFs::ReadFile
                | AccessFs::ReadDir
                | AccessFs::Execute
                | AccessFs::WriteFile
                | AccessFs::RemoveFile
                | AccessFs::RemoveDir
                | AccessFs::MakeReg
                | AccessFs::MakeDir
                | AccessFs::MakeSym
        }
    }
}

/// Check if landlock is available on this kernel.
pub fn is_available() -> bool {
    Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V1))
        .is_ok()
}
