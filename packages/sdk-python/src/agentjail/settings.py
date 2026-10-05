"""Read-only snapshot of the running control plane's configuration."""

from __future__ import annotations

from ._http import HttpClient
from .types import SettingsSnapshot


class Settings:
    """`GET /v1/config` — operator-facing server config.

    Safe-to-display fields only (bind addresses, GC policy, provider
    metadata). Credentials never appear here.
    """

    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def get(self) -> SettingsSnapshot:
        """Return the full settings snapshot.

        Mirrors the operator Settings page in the web UI: registered
        phantom providers, bind addresses, exec defaults, persistence
        paths, and the snapshot GC policy. Secrets are stripped server-
        side before the response is built.
        """
        resp = self._http.request("GET", "/v1/config")
        return resp  # type: ignore[return-value]
