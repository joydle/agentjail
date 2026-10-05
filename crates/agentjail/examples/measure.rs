use agentjail::{Jail, JailConfig, SeccompLevel};
use std::fs;

#[tokio::main]
async fn main() {
    let source = std::path::PathBuf::from("/tmp/measure-src");
    let output = std::path::PathBuf::from("/tmp/measure-out");

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_dir_all(&output);
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&output).unwrap();

    let config = JailConfig {
        source: source.clone(),
        output: output.clone(),
        timeout_secs: 60,
        user_namespace: false,
        seccomp: SeccompLevel::Disabled,
        landlock: false,
        memory_mb: 1024,
        cpu_percent: 400,
        max_pids: 128,
        pid_namespace: true,
        ..Default::default()
    };

    let jail = Jail::new(config).unwrap();

    // Simulate a build-like workload
    let handle = jail
        .spawn(
            "/bin/sh",
            &[
                "-c",
                r#"
        echo Starting build simulation...
        # Allocate some memory (create files in tmpfs)
        for i in $(seq 1 100); do
            dd if=/dev/zero of=/tmp/file$i bs=1024 count=100 2>/dev/null
        done
        echo Memory allocated
        # CPU work
        for i in $(seq 1 1000); do
            echo $i > /dev/null
        done
        echo Done
    "#,
            ],
        )
        .unwrap();

    // Poll stats
    println!("Monitoring resource usage...\n");
    for i in 0..30 {
        if let Some(stats) = handle.stats() {
            println!(
                "[{:>2}] Memory: {:>6} KB | CPU: {:>8} us",
                i,
                stats.memory_peak_bytes / 1024,
                stats.cpu_usage_usec
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let result = handle.wait().await.unwrap();

    println!("\n========== FINAL RESULTS ==========");
    println!("Exit code:   {}", result.exit_code);
    println!("Duration:    {:?}", result.duration);
    if let Some(stats) = result.stats {
        println!(
            "Peak memory: {} KB ({:.1} MB)",
            stats.memory_peak_bytes / 1024,
            stats.memory_peak_bytes as f64 / 1024.0 / 1024.0
        );
        println!(
            "CPU time:    {} us ({:.1} ms)",
            stats.cpu_usage_usec,
            stats.cpu_usage_usec as f64 / 1000.0
        );
    }

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_dir_all(&output);
}
