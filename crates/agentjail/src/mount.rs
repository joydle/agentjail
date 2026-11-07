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

/// Like [`bind_mount`] but a missing (or unreachable) source is an
/// error instead of being skipped. Used for `/workspace` and `/output`:
/// silently running a jail without its source or output would be worse
/// than failing.
pub fn bind_mount_required(src: &Path, dst: &Path, access: Access) -> Result<()> {
    if !src.is_dir() {
        return Err(JailError::PathNotFound(src.to_path_buf()));
    }
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

    let read_only = access == Access::ReadOnly;
    match mount(src, dst, "", MountFlags::BIND, "") {
        Ok(()) => {}
        // Inside a user namespace, a non-recursive bind of a mount that
        // has (locked) submounts is refused with EINVAL — it would reveal
        // what the submounts cover. Bind recursively instead and apply
        // the flags to the whole subtree, so no submount stays writable.
        Err(rustix::io::Errno::INVAL) => {
            mount(src, dst, "", MountFlags::BIND | MountFlags::REC, "")
                .map_err(JailError::Mount)?;
            return set_attr_recursive(dst, read_only, nodev);
        }
        Err(e) => return Err(JailError::Mount(e)),
    }

    let mut flags = MountFlags::BIND | MountFlags::NOSUID;
    if nodev {
        flags |= MountFlags::NODEV;
    }
    if read_only {
        flags |= MountFlags::RDONLY;
    }

    match mount_remount(dst, flags, "") {
        // In a user namespace, a bind inherits the source mount's flags
        // and the restrictive ones are locked: a remount that would drop
        // NOEXEC, or change the atime mode, fails with EPERM. (Ubuntu's
        // /etc/resolv.conf points into /run, a noexec tmpfs.) Retry with
        // the source's flags kept; ours are only ever added.
        Err(rustix::io::Errno::PERM) => {
            mount_remount(dst, flags | locked_flags(dst)?, "").map_err(JailError::Mount)
        }
        r => r.map_err(JailError::Mount),
    }
}

/// The flags of the mount at `path` that a user namespace may not clear:
/// RDONLY, NOSUID, NODEV, NOEXEC and the atime mode.
fn locked_flags(path: &Path) -> Result<MountFlags> {
    // Raw `statfs` f_flags (ST_*): rustix's `StatVfsMountFlags::RELATIME`
    // is MS_RELATIME, not the kernel's ST_RELATIME (0x1000).
    let f = rustix::fs::statvfs(path)
        .map_err(JailError::Mount)?
        .f_flag
        .bits();
    #[allow(clippy::unnecessary_cast)] // c_ulong is u32 on 32-bit targets
    let has = |st: libc::c_ulong| f & st as u64 != 0;
    let mut flags = MountFlags::empty();
    for (st, ms) in [
        (libc::ST_RDONLY, MountFlags::RDONLY),
        (libc::ST_NOSUID, MountFlags::NOSUID),
        (libc::ST_NODEV, MountFlags::NODEV),
        (libc::ST_NOEXEC, MountFlags::NOEXEC),
        (libc::ST_NODIRATIME, MountFlags::NODIRATIME),
    ] {
        if has(st) {
            flags |= ms;
        }
    }
    // The atime mode must match exactly. Relatime is what a remount
    // without an atime flag asks for, so only the other two need saying.
    if has(libc::ST_NOATIME) {
        flags |= MountFlags::NOATIME;
    } else if !has(libc::ST_RELATIME) {
        flags |= MountFlags::STRICTATIME;
    }
    Ok(flags)
}

