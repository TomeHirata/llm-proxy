import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api, type Config, type ProviderPatch } from "../api";

type FieldOverrides = {
  api_key?: { label?: string; placeholder?: string };
  endpoint?: { label?: string; placeholder?: string };
};

type ProviderKind = "apikey" | "oauth_copilot" | "oauth_codex" | "oauth_databricks" | "oauth_anthropic";

const ALL_PROVIDERS: {
  name: string;
  label: string;
  kind: ProviderKind;
  fields: string[];
  fieldOverrides?: FieldOverrides;
}[] = [
  { name: "openai", label: "OpenAI", kind: "apikey", fields: ["api_key"] },
  { name: "anthropic", label: "Anthropic", kind: "oauth_anthropic", fields: ["api_key"] },
  { name: "gemini", label: "Gemini", kind: "apikey", fields: ["api_key"] },
  {
    name: "databricks",
    label: "Databricks",
    kind: "oauth_databricks",
    fields: ["endpoint", "api_key"],
    fieldOverrides: {
      endpoint: { label: "Workspace URL", placeholder: "https://my-workspace.azuredatabricks.net" },
      api_key: { label: "Personal Access Token" },
    },
  },
  { name: "mistral", label: "Mistral", kind: "apikey", fields: ["api_key"] },
  { name: "togetherai", label: "TogetherAI", kind: "apikey", fields: ["api_key"] },
  { name: "azure", label: "Azure OpenAI", kind: "apikey", fields: ["api_key", "endpoint", "api_version"] },
  { name: "bedrock", label: "AWS Bedrock", kind: "apikey", fields: ["region"] },
  { name: "copilot", label: "GitHub Copilot", kind: "oauth_copilot", fields: [] },
  { name: "codex_oauth", label: "OpenAI Codex (OAuth)", kind: "oauth_codex", fields: [] },
];

interface CopilotAccount {
  login: string;
  avatar_url: string | null;
  authenticated_at: number;
}

interface CodexAccount {
  account_id: string;
  email: string | null;
  authenticated_at: number;
}

interface DatabricksAccount {
  workspace_url: string;
  display_name: string | null;
  authenticated_at: number;
}

interface AnthropicAccount {
  email: string | null;
  authenticated_at: number;
}

interface OAuthFlowState {
  providerName: string;
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  interval: number;
  polling: boolean;
  error: string;
}

interface Props {
  proxyOnline: boolean;
  configuredProviders: string[];
}

