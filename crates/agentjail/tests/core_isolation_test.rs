//! Regression tests for the core-isolation fixes: host-root exposure via
//! `/proc`, the default user-namespace handshake, `clone` namespace flags,
//! fail-closed child setup, Landlock after `pivot_root`, the event
//! stream, and config validation.
//!
//! Most of these need real root (cgroups, mounts); they skip otherwise.
//! Run on Linux with `--test-threads=1`.

mod common;

use agentjail::{
    Jail, JailConfig, JailError, JailEvent, Network, RunAs, SeccompLevel, preset_agent,
    preset_build, preset_dev, preset_gpu, preset_install,
};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

fn setup(name: &str) -> (PathBuf, PathBuf) {
    common::setup("core", name)
}

/// Spawn + wait with a hard deadline, so a handshake deadlock fails the
/// test instead of hanging the suite.
async fn run_deadline(
    jail: Jail,
    cmd: &'static str,
    args: &'static [&'static str],
) -> agentjail::Output {
    // Spawn on a plain thread, not `spawn_blocking`: the runtime would
    // wait for a stuck blocking task on shutdown and hang the suite
    // instead of failing this test.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let _guard = rt.enter(); // spawn registers the output pipes with tokio
        let _ = tx.send(jail.spawn(cmd, args));
    });
    let handle = tokio::time::timeout(Duration::from_secs(20), rx)
        .await
        .expect("spawn did not return within 20 s (handshake deadlock?)")
        .unwrap()
        .expect("spawn failed");
    tokio::time::timeout(Duration::from_secs(20), handle.wait())
        .await
        .expect("jail did not exit within 20 s")
        .expect("wait failed")
}

