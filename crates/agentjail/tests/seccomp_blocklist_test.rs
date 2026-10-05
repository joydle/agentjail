//! Sweep test: every syscall in the documented seccomp blocklist returns
//! EPERM under `SeccompLevel::Standard`.
//!
//! The README's "What gets blocked" table is the source of truth; this
//! test verifies the implementation actually matches the docs. Each
//! syscall is exercised via raw `libc.syscall(...)` from Python so the
//! test does not depend on whether userspace tooling exists in the jail.

mod common;

use agentjail::{Jail, SeccompLevel};
use std::fs;

const SWEEP_SCRIPT: &str = r#"
import ctypes, errno, os, platform, sys

libc = ctypes.CDLL(None, use_errno=True)

# Linux syscall numbers vary by architecture. Encode the arches we test on.
SYS_TABLES = {
    "aarch64": {
        "clone3":          435,
        "setns":           268,
        "perf_event_open": 241,
        "userfaultfd":     282,
        "io_uring_setup":  425,
        "personality":      92,
        "memfd_create":    279,
        "mount":            40,
    },
    "x86_64": {
        "clone3":          435,
        "setns":           308,
        "perf_event_open": 298,
        "userfaultfd":     323,
        "io_uring_setup":  425,
        "personality":     135,
        "memfd_create":    319,
        "mount":           165,
    },
}

arch = platform.machine()
if arch not in SYS_TABLES:
    print(f"UNSUPPORTED_ARCH:{arch}")
    sys.exit(0)
SYS = SYS_TABLES[arch]

def call(name, *args):
    """Invoke a syscall by number; return (rc, errno_name)."""
    rc = libc.syscall(SYS[name], *args)
    err = ctypes.get_errno()
    return rc, errno.errorcode.get(err, str(err))

# Each syscall: pick arguments that would succeed if the syscall were
# allowed (best-effort; we only check that seccomp returns EPERM, not
# that the syscall would otherwise have worked).
checks = [
    ("clone3",          (0, 0)),                          # cl_args, size
    ("setns",           (0, 0)),                          # fd, nstype
    ("perf_event_open", (0, 0, -1, -1, 0)),              # attr, pid, cpu, group_fd, flags
    ("userfaultfd",     (0,)),                            # flags
    ("io_uring_setup",  (1, 0)),                          # entries, params
    ("personality",     (0xFFFFFFFF,)),                   # PER_QUERY
    ("memfd_create",    (b"x", 0)),                       # name, flags
    ("mount",           (b"none", b"/tmp", b"tmpfs", 0, 0)),
]

for name, args in checks:
    rc, err = call(name, *args)
    # seccomp returns -1 with EPERM. Anything else means the syscall ran
    # (or failed for an unrelated reason — also a fail for our purposes
    # because the kernel got far enough to evaluate it).
    if rc == -1 and err == "EPERM":
        print(f"{name}: BLOCKED")
    else:
        print(f"{name}: ALLOWED rc={rc} errno={err}")
"#;

#[tokio::test]
async fn seccomp_standard_blocks_documented_syscalls() {
    let (src, out) = common::setup("audit", "seccomp-blocklist-sweep");

    fs::write(src.join("sweep.py"), SWEEP_SCRIPT).unwrap();
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\npython3 /workspace/sweep.py 2>&1\n",
    )
    .unwrap();

    let mut config = common::lightweight_config(src.clone(), out.clone());
    config.seccomp = SeccompLevel::Standard;

    let jail = Jail::new(config).unwrap();
    let r = jail.run("/bin/sh", &["/workspace/t.sh"]).await.unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);

    if stdout.contains("UNSUPPORTED_ARCH") {
        eprintln!("skipping: arch not in syscall table ({stdout})");
        common::cleanup(&src, &out);
        return;
    }

    let must_block = [
        "clone3",
        "setns",
        "perf_event_open",
        "userfaultfd",
        "io_uring_setup",
        "personality",
        "memfd_create",
        "mount",
    ];

    for syscall in must_block {
        assert!(
            stdout.contains(&format!("{syscall}: BLOCKED")),
            "{syscall} should be blocked by SeccompLevel::Standard.\nFull output:\n{stdout}"
        );
    }

    common::cleanup(&src, &out);
}
