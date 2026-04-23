import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api, type Config, type ProviderPatch } from "../api";

const ALL_PROVIDERS = [
  { name: "openai", label: "OpenAI", fields: ["api_key"] },
  { name: "anthropic", label: "Anthropic", fields: ["api_key"] },
  { name: "gemini", label: "Gemini", fields: ["api_key"] },
  { name: "mistral", label: "Mistral", fields: ["api_key"] },
  { name: "togetherai", label: "TogetherAI", fields: ["api_key"] },
  {
    name: "azure",
    label: "Azure OpenAI",
    fields: ["api_key", "endpoint", "api_version"],
  },
  { name: "bedrock", label: "AWS Bedrock", fields: ["region"] },
];

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

  useEffect(() => {
    if (proxyOnline) {
      api.config().then(setCfg).catch(() => setCfg(null));
    }
  }, [proxyOnline]);

  const startEdit = (name: string) => {
    const existing = cfg?.providers[name] ?? {};
    // Treat the redacted sentinel "***" as empty — saving it would overwrite
    // the real key in the config file with the literal string "***".
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
      // Restart proxy so it reloads credentials from the updated config file
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

      {ALL_PROVIDERS.map(({ name, label, fields }) => {
        const configured = configuredProviders.includes(name);
        const isEditing = editing === name;

        return (
          <div
            key={name}
            className="bg-white rounded-lg border border-gray-200 p-4"
          >
            <div className="flex items-center justify-between mb-2">
              <div className="flex items-center gap-2">
                <span className="font-medium text-gray-800">{label}</span>
                <span
                  className={`text-xs px-1.5 py-0.5 rounded font-medium ${
                    configured
                      ? "bg-green-100 text-green-700"
                      : "bg-gray-100 text-gray-500"
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
                    label="API Key"
                    value={draft.api_key}
                    onChange={(v) => setDraft({ ...draft, api_key: v })}
                    secret
                    placeholder={
                      cfg?.providers[name]?.api_key === "***"
                        ? "● ● ● ● ● ●  (leave blank to keep current)"
                        : "sk-..."
                    }
                  />
                )}
                {fields.includes("endpoint") && (
                  <Field
                    label="Endpoint"
                    value={draft.endpoint}
                    onChange={(v) => setDraft({ ...draft, endpoint: v })}
                    placeholder="https://my-resource.openai.azure.com"
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
