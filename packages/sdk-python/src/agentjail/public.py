"""Public — unauthenticated health + live counters."""

from __future__ import annotations

from ._http import HttpClient
from .types import PublicStats


class Public:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def health(self) -> str:
        """Returns the string ``"ok"`` when the server is up."""
        return self._http.request("GET", "/healthz")

    def stats(self) -> PublicStats:
        return self._http.request("GET", "/v1/stats")
