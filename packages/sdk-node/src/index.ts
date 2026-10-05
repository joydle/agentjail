/**
 * `@agentjail/sdk` — TypeScript client for the agentjail control plane.
 *
 * ```ts
 * import { Agentjail } from "@agentjail/sdk";
 *
 * const aj = new Agentjail({
 *   baseUrl: "http://localhost:7000",
 *   apiKey: process.env.AGENTJAIL_API_KEY!,
 * });
 *
 * await aj.credentials.put({ service: "openai", secret: "sk-real" });
 *
 * const session = await aj.sessions.create({
 *   services: ["openai"],
 *   ttlSecs: 600,
 * });
 * // session.env is the set of env vars to export in the sandbox.
 * // The sandbox sees only phantom tokens; real secrets never leave the host.
 * ```
 */

import { Audit } from "./audit.js";
import { Credentials } from "./credentials.js";
import { HttpClient, type HttpConfig } from "./http.js";
import { Jails } from "./jails.js";
import { Public } from "./public.js";
import { Runs } from "./runs.js";
import { Sessions } from "./sessions.js";
import { Settings } from "./settings.js";
import { Snapshots } from "./snapshots.js";
import { Workspaces } from "./workspaces.js";

export { AgentjailError, type AgentjailErrorCode } from "./http.js";
export type {
  AuditList,
  AuditRow,
  CredentialRecord,
  ExecOptions,
  ExecResult,
  ForkChild,
  ForkMeta,
  ForkRequest,
  ForkResult,
  JailConfigSnapshot,
  JailKind,
  JailRecord,
  JailsList,
  JailStatus,
  NetworkSpec,
  PublicStats,
  ResourceStats,
  ProviderInfo,
  RunRequest,
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
  GitSeed,
  WorkspaceCreateRequest,
  WorkspaceDomain,
  WorkspaceExecRequest,
  WorkspaceForkResponse,
  WorkspaceList,
  WorkspaceSpec,
} from "./types.js";

/** Top-level client. Sub-namespaces are independently usable. */
export class Agentjail {
  /** Credentials sub-API. */
  public readonly credentials: Credentials;
  /** Sessions sub-API (create, exec, close). */
  public readonly sessions: Sessions;
  /** One-shot code execution. */
  public readonly runs: Runs;
  /** Audit sub-API. */
  public readonly audit: Audit;
  /** Jail-run ledger sub-API. */
  public readonly jails: Jails;
  /** Persistent workspaces (multi-exec filesystem persistence). */
  public readonly workspaces: Workspaces;
  /** Named snapshots of workspace output dirs. */
  public readonly snapshots: Snapshots;
  /** Read-only snapshot of the running server's configuration. */
  public readonly settings: Settings;
  /** Unauthenticated health + stats. */
  public readonly public: Public;

  constructor(config: HttpConfig) {
    const http = new HttpClient(config);
    this.credentials = new Credentials(http);
    this.sessions = new Sessions(http);
    this.runs = new Runs(http);
    this.audit = new Audit(http);
    this.jails = new Jails(http);
    this.workspaces = new Workspaces(http);
    this.snapshots = new Snapshots(http);
    this.settings = new Settings(http);
    this.public = new Public(http);
  }
}
