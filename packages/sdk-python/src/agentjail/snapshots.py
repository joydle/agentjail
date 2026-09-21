"""Snapshots — capture & restore workspace output dirs."""

from __future__ import annotations

from typing import Any

from ._http import HttpClient
from .types import SnapshotList, SnapshotManifest, SnapshotRecord, Workspace


class Snapshots:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def create(
        self,
        workspace_id: str,
        *,
        name: str | None = None,
    ) -> SnapshotRecord:
        body: dict[str, Any] = {}
        if name is not None:
            body["name"] = name
        return self._http.request(
            "POST", f"/v1/workspaces/{workspace_id}/snapshot", json=body
        )

    def list(
        self,
        *,
        workspace_id: str | None = None,
        limit: int | None = None,
        offset: int | None = None,
        q: str | None = None,
    ) -> SnapshotList:
        """List snapshots; optionally filtered to a workspace or by
        a substring search (``q``) matching ``id`` / ``name`` /
        ``workspace_id``.
        """
        return self._http.request(
            "GET",
            "/v1/snapshots",
            params={
                "workspace_id": workspace_id,
                "limit": limit,
                "offset": offset,
                "q": q,
            },
        )

    def get(self, id: str) -> SnapshotRecord:
        """Fetch the snapshot record by id."""
        return self._http.request("GET", f"/v1/snapshots/{id}")

    def manifest(self, id: str) -> SnapshotManifest:
        """List the files inside a pool-backed snapshot.

        ``kind="incremental"`` with populated ``entries`` when the
        snapshot was captured into a content-addressed object pool;
        ``kind="classic"`` with empty ``entries`` for full-copy
        snapshots where the file list isn't persisted.
        """
        return self._http.request("GET", f"/v1/snapshots/{id}/manifest")

    def delete(self, id: str) -> None:
        """Remove a snapshot + its on-disk dir. Idempotent."""
        self._http.request("DELETE", f"/v1/snapshots/{id}")

    def create_workspace_from(
        self,
        id: str,
        *,
        parent_workspace_id: str,
        label: str | None = None,
    ) -> Workspace:
        """Rehydrate a snapshot into a brand-new workspace.

        ``parent_workspace_id`` is the ownership gate — it must match
        the snapshot's recorded parent. The server returns 404 on
        mismatch so no hints leak about other tenants' snapshots.
        """
        body: dict[str, Any] = {
            "snapshot_id":         id,
            "parent_workspace_id": parent_workspace_id,
        }
        if label is not None:
            body["label"] = label
        return self._http.request("POST", "/v1/workspaces/from-snapshot", json=body)
