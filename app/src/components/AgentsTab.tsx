import { useCallback, useEffect, useState } from "react";
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

  useEffect(() => { refresh(); }, [refresh]);

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
            <div className="flex items-start gap-2">
              <select
                className="text-sm border border-gray-200 rounded-lg px-2 py-1.5 bg-white focus:outline-none focus:ring-1 focus:ring-blue-300 flex-shrink-0"
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
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300"
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
                  className="px-3 py-1.5 rounded-lg text-xs font-medium bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-40 disabled:cursor-not-allowed whitespace-nowrap"
                >
                  {isBusy ? "Saving…" : "Apply"}
                </button>
                {isActive && (
                  <button
                    onClick={() => reset(agent.key)}
                    disabled={isBusy}
                    className="px-3 py-1.5 rounded-lg text-xs font-medium text-red-500 bg-red-50 hover:bg-red-100 disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    Reset
                  </button>
                )}
              </div>
            </div>

            {errors[agent.key] && (
              <p className="text-xs text-red-500">{errors[agent.key]}</p>
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
