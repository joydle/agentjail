//! Test fixtures: temp dirs and synthetic file trees.
//!
//! Fixtures use `/tmp/aj-bench-<pid>-<counter>` so parallel runs don't
//! clobber each other. They're cleaned up on drop (best-effort).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/aj-bench-{}-{}-{}", std::process::id(), tag, n))
}

/// Source + output pair for a one-shot jail.
pub struct Dirs {
    pub source: PathBuf,
    pub output: PathBuf,
}

impl Dirs {
    pub fn fresh(tag: &str) -> Result<Self> {
        let source = unique(&format!("{tag}-src"));
        let output = unique(&format!("{tag}-out"));
        std::fs::create_dir_all(&source)
            .with_context(|| format!("create source {}", source.display()))?;
        std::fs::create_dir_all(&output)
            .with_context(|| format!("create output {}", output.display()))?;
        Ok(Self { source, output })
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.source);
        let _ = std::fs::remove_dir_all(&self.output);
    }
}

/// Fabricate a deterministic tree: `files` regular files of `size_kb` each,
/// spread across 2 levels of subdirectory to exercise directory-walk cost.
///
/// Content is pseudo-random but stable across runs so hash-based snapshots
/// can be compared meaningfully.
pub fn fabricate_tree(root: &Path, files: usize, size_kb: usize) -> Result<u64> {
    let mut total_bytes = 0u64;
    let size = size_kb * 1024;

    for i in 0..files {
        let shard = i / 64; // up to 64 files per subdir
        let subdir = root.join(format!("d{shard:03}"));
        std::fs::create_dir_all(&subdir)
            .with_context(|| format!("create subdir {}", subdir.display()))?;
        let path = subdir.join(format!("f{i:05}.bin"));

        // Deterministic content: seed from index. Cheaper than a real
        // PRNG and still defeats any "all zeros gets specially sparse"
        // optimizations on the filesystem.
        let mut buf = vec![0u8; size];
        for (j, b) in buf.iter_mut().enumerate() {
            *b = ((i.wrapping_mul(2654435761) ^ j) & 0xff) as u8;
        }
        std::fs::write(&path, &buf)
            .with_context(|| format!("write {}", path.display()))?;
        total_bytes += size as u64;
    }

    Ok(total_bytes)
}
