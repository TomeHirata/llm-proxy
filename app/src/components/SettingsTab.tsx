import { type ProxyStatus } from "../api";

interface Props {
  status: ProxyStatus | null;
}

export default function SettingsTab({ status }: Props) {
  return (
    <div className="p-5 max-w-2xl space-y-5">
      {/* Server info */}
      <section className="bg-white rounded-lg border border-gray-200 p-4 space-y-2">
        <h3 className="font-medium text-gray-800">Server</h3>
        <div className="grid grid-cols-2 gap-2 text-sm">
          <InfoRow label="Address" value="http://127.0.0.1:8080" />
          <InfoRow label="Version" value={status?.version ?? "—"} />
          <InfoRow
            label="Uptime"
            value={
              status?.uptime_secs != null
                ? formatUptime(status.uptime_secs)
                : "—"
            }
          />
          <InfoRow
            label="Usage log"
            value={
              status?.usage_log_enabled == null
                ? "—"
                : status.usage_log_enabled
                ? "Enabled"
                : "Disabled"
            }
          />
        </div>
      </section>

      {/* Usage instructions */}
      <section className="bg-white rounded-lg border border-gray-200 p-4 space-y-3">
        <h3 className="font-medium text-gray-800">Quick start</h3>
        <p className="text-sm text-gray-600">
          Point any OpenAI-compatible client at{" "}
          <code className="bg-gray-100 px-1 rounded text-xs">
            http://127.0.0.1:8080/v1
          </code>{" "}
          and prefix the model name with the provider:
        </p>
        <CodeBlock>{`from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:8080/v1", api_key="unused")
client.chat.completions.create(
    model="anthropic/claude-sonnet-4-5",
    messages=[{"role": "user", "content": "hello"}],
)`}</CodeBlock>

        <p className="text-sm text-gray-600 mt-2">
          Route Claude Code through the proxy:
        </p>
        <CodeBlock>{`export ANTHROPIC_BASE_URL="http://localhost:8080/anthropic"
claude`}</CodeBlock>
      </section>

      {/* Config file location */}
      <section className="bg-white rounded-lg border border-gray-200 p-4 space-y-2">
        <h3 className="font-medium text-gray-800">Config file</h3>
        <p className="text-sm text-gray-600">
          The proxy reads credentials and settings from:
        </p>
        <code className="block bg-gray-50 border border-gray-200 rounded px-3 py-2 text-sm font-mono">
          ~/.config/llmproxy/config.yaml
        </code>
        <p className="text-sm text-gray-500">
          Environment variables (
          <code className="text-xs bg-gray-100 px-1 rounded">
            OPENAI_API_KEY
          </code>
          ,{" "}
          <code className="text-xs bg-gray-100 px-1 rounded">
            ANTHROPIC_API_KEY
          </code>
          , …) are also supported and take the highest priority.
        </p>
      </section>
    </div>
  );
}

function InfoRow({ label, value }: { label: string; value: string }) {
  return (
    <>
      <span className="text-gray-500">{label}</span>
      <span className="font-medium font-mono text-sm">{value}</span>
    </>
  );
}

function CodeBlock({ children }: { children: string }) {
  return (
    <pre className="bg-gray-900 text-gray-100 rounded-lg p-3 text-xs overflow-x-auto leading-relaxed">
      {children}
    </pre>
  );
}

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}
