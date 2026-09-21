//! Integration tests for live forking.
//!
//! Run with: cargo test --test fork_test

mod common;

use agentjail::{CloneMethod, Jail, JailConfig};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

fn setup_dirs(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    // Use thread ID to avoid collisions between parallel tests
    let tid = std::thread::current().id();
    let unique = format!("{name}-{tid:?}").replace(|c: char| !c.is_alphanumeric(), "_");
    let (src, out) = common::setup("fork", &unique);
    let fork_out = PathBuf::from(format!("/tmp/aj-fork-{unique}-fork"));
    let _ = fs::remove_dir_all(&fork_out);
    (src, out, fork_out)
}

fn cleanup(source: &PathBuf, output: &PathBuf, fork_output: &PathBuf) {
    common::cleanup(source, output);
    let _ = fs::remove_dir_all(fork_output);
}

fn test_config(source: PathBuf, output: PathBuf) -> JailConfig { common::lightweight_config(source, output) }

#[tokio::test]
async fn test_live_fork_clones_filesystem() {
    let (source, output, fork_output) = setup_dirs("clone");

    fs::write(output.join("state.txt"), "original-state").unwrap();
    fs::create_dir_all(output.join("subdir")).unwrap();
    fs::write(output.join("subdir/nested.txt"), "nested-data").unwrap();

    fs::write(
        source.join("read.sh"),
        "#!/bin/sh\ncat /output/state.txt && cat /output/subdir/nested.txt\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, info) = jail.live_fork(None, &fork_output).unwrap();
    let result = forked.run("/bin/sh", &["/workspace/read.sh"]).await.unwrap();

    assert_eq!(result.exit_code, 0, "stderr: {}", String::from_utf8_lossy(&result.stderr));
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("original-state"), "got: {stdout}");
    assert!(stdout.contains("nested-data"), "got: {stdout}");
    assert_eq!(info.files_cloned, 2);
    assert!(info.bytes_cloned > 0);
    assert!(info.clone_method == CloneMethod::Reflink || info.clone_method == CloneMethod::Copy);

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_independence() {
    let (source, output, fork_output) = setup_dirs("indep");

    fs::write(output.join("data.txt"), "shared").unwrap();
    fs::write(
        source.join("modify.sh"),
        "#!/bin/sh\necho 'modified' > /output/data.txt\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, _) = jail.live_fork(None, &fork_output).unwrap();
    let result = forked.run("/bin/sh", &["/workspace/modify.sh"]).await.unwrap();
    assert_eq!(result.exit_code, 0);

    assert_eq!(fs::read_to_string(output.join("data.txt")).unwrap(), "shared");
    assert!(fs::read_to_string(fork_output.join("data.txt")).unwrap().contains("modified"));

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_while_running() {
    let (source, output, fork_output) = setup_dirs("running");

    fs::write(
        source.join("long.sh"),
        "#!/bin/sh\necho 'running' > /output/marker.txt\nsleep 30\n",
    )
    .unwrap();
    fs::write(
        source.join("check.sh"),
        "#!/bin/sh\ncat /output/marker.txt 2>/dev/null || echo 'NO_MARKER'\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let original = jail.spawn("/bin/sh", &["/workspace/long.sh"]).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Fork while original is running (with freeze)
    let (forked, _info) = jail.live_fork(Some(&original), &fork_output).unwrap();
    let fork_result = forked.run("/bin/sh", &["/workspace/check.sh"]).await.unwrap();

    assert_eq!(fork_result.exit_code, 0, "stderr: {}", String::from_utf8_lossy(&fork_result.stderr));
    let stdout = String::from_utf8_lossy(&fork_result.stdout);
    assert!(stdout.contains("running") || stdout.contains("NO_MARKER"), "got: {stdout}");

    original.kill();
    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_multiple() {
    let (source, output, fork_output) = setup_dirs("multi");
    let fork_output2 = PathBuf::from("/tmp/agentjail-fork-multi-fork2");
    let _ = fs::remove_dir_all(&fork_output2);

    fs::write(output.join("counter.txt"), "0").unwrap();
    fs::write(source.join("read.sh"), "#!/bin/sh\ncat /output/counter.txt\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (f1, i1) = jail.live_fork(None, &fork_output).unwrap();
    let (f2, i2) = jail.live_fork(None, &fork_output2).unwrap();

    let r1 = f1.run("/bin/sh", &["/workspace/read.sh"]).await.unwrap();
    let r2 = f2.run("/bin/sh", &["/workspace/read.sh"]).await.unwrap();

    assert_eq!(r1.exit_code, 0);
    assert_eq!(r2.exit_code, 0);
    assert!(i1.files_cloned > 0);
    assert!(i2.files_cloned > 0);
    assert!(String::from_utf8_lossy(&r1.stdout).contains("0"));
    assert!(String::from_utf8_lossy(&r2.stdout).contains("0"));

    cleanup(&source, &output, &fork_output);
    let _ = fs::remove_dir_all(&fork_output2);
}

// -----------------------------------------------------------------------
// Advanced scenarios
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_live_fork_different_command() {
    let (source, output, fork_output) = setup_dirs("diffcmd");

    fs::write(output.join("state.txt"), "42").unwrap();
    fs::write(source.join("reader.sh"), "#!/bin/sh\necho \"state=$(cat /output/state.txt)\"\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, _) = jail.live_fork(None, &fork_output).unwrap();
    let result = forked.run("/bin/sh", &["/workspace/reader.sh"]).await.unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(String::from_utf8_lossy(&result.stdout).contains("state=42"));

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_chain() {
    let (source, output, fork_output) = setup_dirs("chain");
    let fork_output2 = PathBuf::from("/tmp/agentjail-fork-chain-fork2");
    let _ = fs::remove_dir_all(&fork_output2);

    fs::write(output.join("generation.txt"), "gen-0").unwrap();
    fs::write(
        source.join("evolve.sh"),
        "#!/bin/sh\ncat /output/generation.txt\necho 'evolved' > /output/evolved.txt\n",
    )
    .unwrap();
    fs::write(
        source.join("read_both.sh"),
        "#!/bin/sh\ncat /output/generation.txt; echo; ls /output/\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    // First fork — runs evolve.sh which creates evolved.txt
    let (f1, _) = jail.live_fork(None, &fork_output).unwrap();
    let r1 = f1.run("/bin/sh", &["/workspace/evolve.sh"]).await.unwrap();
    assert_eq!(r1.exit_code, 0);
    assert!(fork_output.join("evolved.txt").exists());

    // Fork-of-fork: new jail from fork_output, then fork it again
    let fork_config = test_config(source.clone(), fork_output.clone());
    let fork_jail = Jail::new(fork_config).unwrap();

    let (f2, info2) = fork_jail.live_fork(None, &fork_output2).unwrap();
    let r2 = f2.run("/bin/sh", &["/workspace/read_both.sh"]).await.unwrap();

    assert_eq!(r2.exit_code, 0);
    let stdout2 = String::from_utf8_lossy(&r2.stdout);
    assert!(stdout2.contains("gen-0"), "got: {stdout2}");
    assert!(stdout2.contains("evolved.txt"), "got: {stdout2}");
    assert!(info2.files_cloned >= 2);

    cleanup(&source, &output, &fork_output);
    let _ = fs::remove_dir_all(&fork_output2);
}

#[tokio::test]
async fn test_live_fork_original_exits_first() {
    let (source, output, fork_output) = setup_dirs("exit-first");

    fs::write(output.join("data.txt"), "snapshot").unwrap();
    fs::write(source.join("quick.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(source.join("slow.sh"), "#!/bin/sh\nsleep 0.2\ncat /output/data.txt\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let original = jail.spawn("/bin/sh", &["/workspace/quick.sh"]).unwrap();

    // Fork, then use normal spawn on the forked jail
    let (forked, _) = jail.live_fork(Some(&original), &fork_output).unwrap();
    let fork_handle = forked.spawn("/bin/sh", &["/workspace/slow.sh"]).unwrap();

    // Original exits first
    let orig_result = original.wait().await.unwrap();
    assert_eq!(orig_result.exit_code, 0);

    // Fork survives
    let fork_result = fork_handle.wait().await.unwrap();
    assert_eq!(fork_result.exit_code, 0, "stderr: {}", String::from_utf8_lossy(&fork_result.stderr));
    assert!(String::from_utf8_lossy(&fork_result.stdout).contains("snapshot"));

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_timeout() {
    let (source, output, fork_output) = setup_dirs("timeout");

    fs::write(output.join("data.txt"), "before-timeout").unwrap();
    fs::write(source.join("hang.sh"), "#!/bin/sh\nsleep 100\n").unwrap();

    let mut config = test_config(source.clone(), output.clone());
    config.timeout_secs = 2;

    let jail = Jail::new(config).unwrap();

    let (forked, _) = jail.live_fork(None, &fork_output).unwrap();
    let result = forked.run("/bin/sh", &["/workspace/hang.sh"]).await.unwrap();

    assert!(result.timed_out || result.exit_code != 0);
    assert!(result.duration.as_secs() < 10);

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_symlink_safety_in_output() {
    let (source, output, fork_output) = setup_dirs("symsafe");

    let secret = PathBuf::from("/tmp/agentjail-fork-symsafe-secret");
    fs::write(&secret, "TOP_SECRET").unwrap();
    std::os::unix::fs::symlink(&secret, output.join("escape")).unwrap();
    fs::write(output.join("legit.txt"), "safe").unwrap();
    fs::write(source.join("check.sh"), "#!/bin/sh\nls /output/\ncat /output/legit.txt\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, info) = jail.live_fork(None, &fork_output).unwrap();

    assert!(!fork_output.join("escape").exists(), "Symlink should not be copied");
    assert_eq!(info.files_cloned, 1);

    let result = forked.run("/bin/sh", &["/workspace/check.sh"]).await.unwrap();
    assert_eq!(result.exit_code, 0);

    cleanup(&source, &output, &fork_output);
    let _ = fs::remove_file(&secret);
}

#[tokio::test]
async fn test_live_fork_preserves_binary_data_in_jail() {
    let (source, output, fork_output) = setup_dirs("binary");

    let binary_data: Vec<u8> = (0..=255).collect();
    fs::write(output.join("data.bin"), &binary_data).unwrap();
    fs::write(source.join("check_bin.sh"), "#!/bin/sh\nwc -c < /output/data.bin\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, info) = jail.live_fork(None, &fork_output).unwrap();
    assert_eq!(info.bytes_cloned, 256);
    assert_eq!(fs::read(fork_output.join("data.bin")).unwrap(), binary_data);

    let result = forked.run("/bin/sh", &["/workspace/check_bin.sh"]).await.unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(String::from_utf8_lossy(&result.stdout).trim().contains("256"));

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_creates_output_dir() {
    let (source, output, _fork_output) = setup_dirs("autocreate");
    let deep_fork = PathBuf::from("/tmp/agentjail-fork-autocreate-deep/a/b/c");
    let _ = fs::remove_dir_all("/tmp/agentjail-fork-autocreate-deep");

    fs::write(output.join("file.txt"), "hello").unwrap();
    fs::write(source.join("read.sh"), "#!/bin/sh\ncat /output/file.txt\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, _) = jail.live_fork(None, &deep_fork).unwrap();
    let result = forked.run("/bin/sh", &["/workspace/read.sh"]).await.unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(String::from_utf8_lossy(&result.stdout).contains("hello"));

    cleanup(&source, &output, &_fork_output);
    let _ = fs::remove_dir_all("/tmp/agentjail-fork-autocreate-deep");
}

#[tokio::test]
async fn test_live_fork_info_accuracy() {
    let (source, output, fork_output) = setup_dirs("forkinfo");

    fs::write(output.join("ten.txt"), "0123456789").unwrap();
    fs::write(output.join("five.txt"), "abcde").unwrap();
    fs::create_dir_all(output.join("sub")).unwrap();
    fs::write(output.join("sub/three.txt"), "xyz").unwrap();
    fs::write(source.join("noop.sh"), "#!/bin/sh\ntrue\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (_forked, info) = jail.live_fork(None, &fork_output).unwrap();

    assert_eq!(info.files_cloned, 3);
    assert_eq!(info.bytes_cloned, 18);
    assert!(!info.was_frozen);
    assert!(info.clone_duration < Duration::from_secs(5));
    assert!(info.files_cow <= info.files_cloned);

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_with_events() {
    let (source, output, fork_output) = setup_dirs("events");

    fs::write(output.join("state.txt"), "event-test").unwrap();
    fs::write(
        source.join("echo.sh"),
        "#!/bin/sh\necho 'fork-stdout-line'\ncat /output/state.txt\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    // Fork, then use spawn_with_events on the forked jail
    let (forked, _) = jail.live_fork(None, &fork_output).unwrap();
    let (handle, _rx) = forked.spawn_with_events("/bin/sh", &["/workspace/echo.sh"]).unwrap();

    let result = handle.wait().await.unwrap();
    assert_eq!(result.exit_code, 0);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("fork-stdout-line"));
    assert!(stdout.contains("event-test"));

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_concurrent_from_running() {
    let (source, output, fork_output) = setup_dirs("concurrent");
    let fork_output2 = PathBuf::from("/tmp/agentjail-fork-concurrent-fork2");
    let fork_output3 = PathBuf::from("/tmp/agentjail-fork-concurrent-fork3");
    let _ = fs::remove_dir_all(&fork_output2);
    let _ = fs::remove_dir_all(&fork_output3);

    fs::write(output.join("shared.txt"), "base-state").unwrap();
    fs::write(source.join("long.sh"), "#!/bin/sh\nsleep 30\n").unwrap();
    fs::write(source.join("read.sh"), "#!/bin/sh\ncat /output/shared.txt\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let original = jail.spawn("/bin/sh", &["/workspace/long.sh"]).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Fork 3 times, then spawn on each
    let (f1, i1) = jail.live_fork(Some(&original), &fork_output).unwrap();
    let (f2, i2) = jail.live_fork(Some(&original), &fork_output2).unwrap();
    let (f3, i3) = jail.live_fork(Some(&original), &fork_output3).unwrap();

    let h1 = f1.spawn("/bin/sh", &["/workspace/read.sh"]).unwrap();
    let h2 = f2.spawn("/bin/sh", &["/workspace/read.sh"]).unwrap();
    let h3 = f3.spawn("/bin/sh", &["/workspace/read.sh"]).unwrap();

    let (r1, r2, r3) = tokio::join!(h1.wait(), h2.wait(), h3.wait());
    let r1 = r1.unwrap();
    let r2 = r2.unwrap();
    let r3 = r3.unwrap();

    assert_eq!(r1.exit_code, 0);
    assert_eq!(r2.exit_code, 0);
    assert_eq!(r3.exit_code, 0);
    assert!(String::from_utf8_lossy(&r1.stdout).contains("base-state"));
    assert!(String::from_utf8_lossy(&r2.stdout).contains("base-state"));
    assert!(String::from_utf8_lossy(&r3.stdout).contains("base-state"));
    assert!(i1.files_cloned > 0);
    assert!(i2.files_cloned > 0);
    assert!(i3.files_cloned > 0);

    original.kill();
    cleanup(&source, &output, &fork_output);
    let _ = fs::remove_dir_all(&fork_output2);
    let _ = fs::remove_dir_all(&fork_output3);
}

#[tokio::test]
async fn test_live_fork_empty_output() {
    let (source, output, fork_output) = setup_dirs("emptyout");

    fs::write(source.join("list.sh"), "#!/bin/sh\nls /output/ | wc -l\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, info) = jail.live_fork(None, &fork_output).unwrap();
    assert_eq!(info.files_cloned, 0);
    assert_eq!(info.bytes_cloned, 0);

    let result = forked.run("/bin/sh", &["/workspace/list.sh"]).await.unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(String::from_utf8_lossy(&result.stdout).trim() == "0");

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_preserves_file_permissions_in_jail() {
    use std::os::unix::fs::PermissionsExt;

    let (source, output, fork_output) = setup_dirs("perms");

    fs::write(output.join("run.sh"), "#!/bin/sh\necho 'executable'\n").unwrap();
    fs::set_permissions(output.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    // Use /bin/sh to source the script instead of direct exec (avoids ETXTBSY
    // race on overlayfs where COW copies may retain transient write handles).
    fs::write(source.join("exec.sh"), "#!/bin/sh\n/bin/sh /output/run.sh\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, _) = jail.live_fork(None, &fork_output).unwrap();

    // Verify permissions are preserved on the cloned file.
    let perms = fs::metadata(fork_output.join("run.sh")).unwrap().permissions().mode();
    assert_eq!(perms & 0o111, 0o111, "Execute bits preserved, got {perms:o}");

    let result = forked.run("/bin/sh", &["/workspace/exec.sh"]).await.unwrap();
    assert_eq!(
        result.exit_code, 0,
        "fork perm test: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("executable"));

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_deep_nesting() {
    let (source, output, fork_output) = setup_dirs("deep");

    let deep = output.join("a/b/c/d/e");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("leaf.txt"), "deep-value").unwrap();
    fs::write(output.join("a/b/mid.txt"), "mid-value").unwrap();
    fs::write(output.join("root.txt"), "root-value").unwrap();
    fs::write(
        source.join("read.sh"),
        "#!/bin/sh\ncat /output/root.txt\ncat /output/a/b/mid.txt\ncat /output/a/b/c/d/e/leaf.txt\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, info) = jail.live_fork(None, &fork_output).unwrap();
    assert_eq!(info.files_cloned, 3);

    let result = forked.run("/bin/sh", &["/workspace/read.sh"]).await.unwrap();
    assert_eq!(result.exit_code, 0);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("deep-value"));
    assert!(stdout.contains("mid-value"));
    assert!(stdout.contains("root-value"));
    assert_eq!(fs::read_to_string(fork_output.join("a/b/c/d/e/leaf.txt")).unwrap(), "deep-value");

    cleanup(&source, &output, &fork_output);
}

#[tokio::test]
async fn test_live_fork_many_files() {
    let (source, output, fork_output) = setup_dirs("many");

    for i in 0..50 {
        let dir = output.join(format!("dir{}", i % 5));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("file{i}.txt")), format!("content-{i}")).unwrap();
    }
    fs::write(
        source.join("count.sh"),
        "#!/bin/sh\nfind /output -type f | wc -l\ncat /output/dir0/file0.txt\ncat /output/dir4/file49.txt\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let (forked, info) = jail.live_fork(None, &fork_output).unwrap();
    assert_eq!(info.files_cloned, 50);
    assert!(info.bytes_cloned > 0);
    assert!(info.clone_duration.as_secs() < 5);

    let result = forked.run("/bin/sh", &["/workspace/count.sh"]).await.unwrap();
    assert_eq!(result.exit_code, 0);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("50"));
    assert!(stdout.contains("content-0"));
    assert!(stdout.contains("content-49"));

    cleanup(&source, &output, &fork_output);
}
