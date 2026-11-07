#!/bin/sh
# Cargo target runner that gives each test binary its own leaf cgroup.
#
# agentjail follows the cgroup v2 leaf rule: it parks only *itself* in
# `<its cgroup>/agentjail-init` and enables controllers on its cgroup,
# which therefore must not contain other processes. Under plain
# `cargo test`, the test binary shares a cgroup with cargo, so
# controller delegation (and every test using memory/cpu/pids limits)
# fails. This runner moves each test binary into a fresh sibling cgroup
# before exec'ing it.
#
# One-time setup (as root), so siblings get the controllers:
#
#   CG=/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)
#   mkdir -p "$CG/supervisor" && echo $$ > "$CG/supervisor/cgroup.procs"
#   echo "+memory +cpu +pids +io" > "$CG/cgroup.subtree_control"
#
# Then:
#
#   cargo test -p agentjail \
#     --config 'target."cfg(target_os = \"linux\")".runner = "crates/agentjail/tests/support/cgroup_leaf_runner.sh"' \
#     -- --test-threads=1
set -eu
cur=$(sed -n 's/^0:://p' /proc/self/cgroup)
parent="/sys/fs/cgroup$(dirname "$cur")"
leaf="$parent/aj-test-$$"
if mkdir "$leaf" 2>/dev/null && echo $$ > "$leaf/cgroup.procs" 2>/dev/null; then
  :
else
  echo "cgroup_leaf_runner: could not create $leaf; running in place" >&2
fi
exec "$@"
