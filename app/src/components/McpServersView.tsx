import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type McpTransport = "stdio" | "http";

interface McpServer {
  id: string;
  name: string;
  transport: McpTransport;
  command: string;
  args: string[];
  env: Record<string, string>;
  url: string;
  headers: Record<string, string>;
  agents: string[];
}

const AGENTS = [
  { key: "claude_code", label: "Claude Code" },
  { key: "codex", label: "Codex CLI" },
  { key: "gemini", label: "Gemini CLI" },
] as const;

type AgentKey = (typeof AGENTS)[number]["key"];

interface Props {
  onBack: () => void;
}

interface FormState {
  name: string;
  transport: McpTransport;
  // stdio
  command: string;
  args: string;   // newline-separated
  env: string;    // KEY=VALUE newline-separated
  // http/sse
  url: string;
  headers: string; // KEY: VALUE newline-separated
  agents: AgentKey[];
}

const emptyForm = (): FormState => ({
  name: "",
  transport: "http",
  command: "",
  args: "",
  env: "",
  url: "",
  headers: "",
  agents: ["claude_code", "codex", "gemini"],
});

function parseKvLines(raw: string, sep: string): Record<string, string> {
  const result: Record<string, string> = {};
  for (const line of raw.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf(sep);
    if (idx === -1) continue;
    result[trimmed.slice(0, idx).trim()] = trimmed.slice(idx + sep.length).trim();
  }
  return result;
}

function serializeKv(map: Record<string, string>, sep: string): string {
  return Object.entries(map).map(([k, v]) => `${k}${sep}${v}`).join("\n");
}

