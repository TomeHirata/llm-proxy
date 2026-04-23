import { Fragment, useEffect, useState } from "react";
import { api, type UsageSummary, type UsageEntry } from "../api";

const RANGES = [
  { label: "1h", value: "1h" },
  { label: "24h", value: "24h" },
  { label: "7d", value: "7d" },
  { label: "30d", value: "30d" },
];

interface Props {
  proxyOnline: boolean;
}

export default function UsageTab({ proxyOnline }: Props) {
  const [range, setRange] = useState("7d");
  const [summary, setSummary] = useState<UsageSummary | null>(null);
  const [recent, setRecent] = useState<UsageEntry[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    if (!proxyOnline) return;
    setLoading(true);
    setError(null);
    try {
      const [s, r] = await Promise.all([
        api.usageSummary(range),
        api.usageRecent(100),
      ]);
      setSummary(s);
      setRecent(r.entries);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, [range, proxyOnline]);

  if (!proxyOnline) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-400">
        Proxy is not running
      </div>
    );
  }

  return (
    <div className="p-5 space-y-6">
      {/* Range picker */}
      <div className="flex items-center gap-2">
        <span className="text-sm text-gray-500 mr-1">Time range:</span>
        {RANGES.map(({ label, value }) => (
          <button
            key={value}
            onClick={() => setRange(value)}
            className={`px-3 py-1 rounded text-sm font-medium transition-colors ${
              range === value
                ? "bg-blue-500 text-white"
                : "bg-gray-100 text-gray-600 hover:bg-gray-200"
            }`}
          >
            {label}
          </button>
        ))}
        <button
          onClick={refresh}
          disabled={loading}
          className="ml-auto px-3 py-1 text-sm text-gray-500 hover:text-gray-700 disabled:opacity-40"
        >
          {loading ? "Loading…" : "↻ Refresh"}
        </button>
      </div>

      {error && (
        <div className="p-3 bg-red-50 text-red-700 rounded text-sm">{error}</div>
      )}

      {/* Summary cards */}
      {summary && (
        <>
          <div className="grid grid-cols-4 gap-4">
            <StatCard label="Requests" value={summary.totals.count} />
            <StatCard
              label="Success rate"
              value={
                summary.totals.count > 0
                  ? `${Math.round((summary.totals.success_count / summary.totals.count) * 100)}%`
                  : "—"
              }
            />
            <StatCard
              label="Prompt tokens"
              value={fmt(summary.totals.prompt_tokens)}
            />
            <StatCard
              label="Completion tokens"
              value={fmt(summary.totals.completion_tokens)}
            />
          </div>

          {/* Per-provider breakdown */}
          {summary.rows.length > 0 && (
            <div>
              <h3 className="text-sm font-semibold text-gray-700 mb-2">
                By provider / model
              </h3>
              <div className="overflow-x-auto rounded-lg border border-gray-200">
                <table className="w-full text-sm">
                  <thead className="bg-gray-50 text-gray-500 uppercase text-xs">
                    <tr>
                      {["Provider", "Model", "Reqs", "OK%", "p50 ms", "p95 ms", "Tokens in", "Tokens out"].map(
                        (h) => (
                          <th key={h} className="px-3 py-2 text-left font-medium">
                            {h}
                          </th>
                        )
                      )}
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-gray-100">
                    {summary.rows.map((r, i) => (
                      <tr key={i} className="hover:bg-gray-50">
                        <td className="px-3 py-2 font-medium">{r.provider}</td>
                        <td className="px-3 py-2 text-gray-600 font-mono text-xs">
                          {r.model_id}
                        </td>
                        <td className="px-3 py-2">{r.count}</td>
                        <td className="px-3 py-2">
                          {r.count > 0
                            ? `${Math.round((r.success_count / r.count) * 100)}%`
                            : "—"}
                        </td>
                        <td className="px-3 py-2">{r.p50_latency_ms}</td>
                        <td className="px-3 py-2">{r.p95_latency_ms}</td>
                        <td className="px-3 py-2">{fmt(r.prompt_tokens)}</td>
                        <td className="px-3 py-2">{fmt(r.completion_tokens)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </>
      )}

      {/* Recent requests */}
      <div>
        <h3 className="text-sm font-semibold text-gray-700 mb-2">
          Recent requests
        </h3>
        <div className="rounded-lg border border-gray-200 overflow-hidden">
          {recent.length === 0 ? (
            <div className="p-6 text-center text-gray-400 text-sm">
              No requests yet
            </div>
          ) : (
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-gray-500 uppercase text-xs">
                <tr>
                  {["Time", "Provider", "Model", "Status", "Latency", "Tokens"].map(
                    (h) => (
                      <th key={h} className="px-3 py-2 text-left font-medium">
                        {h}
                      </th>
                    )
                  )}
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {recent.map((e) => (
                  <Fragment key={e.id}>
                    <tr
                      className="hover:bg-gray-50 cursor-pointer"
                      onClick={() =>
                        setExpanded(expanded === e.id ? null : e.id)
                      }
                    >
                      <td className="px-3 py-2 text-gray-500 whitespace-nowrap">
                        {new Date(e.created_at).toLocaleTimeString()}
                      </td>
                      <td className="px-3 py-2 font-medium">{e.provider}</td>
                      <td className="px-3 py-2 text-gray-600 font-mono text-xs max-w-xs truncate">
                        {e.model_id}
                      </td>
                      <td className="px-3 py-2">
                        <span
                          className={`px-1.5 py-0.5 rounded text-xs font-medium ${
                            e.status >= 200 && e.status < 300
                              ? "bg-green-100 text-green-700"
                              : "bg-red-100 text-red-700"
                          }`}
                        >
                          {e.status}
                        </span>
                      </td>
                      <td className="px-3 py-2 tabular-nums">
                        {e.latency_ms}ms
                      </td>
                      <td className="px-3 py-2 tabular-nums text-gray-600">
                        {e.total_tokens ?? "—"}
                      </td>
                    </tr>
                    {expanded === e.id && e.error && (
                      <tr key={`${e.id}-err`}>
                        <td
                          colSpan={6}
                          className="px-3 py-2 bg-red-50 text-red-700 text-xs font-mono"
                        >
                          {e.error}
                        </td>
                      </tr>
                    )}
                  </Fragment>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  );
}

function StatCard({
  label,
  value,
}: {
  label: string;
  value: string | number;
}) {
  return (
    <div className="bg-white rounded-lg border border-gray-200 p-4">
      <div className="text-xs text-gray-500 uppercase font-medium mb-1">
        {label}
      </div>
      <div className="text-2xl font-semibold tabular-nums">{value}</div>
    </div>
  );
}

function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}
