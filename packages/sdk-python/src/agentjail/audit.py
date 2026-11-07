"""Audit — phantom-proxy request ledger."""

from __future__ import annotations

from ._http import HttpClient
from .types import AuditList


class Audit:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def recent(self, limit: int | None = None) -> AuditList:
        return self._http.request(
            "GET", "/v1/audit", params={"limit": limit}
        )
