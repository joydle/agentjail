"""Minimal HTTP client shared by every sub-API."""

from __future__ import annotations

from typing import Any, Literal

import httpx


AgentjailErrorCode = Literal[
    "BAD_REQUEST",
    "UNAUTHORIZED",
    "FORBIDDEN",
    "NOT_FOUND",
    "CONFLICT",
    "RATE_LIMITED",
    "TIMEOUT",
    "SERVER_ERROR",
    "NETWORK",
    "UNKNOWN",
]


def _status_to_code(status: int) -> AgentjailErrorCode:
    if status == 0:
        return "NETWORK"
    if status in (408, 504):
        return "TIMEOUT"
    if status == 429:
        return "RATE_LIMITED"
    mapping: dict[int, AgentjailErrorCode] = {
        400: "BAD_REQUEST",
        401: "UNAUTHORIZED",
        403: "FORBIDDEN",
        404: "NOT_FOUND",
        409: "CONFLICT",
    }
    if status in mapping:
        return mapping[status]
    if 500 <= status < 600:
        return "SERVER_ERROR"
    if 400 <= status < 500:
        return "BAD_REQUEST"
    return "UNKNOWN"


class AgentjailError(Exception):
    """Raised when the control plane returns a non-2xx status.

    Inspect ``code`` for a stable categorical reason rather than parsing
    ``message``. ``status`` is the raw HTTP code.
    """

    def __init__(self, status: int, body: Any, fallback: str) -> None:
        if isinstance(body, dict) and isinstance(body.get("error"), str):
            message = body["error"]
        else:
            message = fallback
        super().__init__(f"agentjail {status}: {message}")
        self.status = status
        self.code: AgentjailErrorCode = _status_to_code(status)
        self.body = body


class HttpClient:
    """Thin wrapper around `httpx.Client` with Bearer auth + error mapping."""

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str | None = None,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        if not base_url:
            raise ValueError("base_url is required")
        self._base_url = base_url.rstrip("/")
        self._api_key = api_key
        self._client = httpx.Client(
            base_url=self._base_url,
            transport=transport,
            timeout=httpx.Timeout(30.0, connect=5.0),
        )

    def close(self) -> None:
        self._client.close()

    def _headers(self) -> dict[str, str]:
        headers = {"accept": "application/json"}
        if self._api_key:
            headers["authorization"] = f"Bearer {self._api_key}"
        return headers

    def request(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
        params: dict[str, Any] | None = None,
    ) -> Any:
        """Issue a JSON request. Returns the decoded body (or `None` on 204)."""
        clean_params = (
            {k: v for k, v in params.items() if v is not None}
            if params is not None
            else None
        )
        resp = self._client.request(
            method,
            path,
            headers=self._headers(),
            json=json,
            params=clean_params,
        )
        if resp.status_code == 204:
            return None
        text = resp.text
        try:
            parsed: Any = resp.json() if text else None
        except ValueError:
            parsed = None
        if resp.status_code >= 400:
            raise AgentjailError(resp.status_code, parsed, resp.reason_phrase)
        return parsed if parsed is not None else text

    def stream(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
    ) -> httpx.Response:
        """Open a streaming response. Caller is responsible for closing it.

        Used by `runs.stream` / `workspaces.exec_stream` to consume SSE.
        """
        headers = {**self._headers(), "accept": "text/event-stream"}
        if json is not None:
            headers["content-type"] = "application/json"
        req = self._client.build_request(method, path, headers=headers, json=json)
        resp = self._client.send(req, stream=True)
        if resp.status_code >= 400:
            try:
                body = resp.read().decode(errors="replace")
            finally:
                resp.close()
            raise AgentjailError(resp.status_code, body, resp.reason_phrase)
        return resp