fn text(o: &agentjail::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn root_or_skip() -> bool {
    if !common::is_root() {
        eprintln!("skip: needs root");
        return false;
    }
    true
}

/// Config with the full stack on (Standard seccomp, namespaces) but
/// without Landlock, for tests that probe `/proc` or `/output` and
/// don't want Landlock in the way.
fn hardened(src: PathBuf, out: PathBuf, user_namespace: bool) -> JailConfig {
    JailConfig {
        source: src,
        output: out,
        user_namespace,
        landlock: false,
        timeout_secs: 15,
        env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Item 1: root embedder — /proc masking, never host uid 0, nosuid output
// ---------------------------------------------------------------------------

const PROC_WRITE_PROBE: &str = r#"#!/bin/sh
cp=$(cat /proc/sys/kernel/core_pattern)
# Writes the current value back: a no-op if it succeeds, but proves the
# file is writable.
if printf '%s\n' "$cp" > /proc/sys/kernel/core_pattern 2>/dev/null; then
  echo CORE_PATTERN_WRITABLE
else
  echo CORE_PATTERN_DENIED
fi
# 'h' only prints SysRq help to the kernel log.
if printf 'h' > /proc/sysrq-trigger 2>/dev/null; then
  echo SYSRQ_WRITABLE
else
  echo SYSRQ_DENIED
fi
echo "KCORE_BYTES=$(head -c 16 /proc/kcore 2>/dev/null | wc -c)"
echo "KEYS_BYTES=$(cat /proc/keys 2>/dev/null | wc -c)"
"#;

#[tokio::test]
async fn proc_sys_and_sysrq_are_read_only_for_root_embedder() {
    if !root_or_skip() {
        return;
    }
    for userns in [false, true] {
        let (src, out) = setup(&format!("procmask-{userns}"));
        fs::write(src.join("t.sh"), PROC_WRITE_PROBE).unwrap();
        let jail = Jail::new(hardened(src.clone(), out.clone(), userns)).unwrap();
        let o = run_deadline(jail, "/bin/sh", &["/workspace/t.sh"]).await;
        let t = text(&o);
        assert!(
            t.contains("CORE_PATTERN_DENIED"),
            "userns={userns}: core_pattern writable:\n{t}"
        );
        assert!(
            t.contains("SYSRQ_DENIED"),
            "userns={userns}: sysrq-trigger writable:\n{t}"
        );
        assert!(
            t.contains("KCORE_BYTES=0"),
            "userns={userns}: /proc/kcore not masked:\n{t}"
        );
        assert!(
            t.contains("KEYS_BYTES=0"),
            "userns={userns}: /proc/keys not masked:\n{t}"
        );
        common::cleanup(&src, &out);
    }
}

const OWNER_PROBE: &str = r#"#!/bin/sh
echo hello > /output/created
cp /bin/sh /output/suid-sh && chmod 4755 /output/suid-sh
grep ' /output ' /proc/self/mountinfo
grep -E '^(Uid|Gid|Groups|CapEff|CapBnd|NoNewPrivs|Seccomp):' /proc/self/status
cat /proc/self/uid_map
"#;

#[tokio::test]
async fn jail_never_runs_as_host_root() {
    if !root_or_skip() {
        return;
    }
    for userns in [false, true] {
        let (src, out) = setup(&format!("owner-{userns}"));
        fs::write(src.join("t.sh"), OWNER_PROBE).unwrap();
        let jail = Jail::new(hardened(src.clone(), out.clone(), userns)).unwrap();
        let o = run_deadline(jail, "/bin/sh", &["/workspace/t.sh"]).await;
        let t = text(&o);
        assert_eq!(o.exit_code, 0, "userns={userns}:\n{t}");

        let expected = if userns {
            agentjail::DEFAULT_USERNS_ID_BASE
        } else {
            RunAs::NOBODY.uid
        };
        let created = fs::metadata(out.join("created")).unwrap();
        assert_ne!(
            created.uid(),
            0,
            "userns={userns}: jail wrote a file as host root"
        );
        assert_eq!(created.uid(), expected, "userns={userns}");
        assert_eq!(created.gid(), expected, "userns={userns}");

        // A setuid copy is owned by the unprivileged id (not root) and
        // /output is nosuid,nodev, so the bit is inert.
        let suid = fs::metadata(out.join("suid-sh")).unwrap();
        assert_ne!(
            suid.uid(),
            0,
            "userns={userns}: setuid file owned by host root"
        );
        let mountinfo = t
            .lines()
            .find(|l| l.contains(" /output "))
            .expect("no /output mount");
        assert!(
            mountinfo.contains("nosuid") && mountinfo.contains("nodev"),
            "{mountinfo}"
        );

        assert!(t.contains("NoNewPrivs:\t1"), "{t}");
        assert!(t.contains("Seccomp:\t2"), "{t}");
        assert!(t.contains("CapEff:\t0000000000000000"), "{t}");
        assert!(t.contains("CapBnd:\t0000000000000000"), "{t}");
        if userns {
            // Jail root maps to host 100000, never 0.
            let map = t.lines().last().unwrap_or_default();
            let cols: Vec<&str> = map.split_whitespace().collect();
            assert_eq!(cols, ["0", &expected.to_string(), "1"], "uid_map:\n{t}");
        } else {
            assert!(t.contains("Uid:\t65534\t65534\t65534\t65534"), "{t}");
            let groups = t
                .lines()
                .find(|l| l.starts_with("Groups:"))
                .expect("no Groups line");
            assert_eq!(
                groups.trim_start_matches("Groups:").trim(),
                "",
                "supplementary groups kept:\n{t}"
            );
        }
        common::cleanup(&src, &out);
    }
}

#[tokio::test]
async fn pre_seeded_output_stays_writable() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("seeded");
    fs::create_dir_all(out.join("sub")).unwrap();
    fs::write(out.join("sub/data.txt"), "seed").unwrap();
    fs::write(
        src.join("t.sh"),
        "#!/bin/sh\necho changed > /output/sub/data.txt\n",
    )
    .unwrap();
    let jail = Jail::new(hardened(src.clone(), out.clone(), false)).unwrap();
    let o = run_deadline(jail, "/bin/sh", &["/workspace/t.sh"]).await;
    assert_eq!(o.exit_code, 0, "{}", text(&o));
    assert_eq!(
        fs::read_to_string(out.join("sub/data.txt")).unwrap(),
        "changed\n"
    );
    common::cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Items 2 + 8: the default config (user_namespace + landlock) and every
// preset spawn and exit.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_config_spawns_and_exits() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("default");
    let config = JailConfig {
        source: src.clone(),
        output: out.clone(),
        ..Default::default()
    };
    assert!(config.user_namespace && config.landlock, "defaults changed");
    let jail = Jail::new(config).unwrap();
    let o = run_deadline(jail, "/bin/sh", &["-c", "echo default-ok"]).await;
    assert_eq!(o.exit_code, 0, "{}", text(&o));
    assert!(text(&o).contains("default-ok"));
    common::cleanup(&src, &out);
}

/// In a user namespace a bind inherits its source mount's flags locked;
/// remounting it without NOEXEC (or with another atime mode) is EPERM.
/// A source on a `noexec,noatime` tmpfs (like Ubuntu's /run, which
/// /etc/resolv.conf points into) must still mount.
#[tokio::test]
async fn source_on_a_noexec_mount_binds_in_a_user_namespace() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("noexec-src");
    let mounted = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "nosuid,nodev,noexec,noatime", "tmpfs"])
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    if !mounted {
        eprintln!("skip: could not mount a tmpfs");
        return;
    }
    fs::write(src.join("hello"), "from-noexec\n").unwrap();
    let jail = Jail::new(hardened(src.clone(), out.clone(), true)).unwrap();
    let o = run_deadline(jail, "/bin/cat", &["/workspace/hello"]).await;
    let _ = std::process::Command::new("umount").arg(&src).status();
    assert_eq!(o.exit_code, 0, "{}", text(&o));
    assert!(text(&o).contains("from-noexec"), "{}", text(&o));
    common::cleanup(&src, &out);
}

