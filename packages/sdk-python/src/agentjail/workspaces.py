"""Workspaces — persistent multi-exec mount trees."""

from __future__ import annotations

from typing import Any

from ._http import HttpClient
from .types import (
    ExecResult,
    GitRepoEntry,
    NetworkSpec,
    Workspace,
    WorkspaceDomain,
    WorkspaceForkResponse,
    WorkspaceList,
)


class Workspaces:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def create(
        self,
        *,
        git: dict[str, str] | dict[str, list[GitRepoEntry]] | None = None,
        label: str | None = None,
        memory_mb: int | None = None,
        timeout_secs: int | None = None,
        idle_timeout_secs: int | None = None,
        domains: list[WorkspaceDomain] | None = None,
        flavors: list[str] | None = None,
        network: NetworkSpec | None = None,
        seccomp: str | None = None,
        cpu_percent: int | None = None,
        max_pids: int | None = None,
    ) -> Workspace:
        """Create a persistent workspace.

        ``git`` accepts either the single-repo shape ``{"repo": url, "ref"?: ref}``
        or the multi-repo shape ``{"repos": [{"repo": url, "ref"?: ref, "dir"?: subdir}]}``.

        ``flavors`` lists runtime overlays to bind-mount into the jail
        (e.g. ``["nodejs", "python"]``). Each name is validated against
        the server's registry (see ``GET /v1/flavors``); unknown names
        return 400 at create.
        """
        body: dict[str, Any] = {}
        if git is not None:
            body["git"] = git
        if label is not None:
            body["label"] = label
        if memory_mb is not None:
            body["memory_mb"] = memory_mb
        if timeout_secs is not None:
            body["timeout_secs"] = timeout_secs
        if idle_timeout_secs is not None:
            body["idle_timeout_secs"] = idle_timeout_secs
        if domains is not None:
            body["domains"] = domains
        if flavors is not None:
            body["flavors"] = flavors
        if network is not None:
            body["network"] = network
        if seccomp is not None:
            body["seccomp"] = seccomp
        if cpu_percent is not None:
            body["cpu_percent"] = cpu_percent
        if max_pids is not None:
            body["max_pids"] = max_pids
        return self._http.request("POST", "/v1/workspaces", json=body)

    def list(
        self,
        *,
        limit: int | None = None,
        offset: int | None = None,
        q: str | None = None,
    ) -> WorkspaceList:
        """Paginated list, newest first.

        When ``q`` is set, filters to rows whose ``id``, ``label``, or
        ``git_repo`` contain the needle (case-insensitive). ``total``
        reflects the filtered count.
        """
        return self._http.request(
            "GET",
            "/v1/workspaces",
            params={"limit": limit, "offset": offset, "q": q},
        )

    def get(self, workspace_id: str) -> Workspace:
        return self._http.request("GET", f"/v1/workspaces/{workspace_id}")

    def delete(self, workspace_id: str) -> None:
        self._http.request("DELETE", f"/v1/workspaces/{workspace_id}")

    def exec(
        self,
        workspace_id: str,
        *,
        cmd: str,
        args: list[str] | None = None,
        timeout_secs: int | None = None,
        memory_mb: int | None = None,
        env: list[tuple[str, str]] | None = None,
    ) -> ExecResult:
        body: dict[str, Any] = {"cmd": cmd}
        if args is not None:
            body["args"] = args
        if timeout_secs is not None:
            body["timeout_secs"] = timeout_secs
        if memory_mb is not None:
            body["memory_mb"] = memory_mb
        if env is not None:
            body["env"] = [list(pair) for pair in env]
        return self._http.request("POST", f"/v1/workspaces/{workspace_id}/exec", json=body)

    def fork(
        self,
        workspace_id: str,
        *,
        count: int,
        label: str | None = None,
    ) -> WorkspaceForkResponse:
        """Atomic N-way fork of a persistent workspace.

        Captures a single snapshot of the parent (freezing any in-flight
        exec for consistency), spawns ``count`` independent workspaces
        from it, and returns them all. Good for devin/cursor-style
        "N agents off the same state" patterns:

        .. code-block:: python

           r = aj.workspaces.fork(ws["id"], count=3, label="agents")
           for fork in r["forks"]:
               aj.workspaces.exec(fork["id"], cmd="do-thing")
        """
        if count < 1 or count > 16:
            raise ValueError("workspaces.fork: count must be 1..=16")
        body: dict[str, Any] = {"count": count}
        if label is not None:
            body["label"] = label
        return self._http.request(
            "POST", f"/v1/workspaces/{workspace_id}/fork", json=body
        )
