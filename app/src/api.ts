const BASE = "http://127.0.0.1:8080";

async function get<T>(path: string): Promise<T> {
  const r = await fetch(`${BASE}${path}`);
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return r.json();
}

async function put<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(`${BASE}${path}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const err = await r.json().catch(() => ({}));
    throw new Error((err as { error?: string }).error ?? `${r.status}`);
  }
  return r.json();
}

export interface ProxyStatus {
  running: boolean;
  version: string;
  uptime_secs: number;
  usage_log_enabled: boolean;
  configured_providers: string[];
}

export interface SummaryTotals {
  count: number;
  success_count: number;
  prompt_tokens: number;
  completion_tokens: number;
}

export interface SummaryRow {
  provider: string;
  model_id: string;
  count: number;
  success_count: number;
  avg_latency_ms: number;
  p50_latency_ms: number;
  p95_latency_ms: number;
  prompt_tokens: number;
  completion_tokens: number;
}

export interface UsageSummary {
  since: string;
  totals: SummaryTotals;
  rows: SummaryRow[];
}

export interface UsageEntry {
  id: string;
  created_at: string;
  provider: string;
  model_id: string;
  status: number;
  latency_ms: number;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  total_tokens: number | null;
  stream: boolean;
  error: string | null;
}

export interface Config {
  server: { host: string; port: number };
  providers: Record<string, {
    api_key?: string;
    endpoint?: string;
    api_version?: string;
    region?: string;
  }>;
  usage_log: {
    enabled: boolean;
    retention_days: number;
  };
}

export interface ProviderPatch {
  api_key?: string;
  endpoint?: string;
  api_version?: string;
  region?: string;
}

export const api = {
  status: () => get<ProxyStatus>("/admin/status"),
  usageSummary: (since = "7d") =>
    get<UsageSummary>(`/admin/usage/summary?since=${since}`),
  usageRecent: (limit = 50) =>
    get<{ entries: UsageEntry[] }>(`/admin/usage/recent?limit=${limit}`),
  config: () => get<Config>("/admin/config"),
  updateProvider: (name: string, patch: ProviderPatch) =>
    put<{ ok: boolean }>(`/admin/config/provider/${name}`, patch),
};
