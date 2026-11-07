"""Read-only snapshot of the running control plane's configuration."""

from __future__ import annotations

from ._http import HttpClient
from .types import Flavor, SettingsSnapshot, Whoami


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
        resp: SettingsSnapshot = self._http.request("GET", "/v1/config")
        return resp

    def whoami(self) -> Whoami:
        """`GET /v1/whoami`: the tenant and role of the calling API key."""
        resp: Whoami = self._http.request("GET", "/v1/whoami")
        return resp

    def flavors(self) -> list[Flavor]:
        """`GET /v1/flavors`: runtime flavors a workspace can select."""
        resp: list[Flavor] = self._http.request("GET", "/v1/flavors")
        return resp
