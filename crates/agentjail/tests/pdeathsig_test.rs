//! A jail dies with the process that spawned it.
//!
//! The setup process arms `PR_SET_PDEATHSIG(SIGKILL)`. The kernel clears
//! that setting on every euid/egid change, so the engine re-arms it after
//! each one (`setresuid(0)` in a user namespace mapped to an unprivileged
//! host id, the `run_as` drop). This test SIGKILLs a real embedder (this
//! test binary, re-executed) and checks that every process in the jail's
//! cgroup is gone.
//!
//! Needs root; skips otherwise.

mod common;

use agentjail::{Jail, JailConfig};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "AJ_PDEATHSIG_EMBEDDER";

/// The embedder side: runs only when re-executed by
/// [`jail_dies_with_its_embedder`]. Spawns a long jail, prints its
/// cgroup, then waits to be killed.
#[tokio::test]
async fn embedder_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    let userns = mode == "userns";
    let (src, out) = common::setup("pdeathsig", &mode);
    let config = JailConfig {
        source: src,
        output: out,
        user_namespace: userns,
        landlock: false,
        max_pids: 64, // forces a cgroup, which the parent inspects
        timeout_secs: 60,
        env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        ..Default::default()
    };
    let jail = Jail::new(config).unwrap();
    let handle = jail.spawn("/bin/sh", &["-c", "sleep 60"]).unwrap();
    println!("CGROUP={}", handle.cgroup_path().unwrap().display());
    tokio::time::sleep(Duration::from_secs(60)).await;
    drop(handle);
}

fn procs(cgroup: &Path) -> Vec<String> {
    std::fs::read_to_string(cgroup.join("cgroup.procs"))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn jail_dies_with_its_embedder() {
    if std::env::var(CHILD_ENV).is_ok() {
        return;
    }
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    for mode in ["userns", "no-userns"] {
        // Through the leaf runner: the embedder needs a cgroup of its
        // own (agentjail's leaf rule), not this process's.
        let runner = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/cgroup_leaf_runner.sh"
        );
        let mut child = Command::new("/bin/sh")
            .arg(runner)
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "embedder_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let cgroup = loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("{mode}: embedder exited before spawning"))
                .unwrap();
            // libtest prints "test embedder_child ... " on the same line.
            if let Some((_, p)) = line.split_once("CGROUP=") {
                break PathBuf::from(p);
            }
        };
        assert!(!procs(&cgroup).is_empty(), "{mode}: jail not running");

        child.kill().unwrap(); // SIGKILL: no destructors, no cleanup
        child.wait().unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut left = procs(&cgroup);
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            left = procs(&cgroup);
        }
        if !left.is_empty() {
            // Don't leak the survivors into the rest of the suite.
            let _ = std::fs::write(cgroup.join("cgroup.kill"), "1");
        }
        assert!(
            left.is_empty(),
            "{mode}: jail processes {left:?} outlived the embedder"
        );
        let _ = std::fs::remove_dir(&cgroup);
        let (src, out) = common::setup("pdeathsig", mode);
        common::cleanup(&src, &out);
    }
}
