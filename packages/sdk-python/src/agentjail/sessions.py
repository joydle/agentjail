"""Sessions — bundle phantom tokens + exec in the jail."""

from __future__ import annotations

from typing import Any

from ._http import HttpClient
from .types import ExecResult, ServiceId, Session


class Sessions:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def create(
        self,
        *,
        services: list[ServiceId],
        ttl_secs: int | None = None,
        scopes: dict[ServiceId, list[str]] | None = None,
    ) -> Session:
        body: dict[str, Any] = {"services": services}
        if ttl_secs is not None:
            body["ttl_secs"] = ttl_secs
        if scopes is not None:
            body["scopes"] = scopes
        return self._http.request("POST", "/v1/sessions", json=body)

    def list(self) -> list[Session]:
        return self._http.request("GET", "/v1/sessions")

    def get(self, session_id: str) -> Session:
        return self._http.request("GET", f"/v1/sessions/{session_id}")

    def close(self, session_id: str) -> None:
        self._http.request("DELETE", f"/v1/sessions/{session_id}")

    def exec(
        self,
        session_id: str,
        *,
        cmd: str,
        args: list[str] | None = None,
        timeout_secs: int | None = None,
        memory_mb: int | None = None,
        network: Any = None,
        seccomp: str | None = None,
        cpu_percent: int | None = None,
        max_pids: int | None = None,
    ) -> ExecResult:
        body: dict[str, Any] = {"cmd": cmd}
        if args is not None:
            body["args"] = args
        if timeout_secs is not None:
            body["timeout_secs"] = timeout_secs
        if memory_mb is not None:
            body["memory_mb"] = memory_mb
        if network is not None:
            body["network"] = network
        if seccomp is not None:
            body["seccomp"] = seccomp
        if cpu_percent is not None:
            body["cpu_percent"] = cpu_percent
        if max_pids is not None:
            body["max_pids"] = max_pids
        return self._http.request("POST", f"/v1/sessions/{session_id}/exec", json=body)
