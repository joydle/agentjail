//! NVIDIA GPU discovery and passthrough.
//!
//! Discovers host NVIDIA devices and libraries at runtime, then mounts
//! them into the jail. Only the specific GPUs requested are exposed.

use crate::config::{Access, GpuConfig};
use crate::error::{JailError, Result};
use crate::mount::bind_mount_dev;
use std::fs;
use std::path::{Path, PathBuf};

/// Discovered NVIDIA resources on the host.
#[derive(Debug, Clone)]
pub struct NvidiaResources {
    /// /dev/nvidiactl (driver control device).
    pub ctl: PathBuf,
    /// /dev/nvidia-uvm (unified virtual memory, needed for CUDA).
    pub uvm: Option<PathBuf>,
    /// Per-GPU devices: /dev/nvidia0, /dev/nvidia1, ...
    pub gpus: Vec<PathBuf>,
    /// Host directories containing NVIDIA shared libraries.
    pub lib_dirs: Vec<PathBuf>,
}

/// Discover NVIDIA devices and libraries on the host.
///
/// Returns an error if no NVIDIA GPU is detected.
pub fn discover(config: &GpuConfig) -> Result<NvidiaResources> {
    // 1. Find control device
    let ctl = PathBuf::from("/dev/nvidiactl");
    if !ctl.exists() {
        return Err(JailError::Gpu(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no NVIDIA GPU found (/dev/nvidiactl missing)",
        )));
    }

    // 2. Find UVM device
    let uvm = PathBuf::from("/dev/nvidia-uvm");
    let uvm = if uvm.exists() { Some(uvm) } else { None };

    // 3. Find per-GPU devices, filtered by config.devices
    let gpus = discover_gpu_devices(&config.devices)?;
    if gpus.is_empty() {
        return Err(JailError::Gpu(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no NVIDIA GPU devices found in /dev",
        )));
    }

    // 4. Find NVIDIA library directories
    let lib_dirs = discover_nvidia_libs();
    if lib_dirs.is_empty() {
        return Err(JailError::Gpu(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "NVIDIA libraries not found (libcuda.so.1 missing)",
        )));
    }

    Ok(NvidiaResources {
        ctl,
        uvm,
        gpus,
        lib_dirs,
    })
}

/// Find /dev/nvidia<N> devices, optionally filtered to specific indices.
fn discover_gpu_devices(filter: &[u32]) -> Result<Vec<PathBuf>> {
    let mut gpus = Vec::new();

    let entries = fs::read_dir("/dev").map_err(JailError::Gpu)?;
    for entry in entries {
        let entry = entry.map_err(JailError::Gpu)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        // Match /dev/nvidia0, /dev/nvidia1, etc. (not nvidiactl, nvidia-uvm)
        if let Some(idx_str) = name.strip_prefix("nvidia")
            && let Ok(idx) = idx_str.parse::<u32>()
                && (filter.is_empty() || filter.contains(&idx)) {
                    gpus.push(entry.path());
                }
    }

    gpus.sort();
    Ok(gpus)
}

/// Search standard paths for NVIDIA shared libraries.
///
/// Returns directories that contain libcuda.so.1.
fn discover_nvidia_libs() -> Vec<PathBuf> {
    let search_paths = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/usr/local/cuda/lib64",
    ];

    let mut dirs = Vec::new();
    for path in &search_paths {
        let dir = Path::new(path);
        if dir.join("libcuda.so.1").exists() || dir.join("libcuda.so").exists() {
            dirs.push(dir.to_path_buf());
        }
    }

    // Also check ldconfig output as a fallback
    if dirs.is_empty()
        && let Some(dir) = find_nvidia_lib_via_ldconfig() {
            dirs.push(dir);
        }

    dirs
}

/// Parse ldconfig -p output to find libcuda.so.1 location.
fn find_nvidia_lib_via_ldconfig() -> Option<PathBuf> {
    let output = std::process::Command::new("ldconfig")
        .arg("-p")
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("libcuda.so.1") {
            // Format: "	libcuda.so.1 (libc6,x86-64) => /usr/lib/x86_64-linux-gnu/libcuda.so.1"
            if let Some(path_str) = line.split("=>").nth(1) {
                let lib_path = Path::new(path_str.trim());
                return lib_path.parent().map(|p| p.to_path_buf());
            }
        }
    }

    None
}

/// Mount NVIDIA devices and libraries into the jail root.
pub fn setup_mounts(new_root: &Path, resources: &NvidiaResources) -> Result<()> {
    // 1. Mount device nodes (read-write, no NODEV flag)
    let dev_dir = new_root.join("dev");
    fs::create_dir_all(&dev_dir).map_err(JailError::Gpu)?;

    // /dev/nvidiactl
    bind_mount_dev(
        &resources.ctl,
        &dev_dir.join("nvidiactl"),
        Access::ReadWrite,
    )?;

    // /dev/nvidia-uvm
    if let Some(ref uvm) = resources.uvm {
        bind_mount_dev(uvm, &dev_dir.join("nvidia-uvm"), Access::ReadWrite)?;
    }

    // /dev/nvidia0, /dev/nvidia1, ...
    for gpu_path in &resources.gpus {
        if let Some(name) = gpu_path.file_name() {
            bind_mount_dev(gpu_path, &dev_dir.join(name), Access::ReadWrite)?;
        }
    }

    // 2. Mount NVIDIA libraries read-only
    let nvidia_lib_dir = new_root.join("usr/lib/nvidia");
    fs::create_dir_all(&nvidia_lib_dir).map_err(JailError::Gpu)?;

    for host_dir in &resources.lib_dirs {
        mount_nvidia_libs_from(host_dir, &nvidia_lib_dir)?;
    }

    Ok(())
}

/// Mount individual NVIDIA .so files from a host directory into the jail.
fn mount_nvidia_libs_from(host_dir: &Path, jail_lib_dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(host_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Mount libcuda*, libnvidia*, libnvoptix*, libnvcuvid*
        let dominated = name_str.starts_with("libcuda")
            || name_str.starts_with("libnvidia")
            || name_str.starts_with("libnvoptix")
            || name_str.starts_with("libnvcuvid");

        if dominated && name_str.contains(".so") {
            let src = entry.path();
            let dst = jail_lib_dir.join(&name);

            // Resolve symlinks to get the real file
            let real_src = match fs::canonicalize(&src) {
                Ok(p) => p,
                Err(_) => continue,
            };

            if real_src.is_file() {
                crate::mount::bind_mount(&real_src, &dst, Access::ReadOnly)?;
            }
        }
    }

    Ok(())
}

/// Get environment variables needed for GPU access inside the jail.
pub fn env_vars(config: &GpuConfig) -> Vec<(String, String)> {
    let mut vars = vec![
        ("LD_LIBRARY_PATH".into(), "/usr/lib/nvidia".into()),
    ];

    if !config.devices.is_empty() {
        let visible = config
            .devices
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(",");
        vars.push(("CUDA_VISIBLE_DEVICES".into(), visible));
    }

    vars
}