/// The jail's `/` is a read-only tmpfs: even the jail's root (userns)
/// can't create files there, so nothing lands on the host's disk.
#[tokio::test]
async fn jail_root_is_read_only() {
    if !root_or_skip() {
        return;
    }
    for userns in [false, true] {
        let (src, out) = setup(&format!("ro-root-{userns}"));
        let jail = Jail::new(hardened(src.clone(), out.clone(), userns)).unwrap();
        let o = run_deadline(
            jail,
            "/bin/sh",
            &["-c", "for d in / /dev /proc/..; do touch $d/x 2>/dev/null && echo WROTE $d; done; echo done"],
        )
        .await;
        let t = text(&o);
        assert!(
            t.contains("done") && !t.contains("WROTE"),
            "userns={userns}: {t}"
        );
        common::cleanup(&src, &out);
    }
}

/// Process state letter from `/proc/<pid>/stat` (`R`, `S`, `Z`, ...),
/// or `None` once the pid is gone.
fn proc_state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.trim_start().chars().next()
}

/// `PR_SET_PDEATHSIG` fires when the *thread* that forked the child
/// exits. A jail spawned from a short-lived thread (here a plain
/// `std::thread` that returns right after `spawn`; in practice a retired
/// tokio blocking-pool thread) must keep running: the engine forks on
/// its own process-lifetime spawner thread. Both modes, since the
/// setup process's credentials (and so its pdeathsig) differ.
#[tokio::test]
async fn jail_outlives_the_thread_that_spawned_it() {
    if !root_or_skip() {
        return;
    }
    let mut running = Vec::new();
    for userns in [false, true] {
        let (src, out) = setup(&format!("short-thread-{userns}"));
        let mut config = hardened(src.clone(), out.clone(), userns);
        config.timeout_secs = 30;
        let jail = Jail::new(config).unwrap();
        let rt = tokio::runtime::Handle::current();
        let spawner = std::thread::spawn(move || {
            let _guard = rt.enter(); // spawn registers the output pipes with tokio
            let h = jail.spawn("/bin/sh", &["-c", "sleep 8; echo survived"]);
            (jail, h)
        });
        let (jail, handle) = spawner.join().expect("spawning thread panicked");
        // The thread that called `spawn` has exited by now.
        running.push((userns, src, out, jail, handle.expect("spawn failed")));
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    for (userns, _, _, _, handle) in &running {
        let state = proc_state(handle.pid().as_raw());
        assert!(
            matches!(state, Some(s) if s != 'Z' && s != 'X'),
            "userns={userns}: jail died with its spawning thread (state {state:?})"
        );
    }
    for (userns, src, out, _jail, handle) in running {
        let o = tokio::time::timeout(Duration::from_secs(30), handle.wait())
            .await
            .expect("jail did not exit within 30 s")
            .expect("wait failed");
        assert_eq!(o.exit_code, 0, "userns={userns}: {}", text(&o));
        assert!(
            text(&o).contains("survived"),
            "userns={userns}: {}",
            text(&o)
        );
        assert!(o.duration >= Duration::from_secs(7), "{:?}", o.duration);
        common::cleanup(&src, &out);
    }
}

#[tokio::test]
async fn every_preset_spawns() {
    if !root_or_skip() {
        return;
    }
    type Preset = fn(PathBuf, PathBuf) -> JailConfig;
    let presets: Vec<(&str, Preset)> = vec![
        ("build", |s, o| preset_build(s, o)),
        ("install", |s, o| {
            preset_install(s, o, vec!["registry.npmjs.org".into()])
        }),
        ("agent", |s, o| preset_agent(s, o)),
        ("dev", |s, o| preset_dev(s, o)),
    ];
    for (name, make) in presets {
        let (src, out) = setup(&format!("preset-{name}"));
        let jail = Jail::new(make(src.clone(), out.clone())).unwrap();
        let o = run_deadline(
            jail,
            "/bin/sh",
            &["-c", "echo preset-ok > /output/ok && cat /output/ok"],
        )
        .await;
        assert_eq!(o.exit_code, 0, "preset {name}: {}", text(&o));
        assert!(text(&o).contains("preset-ok"), "preset {name}");
        common::cleanup(&src, &out);
    }

    // The GPU preset needs NVIDIA hardware; without it Jail::new refuses.
    let (src, out) = setup("preset-gpu");
    if !std::path::Path::new("/dev/nvidiactl").exists() {
        assert!(matches!(
            Jail::new(preset_gpu(src.clone(), out.clone())),
            Err(JailError::Gpu(_))
        ));
    }
    common::cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Item 4: clone(CLONE_NEW*) is filtered; fork, threads, posix_spawn work
// ---------------------------------------------------------------------------

const CLONE_PROBE: &str = r#"
import ctypes, os, platform, threading
libc = ctypes.CDLL(None, use_errno=True)
SYS_clone = {"x86_64": 56, "aarch64": 220}[platform.machine()]
CLONE_NEWUSER, CLONE_NEWNS, CLONE_NEWNET, SIGCHLD = 0x10000000, 0x00020000, 0x40000000, 17

for name, flag in [("NEWUSER", CLONE_NEWUSER), ("NEWNS", CLONE_NEWNS), ("NEWNET", CLONE_NEWNET)]:
    r = libc.syscall(SYS_clone, flag | SIGCHLD, 0, 0, 0, 0)
    if r == 0:
        os._exit(0)
    if r > 0:
        os.waitpid(r, 0)
        print(f"CLONE_{name}: ALLOWED")
    else:
        print(f"CLONE_{name}: errno={ctypes.get_errno()}")

r = libc.syscall(435, 0, 0)
print(f"CLONE3: rc={r} errno={ctypes.get_errno()}")

pid = os.fork()
if pid == 0:
    os._exit(7)
print("FORK:", os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]))

