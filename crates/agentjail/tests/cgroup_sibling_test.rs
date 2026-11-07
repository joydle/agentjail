//! agentjail must never migrate processes it doesn't own out of its
//! cgroup into `agentjail-init`; it moves only itself.
//!
//! Lives in its own test binary: the cgroup base is initialised once per
//! process, and this test needs to observe that first initialisation.

mod common;

use agentjail::Jail;
use std::time::Duration;

fn cgroup_of(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn unrelated_process_in_our_cgroup_is_not_moved() {
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    let ours = cgroup_of(std::process::id());
    if ours == "/" || ours.is_empty() {
        eprintln!("skip: in the root cgroup (no migration needed there)");
        return;
    }
    // A process that is *not* our descendant: `sh` backgrounds a sleeper
    // and exits, so the sleeper is reparented away from us.
    let out = std::process::Command::new("sh")
        .args(["-c", "sleep 60 >/dev/null 2>&1 & echo $!"])
        .output()
        .unwrap();
    let stranger: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let cg_dir = format!("/sys/fs/cgroup{ours}");
    std::fs::write(format!("{cg_dir}/cgroup.procs"), stranger.to_string()).unwrap();
    assert_eq!(cgroup_of(stranger), ours);

    let (src, outd) = common::setup("cgsib", "x");
    let mut config = common::lightweight_config(src.clone(), outd.clone());
    config.memory_mb = Some(64);
    let jail = Jail::new(config).unwrap();
    let res = jail.spawn("/bin/true", &[]);

    let stranger_cg = cgroup_of(stranger);
    unsafe { libc::kill(stranger as i32, libc::SIGKILL) };
    common::cleanup(&src, &outd);

    assert_eq!(
        stranger_cg, ours,
        "agentjail moved a process it does not own out of its cgroup"
    );
    match res {
        // Can't delegate controllers while a stranger shares the cgroup:
        // a clear error, not a silent migration.
        Err(e) => assert!(e.to_string().contains("will not move"), "{e}"),
        Ok(_) => panic!("controllers can't be enabled with a stranger in the cgroup"),
    }
}
