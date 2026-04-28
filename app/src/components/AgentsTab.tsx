import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

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

const AGENTS = [
  {
    key: "claude_code" as const,
    name: "Claude Code",
    description: "Anthropic's official coding agent CLI",
    baseUrl: "http://localhost:8080/anthropic",
    defaultModel: "anthropic/claude-sonnet-4-6",
    modelHint: "provider/model (e.g. anthropic/claude-sonnet-4-6, openai/gpt-4o)",
    logo: "◆",
    logoColor: "text-orange-500",
  },
  {
    key: "codex" as const,
    name: "OpenAI Codex CLI",
    description: "OpenAI's terminal coding agent",
    baseUrl: "http://localhost:8080/v1",
    defaultModel: "openai/gpt-4o",
    modelHint: "provider/model (e.g. openai/gpt-4o, anthropic/claude-sonnet-4-6)",
    logo: "◎",
    logoColor: "text-green-600",
  },
  {
    key: "gemini" as const,
    name: "Gemini CLI",
    description: "Google's Gemini coding agent CLI",
    baseUrl: "http://localhost:8080/gemini",
    defaultModel: "gemini/gemini-2.0-flash",
    modelHint: "provider/model (e.g. gemini/gemini-2.0-flash, anthropic/claude-sonnet-4-6)",
    logo: "✦",
    logoColor: "text-blue-500",
  },
] as const;

type AgentKey = (typeof AGENTS)[number]["key"];

export default function AgentsTab() {
  const [configs, setConfigs] = useState<AllAgentConfigs | null>(null);
  const [models, setModels] = useState<Record<AgentKey, string>>({
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
      setModels((prev) => {
        const next = { ...prev };
        for (const agent of AGENTS) {
          if (!prev[agent.key]) {
            next[agent.key] = result[agent.key].model || agent.defaultModel;
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
    setBusy(key);
    setErrors((e) => ({ ...e, [key]: "" }));
    try {
      await invoke("apply_agent_config", { agent: key, model: models[key] });
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
                <span className={`text-xl leading-none ${agent.logoColor}`}>
                  {agent.logo}
                </span>
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

            {/* Model input + actions */}
            <div className="flex items-center gap-2">
              <div className="flex-1">
                <input
                  type="text"
                  className="w-full text-sm border border-gray-200 rounded-lg px-3 py-1.5 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300"
                  placeholder={agent.defaultModel}
                  value={models[agent.key]}
                  onChange={(e) =>
                    setModels((prev) => ({ ...prev, [agent.key]: e.target.value }))
                  }
                />
                <p className="text-[11px] text-gray-400 mt-0.5 px-0.5">
                  {agent.modelHint}
                </p>
              </div>
              <div className="flex gap-2 self-start mt-0.5">
                <button
                  onClick={() => apply(agent.key)}
                  disabled={isBusy || !models[agent.key].trim()}
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
