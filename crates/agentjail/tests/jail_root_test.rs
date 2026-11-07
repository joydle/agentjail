//! Spawns leave no `$TMPDIR/agentjail-*` jail roots behind.
//!
//! Its own test binary: other tests' jails would show up in the
//! directory listing while they run. Needs root; skips otherwise.

mod common;

use agentjail::{Jail, JailConfig};
use std::collections::HashSet;
use std::path::PathBuf;

fn jail_roots() -> HashSet<PathBuf> {
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("agentjail-"))
        .map(|e| e.path())
        .collect()
}

#[tokio::test]
async fn spawns_leave_no_jail_root_behind() {
    if !common::is_root() {
        eprintln!("skip: needs root");
        return;
    }
    let (src, out) = common::setup("jailroot", "n");
    let before = jail_roots();
    for userns in [false, true] {
        let jail = Jail::new(JailConfig {
            source: src.clone(),
            output: out.clone(),
            user_namespace: userns,
            landlock: false,
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            ..Default::default()
        })
        .unwrap();
        for _ in 0..3 {
            let o = jail.run("/bin/true", &[]).await.unwrap();
            assert_eq!(o.exit_code, 0, "{}", String::from_utf8_lossy(&o.stderr));
        }
        // A dropped (killed, never waited) handle cleans up too.
        drop(jail.spawn("/bin/sleep", &["30"]).unwrap());
        // So does a failed spawn.
        assert!(jail.spawn("/nonexistent", &[]).is_ok_and(|h| {
            drop(h);
            true
        }));
    }
    let leaked: Vec<_> = jail_roots().difference(&before).cloned().collect();
    assert!(leaked.is_empty(), "jail roots left behind: {leaked:?}");
    common::cleanup(&src, &out);
}
