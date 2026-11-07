//! The host firewall comes back if something removes it.
//!
//! Its own test binary: it deletes the `inet agentjail` table, which
//! would race the isolation assertions in net_isolation_test. Needs
//! root and `nft`; skips otherwise.

mod common;

use agentjail::{FirewallMode, Jail, Network};
use std::process::Command;

fn table_present() -> bool {
    Command::new("nft")
        .args(["list", "table", "inet", "agentjail"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn allowlist_spawn_reinstalls_a_removed_firewall() {
    if !common::is_root() || Command::new("nft").arg("--version").output().is_err() {
        eprintln!("skip: needs root and nft");
        return;
    }
    let (src, out) = common::setup("firewall", "reinstall");
    let mut cfg = common::lightweight_config(src.clone(), out.clone());
    cfg.network = Network::Allowlist(vec!["example.invalid".into()]);
    cfg.firewall = Some(FirewallMode::Require);
    let jail = Jail::new(cfg).unwrap();

    jail.run("/bin/true", &[]).await.unwrap();
    assert!(table_present(), "first Allowlist spawn installs the table");

    // What `nft flush ruleset` (or a firewall reload) does to it.
    let del = Command::new("nft")
        .args(["delete", "table", "inet", "agentjail"])
        .status()
        .unwrap();
    assert!(del.success());
    assert!(!table_present());

    jail.run("/bin/true", &[]).await.unwrap();
    assert!(
        table_present(),
        "the next Allowlist spawn must reinstall it"
    );
    common::cleanup(&src, &out);
}
