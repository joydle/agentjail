"""Jails — history of one-shot + workspace runs."""

from __future__ import annotations

from ._http import HttpClient
from .types import JailKind, JailRecord, JailStatus, JailsList


class Jails:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(
        self,
        *,
        limit: int | None = None,
        offset: int | None = None,
        status: JailStatus | None = None,
        kind: JailKind | None = None,
        q: str | None = None,
    ) -> JailsList:
        return self._http.request(
            "GET",
            "/v1/jails",
            params={"limit": limit, "offset": offset, "status": status, "kind": kind, "q": q},
        )

    def get(self, jail_id: int) -> JailRecord:
        return self._http.request("GET", f"/v1/jails/{jail_id}")
