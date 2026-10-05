//! GPU passthrough tests.
//!
//! Tests that run on any machine (no GPU required) are always active.
//! Tests that need a real GPU are gated behind /dev/nvidiactl detection.
//!
//! Run with: docker compose run --rm dev cargo test --test gpu_test

mod common;

use agentjail::{GpuConfig, Jail, JailConfig};
use std::fs;
use std::path::PathBuf;

fn setup(name: &str) -> (PathBuf, PathBuf) { common::setup("gpu", name) }
fn cleanup(src: &PathBuf, out: &PathBuf) { common::cleanup(src, out) }
fn base_config(src: PathBuf, out: PathBuf) -> JailConfig { common::full_sandbox_config(src, out) }

fn has_nvidia_gpu() -> bool {
    PathBuf::from("/dev/nvidiactl").exists()
}

// ---------------------------------------------------------------------------
// Tests that work without a GPU
// ---------------------------------------------------------------------------

/// GPU must not be accessible when gpu.enabled = false.
#[tokio::test]
async fn test_gpu_disabled_no_nvidia_devices() {
    let (src, out) = setup("disabled");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nls /dev/nvidia* 2>&1 && echo GPU_VISIBLE || echo GPU_HIDDEN\n",
    )
    .unwrap();

    let config = base_config(src.clone(), out.clone());
    // gpu.enabled is false by default
    assert!(!config.gpu.enabled);

    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("GPU_HIDDEN"),
        "GPU devices should not be visible when disabled, got: {stdout}"
    );

    cleanup(&src, &out);
}

/// Enabling GPU without a GPU present must return a clear error at Jail::new() time.
#[tokio::test]
async fn test_gpu_enabled_no_hardware_returns_error() {
    if has_nvidia_gpu() {
        eprintln!("SKIP: GPU present, can't test missing-GPU error path");
        return;
    }

    let (src, out) = setup("no-hw");
    let mut config = base_config(src.clone(), out.clone());
    config.gpu = GpuConfig {
        enabled: true,
        devices: vec![],
    };

    match Jail::new(config) {
        Ok(_) => panic!("Jail::new() should fail when GPU enabled but no NVIDIA hardware"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("NVIDIA") || msg.contains("nvidiactl"),
                "Error should mention NVIDIA, got: {msg}"
            );
        }
    }

    cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Tests that need a real NVIDIA GPU
// ---------------------------------------------------------------------------

/// nvidia-smi must work inside the jail when GPU is enabled.
#[tokio::test]
async fn test_gpu_nvidia_smi() {
    if !has_nvidia_gpu() {
        eprintln!("SKIP: no NVIDIA GPU detected");
        return;
    }

    let (src, out) = setup("smi");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nnvidia-smi --query-gpu=name --format=csv,noheader 2>&1 || echo GPU_FAILED\n",
    )
    .unwrap();

    let mut config = base_config(src.clone(), out.clone());
    config.gpu = GpuConfig {
        enabled: true,
        devices: vec![],
    };

    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        !stdout.contains("GPU_FAILED"),
        "nvidia-smi should work, got stdout={:?} stderr={:?}",
        stdout.trim(),
        String::from_utf8_lossy(&r.stderr).trim()
    );
    // nvidia-smi should output something like "NVIDIA GeForce RTX 4090"
    assert!(
        !stdout.trim().is_empty(),
        "nvidia-smi should output GPU name"
    );

    cleanup(&src, &out);
}

/// CUDA should be able to query device properties via python.
#[tokio::test]
async fn test_gpu_cuda_device_query() {
    if !has_nvidia_gpu() {
        eprintln!("SKIP: no NVIDIA GPU detected");
        return;
    }

    let (src, out) = setup("cuda-query");
    fs::write(
        src.join("t.sh"),
        concat!(
            "#!/bin/sh\n",
            "python3 -c \"\n",
            "try:\n",
            "    import ctypes\n",
            "    libcuda = ctypes.CDLL('libcuda.so.1')\n",
            "    ret = libcuda.cuInit(0)\n",
            "    if ret == 0:\n",
            "        count = ctypes.c_int(0)\n",
            "        libcuda.cuDeviceGetCount(ctypes.byref(count))\n",
            "        print(f'CUDA_DEVICES={count.value}')\n",
            "    else:\n",
            "        print(f'CUDA_INIT_FAILED={ret}')\n",
            "except Exception as e:\n",
            "    print(f'CUDA_ERROR={e}')\n",
            "\" 2>&1\n",
        ),
    )
    .unwrap();

    let mut config = base_config(src.clone(), out.clone());
    config.gpu = GpuConfig {
        enabled: true,
        devices: vec![],
    };

    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("CUDA_DEVICES="),
        "CUDA device query should succeed, got stdout={:?} stderr={:?}",
        stdout.trim(),
        String::from_utf8_lossy(&r.stderr).trim()
    );

    cleanup(&src, &out);
}

/// Specific GPU filtering: requesting GPU index 99 should fail at Jail::new().
#[tokio::test]
async fn test_gpu_device_filter_nonexistent() {
    if !has_nvidia_gpu() {
        eprintln!("SKIP: no NVIDIA GPU detected");
        return;
    }

    let (src, out) = setup("filter-bad");
    let mut config = base_config(src.clone(), out.clone());
    config.gpu = GpuConfig {
        enabled: true,
        devices: vec![99], // GPU 99 doesn't exist
    };

    let result = Jail::new(config);

    assert!(
        result.is_err(),
        "Jail::new() should fail when requesting non-existent GPU index"
    );

    cleanup(&src, &out);
}

/// GPU 0 should be selectable when it exists.
#[tokio::test]
async fn test_gpu_device_filter_gpu0() {
    if !has_nvidia_gpu() {
        eprintln!("SKIP: no NVIDIA GPU detected");
        return;
    }
    if !PathBuf::from("/dev/nvidia0").exists() {
        eprintln!("SKIP: /dev/nvidia0 not found");
        return;
    }

    let (src, out) = setup("filter-gpu0");
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\nls /dev/nvidia0 2>&1 && echo GPU0_OK || echo GPU0_MISSING\n",
    )
    .unwrap();

    let mut config = base_config(src.clone(), out.clone());
    config.gpu = GpuConfig {
        enabled: true,
        devices: vec![0],
    };

    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    assert!(
        stdout.contains("GPU0_OK"),
        "GPU 0 should be visible, got: {stdout}"
    );

    cleanup(&src, &out);
}
