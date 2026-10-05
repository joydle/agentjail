"""Credentials — put/list/delete provider API keys."""

from __future__ import annotations

from ._http import HttpClient
from .types import CredentialRecord, ServiceId


class Credentials:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def put(self, *, service: ServiceId, secret: str) -> CredentialRecord:
        return self._http.request(
            "POST", "/v1/credentials", json={"service": service, "secret": secret}
        )

    def list(self) -> list[CredentialRecord]:
        return self._http.request("GET", "/v1/credentials")

    def delete(self, service: ServiceId) -> None:
        self._http.request("DELETE", f"/v1/credentials/{service}")
