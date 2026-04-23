import { useEffect, useRef, useState, useCallback } from "react";
import { conversationStore, type Conversation, type Message } from "../conversationStore";

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

  // Conversation state
  const [conversations, setConversations] = useState<Conversation[]>(() =>
    conversationStore.list()
  );
  const [activeConvId, setActiveConvId] = useState<string | null>(() => {
    const list = conversationStore.list();
    return list[0]?.id ?? null;
  });
  const [showHistory, setShowHistory] = useState(false);

  const activeConv = activeConvId ? (conversationStore.get(activeConvId) ?? null) : null;
  const messages: Message[] = activeConv?.messages ?? [];

  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<AbortController | null>(null);
  const lastSaveRef = useRef<number>(0); // throttle localStorage writes during streaming

  // Fetch live model lists when configured providers change
  useEffect(() => {
    if (!proxyOnline || configuredProviders.length === 0) return;

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

  const handleProviderChange = (p: string) => {
    setSelectedProvider(p);
    setSelectedModel(modelsByProvider[p]?.[0] ?? "");
  };

  // Restore model selector from a stored conversation.
  // Falls back to custom mode when the provider/model isn't in the current preset lists.
  const applyConvoModel = useCallback((model: string) => {
    const [p, ...rest] = model.split("/");
    const m = rest.join("/");
    const providerAvailable = !!p && configuredProviders.includes(p);
    const modelAvailable = !!m && (modelsByProvider[p] ?? []).includes(m);
    if (providerAvailable && modelAvailable) {
      setSelectedProvider(p);
      setSelectedModel(m);
      setCustomModel("");
      setUseCustom(false);
    } else {
      setSelectedProvider("");
      setSelectedModel("");
      setCustomModel(model);
      setUseCustom(true);
    }
  }, [configuredProviders, modelsByProvider]);

  // On mount and whenever activeConvId changes, restore the model selector.
  useEffect(() => {
    if (!activeConvId) return;
    const convo = conversationStore.get(activeConvId);
    if (convo?.model) applyConvoModel(convo.model);
  }, [activeConvId, applyConvoModel]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const activeModel = useCustom
    ? customModel.trim()
    : selectedProvider && selectedModel
      ? `${selectedProvider}/${selectedModel}`
      : "";

  const newConversation = () => {
    abortRef.current?.abort();
    setActiveConvId(null);
    setError(null);
    setConversations(conversationStore.list());
  };

  const loadConversation = (id: string) => {
    abortRef.current?.abort();
    setActiveConvId(id); // triggers the applyConvoModel effect above
    setShowHistory(false);
    setError(null);
  };

  const deleteConversation = (id: string) => {
    conversationStore.remove(id);
    if (activeConvId === id) setActiveConvId(null);
    setConversations(conversationStore.list());
  };

  const send = async () => {
    const text = input.trim();
    if (!text || !activeModel || streaming) return;
    setError(null);
    setInput("");

    const next: Message[] = [...messages, { role: "user", content: text }];
    // Optimistically update and save
    const convId = activeConvId ?? conversationStore.newId();
    setActiveConvId(convId);
    const [provider] = activeModel.split("/");
    const now = Date.now();
    const convoBase: Conversation = activeConv ?? { id: convId, title: "", provider, model: activeModel, messages: [], createdAt: now, updatedAt: now };
    conversationStore.upsert({ ...convoBase, messages: next });
    setConversations(conversationStore.list());

    const assistantIdx = next.length;
    let latestMessages = [...next, { role: "assistant" as const, content: "" }];

    const ctrl = new AbortController();
    abortRef.current = ctrl;
    setStreaming(true);

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
              latestMessages = latestMessages.map((m, i) =>
                i === assistantIdx
                  ? { ...m, content: m.content + delta }
                  : m
              );
              // Throttle localStorage writes to avoid jank on long streams
              const now = Date.now();
              if (now - lastSaveRef.current >= 500) {
                conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: latestMessages });
                setConversations(conversationStore.list());
                lastSaveRef.current = now;
              }
            }
          } catch {
            // non-JSON line, ignore
          }
        }
      }
    } catch (e: unknown) {
      if ((e as { name?: string }).name === "AbortError") {
        // user cancelled — save the partial response if any content arrived
        const last = latestMessages[latestMessages.length - 1];
        if (last?.role === "assistant" && last.content) {
          conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: latestMessages });
          setConversations(conversationStore.list());
        }
        return;
      } else {
        setError(String(e));
        // Remove the empty assistant bubble
        const trimmed = latestMessages.slice(0, -1);
        conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: trimmed });
        setConversations(conversationStore.list());
      }
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
    // Remove empty assistant bubble if no content arrived
    const last = latestMessages[latestMessages.length - 1];
    if (last?.role === "assistant" && !last.content) {
      const trimmed = latestMessages.slice(0, -1);
      conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: trimmed });
    } else {
      conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: latestMessages });
    }
    setConversations(conversationStore.list());
  };

  if (!proxyOnline) {
    return (
      <div className="flex items-center justify-center h-full text-gray-400 text-sm">
        Start the proxy to use Chat.
      </div>
    );
  }

  const modelsForProvider = selectedProvider ? (modelsByProvider[selectedProvider] ?? []) : [];
  const currentMessages = activeConvId ? (conversationStore.get(activeConvId)?.messages ?? []) : [];

  return (
    <div className="flex h-full">
      {/* History sidebar */}
      {showHistory && (
        <div className="w-56 flex-shrink-0 border-r border-gray-100 bg-gray-50 flex flex-col">
          <div className="flex items-center justify-between px-3 py-2 border-b border-gray-100">
            <span className="text-xs font-medium text-gray-500 uppercase tracking-wide">History</span>
            <button onClick={newConversation} className="text-xs text-blue-500 hover:text-blue-700">+ New</button>
          </div>
          <div className="flex-1 overflow-y-auto">
            {conversations.length === 0 && (
              <p className="text-xs text-gray-400 text-center mt-8 px-3">No saved conversations</p>
            )}
            {conversations.map((c) => (
              <div
                key={c.id}
                className={`group flex items-start justify-between px-3 py-2 cursor-pointer border-b border-gray-100 hover:bg-white ${
                  c.id === activeConvId ? "bg-white" : ""
                }`}
                onClick={() => loadConversation(c.id)}
              >
                <div className="flex-1 min-w-0">
                  <p className="text-xs text-gray-700 truncate">{c.title || "New conversation"}</p>
                  <p className="text-[10px] text-gray-400 mt-0.5">{c.model || c.provider}</p>
                </div>
                <button
                  type="button"
                  onClick={(e) => { e.stopPropagation(); deleteConversation(c.id); }}
                  className="ml-1 opacity-0 group-hover:opacity-100 text-gray-300 hover:text-red-400 text-xs leading-none flex-shrink-0"
                  title="Delete conversation"
                  aria-label="Delete conversation"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="flex flex-col flex-1 min-w-0">
        {/* Model selector bar */}
        <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-100 bg-gray-50 flex-wrap">
          {/* History toggle */}
          <button
            type="button"
            onClick={() => setShowHistory((v) => !v)}
            className={`text-xs px-2 py-1 rounded border whitespace-nowrap ${showHistory ? "border-blue-300 text-blue-600 bg-blue-50" : "border-gray-200 text-gray-400 hover:text-gray-600"}`}
            title="Conversation history"
            aria-label="Toggle conversation history"
          >
            ☰
          </button>

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

          {currentMessages.length > 0 && (
            <button
              onClick={newConversation}
              className="text-xs text-gray-400 hover:text-red-500 whitespace-nowrap"
            >
              clear
            </button>
          )}
        </div>

        {/* Messages */}
        <div className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
          {currentMessages.length === 0 && (
            <p className="text-center text-gray-300 text-sm mt-16">
              Send a message to start chatting
            </p>
          )}
          {currentMessages.map((msg, i) => (
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
                {streaming && i === currentMessages.length - 1 && msg.role === "assistant" && (
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
    </div>
  );
}
