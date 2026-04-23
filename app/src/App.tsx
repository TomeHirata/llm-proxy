import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api, type ProxyStatus } from "./api";
import UsageTab from "./components/UsageTab";
import ProvidersTab from "./components/ProvidersTab";
import SettingsTab from "./components/SettingsTab";
import ChatTab from "./components/ChatTab";

type Tab = "usage" | "providers" | "settings" | "chat";

export default function App() {
  const [tab, setTab] = useState<Tab>("usage");
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [proxyOnline, setProxyOnline] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const refreshStatus = async () => {
    try {
      const s = await api.status();
      setStatus(s);
      setProxyOnline(true);
    } catch {
      setProxyOnline(false);
      setStatus(null);
    }
  };

  useEffect(() => {
    refreshStatus();
    const id = setInterval(refreshStatus, 5000);
    return () => clearInterval(id);
  }, []);

  const handleStartStop = async () => {
    setActionError(null);
    try {
      if (proxyOnline) {
        await invoke("stop_proxy");
      } else {
        await invoke("start_proxy");
      }
    } catch (e) {
      setActionError(String(e));
    }
    setTimeout(refreshStatus, 800);
  };

  const tabs: { key: Tab; label: string }[] = [
    { key: "usage", label: "Usage" },
    { key: "providers", label: "Providers" },
    { key: "chat", label: "Chat" },
    { key: "settings", label: "Settings" },
  ];

  return (
    <div className="flex flex-col h-screen">
      {/* Header */}
      <header className="flex items-center justify-between px-5 py-3 bg-white border-b border-gray-200">
        <div className="flex items-center gap-3">
          <span className="font-semibold text-gray-800 text-lg">llmproxy</span>
          {status && (
            <span className="text-xs text-gray-500">v{status.version}</span>
          )}
        </div>
        <div className="flex items-center gap-3">
          <span
            className={`flex items-center gap-1.5 text-sm font-medium ${
              proxyOnline ? "text-green-600" : "text-red-500"
            }`}
          >
            <span
              className={`inline-block w-2 h-2 rounded-full ${
                proxyOnline ? "bg-green-500" : "bg-red-400"
              }`}
            />
            {proxyOnline ? `Running · :${status?.uptime_secs != null ? formatUptime(status.uptime_secs) : ""}` : "Stopped"}
          </span>
          <button
            onClick={handleStartStop}
            className={`px-3 py-1 rounded text-sm font-medium transition-colors ${
              proxyOnline
                ? "bg-red-50 text-red-600 hover:bg-red-100"
                : "bg-green-50 text-green-700 hover:bg-green-100"
            }`}
          >
            {proxyOnline ? "Stop" : "Start"}
          </button>
        </div>
      </header>

      {/* Error banner */}
      {actionError && (
        <div className="flex items-center justify-between px-5 py-2 bg-red-50 border-b border-red-200 text-sm text-red-700">
          <span>{actionError}</span>
          <button onClick={() => setActionError(null)} className="ml-4 text-red-400 hover:text-red-600">✕</button>
        </div>
      )}

      {/* Tab bar */}
      <nav className="flex gap-0 bg-white border-b border-gray-200 px-5">
        {tabs.map(({ key, label }) => (
          <button
            key={key}
            onClick={() => setTab(key)}
            className={`px-4 py-2.5 text-sm font-medium border-b-2 transition-colors ${
              tab === key
                ? "border-blue-500 text-blue-600"
                : "border-transparent text-gray-500 hover:text-gray-700"
            }`}
          >
            {label}
          </button>
        ))}
      </nav>

      {/* Content */}
      <main className="flex-1 overflow-auto">
        {tab === "usage" && <UsageTab proxyOnline={proxyOnline} />}
        {tab === "providers" && (
          <ProvidersTab proxyOnline={proxyOnline} />
        )}
        {tab === "chat" && <ChatTab proxyOnline={proxyOnline} />}
        {tab === "settings" && <SettingsTab status={status} />}
      </main>
    </div>
  );
}

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}
