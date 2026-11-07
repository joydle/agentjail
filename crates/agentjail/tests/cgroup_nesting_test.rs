//! A second agentjail process started from inside `<cg>/agentjail-init`
//! (e.g. spawned by the first) must create its jails under `<cg>`, not
//! nest another `agentjail-init` level per generation.
//!
//! The child generation is this same test binary re-executed with
//! `AJ_NESTING_CHILD=1`.

mod common;

use agentjail::Jail;
use std::path::{Path, PathBuf};

fn own_cgroup() -> String {
    std::fs::read_to_string("/proc/self/cgroup")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("")
        .to_string()
}

/// Spawn a jail with limits and return its cgroup directory.
fn jail_cgroup(tag: &str) -> PathBuf {
    let (src, out) = common::setup("cgnest", tag);
    let mut config = common::lightweight_config(src.clone(), out.clone());
    config.max_pids = 32;
    let jail = Jail::new(config).unwrap();
    let h = jail.spawn("/bin/sleep", &["2"]).expect("spawn with limits");
    let p = h.cgroup_path().unwrap();
    h.kill();
    common::cleanup(&src, &out);
    p
}

#[tokio::test]
async fn second_generation_does_not_nest() {
    if std::env::var_os("AJ_NESTING_CHILD").is_some() {
        // Child generation: report where our jail's cgroup landed.
        println!("CHILD_JAIL_CG={}", jail_cgroup("child").display());
        return;
    }
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    let start = own_cgroup();
    let base = Path::new("/sys/fs/cgroup").join(start.trim_start_matches('/'));

    let first = jail_cgroup("parent");
    assert_eq!(first.parent().unwrap(), base, "first generation");

    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "second_generation_does_not_nest", "--nocapture"])
        .env("AJ_NESTING_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let child = stdout
        .lines()
        .find_map(|l| l.strip_prefix("CHILD_JAIL_CG="))
        .unwrap_or_else(|| {
            panic!(
                "child failed: {stdout} {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    assert_eq!(
        Path::new(child).parent().unwrap(),
        base,
        "second generation nested deeper"
    );
    assert!(!base.join("agentjail-init/agentjail-init").exists());
}