export default function ProvidersTab({ proxyOnline, configuredProviders }: Props) {
  const [cfg, setCfg] = useState<Config | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // OAuth state
  const [copilotAccount, setCopilotAccount] = useState<CopilotAccount | null>(null);
  const [codexAccount, setCodexAccount] = useState<CodexAccount | null>(null);
  const [databricksAccount, setDatabricksAccount] = useState<DatabricksAccount | null>(null);
  const [anthropicAccount, setAnthropicAccount] = useState<AnthropicAccount | null>(null);
  const [oauthFlow, setOAuthFlow] = useState<OAuthFlowState | null>(null);
  const [oauthBusy, setOAuthBusy] = useState<string | null>(null);
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshOAuthStatus = useCallback(async () => {
    try {
      const c = await invoke<CopilotAccount | null>("copilot_oauth_status");
      setCopilotAccount(c);
    } catch { /* non-fatal */ }
    try {
      const d = await invoke<CodexAccount | null>("codex_oauth_status");
      setCodexAccount(d);
    } catch { /* non-fatal */ }
    try {
      const db = await invoke<DatabricksAccount | null>("databricks_oauth_status");
      setDatabricksAccount(db);
    } catch { /* non-fatal */ }
    try {
      const anth = await invoke<AnthropicAccount | null>("anthropic_oauth_status");
      setAnthropicAccount(anth);
    } catch { /* non-fatal */ }
  }, []);

  useEffect(() => {
    if (proxyOnline) {
      api.config().then(setCfg).catch(() => setCfg(null));
    }
    refreshOAuthStatus();
  }, [proxyOnline, refreshOAuthStatus]);

  // Poll device flow while active
  useEffect(() => {
    if (!oauthFlow?.polling) return;

    const poll = async () => {
      try {
        if (oauthFlow.providerName === "copilot") {
          const account = await invoke<CopilotAccount | null>("copilot_poll_device_flow", {
            deviceCode: oauthFlow.deviceCode,
          });
          if (account) {
            setCopilotAccount(account);
            setOAuthFlow(null);
            setOAuthBusy(null);
            return;
          }
        } else {
          const account = await invoke<CodexAccount | null>("codex_poll_device_flow", {
            deviceCode: oauthFlow.deviceCode,
            userCode: oauthFlow.userCode,
          });
          if (account) {
            setCodexAccount(account);
            setOAuthFlow(null);
            setOAuthBusy(null);
            return;
          }
        }
        pollTimer.current = setTimeout(poll, oauthFlow.interval * 1000);
      } catch (e) {
        setOAuthFlow((prev) => prev ? { ...prev, polling: false, error: String(e) } : null);
        setOAuthBusy(null);
      }
    };

    pollTimer.current = setTimeout(poll, oauthFlow.interval * 1000);
    return () => { if (pollTimer.current) clearTimeout(pollTimer.current); };
  }, [oauthFlow?.polling, oauthFlow?.deviceCode, oauthFlow?.providerName, oauthFlow?.interval]);

  const startOAuth = async (providerName: string) => {
    if (pollTimer.current) clearTimeout(pollTimer.current);
    setOAuthBusy(providerName);
    setOAuthFlow(null);
    try {
      const info = await invoke<{ device_code: string; user_code: string; verification_uri: string; interval: number }>(
        providerName === "copilot" ? "copilot_start_device_flow" : "codex_start_device_flow"
      );
      setOAuthFlow({
        providerName,
        deviceCode: info.device_code,
        userCode: info.user_code,
        verificationUri: info.verification_uri,
        interval: info.interval,
        polling: true,
        error: "",
      });
    } catch (e) {
      setOAuthFlow({ providerName, deviceCode: "", userCode: "", verificationUri: "", interval: 5, polling: false, error: String(e) });
      setOAuthBusy(null);
    }
  };

  const cancelOAuth = () => {
    if (pollTimer.current) clearTimeout(pollTimer.current);
    setOAuthFlow(null);
    setOAuthBusy(null);
  };

  const oauthLogout = async (providerName: string) => {
    setOAuthBusy(providerName);
    try {
      if (providerName === "copilot") {
        await invoke("copilot_oauth_logout");
        setCopilotAccount(null);
      } else if (providerName === "codex_oauth") {
        await invoke("codex_oauth_logout");
        setCodexAccount(null);
      } else if (providerName === "databricks") {
        await invoke("databricks_oauth_logout");
        setDatabricksAccount(null);
      } else if (providerName === "anthropic") {
        await invoke("anthropic_oauth_logout");
        setAnthropicAccount(null);
      }
    } catch { /* non-fatal */ }
    finally { setOAuthBusy(null); }
  };

  const startEdit = (name: string) => {
    const existing = cfg?.providers[name] ?? {};
    const apiKey = existing.api_key === "***" ? "" : (existing.api_key ?? "");
    setDraft({
      api_key: apiKey,
      endpoint: existing.endpoint ?? "",
      api_version: existing.api_version ?? "",
      region: existing.region ?? "",
    });
    setEditing(name);
    setError(null);
  };

  const save = async (name: string) => {
    setSaving(true);
    setError(null);
    const patch: ProviderPatch = {};
    if (draft.api_key) patch.api_key = draft.api_key;
    if (draft.endpoint) patch.endpoint = draft.endpoint;
    if (draft.api_version) patch.api_version = draft.api_version;
    if (draft.region) patch.region = draft.region;
    try {
      await api.updateProvider(name, patch);
      setEditing(null);
      setSaved(name);
      setRestarting(true);
      try {
        await invoke("stop_proxy");
        await new Promise((r) => setTimeout(r, 600));
        await invoke("start_proxy");
        await new Promise((r) => setTimeout(r, 800));
        const updated = await api.config();
        setCfg(updated);
      } finally {
        setRestarting(false);
      }
      setTimeout(() => setSaved(null), 3000);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  if (!proxyOnline) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-400">
        Proxy is not running
      </div>
    );
  }

  return (
    <div className="p-5 space-y-3 max-w-2xl">
      <p className="text-sm text-gray-500">
        API keys are saved to{" "}
        <code className="bg-gray-100 px-1 rounded text-xs">
          ~/.config/llmproxy/config.yaml
        </code>
        . Existing env-var overrides still take precedence at runtime.
      </p>

      {error && (
        <div className="p-3 bg-red-50 text-red-700 rounded text-sm">{error}</div>
      )}

      {ALL_PROVIDERS.map(({ name, label, kind, fields, fieldOverrides }) => {
        const configured = configuredProviders.includes(name);
        const isEditing = editing === name;

        // Pure device-code OAuth providers (Copilot, Codex)
        if (kind === "oauth_copilot" || kind === "oauth_codex") {
          const account = kind === "oauth_copilot" ? copilotAccount : codexAccount;
          const isBusy = oauthBusy === name;
          const activeFlow = oauthFlow?.providerName === name ? oauthFlow : null;
          const displayName =
            account
              ? kind === "oauth_copilot"
                ? (account as CopilotAccount).login
                : ((account as CodexAccount).email ?? (account as CodexAccount).account_id)
              : null;

          return (
            <div key={name} className="bg-white rounded-lg border border-gray-200 p-4">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span className="font-medium text-gray-800">{label}</span>
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded font-medium ${
                      configured ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"
                    }`}
                  >
                    {configured ? "Connected" : "Not connected"}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  {account ? (
                    <>
                      <span className="text-xs text-gray-500">{displayName}</span>
                      <button
                        onClick={() => oauthLogout(name)}
                        disabled={isBusy}
                        className="text-xs px-3 py-1.5 rounded-lg text-red-500 bg-red-50 hover:bg-red-100 disabled:opacity-40"
                      >
                        {isBusy ? "Signing out…" : "Sign out"}
                      </button>
                    </>
                  ) : activeFlow ? (
                    <button
                      onClick={cancelOAuth}
                      className="text-xs px-3 py-1.5 rounded-lg text-gray-500 bg-gray-100 hover:bg-gray-200"
                    >
                      Cancel
                    </button>
                  ) : (
                    <button
                      onClick={() => startOAuth(name)}
                      disabled={isBusy}
                      className="text-xs px-3 py-1.5 rounded-lg bg-gray-800 text-white hover:bg-gray-700 disabled:opacity-40"
                    >
                      {isBusy ? "Starting…" : "Sign in"}
                    </button>
                  )}
                </div>
              </div>

              {activeFlow?.polling && (
                <div className="mt-3 bg-blue-50 border border-blue-200 rounded-lg px-3 py-2.5 space-y-1.5">
                  <p className="text-xs text-blue-800 font-medium">
                    Open this URL and enter the code below:
                  </p>
                  <a
                    href={activeFlow.verificationUri}
                    target="_blank"
                    rel="noreferrer"
                    className="text-xs text-blue-600 underline break-all"
                  >
                    {activeFlow.verificationUri}
                  </a>
                  <div className="flex items-center gap-2">
                    <code className="text-sm font-mono font-bold tracking-widest text-blue-900 bg-blue-100 px-2 py-0.5 rounded">
                      {activeFlow.userCode}
                    </code>
                    <span className="text-xs text-blue-500 animate-pulse">Waiting…</span>
                  </div>
                </div>
              )}

              {activeFlow?.error && (
                <p className="text-xs text-red-500 mt-2">{activeFlow.error}</p>
              )}
            </div>
          );
        }

        // Databricks: API key form + OAuth browser section
        if (kind === "oauth_databricks") {
          const account = databricksAccount;
          const isBusy = oauthBusy === name;
          const workspaceUrl = cfg?.providers[name]?.endpoint ?? "";
          const displayName = account
            ? (account.display_name ?? account.workspace_url)
            : null;

          return (
            <div key={name} className="bg-white rounded-lg border border-gray-200 p-4 space-y-3">
              {/* Header row */}
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span className="font-medium text-gray-800">{label}</span>
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded font-medium ${
                      configured ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"
                    }`}
                  >
                    {configured ? "Configured" : "Not configured"}
                  </span>
                  {saved === name && (
                    <span className="text-xs text-green-600 font-medium">
                      {restarting ? "Restarting proxy…" : "✓ Saved & restarted"}
                    </span>
                  )}
                </div>
                {!isEditing && (
                  <button
                    onClick={() => startEdit(name)}
                    className="text-sm text-blue-600 hover:text-blue-700"
                  >
                    {configured ? "Edit" : "Configure"}
                  </button>
                )}
              </div>

              {/* API key edit form */}
              {isEditing && (
                <div className="space-y-2">
                  {fields.includes("endpoint") && (
                    <Field
                      label={fieldOverrides?.endpoint?.label ?? "Endpoint"}
                      value={draft.endpoint}
                      onChange={(v) => setDraft({ ...draft, endpoint: v })}
                      placeholder={fieldOverrides?.endpoint?.placeholder ?? "https://..."}
                    />
                  )}
                  {fields.includes("api_key") && (
                    <Field
                      label={fieldOverrides?.api_key?.label ?? "API Key"}
                      value={draft.api_key}
                      onChange={(v) => setDraft({ ...draft, api_key: v })}
                      secret
                      placeholder={
                        cfg?.providers[name]?.api_key === "***"
                          ? "● ● ● ● ● ●  (leave blank to keep current)"
                          : (fieldOverrides?.api_key?.placeholder ?? "dapi-...")
                      }
                    />
                  )}
                  <div className="flex gap-2 pt-1">
                    <button
                      onClick={() => save(name)}
                      disabled={saving}
                      className="px-3 py-1.5 bg-blue-500 text-white rounded text-sm font-medium hover:bg-blue-600 disabled:opacity-50"
                    >
                      {saving ? "Saving…" : "Save"}
                    </button>
                    <button
                      onClick={() => setEditing(null)}
                      className="px-3 py-1.5 text-gray-600 rounded text-sm hover:bg-gray-100"
                    >
                      Cancel
                    </button>
                  </div>
                </div>
              )}

              {/* OAuth section */}
              <div className="border-t border-gray-100 pt-3">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-gray-600 font-medium">OAuth (browser)</span>
                    {account && (
                      <span className="text-xs px-1.5 py-0.5 rounded font-medium bg-green-100 text-green-700">
                        Active
                      </span>
                    )}
                  </div>
                  <div className="flex items-center gap-2">
                    {account ? (
                      <>
                        {displayName && <span className="text-xs text-gray-500">{displayName}</span>}
                        <button
                          onClick={() => oauthLogout(name)}
                          disabled={isBusy}
                          className="text-xs px-3 py-1.5 rounded-lg text-red-500 bg-red-50 hover:bg-red-100 disabled:opacity-40"
                        >
                          {isBusy ? "Signing out…" : "Sign out"}
                        </button>
                      </>
                    ) : (
                      <button
                        onClick={async () => {
                          if (!workspaceUrl) {
                            setError("Configure a Workspace URL above first.");
                            return;
                          }
                          setOAuthBusy(name);
                          try {
                            const acct = await invoke<DatabricksAccount>("databricks_start_browser_flow", { workspaceUrl });
                            setDatabricksAccount(acct);
                          } catch (e) {
                            setError(String(e));
                          } finally {
                            setOAuthBusy(null);
                          }
                        }}
                        disabled={isBusy || !workspaceUrl}
                        title={!workspaceUrl ? "Configure workspace URL above first" : undefined}
                        className="text-xs px-3 py-1.5 rounded-lg bg-gray-800 text-white hover:bg-gray-700 disabled:opacity-40"
                      >
                        {isBusy ? (
                          <span className="flex items-center gap-1">
                            <span className="animate-spin inline-block w-3 h-3 border border-white border-t-transparent rounded-full" />
                            Waiting for browser…
                          </span>
                        ) : "Sign in with Databricks"}
                      </button>
                    )}
                  </div>
                </div>
                {!workspaceUrl && !account && (
                  <p className="text-xs text-gray-400 mt-1">Configure a workspace URL above to enable OAuth sign-in.</p>
                )}
              </div>
            </div>
          );
        }

        // Anthropic: API key form + OAuth browser PKCE section
        if (kind === "oauth_anthropic") {
          const account = anthropicAccount;
          const isBusy = oauthBusy === name;
          const displayName = account?.email ?? null;

          return (
            <div key={name} className="bg-white rounded-lg border border-gray-200 p-4 space-y-3">
              {/* Header row */}
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span className="font-medium text-gray-800">{label}</span>
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded font-medium ${
                      configured ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"
                    }`}
                  >
                    {configured ? "Configured" : "Not configured"}
                  </span>
                  {saved === name && (
                    <span className="text-xs text-green-600 font-medium">
                      {restarting ? "Restarting proxy…" : "✓ Saved & restarted"}
                    </span>
                  )}
                </div>
                {!isEditing && (
                  <button
                    onClick={() => startEdit(name)}
                    className="text-sm text-blue-600 hover:text-blue-700"
                  >
                    {configured ? "Edit" : "Configure"}
                  </button>
                )}
              </div>

              {/* API key edit form */}
              {isEditing && (
                <div className="space-y-2">
                  <Field
                    label="API Key"
                    value={draft.api_key}
                    onChange={(v) => setDraft({ ...draft, api_key: v })}
                    secret
                    placeholder={
                      cfg?.providers[name]?.api_key === "***"
                        ? "● ● ● ● ● ●  (leave blank to keep current)"
                        : "sk-ant-..."
                    }
                  />
                  <div className="flex gap-2 pt-1">
                    <button
                      onClick={() => save(name)}
                      disabled={saving}
                      className="px-3 py-1.5 bg-blue-500 text-white rounded text-sm font-medium hover:bg-blue-600 disabled:opacity-50"
                    >
                      {saving ? "Saving…" : "Save"}
                    </button>
                    <button
                      onClick={() => setEditing(null)}
                      className="px-3 py-1.5 text-gray-600 rounded text-sm hover:bg-gray-100"
                    >
                      Cancel
                    </button>
                  </div>
                </div>
              )}

              {/* OAuth section */}
              <div className="border-t border-gray-100 pt-3">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-gray-600 font-medium">OAuth (browser)</span>
                    {account && (
                      <span className="text-xs px-1.5 py-0.5 rounded font-medium bg-green-100 text-green-700">
                        Active — overrides API key
                      </span>
                    )}
                  </div>
                  <div className="flex items-center gap-2">
                    {account ? (
                      <>
                        {displayName && <span className="text-xs text-gray-500">{displayName}</span>}
                        <button
                          onClick={() => oauthLogout(name)}
                          disabled={isBusy}
                          className="text-xs px-3 py-1.5 rounded-lg text-red-500 bg-red-50 hover:bg-red-100 disabled:opacity-40"
                        >
                          {isBusy ? "Signing out…" : "Sign out"}
                        </button>
                      </>
                    ) : (
                      <button
                        onClick={async () => {
                          setOAuthBusy(name);
                          try {
                            const acct = await invoke<AnthropicAccount>("anthropic_start_browser_flow");
                            setAnthropicAccount(acct);
                          } catch (e) {
                            setError(String(e));
                          } finally {
                            setOAuthBusy(null);
                          }
                        }}
                        disabled={isBusy}
                        className="text-xs px-3 py-1.5 rounded-lg bg-gray-800 text-white hover:bg-gray-700 disabled:opacity-40"
                      >
                        {isBusy ? (
                          <span className="flex items-center gap-1">
                            <span className="animate-spin inline-block w-3 h-3 border border-white border-t-transparent rounded-full" />
                            Waiting for browser…
                          </span>
                        ) : "Sign in with Anthropic"}
                      </button>
                    )}
                  </div>
                </div>
              </div>
            </div>
          );
        }

        // API-key provider
        return (
          <div key={name} className="bg-white rounded-lg border border-gray-200 p-4">
            <div className="flex items-center justify-between mb-2">
              <div className="flex items-center gap-2">
                <span className="font-medium text-gray-800">{label}</span>
                <span
                  className={`text-xs px-1.5 py-0.5 rounded font-medium ${
                    configured ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"
                  }`}
                >
                  {configured ? "Configured" : "Not configured"}
                </span>
                {saved === name && (
                  <span className="text-xs text-green-600 font-medium">
                    {restarting ? "Restarting proxy…" : "✓ Saved & restarted"}
                  </span>
                )}
              </div>
              {!isEditing && (
                <button
                  onClick={() => startEdit(name)}
                  className="text-sm text-blue-600 hover:text-blue-700"
                >
                  {configured ? "Edit" : "Configure"}
                </button>
              )}
            </div>

            {isEditing && (
              <div className="space-y-2 mt-3">
                {fields.includes("api_key") && (
                  <Field
                    label={fieldOverrides?.api_key?.label ?? "API Key"}
                    value={draft.api_key}
                    onChange={(v) => setDraft({ ...draft, api_key: v })}
                    secret
                    placeholder={
                      cfg?.providers[name]?.api_key === "***"
                        ? "● ● ● ● ● ●  (leave blank to keep current)"
                        : (fieldOverrides?.api_key?.placeholder ?? "sk-...")
                    }
                  />
                )}
                {fields.includes("endpoint") && (
                  <Field
                    label={fieldOverrides?.endpoint?.label ?? "Endpoint"}
                    value={draft.endpoint}
                    onChange={(v) => setDraft({ ...draft, endpoint: v })}
                    placeholder={fieldOverrides?.endpoint?.placeholder ?? "https://my-resource.openai.azure.com"}
                  />
                )}
                {fields.includes("api_version") && (
                  <Field
                    label="API version"
                    value={draft.api_version}
                    onChange={(v) => setDraft({ ...draft, api_version: v })}
                    placeholder="2024-02-01"
                  />
                )}
                {fields.includes("region") && (
                  <Field
                    label="Region"
                    value={draft.region}
                    onChange={(v) => setDraft({ ...draft, region: v })}
                    placeholder="us-east-1"
                  />
                )}
                <div className="flex gap-2 pt-1">
                  <button
                    onClick={() => save(name)}
                    disabled={saving}
                    className="px-3 py-1.5 bg-blue-500 text-white rounded text-sm font-medium hover:bg-blue-600 disabled:opacity-50"
                  >
                    {saving ? "Saving…" : "Save"}
                  </button>
                  <button
                    onClick={() => setEditing(null)}
                    className="px-3 py-1.5 text-gray-600 rounded text-sm hover:bg-gray-100"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function Field({
  label,
  value,
  onChange,
  secret = false,
  placeholder = "",
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  secret?: boolean;
  placeholder?: string;
}) {
  const [show, setShow] = useState(false);
  return (
    <div>
      <label className="block text-xs text-gray-500 mb-0.5">{label}</label>
      <div className="relative">
        <input
          type={secret && !show ? "password" : "text"}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          className="w-full px-3 py-1.5 border border-gray-300 rounded text-sm focus:outline-none focus:ring-1 focus:ring-blue-400"
        />
        {secret && (
          <button
            type="button"
            onClick={() => setShow(!show)}
            className="absolute right-2 top-1/2 -translate-y-1/2 text-xs text-gray-400 hover:text-gray-600"
          >
            {show ? "Hide" : "Show"}
          </button>
        )}
      </div>
    </div>
  );
}
