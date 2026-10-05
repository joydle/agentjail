import { createContext, use, useState, useCallback, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { createApi, type Api, type Whoami } from "./api";

interface AuthState {
  apiKey: string;
  baseUrl: string;
  api: Api;
}

interface AuthContext {
  auth: AuthState | null;
  login: (baseUrl: string, apiKey: string) => Promise<void>;
  logout: () => void;
  isLoading: boolean;
  error: string | null;
}

const Ctx = createContext<AuthContext | null>(null);
const STORAGE_KEY = "agentjail_auth";

function loadSaved(): { baseUrl: string; apiKey: string } | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [auth, setAuth] = useState<AuthState | null>(() => {
    const saved = loadSaved();
    if (!saved) return null;
    return { ...saved, api: createApi(saved.baseUrl, saved.apiKey) };
  });
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const login = useCallback(async (baseUrl: string, apiKey: string) => {
    setIsLoading(true);
    setError(null);
    try {
      const url = baseUrl.replace(/\/+$/, "");
      const res = await fetch(`${url}/healthz`);
      if (!res.ok) throw new Error(`Server returned ${res.status}`);
      const api = createApi(url, apiKey);
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ baseUrl: url, apiKey }));
      setAuth({ baseUrl: url, apiKey, api });
    } catch (e) {
      setError(e instanceof Error ? e.message : "Connection failed");
      throw e;
    } finally {
      setIsLoading(false);
    }
  }, []);

  const logout = useCallback(() => {
    localStorage.removeItem(STORAGE_KEY);
    setAuth(null);
  }, []);

  return (
    <Ctx value={{ auth, login, logout, isLoading, error }}>
      {children}
    </Ctx>
  );
}

export function useAuth() {
  const ctx = use(Ctx);
  if (!ctx) throw new Error("useAuth must be used within AuthProvider");
  return ctx;
}

export function useApi(): Api {
  const { auth } = useAuth();
  if (!auth) throw new Error("Not authenticated");
  return auth.api;
}

/**
 * The active session's tenant + role, resolved via `/v1/whoami`.
 * Returns `null` while the fetch is in flight or when auth is absent,
 * so callers can render safely before the scope is known.
 *
 * Cached against the API-key prefix so switching accounts invalidates
 * the value; 5-minute staleTime is generous — role changes require a
 * server-side key edit + redeploy today.
 */
export function useWhoami(): Whoami | null {
  const { auth } = useAuth();
  const api = auth?.api;
  const key = auth?.apiKey.slice(0, 6) ?? "anon";
  const { data } = useQuery({
    queryKey: ["whoami", key],
    queryFn: () => api!.whoami(),
    enabled: !!api,
    staleTime: 5 * 60 * 1000,
  });
  return data ?? null;
}

/**
 * Convenience — `true` when the active session is an admin, `false`
 * otherwise (including while whoami is still loading). Use for
 * hiding admin-only panels; prefer checking the explicit role string
 * when rendering status badges.
 */
export function useIsAdmin(): boolean {
  return useWhoami()?.role === "admin";
}