out = []
t = threading.Thread(target=lambda: out.append("ran"))
t.start(); t.join()
print("THREAD:", out)

pid = os.posix_spawn("/bin/sh", ["sh", "-c", "exit 3"], {})
print("POSIX_SPAWN:", os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]))
"#;

#[tokio::test]
async fn clone_namespace_flags_blocked_but_fork_threads_spawn_work() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("clone");
    fs::write(src.join("p.py"), CLONE_PROBE).unwrap();
    let mut config = hardened(src.clone(), out.clone(), true);
    config.seccomp = SeccompLevel::Standard;
    config.max_pids = 64;
    let jail = Jail::new(config).unwrap();
    let o = run_deadline(jail, "/usr/bin/python3", &["/workspace/p.py"]).await;
    let t = text(&o);
    assert_eq!(o.exit_code, 0, "{t}");
    for ns in ["NEWUSER", "NEWNS", "NEWNET"] {
        assert!(
            t.contains(&format!("CLONE_{ns}: errno={}", libc::EPERM)),
            "clone(CLONE_{ns}) not blocked:\n{t}"
        );
    }
    assert!(
        t.contains(&format!("CLONE3: rc=-1 errno={}", libc::ENOSYS)),
        "{t}"
    );
    assert!(t.contains("FORK: 7"), "{t}");
    assert!(t.contains("THREAD: ['ran']"), "{t}");
    assert!(t.contains("POSIX_SPAWN: 3"), "{t}");
    common::cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Item 6: stdin is /dev/null by default
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stdin_is_dev_null_by_default() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("stdin");
    // Make our own stdin a pipe so "inherited" is distinguishable from
    // /dev/null even when the test runner's stdin already is /dev/null.
    let mut p = [0; 2];
    // SAFETY: valid array; dup/dup2 on fds we own.
    let saved = unsafe {
        assert_eq!(libc::pipe(p.as_mut_ptr()), 0);
        let saved = libc::dup(0);
        libc::dup2(p[0], 0);
        saved
    };
    let mut config = hardened(src.clone(), out.clone(), false);
    config.seccomp = SeccompLevel::Standard;
    let jail = Jail::new(config).unwrap();
    let r = jail.spawn("/bin/sh", &["-c", "readlink /proc/self/fd/0"]);
    // SAFETY: restore our stdin and close the pipe.
    unsafe {
        libc::dup2(saved, 0);
        libc::close(saved);
        libc::close(p[0]);
        libc::close(p[1]);
    }
    let o = r.unwrap().wait().await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "/dev/null",
        "{}",
        text(&o)
    );
    common::cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Item 7: Landlock after pivot_root
