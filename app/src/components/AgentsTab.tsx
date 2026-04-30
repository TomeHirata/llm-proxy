import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import claudeCodeLogo from "../assets/claude-code-logo.png";
import codexLogo from "../assets/codex-logo.png";

interface AgentStatus {
  config_path: string;
  config_exists: boolean;
  active: boolean;
  model: string;
}

interface AllAgentConfigs {
  claude_code: AgentStatus;
  codex: AgentStatus;
  gemini: AgentStatus;
}

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

interface DeviceFlowInfo {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
}

type OAuthAgent = "claude_code" | "codex";

interface OAuthFlowState {
  agent: OAuthAgent;
  deviceCode: string;
  userCode: string;
  userCodeForCodex?: string;
  verificationUri: string;
  interval: number;
  polling: boolean;
  error: string;
}

interface Props {
  configuredProviders: string[];
}

const PROVIDER_LABELS: Record<string, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
  gemini: "Gemini",
  mistral: "Mistral",
  togetherai: "TogetherAI",
  bedrock: "AWS Bedrock",
  azure: "Azure OpenAI",
  copilot: "GitHub Copilot",
  codex_oauth: "Codex (OAuth)",
};

const AGENTS = [
  {
    key: "claude_code" as const,
    name: "Claude Code",
    description: "Anthropic's official coding agent CLI",
    baseUrl: "http://localhost:8080/anthropic",
    defaultProvider: "anthropic",
    defaultModel: "claude-sonnet-4-6",
    logoImg: claudeCodeLogo,
    logo: null,
    logoColor: "",
    oauthAgent: "claude_code" as OAuthAgent,
    oauthLabel: "GitHub Copilot",
  },
  {
    key: "codex" as const,
    name: "OpenAI Codex CLI",
    description: "OpenAI's terminal coding agent",
    baseUrl: "http://localhost:8080/openai/v1",
    defaultProvider: "openai",
    defaultModel: "gpt-4o",
    logoImg: codexLogo,
    logo: null,
    logoColor: "",
    oauthAgent: "codex" as OAuthAgent,
    oauthLabel: "OpenAI",
  },
  {
    key: "gemini" as const,
    name: "Gemini CLI",
    description: "Google's Gemini coding agent CLI",
    baseUrl: "http://localhost:8080/gemini",
    defaultProvider: "gemini",
    defaultModel: "gemini-2.5-flash",
    logoImg: null,
    logo: "✦",
    logoColor: "text-blue-500",
    oauthAgent: null,
    oauthLabel: null,
  },
] as const;

type AgentKey = (typeof AGENTS)[number]["key"];

function splitModel(full: string, defaultProvider: string): { provider: string; modelId: string } {
  const idx = full.indexOf("/");
  if (idx === -1) return { provider: defaultProvider, modelId: full };
  return { provider: full.slice(0, idx), modelId: full.slice(idx + 1) };
}