/// `mount_setattr(AT_RECURSIVE)`: add NOSUID (+ NODEV, + RDONLY) to the
/// mount at `dst` and every mount below it. Linux 5.12+; on older
/// kernels this fails, and so does the bind (fail closed).
fn set_attr_recursive(dst: &Path, read_only: bool, nodev: bool) -> Result<()> {
    #[repr(C)]
    struct MountAttr {
        attr_set: u64,
        attr_clr: u64,
        propagation: u64,
        userns_fd: u64,
    }
    const MOUNT_ATTR_RDONLY: u64 = 0x1;
    const MOUNT_ATTR_NOSUID: u64 = 0x2;
    const MOUNT_ATTR_NODEV: u64 = 0x4;
    const AT_RECURSIVE: libc::c_uint = 0x8000;

    let mut set = MOUNT_ATTR_NOSUID;
    if nodev {
        set |= MOUNT_ATTR_NODEV;
    }
    if read_only {
        set |= MOUNT_ATTR_RDONLY;
    }
    let attr = MountAttr {
        attr_set: set,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let path = CString::new(dst.as_os_str().as_encoded_bytes())
        .map_err(|_| JailError::Mount(rustix::io::Errno::INVAL))?;
    // SAFETY: valid C string and a properly sized, initialised mount_attr.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            path.as_ptr(),
            AT_RECURSIVE,
            &attr as *const MountAttr,
            std::mem::size_of::<MountAttr>(),
        )
    };
    if rc != 0 {
        return Err(JailError::Mount(rustix::io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        )));
    }
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

/// Mount a fresh procfs at `dst` with `hidepid=invisible` so processes
/// belonging to other uids are not listed. Defence-in-depth against
/// information leaks through `/proc/<pid>/*`.
///
/// Must run inside the jail's pid namespace (so the mount shows only
/// the jail's processes) and before the host root is detached: with a
/// user namespace the kernel only permits a procfs mount while a fully
/// visible procfs is still present in the mount namespace.
pub fn mount_proc(dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(JailError::Io)?;

    let flags = MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;
    mount("proc", dst, "proc", flags, "hidepid=invisible").map_err(JailError::Mount)?;

    Ok(())
}

/// `/proc` entries bind-mounted read-only over themselves (runc's
/// default `readonlyPaths`). `/proc/sys` and `/proc/sysrq-trigger` are
/// the important ones: their permission checks are plain DAC, so any
/// process that is (or maps to) host uid 0 could otherwise rewrite
/// `kernel.core_pattern` or trigger SysRq without holding a capability.
pub const PROC_READONLY: &[&str] = &["bus", "fs", "irq", "sys", "sysrq-trigger"];

/// `/proc` files hidden behind `/dev/null` (runc's default
/// `maskedPaths`, file entries).
pub const PROC_MASKED_FILES: &[&str] = &[
    "kcore",
    "keys",
    "latency_stats",
    "timer_list",
    "timer_stats",
    "sched_debug",
];

/// `/proc` directories hidden behind an empty read-only tmpfs.
pub const PROC_MASKED_DIRS: &[&str] = &["acpi", "asound", "scsi"];

/// Mask the sensitive parts of a freshly mounted procfs at `proc_dir`.
///
/// Entries that don't exist on this kernel are skipped; an entry that
/// exists but cannot be masked is an error (fail closed). `/sys` is not
/// mounted in the jail at all, so `/sys/firmware` needs no mask.
pub fn mask_proc(proc_dir: &Path) -> Result<()> {
    let ro = MountFlags::RDONLY | MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;

    for name in PROC_READONLY {
        let p = proc_dir.join(name);
        if fs::symlink_metadata(&p).is_err() {
            continue;
        }
        mount(&p, &p, "", MountFlags::BIND | MountFlags::REC, "").map_err(JailError::Mount)?;
        mount_remount(&p, MountFlags::BIND | ro, "").map_err(JailError::Mount)?;
    }
    for name in PROC_MASKED_FILES {
        let p = proc_dir.join(name);
        if fs::symlink_metadata(&p).is_err() {
            continue;
        }
        mount(Path::new("/dev/null"), &p, "", MountFlags::BIND, "").map_err(JailError::Mount)?;
    }
    for name in PROC_MASKED_DIRS {
        let p = proc_dir.join(name);
        if fs::symlink_metadata(&p).is_err() {
            continue;
        }
        mount("tmpfs", &p, "tmpfs", ro, "size=4k,mode=0555").map_err(JailError::Mount)?;
    }
    Ok(())
}

