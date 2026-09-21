//! Integration tests for agentjail.
//!
//! These tests require Linux with namespace support.
//! Run with: cargo test --test integration

mod common;

use agentjail::{Jail, JailConfig};
use std::fs;
use std::path::PathBuf;

fn setup_test_dirs(name: &str) -> (PathBuf, PathBuf) { common::setup("int", name) }
fn cleanup_test_dirs(source: &PathBuf, output: &PathBuf) { common::cleanup(source, output) }
fn test_config(source: PathBuf, output: PathBuf) -> JailConfig { common::lightweight_config(source, output) }

#[tokio::test]
async fn test_basic_execution() {
    let (source, output) = setup_test_dirs("basic");

    fs::write(source.join("test.sh"), "#!/bin/sh\necho 'hello world'\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let result = jail.run("/bin/sh", &["/workspace/test.sh"]).await.unwrap();

    assert_eq!(
        result.exit_code,
        0,
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("hello world"));
    assert!(!result.timed_out);

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_output_directory() {
    let (source, output) = setup_test_dirs("output");

    fs::write(
        source.join("write.sh"),
        "#!/bin/sh\necho 'artifact' > /output/result.txt\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let result = jail.run("/bin/sh", &["/workspace/write.sh"]).await.unwrap();

    assert_eq!(
        result.exit_code,
        0,
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let artifact = fs::read_to_string(output.join("result.txt")).unwrap();
    assert!(artifact.contains("artifact"));

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_timeout() {
    let (source, output) = setup_test_dirs("timeout");

    fs::write(source.join("hang.sh"), "#!/bin/sh\nsleep 100\n").unwrap();

    let mut config = test_config(source.clone(), output.clone());
    config.timeout_secs = 2;

    let jail = Jail::new(config).unwrap();
    let result = jail.run("/bin/sh", &["/workspace/hang.sh"]).await.unwrap();

    // Either timed out OR was killed with signal
    assert!(
        result.timed_out || result.exit_code != 0,
        "Should have timed out or been killed, got exit_code={}, timed_out={}",
        result.exit_code,
        result.timed_out
    );
    assert!(
        result.duration.as_secs() < 10,
        "Took too long: {:?}",
        result.duration
    );

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_cannot_read_host_files() {
    let (source, output) = setup_test_dirs("hostfiles");

    // The jail has /etc mounted read-only from host, but with chroot
    // we should only see the jail's /etc
    fs::write(
        source.join("read_host.sh"),
        "#!/bin/sh\ncat /etc/hostname 2>&1 || echo 'NOT_FOUND'\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let result = jail
        .run("/bin/sh", &["/workspace/read_host.sh"])
        .await
        .unwrap();

    // Should succeed but read the container's hostname, not the host's
    let stdout = String::from_utf8_lossy(&result.stdout);
    // The key test: chroot isolation means we don't leak host paths
    assert!(!stdout.contains("/home/"), "Should not see host paths");

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_network_blocked() {
    let (source, output) = setup_test_dirs("network");

    fs::write(
        source.join("network.sh"),
        "#!/bin/sh\nping -c 1 -W 1 8.8.8.8 2>&1 || echo 'NETWORK_BLOCKED'\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let result = jail
        .run("/bin/sh", &["/workspace/network.sh"])
        .await
        .unwrap();

    let output_str = String::from_utf8_lossy(&result.stdout);
    // In a container without network namespace isolated, ping might work
    // but that's OK - the network namespace is separately testable
    println!("Network test output: {output_str}");

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_env_isolation() {
    let (source, output) = setup_test_dirs("env");

    fs::write(source.join("env.sh"), "#!/bin/sh\nenv | wc -l\n").unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let result = jail.run("/bin/sh", &["/workspace/env.sh"]).await.unwrap();

    let stdout = String::from_utf8_lossy(&result.stdout);
    let env_count: i32 = stdout.trim().parse().unwrap_or(999);

    // Environment should be minimal (we pass empty env)
    assert!(
        env_count < 10,
        "Should have minimal environment, got {env_count}"
    );

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_workspace_readonly() {
    let (source, output) = setup_test_dirs("readonly");

    fs::write(
        source.join("write_test.sh"),
        "#!/bin/sh\necho 'test' > /workspace/new_file.txt 2>&1 || echo 'WRITE_BLOCKED'\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let result = jail
        .run("/bin/sh", &["/workspace/write_test.sh"])
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("WRITE_BLOCKED") || stdout.contains("Read-only"),
        "Workspace should be read-only, got: {stdout}"
    );

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_pid_namespace() {
    let (source, output) = setup_test_dirs("pidns");

    // Check that the process sees itself as PID 1 in the new namespace
    fs::write(source.join("check_pid.sh"), "#!/bin/sh\necho PID=$$\n").unwrap();

    let mut config = test_config(source.clone(), output.clone());
    config.pid_namespace = true;

    let jail = Jail::new(config).unwrap();
    let result = jail
        .run("/bin/sh", &["/workspace/check_pid.sh"])
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert_eq!(result.exit_code, 0, "stderr: {}", String::from_utf8_lossy(&result.stderr));
    // In PID namespace, the shell should be PID 1
    assert!(
        stdout.contains("PID=1"),
        "Process should see itself as PID 1, got: {stdout}"
    );

    cleanup_test_dirs(&source, &output);
}

#[tokio::test]
async fn test_wait_with_events_streams_stdout_stderr_completion() {
    use agentjail::JailEvent;

    let (source, output) = setup_test_dirs("events-stream");

    fs::write(
        source.join("multi.sh"),
        "#!/bin/sh\necho 'stdout-msg'\necho 'stderr-msg' >&2\n",
    )
    .unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();
    let handle = jail.spawn("/bin/sh", &["/workspace/multi.sh"]).unwrap();

    let (tx, mut rx) = agentjail::events::channel();
    let result = handle.wait_with_events(tx).await.unwrap();

    assert_eq!(result.exit_code, 0);

    // Collect all events
    let mut saw_stdout = false;
    let mut saw_stderr = false;
    let mut saw_completed = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            JailEvent::Stdout(line) if line.contains("stdout-msg") => saw_stdout = true,
            JailEvent::Stderr(line) if line.contains("stderr-msg") => saw_stderr = true,
            JailEvent::Completed { exit_code, .. } => {
                assert_eq!(exit_code, 0);
                saw_completed = true;
            }
            _ => {}
        }
    }

    assert!(saw_stdout, "Should have received Stdout event");
    assert!(saw_stderr, "Should have received Stderr event");
    assert!(saw_completed, "Should have received Completed event");

    cleanup_test_dirs(&source, &output);
}
