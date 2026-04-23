import { useEffect, useRef, useState } from "react";

const PROXY_BASE = "http://127.0.0.1:8080";

const FALLBACK_MODELS: Record<string, string[]> = {
  bedrock: [
    "anthropic.claude-3-5-sonnet-20241022-v2:0",
    "amazon.nova-pro-v1:0",
  ],
  azure: [],
};

const PROVIDER_LABELS: Record<string, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
  gemini: "Gemini",
  mistral: "Mistral",
  togetherai: "TogetherAI",
  bedrock: "AWS Bedrock",
  azure: "Azure OpenAI",
};

interface Message {
  role: "user" | "assistant";
  content: string;
}

interface Props {
  proxyOnline: boolean;
  configuredProviders: string[];
}

export default function ChatTab({ proxyOnline, configuredProviders }: Props) {
  const [modelsByProvider, setModelsByProvider] = useState<Record<string, string[]>>({});
  const [modelErrors, setModelErrors] = useState<Record<string, string>>({});
  const [loadingModels, setLoadingModels] = useState(false);

  // Two-step selection: provider → model
  const [selectedProvider, setSelectedProvider] = useState("");
  const [selectedModel, setSelectedModel] = useState("");
  const [useCustom, setUseCustom] = useState(false);
  const [customModel, setCustomModel] = useState("");

  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<AbortController | null>(null);

  // Fetch live model lists when configured providers change
  useEffect(() => {
    if (!proxyOnline || configuredProviders.length === 0) return;

    // Immediately fix the provider select so it's never blank while loading
    setSelectedProvider((prev) => prev || configuredProviders[0]);

    setLoadingModels(true);
    Promise.all(
      configuredProviders.map(async (p) => {
        try {
          const r = await fetch(`${PROXY_BASE}/admin/models/${p}`);
          const j = await r.json();
          if (!r.ok) return { p, models: FALLBACK_MODELS[p] ?? [], error: j.error as string | undefined };
          return { p, models: j.models as string[], error: undefined };
        } catch {
          return { p, models: FALLBACK_MODELS[p] ?? [], error: undefined };
        }
      })
    ).then((results) => {
      const map: Record<string, string[]> = {};
      const errs: Record<string, string> = {};
      for (const { p, models, error } of results) {
        map[p] = models;
        if (error) errs[p] = error;
      }
      setModelsByProvider(map);
      setModelErrors(errs);
      setLoadingModels(false);

      // Auto-select first model for the current provider (or first provider with models).
      // Preserve the current selection if it's still valid — the effect re-runs on
      // every status poll even when providers haven't changed.
      setSelectedProvider((currentProvider) => {
        const target =
          (map[currentProvider]?.length ? currentProvider : null) ??
          configuredProviders.find((p) => map[p]?.length) ??
          currentProvider;
        setSelectedModel((currentModel) => {
          if (target === currentProvider && map[target]?.includes(currentModel)) {
            return currentModel;
          }
          return map[target]?.[0] ?? "";
        });
        return target;
      });
    });
  }, [proxyOnline, configuredProviders]);

  // When provider changes, reset model to first in list
  const handleProviderChange = (p: string) => {
    setSelectedProvider(p);
    setSelectedModel(modelsByProvider[p]?.[0] ?? "");
    // Never auto-switch to custom — user does that explicitly
  };

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const activeModel = useCustom
    ? customModel.trim()
    : selectedProvider && selectedModel
      ? `${selectedProvider}/${selectedModel}`
      : "";

  const send = async () => {
    const text = input.trim();
    if (!text || !activeModel || streaming) return;
    setError(null);
    setInput("");

    const next: Message[] = [...messages, { role: "user", content: text }];
    setMessages(next);

    const assistantIdx = next.length;
    setMessages([...next, { role: "assistant", content: "" }]);
    setStreaming(true);

    const ctrl = new AbortController();
    abortRef.current = ctrl;

    try {
      const res = await fetch(`${PROXY_BASE}/v1/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model: activeModel, messages: next, stream: true }),
        signal: ctrl.signal,
      });

      if (!res.ok) {
        const body = await res.text();
        throw new Error(`${res.status} ${body}`);
      }

      const reader = res.body!.getReader();
      const dec = new TextDecoder();
      let buf = "";

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const lines = buf.split("\n");
        buf = lines.pop() ?? "";

        for (const line of lines) {
          if (!line.startsWith("data: ")) continue;
          const payload = line.slice(6).trim();
          if (payload === "[DONE]") continue;
          try {
            const chunk = JSON.parse(payload);
            const delta: string = chunk.choices?.[0]?.delta?.content ?? "";
            if (delta) {
              setMessages((prev) => {
                const copy = [...prev];
                copy[assistantIdx] = {
                  role: "assistant",
                  content: copy[assistantIdx].content + delta,
                };
                return copy;
              });
            }
          } catch {
            // non-JSON line, ignore
          }
        }
      }
    } catch (e: unknown) {
      if ((e as { name?: string }).name === "AbortError") {
        // user cancelled — leave the partial response
        return;
      } else {
        setError(String(e));
        setMessages((prev) => prev.slice(0, -1));
      }
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
    // Remove the assistant bubble if no content arrived (e.g. all-thinking response)
    setMessages((prev) => {
      const last = prev[prev.length - 1];
      if (last?.role === "assistant" && !last.content) return prev.slice(0, -1);
      return prev;
    });
  };

  if (!proxyOnline) {
    return (
      <div className="flex items-center justify-center h-full text-gray-400 text-sm">
        Start the proxy to use Chat.
      </div>
    );
  }

  const modelsForProvider = selectedProvider ? (modelsByProvider[selectedProvider] ?? []) : [];

  return (
    <div className="flex flex-col h-full">
      {/* Model selector bar */}
      <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-100 bg-gray-50 flex-wrap">
        {/* Provider select */}
        <select
          className="text-sm border border-gray-200 rounded px-2 py-1 bg-white focus:outline-none focus:ring-1 focus:ring-blue-300"
          value={selectedProvider}
          onChange={(e) => handleProviderChange(e.target.value)}
          disabled={loadingModels}
        >
          {configuredProviders.length === 0 && (
            <option value="" disabled>No providers configured</option>
          )}
          {configuredProviders.map((p) => (
            <option key={p} value={p}>{PROVIDER_LABELS[p] ?? p}</option>
          ))}
        </select>

        {/* Model select or custom input */}
        {useCustom ? (
          <input
            className="flex-1 min-w-[200px] text-sm border border-gray-200 rounded px-2 py-1 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300"
            placeholder="provider/model-id"
            value={customModel}
            onChange={(e) => setCustomModel(e.target.value)}
          />
        ) : selectedProvider && modelErrors[selectedProvider] && !loadingModels ? (
          <div className="flex-1 min-w-[200px] text-xs text-red-500 px-2 py-1 border border-red-200 rounded bg-red-50 truncate" title={modelErrors[selectedProvider]}>
            {modelErrors[selectedProvider]}
          </div>
        ) : (
          <select
            className="flex-1 min-w-[200px] text-sm border border-gray-200 rounded px-2 py-1 bg-white focus:outline-none focus:ring-1 focus:ring-blue-300"
            value={selectedModel}
            onChange={(e) => setSelectedModel(e.target.value)}
            disabled={loadingModels || modelsForProvider.length === 0}
          >
            {loadingModels && <option value="" disabled>Loading…</option>}
            {!loadingModels && modelsForProvider.length === 0 && (
              <option value="" disabled>No models found</option>
            )}
            {modelsForProvider.map((id) => (
              <option key={id} value={id}>{id}</option>
            ))}
          </select>
        )}

        {/* Custom toggle */}
        <button
          onClick={() => {
            const next = !useCustom;
            setUseCustom(next);
            if (next) setCustomModel(activeModel || `${selectedProvider}/`);
          }}
          className="text-xs text-gray-400 hover:text-gray-600 whitespace-nowrap"
        >
          {useCustom ? "← presets" : "custom →"}
        </button>

        {messages.length > 0 && (
          <button
            onClick={() => { abortRef.current?.abort(); setMessages([]); setError(null); }}
            className="text-xs text-gray-400 hover:text-red-500 whitespace-nowrap"
          >
            clear
          </button>
        )}
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
        {messages.length === 0 && (
          <p className="text-center text-gray-300 text-sm mt-16">
            Send a message to start chatting
          </p>
        )}
        {messages.map((msg, i) => (
          <div
            key={i}
            className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}
          >
            <div
              className={`max-w-[80%] rounded-2xl px-4 py-2 text-sm whitespace-pre-wrap leading-relaxed ${
                msg.role === "user"
                  ? "bg-blue-500 text-white rounded-br-sm"
                  : "bg-gray-100 text-gray-800 rounded-bl-sm"
              }`}
            >
              {msg.content}
              {streaming && i === messages.length - 1 && msg.role === "assistant" && (
                <span className="inline-block w-1.5 h-3.5 ml-0.5 bg-gray-400 animate-pulse rounded-sm align-middle" />
              )}
            </div>
          </div>
        ))}
        {error && (
          <div className="text-xs text-red-500 text-center py-1">{error}</div>
        )}
        <div ref={bottomRef} />
      </div>

      {/* Input */}
      <div className="px-4 py-3 border-t border-gray-100 flex gap-2">
        <textarea
          rows={1}
          className="flex-1 text-sm border border-gray-200 rounded-xl px-3 py-2 resize-none focus:outline-none focus:ring-1 focus:ring-blue-300 leading-relaxed"
          placeholder="Message…"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
          style={{ maxHeight: 120, overflowY: "auto" }}
        />
        {streaming ? (
          <button
            onClick={() => abortRef.current?.abort()}
            className="px-4 py-2 rounded-xl text-sm font-medium bg-gray-100 text-gray-600 hover:bg-gray-200"
          >
            Stop
          </button>
        ) : (
          <button
            onClick={send}
            disabled={!input.trim() || !activeModel}
            className="px-4 py-2 rounded-xl text-sm font-medium bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Send
          </button>
        )}
      </div>
    </div>
  );
}