export default function AgentsTab({ configuredProviders }: Props) {
  const [configs, setConfigs] = useState<AllAgentConfigs | null>(null);
  const [providers, setProviders] = useState<Record<AgentKey, string>>({
    claude_code: "",
    codex: "",
    gemini: "",
  });
  const [modelIds, setModelIds] = useState<Record<AgentKey, string>>({
    claude_code: "",
    codex: "",
    gemini: "",
  });
  const [busy, setBusy] = useState<AgentKey | null>(null);
  const [errors, setErrors] = useState<Record<AgentKey, string>>({
    claude_code: "",
    codex: "",
    gemini: "",
  });

  const [copilotAccount, setCopilotAccount] = useState<CopilotAccount | null>(null);
  const [codexAccount, setCodexAccount] = useState<CodexAccount | null>(null);
  const [oauthFlow, setOAuthFlow] = useState<OAuthFlowState | null>(null);
  const [oauthBusy, setOAuthBusy] = useState<OAuthAgent | null>(null);
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshOAuthStatus = useCallback(async () => {
    try {
      const c = await invoke<CopilotAccount | null>("copilot_oauth_status");
      setCopilotAccount(c);
    } catch {
      // non-fatal
    }
    try {
      const d = await invoke<CodexAccount | null>("codex_oauth_status");
      setCodexAccount(d);
    } catch {
      // non-fatal
    }
  }, []);

  const refresh = useCallback(async () => {
    try {
      const result = await invoke<AllAgentConfigs>("read_agent_configs");
      setConfigs(result);
      setProviders((prev) => {
        const next = { ...prev };
        for (const agent of AGENTS) {
          if (!prev[agent.key]) {
            const full = result[agent.key].model || `${agent.defaultProvider}/${agent.defaultModel}`;
            next[agent.key] = splitModel(full, agent.defaultProvider).provider;
          }
        }
        return next;
      });
      setModelIds((prev) => {
        const next = { ...prev };
        for (const agent of AGENTS) {
          if (!prev[agent.key]) {
            const full = result[agent.key].model || `${agent.defaultProvider}/${agent.defaultModel}`;
            next[agent.key] = splitModel(full, agent.defaultProvider).modelId;
          }
        }
        return next;
      });
    } catch {
      // non-fatal
    }
  }, []);

  useEffect(() => {
    refresh();
    refreshOAuthStatus();
  }, [refresh, refreshOAuthStatus]);

  // Poll the device flow while active
  useEffect(() => {
    if (!oauthFlow?.polling) return;

    const poll = async () => {
      try {
        if (oauthFlow.agent === "claude_code") {
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
            userCode: oauthFlow.userCodeForCodex ?? oauthFlow.userCode,
          });
          if (account) {
            setCodexAccount(account);
            setOAuthFlow(null);
            setOAuthBusy(null);
            return;
          }
        }
        // Still pending — schedule next poll
        pollTimer.current = setTimeout(poll, oauthFlow.interval * 1000);
      } catch (e) {
        setOAuthFlow((prev) =>
          prev ? { ...prev, polling: false, error: String(e) } : null
        );
        setOAuthBusy(null);
      }
    };

    pollTimer.current = setTimeout(poll, oauthFlow.interval * 1000);
    return () => {
      if (pollTimer.current) clearTimeout(pollTimer.current);
    };
  }, [oauthFlow?.polling, oauthFlow?.deviceCode, oauthFlow?.agent, oauthFlow?.interval]);

  const startOAuth = async (agent: OAuthAgent) => {
    if (pollTimer.current) clearTimeout(pollTimer.current);
    setOAuthBusy(agent);
    setOAuthFlow(null);
    try {
      if (agent === "claude_code") {
        const info = await invoke<DeviceFlowInfo>("copilot_start_device_flow");
        setOAuthFlow({
          agent,
          deviceCode: info.device_code,
          userCode: info.user_code,
          verificationUri: info.verification_uri,
          interval: info.interval,
          polling: true,
          error: "",
        });
      } else {
        const info = await invoke<DeviceFlowInfo>("codex_start_device_flow");
        setOAuthFlow({
          agent,
          deviceCode: info.device_code,
          userCode: info.user_code,
          userCodeForCodex: info.user_code,
          verificationUri: info.verification_uri,
          interval: info.interval,
          polling: true,
          error: "",
        });
      }
    } catch (e) {
      setOAuthFlow({ agent, deviceCode: "", userCode: "", verificationUri: "", interval: 5, polling: false, error: String(e) });
      setOAuthBusy(null);
    }
  };

  const cancelOAuth = () => {
    if (pollTimer.current) clearTimeout(pollTimer.current);
    setOAuthFlow(null);
    setOAuthBusy(null);
  };

  const logout = async (agent: OAuthAgent) => {
    setOAuthBusy(agent);
    try {
      if (agent === "claude_code") {
        await invoke("copilot_oauth_logout");
        setCopilotAccount(null);
      } else {
        await invoke("codex_oauth_logout");
        setCodexAccount(null);
      }
    } catch {
      // non-fatal
    } finally {
      setOAuthBusy(null);
    }
  };

  const apply = async (key: AgentKey) => {
    const model = `${providers[key]}/${modelIds[key]}`.trim();
    setBusy(key);
    setErrors((e) => ({ ...e, [key]: "" }));
    try {
      await invoke("apply_agent_config", { agent: key, model });
      await refresh();
    } catch (e) {
      setErrors((prev) => ({ ...prev, [key]: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const reset = async (key: AgentKey) => {
    setBusy(key);
    setErrors((e) => ({ ...e, [key]: "" }));
    try {
      await invoke("reset_agent_config", { agent: key });
      await refresh();
    } catch (e) {
      setErrors((prev) => ({ ...prev, [key]: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const providerOptions = configuredProviders.length > 0 ? configuredProviders : Object.keys(PROVIDER_LABELS);

  return (
    <div className="p-5 max-w-2xl space-y-4">
      <div className="space-y-1">
        <h2 className="font-semibold text-gray-800">Coding Agents</h2>
        <p className="text-sm text-gray-500">
          Route your coding agents through llmproxy to use any configured provider.
        </p>
      </div>

      {AGENTS.map((agent) => {
        const status = configs?.[agent.key];
        const isActive = status?.active ?? false;
        const isBusy = busy === agent.key;
        const oauthAccount =
          agent.oauthAgent === "claude_code"
            ? copilotAccount
            : agent.oauthAgent === "codex"
            ? codexAccount
            : null;
        const isOAuthBusy = agent.oauthAgent !== null && oauthBusy === agent.oauthAgent;
        const activeFlow =
          agent.oauthAgent !== null && oauthFlow?.agent === agent.oauthAgent ? oauthFlow : null;

        return (
          <section
            key={agent.key}
            className="bg-white rounded-lg border border-gray-200 p-4 space-y-3"
          >
            {/* Header */}
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                {agent.logoImg ? (
                  <img src={agent.logoImg} alt={agent.name} className="w-5 h-5 object-contain" />
                ) : (
                  <span className={`text-xl leading-none ${agent.logoColor}`}>
                    {agent.logo}
                  </span>
                )}
                <div>
                  <div className="flex items-center gap-2">
                    <span className="font-medium text-gray-800 text-sm">
                      {agent.name}
                    </span>
                    {isActive ? (
                      <span className="inline-flex items-center gap-1 text-[11px] font-medium text-green-700 bg-green-50 border border-green-200 rounded-full px-2 py-0.5">
                        <span className="w-1.5 h-1.5 rounded-full bg-green-500 inline-block" />
                        Active
                      </span>
                    ) : (
                      <span className="text-[11px] font-medium text-gray-400 bg-gray-100 border border-gray-200 rounded-full px-2 py-0.5">
                        Not configured
                      </span>
                    )}
                  </div>
                  <p className="text-xs text-gray-400 mt-0.5">{agent.description}</p>
                </div>
              </div>
            </div>

            {/* Config details (shown when active) */}
            {isActive && status && (
              <div className="text-xs text-gray-500 bg-gray-50 rounded px-3 py-2 space-y-1">
                <div>
                  <span className="text-gray-400">Base URL: </span>
                  <code className="font-mono">{agent.baseUrl}</code>
                </div>
                <div>
                  <span className="text-gray-400">Config: </span>
                  <code className="font-mono">{status.config_path}</code>
                </div>
              </div>
            )}

            {/* Provider + model inputs */}
            <div className="flex items-center gap-2">
              <select
                className="text-sm border border-gray-200 rounded-lg px-2 py-[7px] bg-white focus:outline-none focus:ring-1 focus:ring-blue-300 flex-shrink-0 h-[34px]"
                value={providers[agent.key]}
                onChange={(e) => setProviders((prev) => ({ ...prev, [agent.key]: e.target.value }))}
              >
                {providerOptions.map((p) => (
                  <option key={p} value={p}>{PROVIDER_LABELS[p] ?? p}</option>
                ))}
              </select>
              <div className="flex-1">
                <input
                  type="text"
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-[7px] font-mono focus:outline-none focus:ring-1 focus:ring-blue-300 h-[34px]"
                  placeholder={agent.defaultModel}
                  value={modelIds[agent.key]}
                  onChange={(e) =>
                    setModelIds((prev) => ({ ...prev, [agent.key]: e.target.value }))
                  }
                />
              </div>
              <div className="flex gap-2 flex-shrink-0">
                <button
                  onClick={() => apply(agent.key)}
                  disabled={isBusy || !modelIds[agent.key].trim()}
                  className="px-3 py-[7px] rounded-lg text-xs font-medium bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-40 disabled:cursor-not-allowed whitespace-nowrap h-[34px]"
                >
                  {isBusy ? "Saving…" : "Apply"}
                </button>
                {isActive && (
                  <button
                    onClick={() => reset(agent.key)}
                    disabled={isBusy}
                    className="px-3 py-[7px] rounded-lg text-xs font-medium text-red-500 bg-red-50 hover:bg-red-100 disabled:opacity-40 disabled:cursor-not-allowed h-[34px]"
                  >
                    Reset
                  </button>
                )}
              </div>
            </div>

            {errors[agent.key] && (
              <p className="text-xs text-red-500">{errors[agent.key]}</p>
            )}

            {/* OAuth section (Claude Code and Codex only) */}
            {agent.oauthAgent && (
              <div className="border-t border-gray-100 pt-3">
                <div className="flex items-center justify-between">
                  <div>
                    <p className="text-xs font-medium text-gray-600">
                      {agent.oauthLabel} OAuth
                    </p>
                    {oauthAccount ? (
                      <p className="text-xs text-gray-400 mt-0.5">
                        Signed in as{" "}
                        <span className="font-medium text-gray-600">
                          {agent.oauthAgent === "claude_code"
                            ? (oauthAccount as CopilotAccount).login
                            : (oauthAccount as CodexAccount).email ?? (oauthAccount as CodexAccount).account_id}
                        </span>
                      </p>
                    ) : (
                      <p className="text-xs text-gray-400 mt-0.5">
                        Sign in to use {agent.oauthLabel} via llmproxy
                      </p>
                    )}
                  </div>
                  {oauthAccount ? (
                    <button
                      onClick={() => logout(agent.oauthAgent!)}
                      disabled={isOAuthBusy}
                      className="text-xs px-3 py-1.5 rounded-lg text-red-500 bg-red-50 hover:bg-red-100 disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      {isOAuthBusy ? "Signing out…" : "Sign out"}
                    </button>
                  ) : activeFlow ? (
                    <button
                      onClick={cancelOAuth}
                      className="text-xs px-3 py-1.5 rounded-lg text-gray-500 bg-gray-100 hover:bg-gray-200"
                    >
                      Cancel
                    </button>
                  ) : (
                    <button
                      onClick={() => startOAuth(agent.oauthAgent!)}
                      disabled={isOAuthBusy}
                      className="text-xs px-3 py-1.5 rounded-lg bg-gray-800 text-white hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      {isOAuthBusy ? "Starting…" : "Sign in"}
                    </button>
                  )}
                </div>

                {/* Device flow pending */}
                {activeFlow && activeFlow.polling && (
                  <div className="mt-2 bg-blue-50 border border-blue-200 rounded-lg px-3 py-2.5 space-y-1.5">
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
                  <p className="text-xs text-red-500 mt-1">{activeFlow.error}</p>
                )}
              </div>
            )}
          </section>
        );
      })}

      <p className="text-xs text-gray-400 pt-1">
        Restart the coding agent after applying changes for them to take effect.
      </p>
    </div>
  );
}
