"""Hermetic tests — use an in-process httpx transport so nothing hits the network."""

from __future__ import annotations

import json
from typing import Any, Callable

import httpx
import pytest

from agentjail import Agentjail, AgentjailError


def make(handler: Callable[[httpx.Request], httpx.Response], api_key: str | None = "k") -> Agentjail:
    return Agentjail(base_url="http://api", api_key=api_key, transport=httpx.MockTransport(handler))


# ---- HttpClient -------------------------------------------------------


def test_strips_trailing_slashes() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        return httpx.Response(200, json=[])

    aj = Agentjail(
        base_url="http://api.example///",
        api_key=None,
        transport=httpx.MockTransport(h),
    )
    aj.credentials.list()
    assert seen["url"] == "http://api.example/v1/credentials"


def test_sends_authorization_when_api_key() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["auth"] = req.headers.get("authorization")
        return httpx.Response(200, json=[])

    aj = make(h, api_key="aj_test")
    aj.credentials.list()
    assert seen["auth"] == "Bearer aj_test"


def test_omits_authorization_when_no_key() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["auth"] = req.headers.get("authorization")
        return httpx.Response(200, json=[])

    aj = make(h, api_key=None)
    aj.credentials.list()
    assert seen["auth"] is None


def test_wraps_non_2xx_in_agentjail_error() -> None:
    def h(_: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"error": "bad request: bad secret"})

    aj = make(h)
    with pytest.raises(AgentjailError) as excinfo:
        aj.credentials.put(service="openai", secret="")
    assert excinfo.value.status == 400
    assert "bad secret" in str(excinfo.value)


@pytest.mark.parametrize(
    ("status", "code"),
    [
        (400, "BAD_REQUEST"),
        (401, "UNAUTHORIZED"),
        (403, "FORBIDDEN"),
        (404, "NOT_FOUND"),
        (409, "CONFLICT"),
        (429, "RATE_LIMITED"),
        (504, "TIMEOUT"),
        (500, "SERVER_ERROR"),
        (502, "SERVER_ERROR"),
    ],
)
def test_status_maps_to_error_code(status: int, code: str) -> None:
    def h(_: httpx.Request) -> httpx.Response:
        return httpx.Response(status, json={"error": "x"})

    aj = make(h)
    with pytest.raises(AgentjailError) as excinfo:
        aj.credentials.put(service="openai", secret="x")
    assert excinfo.value.code == code


# ---- credentials ------------------------------------------------------


def test_credentials_put_posts_json() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["method"] = req.method
        seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "service": "openai",
                "added_at": "2026-04-19T00:00:00Z",
                "updated_at": "2026-04-19T00:00:00Z",
                "fingerprint": "deadbeef",
            },
        )

    aj = make(h)
    r = aj.credentials.put(service="openai", secret="sk-real")
    assert seen["method"] == "POST"
    assert seen["body"] == {"service": "openai", "secret": "sk-real"}
    assert r["service"] == "openai"


# ---- sessions ---------------------------------------------------------


def test_sessions_create_sends_ttl_secs() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "id": "sess_abc",
                "created_at": "2026-04-19T00:00:00Z",
                "expires_at": "2026-04-19T00:10:00Z",
                "services": ["openai"],
                "env": {"OPENAI_API_KEY": "phm_..."},
            },
        )

    aj = make(h)
    s = aj.sessions.create(services=["openai"], ttl_secs=600)
    assert seen["body"] == {"services": ["openai"], "ttl_secs": 600}
    assert s["id"] == "sess_abc"


def test_sessions_omits_ttl_when_unset() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "id": "sess_abc",
                "created_at": "2026-04-19T00:00:00Z",
                "expires_at": None,
                "services": ["openai"],
                "env": {},
            },
        )

    aj = make(h)
    aj.sessions.create(services=["openai"])
    assert list(seen["body"].keys()) == ["services"]


# ---- runs -------------------------------------------------------------


def test_runs_fork_requires_child() -> None:
    aj = make(lambda _: httpx.Response(200, json={}))
    with pytest.raises(ValueError):
        aj.runs.fork(parent_code="1")


def test_runs_fork_n_way_children() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(req.content)
        exec_ok = {
            "stdout": "", "stderr": "", "exit_code": 0,
            "duration_ms": 1, "timed_out": False, "oom_killed": False,
        }
        meta = {
            "clone_ms": 1, "files_cloned": 0, "files_cow": 0,
            "bytes_cloned": 0, "method": "reflink", "was_frozen": True,
        }
        return httpx.Response(
            200,
            json={
                "parent": exec_ok,
                "child": exec_ok, "children": [exec_ok, exec_ok],
                "fork": meta, "forks": [meta, meta],
            },
        )

    aj = make(h)
    r = aj.runs.fork(
        parent_code="1",
        children=[{"code": "a"}, {"code": "b", "memory_mb": 128}],
    )
    assert seen["body"] == {
        "parent_code": "1",
        "children": [{"code": "a"}, {"code": "b", "memory_mb": 128}],
    }
    assert len(r["children"]) == 2


