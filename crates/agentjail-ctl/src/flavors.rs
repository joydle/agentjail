//! Jail flavors — pre-built read-only rootfs layers (nodejs, python,
//! bun, ...) the control plane hands to [`agentjail::JailConfig`] as
//! `readonly_overlays`. A flavor is just a host directory that ships a
//! runtime + its package manager; the jail engine bind-mounts it at
//! `/opt/flavors/<name>/` read-only and prepends `/opt/flavors/<name>/bin`
//! to `PATH` if that subdir exists. The engine is deliberately
//! oblivious to "what's inside" — adding bun / deno / elixir is a
//! matter of dropping a directory under `$state_dir/flavors/`, not
//! changing agentjail code.
//!
//! ## Layout on disk
//!
//! ```text
//! $state_dir/flavors/
//!   nodejs/
//!     bin/node, npm, pnpm, ...
//!     lib/...
//!     meta.json  — optional { "name": "nodejs", "version": "20.11" }
//!   python/
//!     bin/python3, pip, uv, ...
//!     lib/...
//! ```
//!
//! A flavor without a `bin/` dir still bind-mounts but doesn't affect
//! PATH — useful for shipping pure-data layers (e.g. model weights).

use std::path::{Path, PathBuf};

/// One flavor. Returned by [`FlavorRegistry::resolve`] so callers get
/// both the name and the resolved host path in a single place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flavor {
    /// Short, URL-safe identifier the operator specifies in a
    /// workspace's config (e.g. `"nodejs"`, `"python"`, `"bun"`).
    pub name: String,
    /// Absolute host path to the flavor's directory. Bind-mounted
    /// read-only into jails that select this flavor.
    pub path: PathBuf,
}

/// Contract for flavor lookup. A single method so the DB-backed
/// per-tenant variant can slot in later (same pattern as the other
/// stores in this crate).
pub trait FlavorRegistry: Send + Sync + 'static {
    /// List every flavor this registry knows about. Sort order is
    /// implementation-defined; callers that care about stability
    /// should sort on the result.
    fn list(&self) -> Vec<Flavor>;

    /// Resolve a list of requested names into [`Flavor`] records.
    /// Returns `Err(name)` for the first unknown name so the caller
    /// can surface a precise error to the operator.
    fn resolve(&self, names: &[String]) -> std::result::Result<Vec<Flavor>, String> {
        // Default impl uses `list()` — cheap for the in-memory scanner;
        // a DB-backed impl should override with a targeted lookup.
        let all = self.list();
        let mut out = Vec::with_capacity(names.len());
        for n in names {
            match all.iter().find(|f| &f.name == n) {
                Some(f) => out.push(f.clone()),
                None    => return Err(n.clone()),
            }
        }
        Ok(out)
    }
}

/// Filesystem-backed registry: scans `$state_dir/flavors/*` on every
/// call so operators can add a flavor dir while the server is running.
/// Cheap — the directory is small (tens of entries) and lookups are
/// not on the hot path.
pub struct DirFlavorRegistry {
    root: PathBuf,
}

impl DirFlavorRegistry {
    /// Scan the given root. The root doesn't have to exist yet — an
    /// absent directory is treated as "no flavors configured", so the
    /// control plane starts cleanly on a fresh install.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The on-disk root this registry reads from. Useful for logging
    /// at startup so operators know where to drop flavor dirs.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl FlavorRegistry for DirFlavorRegistry {
    fn list(&self) -> Vec<Flavor> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(r)  => r,
            Err(_) => return out, // missing root = no flavors, not an error
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if !kind.is_dir() && !kind.is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !is_safe_flavor_name(&name) {
                tracing::warn!(flavor = %name, "skipping flavor with unsafe name");
                continue;
            }
            out.push(Flavor { name, path: entry.path() });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Flavor names end up as path components inside the jail
/// (`/opt/flavors/<name>/`) and as bind-mount targets on the host, so
/// restrict them to a conservative slug. Same shape as tenant ids.
fn is_safe_flavor_name(n: &str) -> bool {
    if n.is_empty() || n.len() > 32 {
        return false;
    }
    let mut chars = n.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_rules() {
        assert!(is_safe_flavor_name("nodejs"));
        assert!(is_safe_flavor_name("python-3"));
        assert!(is_safe_flavor_name("bun_canary"));
        // reject uppercase, leading dash, path traversal, too long
        assert!(!is_safe_flavor_name("NodeJS"));
        assert!(!is_safe_flavor_name("-bad"));
        assert!(!is_safe_flavor_name("../escape"));
        assert!(!is_safe_flavor_name(""));
        assert!(!is_safe_flavor_name(&"a".repeat(33)));
    }

    #[test]
    fn dir_registry_handles_missing_root() {
        let reg = DirFlavorRegistry::new("/does/not/exist/ever");
        assert!(reg.list().is_empty());
    }

    #[test]
    fn dir_registry_lists_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("nodejs")).unwrap();
        std::fs::create_dir(tmp.path().join("python")).unwrap();
        // A random file at the root — should be ignored.
        std::fs::write(tmp.path().join("readme.txt"), b"").unwrap();
        // An unsafe name — should be skipped with a warning.
        std::fs::create_dir(tmp.path().join("BAD-name")).unwrap();

        let reg = DirFlavorRegistry::new(tmp.path());
        let list = reg.list();
        let names: Vec<&str> = list.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["nodejs", "python"]);
    }

    #[test]
    fn resolve_returns_unknown_name_as_err() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("nodejs")).unwrap();

        let reg = DirFlavorRegistry::new(tmp.path());
        assert!(reg.resolve(&["nodejs".into()]).is_ok());
        assert_eq!(
            reg.resolve(&["nodejs".into(), "rust".into()]).unwrap_err(),
            "rust",
        );
    }
}
