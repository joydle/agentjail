"""Runs — one-shot code execution + fork + streaming."""

from __future__ import annotations

import json as _json
from collections.abc import Iterator
from typing import Any

from ._http import HttpClient
from .types import ExecResult, ForkChild, ForkResult, StreamEvent


class Runs:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def create(
        self,
        *,
        code: str,
        language: str | None = None,
        timeout_secs: int | None = None,
        memory_mb: int | None = None,
        network: Any = None,
        seccomp: str | None = None,
        cpu_percent: int | None = None,
        max_pids: int | None = None,
    ) -> ExecResult:
        body: dict[str, Any] = {"code": code}
        if language is not None:
            body["language"] = language
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
        return self._http.request("POST", "/v1/runs", json=body)

    def fork(
        self,
        *,
        parent_code: str,
        child_code: str | None = None,
        children: list[ForkChild] | None = None,
        language: str | None = None,
        fork_after_ms: int | None = None,
        timeout_secs: int | None = None,
        memory_mb: int | None = None,
        network: Any = None,
        seccomp: str | None = None,
        cpu_percent: int | None = None,
        max_pids: int | None = None,
    ) -> ForkResult:
        if not child_code and not children:
            raise ValueError("fork: provide either child_code or children")
        if child_code and children:
            raise ValueError("fork: child_code and children are mutually exclusive")
        body: dict[str, Any] = {"parent_code": parent_code}
        if child_code:
            body["child_code"] = child_code
        if children:
            body["children"] = children
        if language is not None:
            body["language"] = language
        if fork_after_ms is not None:
            body["fork_after_ms"] = fork_after_ms
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
        return self._http.request("POST", "/v1/runs/fork", json=body)

    def stream(
        self,
        *,
        code: str,
        language: str | None = None,
        timeout_secs: int | None = None,
        memory_mb: int | None = None,
    ) -> Iterator[StreamEvent]:
        """Run code and yield SSE events until the server closes the stream.

        Usage:

            for ev in aj.runs.stream(code="print(1)", language="python"):
                if ev["type"] == "stdout":
                    print(ev["line"])
        """
        body: dict[str, Any] = {"code": code}
        if language is not None:
            body["language"] = language
        if timeout_secs is not None:
            body["timeout_secs"] = timeout_secs
        if memory_mb is not None:
            body["memory_mb"] = memory_mb
        resp = self._http.stream("POST", "/v1/runs/stream", json=body)
        try:
            yield from _parse_sse(resp.iter_text())
        finally:
            resp.close()


def _parse_sse(chunks: Iterator[str]) -> Iterator[StreamEvent]:
    buffer = ""
    for chunk in chunks:
        buffer += chunk
        while True:
            sep = buffer.find("\n\n")
            if sep < 0:
                break
            frame = buffer[:sep]
            buffer = buffer[sep + 2 :]
            parsed = _parse_frame(frame)
            if parsed is not None:
                yield parsed


def _parse_frame(frame: str) -> StreamEvent | None:
    event_type = ""
    data = ""
    for raw in frame.split("\n"):
        if raw.startswith(":"):
            continue
        if raw.startswith("event:"):
            event_type = raw[6:].strip()
        elif raw.startswith("data:"):
            data += raw[5:].lstrip()
    if not event_type:
        return None
    if event_type in {"stdout", "stderr"}:
        return {"type": event_type, "line": data}  # type: ignore[return-value]
    if event_type in {"started", "stats", "completed", "error"}:
        try:
            payload = _json.loads(data)
        except ValueError:
            return None
        return {"type": event_type, **payload}  # type: ignore[return-value]
    return None