def test_runs_stream_parses_sse_frames() -> None:
    sse_body = (
        b"event: started\ndata: {\"pid\":1234}\n\n"
        b"event: stdout\ndata: hello\n\n"
        b"event: stdout\ndata: world\n\n"
        b"event: stderr\ndata: warn\n\n"
        b'event: completed\ndata: {"exit_code":0,"duration_ms":42,"timed_out":false,'
        b'"oom_killed":false,"memory_peak_bytes":1048576,"cpu_usage_usec":1000}\n\n'
    )

    def h(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            content=sse_body,
            headers={"content-type": "text/event-stream"},
        )

    aj = make(h)
    events = list(aj.runs.stream(code="print(1)", language="python"))
    assert events == [
        {"type": "started", "pid": 1234},
        {"type": "stdout", "line": "hello"},
        {"type": "stdout", "line": "world"},
        {"type": "stderr", "line": "warn"},
        {
            "type": "completed", "exit_code": 0, "duration_ms": 42,
            "timed_out": False, "oom_killed": False,
            "memory_peak_bytes": 1048576, "cpu_usage_usec": 1000,
        },
    ]


# ---- workspaces + snapshots ------------------------------------------


def test_workspaces_create_nested_git_and_idle() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "id": "wrk_abc",
                "created_at": "2026-04-19T00:00:00Z",
                "deleted_at": None,
                "source_dir": "/s/source",
                "output_dir": "/s/output",
                "config": {
                    "memory_mb": 512, "timeout_secs": 300, "cpu_percent": 100,
                    "max_pids": 64, "network_mode": "none", "network_domains": [],
                    "seccomp": "standard", "idle_timeout_secs": 60,
                },
                "git_repo": "https://example/org/repo", "git_ref": "main",
                "label": "ci", "domains": [],
                "last_exec_at": None, "paused_at": None, "auto_snapshot": None,
            },
        )

    aj = make(h)
    ws = aj.workspaces.create(
        git={"repo": "https://example/org/repo", "ref": "main"},
        label="ci",
        idle_timeout_secs=60,
    )
    assert seen["body"] == {
        "git": {"repo": "https://example/org/repo", "ref": "main"},
        "label": "ci",
        "idle_timeout_secs": 60,
    }
    assert ws["id"] == "wrk_abc"


def test_workspaces_create_accepts_multi_repo_git() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "id": "wrk_multi",
                "created_at": "2026-04-19T00:00:00Z",
                "deleted_at": None,
                "source_dir": "/s", "output_dir": "/o",
                "config": {
                    "memory_mb": 512, "timeout_secs": 300, "cpu_percent": 100,
                    "max_pids": 64, "network_mode": "none", "network_domains": [],
                    "seccomp": "standard", "idle_timeout_secs": 0,
                },
                "git_repo": None, "git_ref": None,
                "label": None, "domains": [],
                "last_exec_at": None, "paused_at": None, "auto_snapshot": None,
            },
        )

    aj = make(h)
    aj.workspaces.create(
        git={
            "repos": [
                {"repo": "https://github.com/org/a"},
                {"repo": "https://github.com/org/b", "ref": "main", "dir": "b-main"},
            ],
        },
    )
    assert seen["body"] == {
        "git": {
            "repos": [
                {"repo": "https://github.com/org/a"},
                {"repo": "https://github.com/org/b", "ref": "main", "dir": "b-main"},
            ],
        },
    }


def test_workspaces_fork_posts_count_and_returns_forks() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        seen["body"] = json.loads(req.content)
        base_config = {
            "memory_mb": 512, "timeout_secs": 300, "cpu_percent": 100,
            "max_pids": 64, "network_mode": "none", "network_domains": [],
            "seccomp": "standard", "idle_timeout_secs": 0,
        }

        def mk(wid: str) -> dict[str, Any]:
            return {
                "id": wid,
                "created_at": "2026-04-19T00:00:00Z",
                "deleted_at": None,
                "source_dir": f"/s/{wid}", "output_dir": f"/o/{wid}",
                "config": base_config,
                "git_repo": None, "git_ref": None,
                "label": None, "domains": [],
                "last_exec_at": None, "paused_at": None, "auto_snapshot": None,
            }

        return httpx.Response(
            200,
            json={
                "parent": mk("wrk_parent"),
                "forks": [mk("wrk_f0"), mk("wrk_f1"), mk("wrk_f2")],
                "snapshot_id": "snap_origin",
            },
        )

    aj = make(h)
    r = aj.workspaces.fork("wrk_parent", count=3, label="agents")
    assert seen["url"] == "http://api/v1/workspaces/wrk_parent/fork"
    assert seen["body"] == {"count": 3, "label": "agents"}
    assert len(r["forks"]) == 3
    assert r["snapshot_id"] == "snap_origin"


def test_workspaces_fork_rejects_invalid_count() -> None:
    aj = make(lambda _: httpx.Response(200, json={}))
    with pytest.raises(ValueError):
        aj.workspaces.fork("wrk_x", count=0)
    with pytest.raises(ValueError):
        aj.workspaces.fork("wrk_x", count=17)


