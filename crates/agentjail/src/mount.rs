//! Filesystem mounts for jail isolation.

use crate::config::Access;
use crate::error::{JailError, Result};
use rustix::mount::{MountFlags, MountPropagationFlags, mount, mount_remount};
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Bind mount a source path to a destination inside the jail.
pub fn bind_mount(src: &Path, dst: &Path, access: Access) -> Result<()> {
    do_bind_mount(src, dst, access, true)
}

/// Bind mount a device node. Same as bind_mount but omits NODEV
/// so the device remains accessible.
pub fn bind_mount_dev(src: &Path, dst: &Path, access: Access) -> Result<()> {
    do_bind_mount(src, dst, access, false)
}

fn do_bind_mount(src: &Path, dst: &Path, access: Access, nodev: bool) -> Result<()> {
    if !src.exists() {
        return Ok(()); // Skip non-existent paths
    }

    if src.is_dir() {
        fs::create_dir_all(dst).map_err(JailError::Io)?;
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(JailError::Io)?;
        }
        fs::File::create(dst).map_err(JailError::Io)?;
    }

    mount(src, dst, "", MountFlags::BIND, "").map_err(JailError::Mount)?;

    let mut flags = MountFlags::BIND | MountFlags::NOSUID;
    if nodev {
        flags |= MountFlags::NODEV;
    }
    if access == Access::ReadOnly {
        flags |= MountFlags::RDONLY;
    }

    mount_remount(dst, flags, "").map_err(JailError::Mount)?;

    Ok(())
}

/// Mount a tmpfs at the given path.
pub fn mount_tmpfs(dst: &Path, size_mb: u64) -> Result<()> {
    fs::create_dir_all(dst).map_err(JailError::Io)?;

    let options = format!("size={size_mb}m,mode=1777");
    let flags = MountFlags::NOSUID | MountFlags::NODEV;

    mount("tmpfs", dst, "tmpfs", flags, &options).map_err(JailError::Mount)?;

    Ok(())
}

/// Mount a tmpfs with NOEXEC (for /tmp — prevents write+execute bypass).
pub fn mount_tmpfs_noexec(dst: &Path, size_mb: u64) -> Result<()> {
    fs::create_dir_all(dst).map_err(JailError::Io)?;

    let options = format!("size={size_mb}m,mode=1777");
    let flags = MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;

    mount("tmpfs", dst, "tmpfs", flags, &options).map_err(JailError::Mount)?;

    Ok(())
}

/// Mount /proc inside the jail with `hidepid=invisible` so processes
/// belonging to other uids are not listed. Defence-in-depth against
/// information leaks through `/proc/<pid>/*`.
pub fn mount_proc(dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(JailError::Io)?;

    let flags = MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;
    mount("proc", dst, "proc", flags, "hidepid=invisible").map_err(JailError::Mount)?;

    Ok(())
}

/// Bind-mount `path` onto itself. `pivot_root(2)` requires the new
/// root to be a mount point, and a freshly-`mkdir`'d directory is not
/// one; this turns it into one without changing its contents.
pub fn bind_self(path: &Path) -> Result<()> {
    mount(path, path, "", MountFlags::BIND | MountFlags::REC, "").map_err(JailError::Mount)
}

/// Create a pre-pivot temp root at a 128-bit-random path with mode
/// 0o700. `mkdir(2)` (not `mkdir -p`) fails EEXIST on any pre-planted
/// file/dir/symlink, so a host-side attacker can't race a symlink in
/// front of us before the bind mounts land.
pub fn make_jail_root() -> Result<PathBuf> {
    let mut bytes = [0u8; 16];
    // SAFETY: getrandom writes at most `bytes.len()` bytes into the
    // buffer; flags=0 requests default (blocking for entropy, but on
    // modern kernels this is effectively non-blocking after boot).
    let n = unsafe {
        libc::getrandom(
            bytes.as_mut_ptr() as *mut libc::c_void,
            bytes.len(),
            0,
        )
    };
    if n != bytes.len() as isize {
        return Err(JailError::Io(std::io::Error::last_os_error()));
    }
    let mut suffix = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut suffix, "{b:02x}");
    }

    let path = std::env::temp_dir().join(format!("agentjail-{suffix}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(JailError::Io)?;
    Ok(path)
}

/// Swap the process root with `new_root` and detach the old root.
///
/// After this returns:
/// - `/` is the former `new_root`.
/// - The previous filesystem tree is no longer reachable (not just
///   hidden — the mount is gone, so `/proc/self/root/..` and friends
///   cannot walk out).
///
/// `new_root` must already be a mount point (see [`bind_self`]).
pub fn pivot_into(new_root: &Path) -> Result<()> {
    std::env::set_current_dir(new_root).map_err(JailError::Exec)?;

    // pivot_root(".", ".") is the canonical idiom: new-root and
    // put_old both resolve to the cwd, so we don't need a free
    // subdirectory for the old root. After the call the old root is
    // mounted on top of itself at "/"; we immediately detach it.
    let dot = CString::new(".").expect("no nuls in \".\"");
    // SAFETY: both pointers are valid C strings; the syscall is the
    // libc wrapper around the kernel's SYS_pivot_root, no memory
    // invariants to uphold.
    let rc = unsafe { libc::syscall(libc::SYS_pivot_root, dot.as_ptr(), dot.as_ptr()) };
    if rc != 0 {
        return Err(JailError::Mount(rustix::io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        )));
    }

    // Detach the old root. MNT_DETACH is lazy so busy mounts under
    // it (e.g. our own /proc bind) don't block us.
    // SAFETY: "." is a valid C string; umount2 has no memory safety
    // requirements beyond that.
    let rc = unsafe { libc::umount2(dot.as_ptr(), libc::MNT_DETACH) };
    if rc != 0 {
        return Err(JailError::Mount(rustix::io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        )));
    }

    std::env::set_current_dir("/").map_err(JailError::Exec)
}