export default function McpServersView({ onBack }: Props) {
  const [servers, setServers] = useState<McpServer[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(emptyForm());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [importing, setImporting] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const s = await invoke<McpServer[]>("read_mcp_servers");
      setServers(s);
    } catch { /* non-fatal */ }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const agentCount = (key: AgentKey) =>
    servers.filter((s) => s.agents.includes(key)).length;

  const openAdd = () => {
    setEditingId(null);
    setForm(emptyForm());
    setError("");
    setShowForm(true);
  };

  const openEdit = (s: McpServer) => {
    setEditingId(s.id);
    setForm({
      name: s.name,
      transport: s.transport ?? "stdio",
      command: s.command,
      args: s.args.join("\n"),
      env: serializeKv(s.env, "="),
      url: s.url,
      headers: serializeKv(s.headers, ": "),
      agents: s.agents as AgentKey[],
    });
    setError("");
    setShowForm(true);
  };

  const submit = async () => {
    if (!form.name.trim()) {
      setError("Name is required.");
      return;
    }
    if (form.transport === "stdio" && !form.command.trim()) {
      setError("Command is required for stdio servers.");
      return;
    }
    if ((form.transport === "sse" || form.transport === "http") && !form.url.trim()) {
      setError("URL is required for HTTP/SSE servers.");
      return;
    }
    setBusy(true);
    setError("");
    const payload = {
      name: form.name.trim(),
      transport: form.transport,
      command: form.command.trim(),
      args: form.args.split("\n").map((a) => a.trim()).filter(Boolean),
      env: parseKvLines(form.env, "="),
      url: form.url.trim(),
      headers: parseKvLines(form.headers, ":"),
      agents: form.agents,
    };
    try {
      if (editingId) {
        await invoke("update_mcp_server", { id: editingId, server: payload });
      } else {
        await invoke("add_mcp_server", { server: payload });
      }
      setShowForm(false);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    setBusy(true);
    try {
      await invoke("remove_mcp_server", { id });
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const importExisting = async () => {
    setImporting(true);
    setError("");
    try {
      const imported = await invoke<McpServer[]>("import_mcp_servers");
      await refresh();
      if (imported.length === 0) {
        setError("No new MCP servers found in agent config files.");
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setImporting(false);
    }
  };

  const toggleAgent = (key: AgentKey) => {
    setForm((f) => ({
      ...f,
      agents: f.agents.includes(key)
        ? f.agents.filter((a) => a !== key)
        : [...f.agents, key],
    }));
  };

  const isHttp = form.transport === "http";

  return (
    <div className="p-5 max-w-2xl space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <button
            onClick={onBack}
            className="text-gray-400 hover:text-gray-600 text-lg leading-none"
            title="Back"
          >
            ←
          </button>
          <h2 className="font-semibold text-gray-800">MCP Server Management</h2>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={importExisting}
            disabled={importing}
            className="flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg border border-gray-200 text-gray-600 hover:bg-gray-50 disabled:opacity-40"
          >
            <span>↓</span>
            {importing ? "Importing…" : "Import Existing"}
          </button>
          <button
            onClick={openAdd}
            className="flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg bg-gray-800 text-white hover:bg-gray-700"
          >
            <span>+</span> Add MCP
          </button>
        </div>
      </div>

      {/* Summary bar */}
      <div className="bg-white rounded-lg border border-gray-200 px-4 py-2.5 flex items-center justify-between">
        <span className="text-sm text-gray-600 font-medium">
          {servers.length} MCP server{servers.length !== 1 ? "s" : ""} configured
        </span>
        <div className="flex items-center gap-3">
          {AGENTS.map((a) => (
            <span key={a.key} className="text-xs font-medium text-gray-500">
              {a.label}:{" "}
              <span className={agentCount(a.key) > 0 ? "text-blue-600" : "text-gray-400"}>
                {agentCount(a.key)}
              </span>
            </span>
          ))}
        </div>
      </div>

      {error && (
        <p className="text-xs text-red-500 bg-red-50 rounded px-3 py-2">{error}</p>
      )}

      {/* Add / Edit form */}
      {showForm && (
        <div className="bg-white rounded-lg border border-gray-200 p-4 space-y-3">
          <h3 className="text-sm font-medium text-gray-800">
            {editingId ? "Edit MCP Server" : "Add MCP Server"}
          </h3>

          <div className="space-y-2">
            <label className="block text-xs text-gray-500">Name</label>
            <input
              type="text"
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="e.g. github"
              className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 focus:outline-none focus:ring-1 focus:ring-blue-300"
            />
          </div>

          {/* Transport selector */}
          <div className="space-y-2">
            <label className="block text-xs text-gray-500">Transport</label>
            <div className="flex gap-2">
              {(["http", "stdio"] as McpTransport[]).map((t) => (
                <button
                  key={t}
                  onClick={() => setForm({ ...form, transport: t })}
                  className={`px-3 py-1 rounded-lg text-xs font-medium border transition-colors ${
                    form.transport === t
                      ? "bg-gray-800 text-white border-gray-800"
                      : "bg-white text-gray-600 border-gray-200 hover:bg-gray-50"
                  }`}
                >
                  {t === "stdio" ? "Stdio" : "HTTP"}
                </button>
              ))}
            </div>
          </div>

          {isHttp ? (
            <>
              <div className="space-y-2">
                <label className="block text-xs text-gray-500">URL</label>
                <input
                  type="text"
                  value={form.url}
                  onChange={(e) => setForm({ ...form, url: e.target.value })}
                  placeholder="https://api.example.com/mcp/"
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300"
                />
              </div>

              <div className="space-y-2">
                <label className="block text-xs text-gray-500">Headers (Key: Value, one per line)</label>
                <textarea
                  value={form.headers}
                  onChange={(e) => setForm({ ...form, headers: e.target.value })}
                  placeholder={"Authorization: Bearer ghp_xxxx"}
                  rows={3}
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300 resize-none"
                />
              </div>
            </>
          ) : (
            <>
              <div className="space-y-2">
                <label className="block text-xs text-gray-500">Command</label>
                <input
                  type="text"
                  value={form.command}
                  onChange={(e) => setForm({ ...form, command: e.target.value })}
                  placeholder="e.g. npx"
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300"
                />
              </div>

              <div className="space-y-2">
                <label className="block text-xs text-gray-500">Args (one per line)</label>
                <textarea
                  value={form.args}
                  onChange={(e) => setForm({ ...form, args: e.target.value })}
                  placeholder={"-y\n@modelcontextprotocol/server-filesystem\n/tmp"}
                  rows={3}
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300 resize-none"
                />
              </div>

              <div className="space-y-2">
                <label className="block text-xs text-gray-500">Env vars (KEY=VALUE, one per line)</label>
                <textarea
                  value={form.env}
                  onChange={(e) => setForm({ ...form, env: e.target.value })}
                  placeholder="API_KEY=abc123"
                  rows={2}
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300 resize-none"
                />
              </div>
            </>
          )}

          <div className="space-y-2">
            <label className="block text-xs text-gray-500">Include in agents</label>
            <div className="flex gap-3">
              {AGENTS.map((a) => (
                <label key={a.key} className="flex items-center gap-1.5 text-sm text-gray-700 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={form.agents.includes(a.key)}
                    onChange={() => toggleAgent(a.key)}
                    className="rounded"
                  />
                  {a.label}
                </label>
              ))}
            </div>
          </div>

          {error && <p className="text-xs text-red-500">{error}</p>}

          <div className="flex gap-2 pt-1">
            <button
              onClick={submit}
              disabled={busy}
              className="px-3 py-1.5 bg-blue-500 text-white rounded-lg text-xs font-medium hover:bg-blue-600 disabled:opacity-40"
            >
              {busy ? "Saving…" : editingId ? "Save" : "Add"}
            </button>
            <button
              onClick={() => { setShowForm(false); setError(""); }}
              className="px-3 py-1.5 text-gray-600 rounded-lg text-xs hover:bg-gray-100"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Server list */}
      {servers.length === 0 && !showForm ? (
        <div className="flex flex-col items-center justify-center py-16 text-center space-y-2">
          <div className="w-14 h-14 rounded-full bg-gray-100 flex items-center justify-center text-gray-400 text-2xl">
            ⬛
          </div>
          <p className="font-medium text-gray-700">No servers yet</p>
          <p className="text-sm text-gray-400">Click the button in the top right to add your first MCP server</p>
        </div>
      ) : (
        <div className="space-y-2">
          {servers.map((s) => (
            <div key={s.id} className="bg-white rounded-lg border border-gray-200 p-4">
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <span className="font-medium text-sm text-gray-800">{s.name}</span>
                    <span className="text-[11px] px-1.5 py-0.5 rounded-full bg-gray-100 text-gray-500 border border-gray-200 font-medium uppercase">
                      {s.transport ?? "stdio"}
                    </span>
                    {s.agents.map((a) => {
                      const agent = AGENTS.find((ag) => ag.key === a);
                      return agent ? (
                        <span key={a} className="text-[11px] px-1.5 py-0.5 rounded-full bg-blue-50 text-blue-600 border border-blue-100 font-medium">
                          {agent.label}
                        </span>
                      ) : null;
                    })}
                  </div>
                  <code className="text-xs text-gray-500 font-mono mt-0.5 block truncate">
                    {s.transport === "http" ? s.url : `${s.command} ${s.args.join(" ")}`}
                  </code>
                </div>
                <div className="flex gap-2 flex-shrink-0">
                  <button
                    onClick={() => openEdit(s)}
                    className="text-xs text-blue-500 hover:text-blue-700"
                  >
                    Edit
                  </button>
                  <button
                    onClick={() => remove(s.id)}
                    disabled={busy}
                    className="text-xs text-red-400 hover:text-red-600 disabled:opacity-40"
                  >
                    Remove
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      <p className="text-xs text-gray-400 pt-1">
        MCP servers are written to each agent's config when you click Apply in the Agents tab.
      </p>
    </div>
  );
}