// ---------------------------------------------------------------------------

const LANDLOCK_PROBE: &str = r#"#!/bin/sh
cat /workspace/hello.txt
echo out > /output/w && echo OUTPUT_WRITE_OK
echo tmp > /tmp/w && echo TMP_WRITE_OK
ls /proc/self >/dev/null && echo PROC_READ_OK
ls / >/dev/null 2>&1 && echo ROOT_LISTED || echo ROOT_DENIED
touch /etc/planted 2>/dev/null && echo ETC_WRITABLE || echo ETC_DENIED
touch /workspace/x 2>/dev/null && echo WS_WRITABLE || echo WS_DENIED
mkdir /newdir 2>/dev/null && echo ROOT_MKDIR_OK || echo ROOT_MKDIR_DENIED
"#;

#[tokio::test]
async fn landlock_preset_allows_toolchain_and_denies_the_rest() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("landlock");
    fs::write(src.join("hello.txt"), "HELLO_FROM_SOURCE\n").unwrap();
    fs::write(src.join("t.sh"), LANDLOCK_PROBE).unwrap();
    let config = preset_agent(src.clone(), out.clone());
    assert!(config.landlock);
    let jail = Jail::new(config).unwrap();
    let o = run_deadline(jail, "/bin/sh", &["/workspace/t.sh"]).await;
    let t = text(&o);
    for want in [
        "HELLO_FROM_SOURCE",
        "OUTPUT_WRITE_OK",
        "TMP_WRITE_OK",
        "PROC_READ_OK",
        "ROOT_DENIED",
        "ETC_DENIED",
        "WS_DENIED",
        "ROOT_MKDIR_DENIED",
    ] {
        assert!(t.contains(want), "missing {want}:\n{t}");
    }
    common::cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Item 9: spawn_with_events streams
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_with_events_streams_lines_and_completion() {
    if !root_or_skip() {
        return;
    }
    let (src, out) = setup("events");
    let jail = Jail::new(common::lightweight_config(src.clone(), out.clone())).unwrap();
    let (handle, mut rx) = jail
        .spawn_with_events("/bin/sh", &["-c", "echo one; echo two >&2; echo three"])
        .unwrap();
    let o = handle.wait().await.unwrap();
    assert_eq!(o.exit_code, 0);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "one\nthree\n");

    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    assert!(
        matches!(events.first(), Some(JailEvent::Started { .. })),
        "{events:?}"
    );
    let stdout: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let JailEvent::Stdout(l) = e {
                Some(l.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(stdout, ["one", "three"], "{events:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, JailEvent::Stderr(l) if l == "two")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, JailEvent::Completed { exit_code: 0, .. })),
        "{events:?}"
    );
    common::cleanup(&src, &out);
}

// ---------------------------------------------------------------------------
// Item 10: overflow-safe, validated resource config
// ---------------------------------------------------------------------------

#[test]
fn config_validation_rejects_overflow_and_zero_limits() {
    let (src, out) = setup("validate");
    let base = || JailConfig {
        source: src.clone(),
        output: out.clone(),
        ..Default::default()
    };
    let bad = |c: JailConfig| matches!(Jail::new(c), Err(JailError::BadConfig(_)));

    assert!(
        bad(JailConfig {
            memory_mb: Some(0),
            ..base()
        }),
        "memory_mb = Some(0) accepted"
    );
    assert!(
        bad(JailConfig {
            memory_mb: Some(u64::MAX),
            ..base()
        }),
        "memory overflow accepted"
    );
    assert!(
        bad(JailConfig {
            memory_mb: Some(u64::MAX / 1024),
            ..base()
        }),
        "memory overflow accepted"
    );
    assert!(
        bad(JailConfig {
            io_read_mbps: u64::MAX,
            ..base()
        }),
        "io read overflow accepted"
    );
    assert!(
        bad(JailConfig {
            io_write_mbps: 1 << 50,
            ..base()
        }),
        "io write overflow accepted"
    );
    assert!(
        bad(JailConfig {
            cpu_percent: u64::MAX,
            ..base()
        }),
        "cpu overflow accepted"
    );
    assert!(
        bad(JailConfig {
            pid_namespace: false,
            ..base()
        }),
        "userns without pidns accepted"
    );
    assert!(
        bad(JailConfig {
            userns_id_base: 0,
            ..base()
        }),
        "userns base 0 accepted"
    );
    assert!(
        bad(JailConfig {
            userns_id_count: 0,
            ..base()
        }),
        "userns count 0 accepted"
    );
    assert!(bad(JailConfig {
        userns_id_base: u32::MAX - 1,
        userns_id_count: 2,
        ..base()
    }));
    assert!(bad(JailConfig {
        run_as: RunAs { uid: 0, gid: 65534 },
        user_namespace: false,
        ..base()
    }));
    assert!(
        bad(JailConfig {
            proxy_allowed_ports: Some(vec![]),
            ..base()
        }),
        "empty proxy port list accepted"
    );
    assert!(
        bad(JailConfig {
            proxy_allowed_ports: Some(vec![443, 0]),
            ..base()
        }),
        "proxy port 0 accepted"
    );

    // Valid edge cases.
    let ok = |c: JailConfig| c.validate().is_ok();
    assert!(
        ok(JailConfig {
            memory_mb: None,
            ..base()
        }),
        "None (unlimited) rejected"
    );
    assert!(ok(JailConfig {
        memory_mb: Some(1),
        ..base()
    }));
    assert!(
        bad(JailConfig {
            pid_namespace: false,
            user_namespace: false,
            ..base()
        }),
        "no pid namespace accepted: the jail would see host processes"
    );
    assert!(ok(JailConfig {
        network: Network::Loopback,
        ..base()
    }));
    assert!(ok(JailConfig {
        network: Network::Allowlist(vec!["example.com".into()]),
        proxy_allowed_ports: Some(vec![443, 8443]),
        firewall: Some(agentjail::FirewallMode::Off),
        ..base()
    }));
    common::cleanup(&src, &out);
}
