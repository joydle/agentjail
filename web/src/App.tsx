import { BrowserRouter, Routes, Route, Navigate, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AuthProvider, useAuth, useWhoami } from "./lib/auth";
import { Shell } from "./components/Shell";
import { Login } from "./components/Login";
import { Orbit } from "./components/Orbit";
import { Landing } from "./pages/Landing";
import { Overview } from "./pages/Overview";
import { Jails } from "./pages/Jails";
import { Sessions } from "./pages/Sessions";
import { Credentials } from "./pages/Credentials";
import { Stream } from "./pages/Stream";
import { Playground } from "./pages/Playground";
import { Settings } from "./pages/Settings";
import { Accounts } from "./pages/Accounts";
import { Workspaces } from "./pages/Workspaces";
import { Snapshots } from "./pages/Snapshots";
import { DocsShell } from "./components/docs/DocsShell";
import { Quickstart } from "./pages/docs/Quickstart";
import { Sdk } from "./pages/docs/Sdk";
import { Phantom } from "./pages/docs/Phantom";
import { Network } from "./pages/docs/Network";
import { Forking } from "./pages/docs/Forking";
import { Security } from "./pages/docs/Security";
import { PlaygroundDoc } from "./pages/docs/PlaygroundDoc";
import { Workspaces as WorkspacesDoc } from "./pages/docs/Workspaces";
import { Snapshots as SnapshotsDoc } from "./pages/docs/Snapshots";
import { Gateway as GatewayDoc } from "./pages/docs/Gateway";

const qc = new QueryClient({
  defaultOptions: {
    queries: { refetchOnWindowFocus: false, retry: 1 },
  },
});

function Gate() {
  const { auth } = useAuth();
  return (
    <Routes>
      <Route path="/docs" element={<DocsShell />}>
        <Route index               element={<Navigate to="quickstart" replace />} />
        <Route path="quickstart"   element={<Quickstart />} />
        <Route path="sdk"          element={<Sdk />} />
        <Route path="playground"   element={<PlaygroundDoc />} />
        <Route path="phantom"      element={<Phantom />} />
        <Route path="network"      element={<Network />} />
        <Route path="forking"      element={<Forking />} />
        <Route path="workspaces"   element={<WorkspacesDoc />} />
        <Route path="snapshots"    element={<SnapshotsDoc />} />
        <Route path="gateway"      element={<GatewayDoc />} />
        <Route path="security"     element={<Security />} />
      </Route>
      {auth ? (
        <>
          {/* Tenant-prefixed routes. The `:tenant` segment is purely a
              URL affordance — the server auth scope is the source of
              truth, so an operator can't escalate by editing the URL. */}
          <Route path="/t/:tenant" element={<Shell />}>
            <Route index                     element={<Overview />} />
            <Route path="projects"           element={<Workspaces />} />
            <Route path="sessions"           element={<Sessions />} />
            <Route path="integrations"       element={<Credentials />} />
            <Route path="playground"         element={<Playground />} />
            <Route path="operator/ledger"    element={<Jails />} />
            <Route path="operator/snapshots" element={<Snapshots />} />
            <Route path="operator/audit"     element={<Stream />} />
            <Route path="operator/settings"  element={<Settings />} />
            <Route path="operator/accounts"  element={<Accounts />} />
          </Route>

          {/* Un-prefixed root + legacy aliases — redirect into the
              caller's own tenant once whoami has resolved. */}
          <Route path="/"                element={<RedirectToTenantHome />} />
          <Route path="/projects"        element={<RedirectToTenantHome sub="projects" />} />
          <Route path="/sessions"        element={<RedirectToTenantHome sub="sessions" />} />
          <Route path="/integrations"    element={<RedirectToTenantHome sub="integrations" />} />
          <Route path="/playground"      element={<RedirectToTenantHome sub="playground" />} />
          <Route path="/operator/*"      element={<RedirectToTenantHome legacyOperator />} />

          {/* Older aliases kept so bookmarks keep working. */}
          <Route path="/workspaces"  element={<RedirectToTenantHome sub="projects" />} />
          <Route path="/credentials" element={<RedirectToTenantHome sub="integrations" />} />
          <Route path="/jails"       element={<RedirectToTenantHome sub="operator/ledger" />} />
          <Route path="/snapshots"   element={<RedirectToTenantHome sub="operator/snapshots" />} />
          <Route path="/stream"      element={<RedirectToTenantHome sub="operator/audit" />} />
          <Route path="/settings"    element={<RedirectToTenantHome sub="operator/settings" />} />

          <Route path="/login" element={<Navigate to="/" replace />} />
          <Route path="*"      element={<RedirectToTenantHome />} />
        </>
      ) : (
        <>
          <Route path="/"      element={<Landing />} />
          <Route path="/login" element={<Login />} />
          <Route path="*"      element={<Navigate to="/" replace />} />
        </>
      )}
    </Routes>
  );
}

/**
 * Redirect into the caller's own tenant once `whoami` resolves.
 *
 * `sub` appends a sub-path (e.g. `projects` → `/t/<tenant>/projects`).
 * `legacyOperator` carries the existing operator sub-path from the
 * URL so `/operator/ledger?…` lands on `/t/<tenant>/operator/ledger?…`.
 *
 * Renders nothing while whoami is in flight — that's a flash of blank
 * header under an authenticated session, not a login flicker, so it's
 * fine visually.
 */
function RedirectToTenantHome(
  { sub, legacyOperator = false }: { sub?: string; legacyOperator?: boolean } = {},
) {
  const me = useWhoami();
  const location = useLocation();
  if (!me) return null;
  const tail = legacyOperator
    ? location.pathname.replace(/^\/operator\/?/, "operator/")
    : sub;
  const to = tail ? `/t/${me.tenant}/${tail}` : `/t/${me.tenant}`;
  return <Navigate to={to + location.search} replace />;
}

export default function App() {
  return (
    <QueryClientProvider client={qc}>
      <AuthProvider>
        <Orbit />
        <BrowserRouter>
          <Gate />
        </BrowserRouter>
      </AuthProvider>
    </QueryClientProvider>
  );
}