/// Make the root mount private to prevent mount propagation.
pub fn make_root_private() -> Result<()> {
    rustix::mount::mount_change(
        "/",
        MountPropagationFlags::REC | MountPropagationFlags::PRIVATE,
    )
    .map_err(JailError::Mount)?;

    Ok(())
}

/// Setup minimal root filesystem for the jail.
///
/// Creates:
/// - /workspace (source, read-only by default; read-write when
///   `source_rw` is true — used for persistent workspaces)
/// - /output (artifacts, read-write)
/// - /bin, /lib, /lib64, /usr (system binaries, read-only)
/// - /tmp (tmpfs)
/// - /proc
/// - /dev (minimal)
pub fn setup_root(
    new_root: &Path,
    source: &Path,
    output: &Path,
    source_rw: bool,
    readonly_overlays: &[PathBuf],
) -> Result<()> {
    fs::create_dir_all(new_root).map_err(JailError::Io)?;

    // pivot_root(2) requires the new root to be a mount point. Bind
    // the directory onto itself so the later pivot succeeds.
    bind_self(new_root)?;

    // User directories
    let workspace = new_root.join("workspace");
    let output_dir = new_root.join("output");

    let source_access = if source_rw { Access::ReadWrite } else { Access::ReadOnly };
    bind_mount(source, &workspace, source_access)?;
    bind_mount(output, &output_dir, Access::ReadWrite)?;

    // System directories (read-only). Warn on missing so operators notice
    // broken jails instead of getting cryptic "command not found" errors.
    let system_dirs = ["/bin", "/lib", "/lib64", "/usr"];
    for dir in &system_dirs {
        let src = Path::new(dir);
        if !src.exists() {
            continue; // lib64 doesn't exist on aarch64, etc.
        }
        let dst = new_root.join(dir.trim_start_matches('/'));
        bind_mount(src, &dst, Access::ReadOnly)?;
    }

    // Mount a minimal /etc — only files needed for dynamic linking and
    // DNS. Full /etc leaks host secrets (shadow, ssh keys, machine-id).
    //
    // We deliberately do NOT bind the whole `/etc/ssl` or
    // `/etc/alternatives` directories:
    //   - `/etc/ssl` commonly contains `private/` with real TLS keys
    //     (perms are usually 0700, but the filenames are themselves a
    //     disclosure, and a uid-matched jail would read the bodies).
    //     We bind only the CA bundle and OpenSSL's default config.
    //   - `/etc/alternatives` is a symlink tree pointing into `/usr/bin`
    //     etc. Since `/usr` and `/bin` are already bound, the binaries
    //     are reachable directly; the symlinks themselves are not
    //     required for agent workloads and re-exporting them just
    //     widens the surface.
    let etc_dst = new_root.join("etc");
    mount_tmpfs(&etc_dst, 1)?;
    let safe_etc_files = [
        "ld.so.cache",
        "ld.so.conf",
        "resolv.conf",
        "nsswitch.conf",
        "passwd",
        "group",
        // Specific SSL files, not the whole directory.
        "ssl/certs/ca-certificates.crt",
        "ssl/openssl.cnf",
    ];
    for name in &safe_etc_files {
        let src = Path::new("/etc").join(name);
        let dst = etc_dst.join(name);
        bind_mount(&src, &dst, Access::ReadOnly)?;
    }

    // Temp and proc — NOEXEC on /tmp to prevent write+execute bypass.
    mount_tmpfs_noexec(&new_root.join("tmp"), 100)?;
    mount_proc(&new_root.join("proc"))?;

    // Minimal /dev
    let dev = new_root.join("dev");
    fs::create_dir_all(&dev).map_err(JailError::Io)?;

    // Device nodes use bind_mount_dev (omits NODEV so the device works).
    // /dev/null needs write (programs redirect to it).
    // /dev/zero, /dev/urandom, /dev/random are read-only entropy/zero sources.
    let dev_rw = ["/dev/null"];
    let dev_ro = ["/dev/zero", "/dev/urandom", "/dev/random"];

    for node in &dev_rw {
        let src = Path::new(node);
        let dst = new_root.join(node.trim_start_matches('/'));
        bind_mount_dev(src, &dst, Access::ReadWrite)?;
    }
    for node in &dev_ro {
        let src = Path::new(node);
        let dst = new_root.join(node.trim_start_matches('/'));
        bind_mount_dev(src, &dst, Access::ReadOnly)?;
    }

    // Flavor overlays — each host dir becomes `/opt/flavors/<basename>/`
    // read-only inside the jail. Duplicate basenames fail loud: the
    // caller would silently lose a mount otherwise.
    if !readonly_overlays.is_empty() {
        let flavors_root = new_root.join("opt/flavors");
        fs::create_dir_all(&flavors_root).map_err(JailError::Io)?;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for overlay in readonly_overlays {
            let name = overlay
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| JailError::BadConfig(format!(
                    "readonly overlay has no utf-8 basename: {}",
                    overlay.display()
                )))?;
            if !seen.insert(name.to_string()) {
                return Err(JailError::BadConfig(format!(
                    "duplicate flavor basename {name:?} in readonly_overlays"
                )));
            }
            let dst = flavors_root.join(name);
            bind_mount(overlay, &dst, Access::ReadOnly)?;
        }
    }

    Ok(())
}