def test_snapshots_list_filters_by_workspace() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        return httpx.Response(200, json={"rows": [], "total": 0, "limit": 50, "offset": 0})

    aj = make(h)
    aj.snapshots.list(workspace_id="wrk_abc")
    assert seen["url"] == "http://api/v1/snapshots?workspace_id=wrk_abc"


def test_snapshots_create_workspace_from() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "id": "wrk_new", "created_at": "2026-04-19T00:00:00Z",
                "deleted_at": None, "source_dir": "/s", "output_dir": "/o",
                "config": {
                    "memory_mb": 512, "timeout_secs": 300, "cpu_percent": 100,
                    "max_pids": 64, "network_mode": "none", "network_domains": [],
                    "seccomp": "standard", "idle_timeout_secs": 0,
                },
                "git_repo": None, "git_ref": None, "label": "recovered",
                "domains": [], "last_exec_at": None,
                "paused_at": None, "auto_snapshot": None,
            },
        )

    aj = make(h)
    ws = aj.snapshots.create_workspace_from(
        "snap_xyz", parent_workspace_id="wrk_parent", label="recovered"
    )
    assert seen["url"] == "http://api/v1/workspaces/from-snapshot"
    assert seen["body"] == {
        "snapshot_id": "snap_xyz",
        "parent_workspace_id": "wrk_parent",
        "label": "recovered",
    }
    assert ws["id"] == "wrk_new"


# ---- public -----------------------------------------------------------


def test_public_health_returns_plain_text() -> None:
    def h(_: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json="ok")

    aj = make(h)
    assert aj.public.health() == "ok"


def test_public_stats_returns_counters() -> None:
    def h(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={"active_execs": 2, "total_execs": 100, "sessions": 3, "credentials": 4},
        )

    aj = make(h)
    s = aj.public.stats()
    assert s["active_execs"] == 2
    assert s["credentials"] == 4


# ---- settings ---------------------------------------------------------


def test_settings_get_returns_full_snapshot() -> None:
    seen: dict[str, Any] = {}
    payload = {
        "proxy": {
            "base_url": "http://10.0.0.1:8443",
            "bind_addr": "127.0.0.1:8443",
            "providers": [
                {
                    "service_id": "openai",
                    "upstream_base": "https://api.openai.com",
                    "request_prefix": "/v1/openai/",
                }
            ],
        },
        "control_plane": {"bind_addr": "127.0.0.1:7000"},
        "gateway": None,
        "exec": {"default_memory_mb": 512, "default_timeout_secs": 300, "max_concurrent": 16},
        "persistence": {"state_dir": "/var/lib/agentjail", "snapshot_pool_dir": None, "idle_check_secs": 30},
        "snapshots": {"gc": None},
    }

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        seen["method"] = req.method
        return httpx.Response(200, json=payload)

    aj = make(h)
    s = aj.settings.get()
    assert seen["method"] == "GET"
    assert seen["url"] == "http://api/v1/config"
    assert s["proxy"]["providers"][0]["service_id"] == "openai"
    assert s["exec"]["default_memory_mb"] == 512  # type: ignore[index]
    assert s["gateway"] is None


# ---- snapshots.manifest -----------------------------------------------


def test_snapshots_manifest_returns_entries() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        return httpx.Response(
            200,
            json={
                "kind": "incremental",
                "entries": [
                    {"path": "a.txt", "mode": 0o644, "hash": "aa", "size": 10},
                    {"path": "b/c.txt", "mode": 0o755, "hash": "bb", "size": 20},
                ],
            },
        )

    aj = make(h)
    m = aj.snapshots.manifest("snap_abc")
    assert seen["url"] == "http://api/v1/snapshots/snap_abc/manifest"
    assert m["kind"] == "incremental"
    assert len(m["entries"]) == 2
    assert m["entries"][0]["path"] == "a.txt"


def test_snapshots_manifest_classic_is_empty() -> None:
    def h(_: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"kind": "classic", "entries": []})

    aj = make(h)
    m = aj.snapshots.manifest("snap_classic")
    assert m["kind"] == "classic"
    assert m["entries"] == []


# ---- list q params ----------------------------------------------------


def test_workspaces_list_sends_q() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        return httpx.Response(200, json={"rows": [], "total": 0, "limit": 50, "offset": 0})

    aj = make(h)
    aj.workspaces.list(limit=50, q="review-bot")
    assert "q=review-bot" in seen["url"]
    assert "limit=50" in seen["url"]


def test_workspaces_list_omits_q_when_unset() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        return httpx.Response(200, json={"rows": [], "total": 0, "limit": 50, "offset": 0})

    aj = make(h)
    aj.workspaces.list(limit=50)
    assert "q=" not in seen["url"]


def test_snapshots_list_sends_q_with_workspace_id() -> None:
    seen: dict[str, Any] = {}

    def h(req: httpx.Request) -> httpx.Response:
        seen["url"] = str(req.url)
        return httpx.Response(200, json={"rows": [], "total": 0, "limit": 50, "offset": 0})

    aj = make(h)
    aj.snapshots.list(workspace_id="wrk_a", q="baseline")
    assert "q=baseline" in seen["url"]
    assert "workspace_id=wrk_a" in seen["url"]
