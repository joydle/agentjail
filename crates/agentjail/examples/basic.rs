//! Basic usage example.
//!
//! Run with: cargo run --example basic

use agentjail::{Jail, JailConfig, preset_build};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create temp directories for demo
    let source = PathBuf::from("/tmp/agentjail-demo-src");
    let output = PathBuf::from("/tmp/agentjail-demo-out");

    std::fs::create_dir_all(&source)?;
    std::fs::create_dir_all(&output)?;

    // Write a simple script to execute
    std::fs::write(
        source.join("hello.sh"),
        "#!/bin/sh\necho 'Hello from jail!'\n",
    )?;

    // Using a preset
    let config = preset_build(&source, &output);

    // Or configure manually
    let config = JailConfig {
        source: source.clone(),
        output: output.clone(),
        memory_mb: 256,
        timeout_secs: 10,
        ..Default::default()
    };

    let jail = Jail::new(config)?;

    println!("Running command in jail...");
    let result = jail.run("/bin/sh", &["/workspace/hello.sh"]).await?;

    println!("Exit code: {}", result.exit_code);
    println!("Stdout: {}", String::from_utf8_lossy(&result.stdout));
    println!("Stderr: {}", String::from_utf8_lossy(&result.stderr));
    println!("Duration: {:?}", result.duration);
    println!("Timed out: {}", result.timed_out);

    // Cleanup
    std::fs::remove_dir_all(&source)?;
    std::fs::remove_dir_all(&output)?;

    Ok(())
}
