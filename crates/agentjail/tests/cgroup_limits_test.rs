//! Cgroup limit plumbing: memory limits always come with
//! `memory.swap.max = 0`, and `pids.max` is applied when configured.
//!
//! Needs root and a cgroup where agentjail can be the only process (see
//! `tests/support/cgroup_leaf_runner.sh`). Skips when not root.

mod common;

use agentjail::Jail;
use std::fs;

#[tokio::test]
async fn memory_limit_disables_swap_and_pids_max_is_set() {
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    let (src, out) = common::setup("cglim", "swap");
    let mut config = common::lightweight_config(src.clone(), out.clone());
    config.memory_mb = Some(64);
    config.max_pids = 17;
    let jail = Jail::new(config).unwrap();
    let handle = jail
        .spawn("/bin/sleep", &["5"])
        .expect("spawn with cgroup limits (is agentjail alone in its cgroup?)");
    let cg = handle
        .cgroup_path()
        .expect("limits configured → cgroup exists");

    let read = |f: &str| fs::read_to_string(cg.join(f)).map(|s| s.trim().to_string());
    assert_eq!(
        read("memory.max").unwrap(),
        (64u64 * 1024 * 1024).to_string()
    );
    assert_eq!(read("pids.max").unwrap(), "17");
    match read("memory.swap.max") {
        Ok(v) => assert_eq!(v, "0", "swap must be disabled when a memory limit is set"),
        Err(e) => eprintln!("note: no swap accounting on this kernel ({e})"),
    }

    handle.kill();
    let _ = handle.wait().await;
    common::cleanup(&src, &out);
}
