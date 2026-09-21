"""agentjail — Python client for the agentjail control plane.

```python
from agentjail import Agentjail

aj = Agentjail(base_url="http://localhost:7000", api_key="aj_local_...")
aj.credentials.put(service="openai", secret="sk-real")

session = aj.sessions.create(services=["openai"], ttl_secs=600)
# Hand session.env to whatever sandbox you use; the sandbox only sees
# phantom tokens + base URLs pointing at the proxy.
```
"""

from ._http import AgentjailError, AgentjailErrorCode, HttpClient
from .audit import Audit
from .credentials import Credentials
from .jails import Jails
from .public import Public
from .runs import Runs
from .sessions import Sessions
from .settings import Settings
from .snapshots import Snapshots
from .types import (
    AuditList,
    AuditRow,
    CredentialRecord,
    ExecResult,
    ForkChild,
    ForkMeta,
    ForkResult,
    GitRepoEntry,
    JailConfigSnapshot,
    JailKind,
    JailRecord,
    JailsList,
    JailStatus,
    NetworkSpec,
    ProviderInfo,
    PublicStats,
    ResourceStats,
    SeccompSpec,
    ServiceId,
    Session,
    SettingsSnapshot,
    SnapshotList,
    SnapshotManifest,
    SnapshotManifestEntry,
    SnapshotRecord,
    StreamEvent,
    Workspace,
    WorkspaceDomain,
    WorkspaceForkResponse,
    WorkspaceList,
    WorkspaceSpec,
)
from .workspaces import Workspaces

__all__ = [
    "Agentjail",
    "AgentjailError",
    "AgentjailErrorCode",
    # sub-APIs
    "Audit",
    "Credentials",
    "Jails",
    "Public",
    "Runs",
    "Sessions",
    "Settings",
    "Snapshots",
    "Workspaces",
    # types
    "AuditList",
    "AuditRow",
    "CredentialRecord",
    "ExecResult",
    "ForkChild",
    "ForkMeta",
    "ForkResult",
    "GitRepoEntry",
    "JailConfigSnapshot",
    "JailKind",
    "JailRecord",
    "JailsList",
    "JailStatus",
    "NetworkSpec",
    "ProviderInfo",
    "PublicStats",
    "ResourceStats",
    "SeccompSpec",
    "ServiceId",
    "Session",
    "SettingsSnapshot",
    "SnapshotList",
    "SnapshotManifest",
    "SnapshotManifestEntry",
    "SnapshotRecord",
    "StreamEvent",
    "Workspace",
    "WorkspaceDomain",
    "WorkspaceForkResponse",
    "WorkspaceList",
    "WorkspaceSpec",
]


class Agentjail:
    """Top-level client. Sub-namespaces are independently usable."""

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str | None = None,
        transport: "httpx.BaseTransport | None" = None,  # noqa: F821
    ) -> None:
        self._http = HttpClient(base_url=base_url, api_key=api_key, transport=transport)
        self.credentials = Credentials(self._http)
        self.sessions = Sessions(self._http)
        self.runs = Runs(self._http)
        self.audit = Audit(self._http)
        self.jails = Jails(self._http)
        self.workspaces = Workspaces(self._http)
        self.snapshots = Snapshots(self._http)
        self.settings = Settings(self._http)
        self.public = Public(self._http)

    def close(self) -> None:
        """Close the underlying HTTP connection pool. Safe to call twice."""
        self._http.close()

    def __enter__(self) -> "Agentjail":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


# httpx re-export so the transport type hint resolves without an import at
# the module top (avoids an unconditional httpx import when type-checking).
import httpx  # noqa: E402