/// Mount the jail's root filesystem: a small tmpfs on the (empty)
/// pre-pivot directory. `pivot_root(2)` needs a mount point, and on a
/// private tmpfs the mount points and placeholder files created below
/// never touch the host's disk, so the host directory stays empty and
/// is removed after the jail exits. [`seal_root`] makes it read-only
/// before the pivot.
pub fn mount_root(path: &Path) -> Result<()> {
    let flags = MountFlags::NOSUID | MountFlags::NODEV;
    mount(
        "tmpfs",
        path,
        "tmpfs",
        flags,
        "size=1m,nr_inodes=4096,mode=0755",
    )
    .map_err(JailError::Mount)
}

/// Remount the root tmpfs read-only once every mount point exists.
pub fn seal_root(path: &Path) -> Result<()> {
    let flags = MountFlags::BIND | MountFlags::RDONLY | MountFlags::NOSUID | MountFlags::NODEV;
    mount_remount(path, flags, "").map_err(JailError::Mount)
}

/// A fresh path for a jail's pre-pivot root: `$TMPDIR/agentjail-<128
/// random bits>`. Chosen by the parent before fork so it can remove the
/// (then empty) directory once the jail is gone; created by the child
/// with [`make_jail_root`].
pub fn jail_root_path() -> Result<PathBuf> {
    let mut bytes = [0u8; 16];
    // SAFETY: getrandom writes at most `bytes.len()` bytes into the
    // buffer.
    let n = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if n != bytes.len() as isize {
        return Err(JailError::Io(std::io::Error::last_os_error()));
    }
    let mut suffix = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut suffix, "{b:02x}");
    }
    Ok(std::env::temp_dir().join(format!("agentjail-{suffix}")))
}

/// Create the pre-pivot root at `path` with mode 0o700. `mkdir(2)` (not
/// `mkdir -p`) fails EEXIST on any pre-planted file/dir/symlink, so a
/// host-side attacker can't race a symlink in front of us before the
/// bind mounts land.
pub fn make_jail_root(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(JailError::Io)
}

/// Swap the process root with `new_root` and detach the old root.
///
/// After this returns:
/// - `/` is the former `new_root`.
/// - The previous filesystem tree is no longer reachable (not just
///   hidden — the mount is gone, so `/proc/self/root/..` and friends
///   cannot walk out).
///
/// `new_root` must already be a mount point (see [`mount_root`]).
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
/// - /proc (empty mount point; procfs is mounted by the jail's init)
/// - /dev (minimal)
pub fn setup_root(
    new_root: &Path,
    source: &Path,
    output: &Path,
    source_rw: bool,
    readonly_overlays: &[PathBuf],
) -> Result<()> {
    fs::create_dir_all(new_root).map_err(JailError::Io)?;

    mount_root(new_root)?;

    // User directories
    let workspace = new_root.join("workspace");
    let output_dir = new_root.join("output");

    let source_access = if source_rw {
        Access::ReadWrite
    } else {
        Access::ReadOnly
    };
    // Both binds are NOSUID|NODEV (see `do_bind_mount`): a setuid file
    // or device node planted in /output or a writable /workspace is
    // inert inside the jail.
    bind_mount_required(source, &workspace, source_access)?;
    bind_mount_required(output, &output_dir, Access::ReadWrite)?;

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
    // Not bound as whole directories:
    //   - `/etc/ssl` often holds `private/` with real TLS keys (usually
    //     0700, but the names are a disclosure, and a uid-matched jail
    //     would read the bodies). Only the CA bundle and OpenSSL's
    //     default config are bound.
    //   - `/etc/alternatives` is a symlink tree into `/usr/bin`, which is
    //     already bound; the links only widen the surface.
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

    // NOEXEC on /tmp to prevent write+execute bypass. /proc is mounted
    // later, from inside the jail's pid namespace (see `mount_proc`).
    mount_tmpfs_noexec(&new_root.join("tmp"), 100)?;
    fs::create_dir_all(new_root.join("proc")).map_err(JailError::Io)?;

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
                .ok_or_else(|| {
                    JailError::BadConfig(format!(
                        "readonly overlay has no utf-8 basename: {}",
                        overlay.display()
                    ))
                })?;
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
