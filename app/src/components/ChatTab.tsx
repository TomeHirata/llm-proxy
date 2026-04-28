import { useEffect, useRef, useState, useCallback } from "react";
import { conversationStore, type Conversation, type Message as StoredMessage } from "../conversationStore";

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
  databricks: "Databricks",
};

type ContentPart =
  | { type: "text"; text: string }
  | { type: "image_url"; image_url: { url: string } }
  | { type: "input_audio"; input_audio: { data: string; format: string } };

interface Message {
  role: "user" | "assistant";
  content: string | ContentPart[];
}

interface Attachment {
  id: string;
  name: string;
  mediaType: "image" | "audio";
  data: string;
  mimeType: string;
  format?: string;
}

interface Props {
  proxyOnline: boolean;
  configuredProviders: string[];
}

const ACCEPTED_TYPES = [
  "image/jpeg", "image/png", "image/gif", "image/webp",
  "audio/mpeg", "audio/mp3", "audio/wav", "audio/ogg", "audio/webm",
].join(",");

function audioMime(format: string): string {
  switch (format) {
    case "wav": return "audio/wav";
    case "ogg": return "audio/ogg";
    case "webm": return "audio/webm";
    default: return "audio/mpeg";
  }
}

function renderMessageContent(content: string | ContentPart[], isStreaming: boolean, isLast: boolean) {
  const cursor = isStreaming && isLast ? (
    <span className="inline-block w-1.5 h-3.5 ml-0.5 bg-gray-400 animate-pulse rounded-sm align-middle" />
  ) : null;

  if (typeof content === "string") {
    return <><span className="whitespace-pre-wrap">{content}</span>{cursor}</>;
  }

  return (
    <>
      {content.map((part, i) => {
        if (part.type === "text") {
          return <span key={i} className="whitespace-pre-wrap">{part.text}</span>;
        }
        if (part.type === "image_url") {
          return (
            <img
              key={i}
              src={part.image_url.url}
              alt="attachment"
              className="max-w-full rounded-lg mt-1 block"
              style={{ maxHeight: 300 }}
            />
          );
        }
        if (part.type === "input_audio") {
          return (
            <audio
              key={i}
              controls
              className="mt-1 w-full max-w-xs block"
              src={`data:${audioMime(part.input_audio.format)};base64,${part.input_audio.data}`}
            />
          );
        }
        return null;
      })}
      {cursor}
    </>
  );
}

export default function ChatTab({ proxyOnline, configuredProviders }: Props) {
  const [modelsByProvider, setModelsByProvider] = useState<Record<string, string[]>>({});
  const [modelErrors, setModelErrors] = useState<Record<string, string>>({});
  const [loadingModels, setLoadingModels] = useState(false);

  const [selectedProvider, setSelectedProvider] = useState("");
  const [selectedModel, setSelectedModel] = useState("");
  const [useCustom, setUseCustom] = useState(false);
  const [customModel, setCustomModel] = useState("");

  const [conversations, setConversations] = useState<Conversation[]>(() =>
    conversationStore.list()
  );
  const [activeConvId, setActiveConvId] = useState<string | null>(() => {
    const list = conversationStore.list();
    return list[0]?.id ?? null;
  });
  const [showHistory, setShowHistory] = useState(false);

  const activeConv = activeConvId ? (conversationStore.get(activeConvId) ?? null) : null;
  const messages: Message[] = (activeConv?.messages ?? []) as Message[];

  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [streamingMessages, setStreamingMessages] = useState<Message[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<AbortController | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const appliedModelForConvRef = useRef<string | null>(null);

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

  useEffect(() => {
    if (!activeConvId) {
      appliedModelForConvRef.current = null;
      return;
    }
    // Don't re-apply while models are still loading or unavailable
    if (loadingModels || Object.keys(modelsByProvider).length === 0) return;
    // Only apply once per conversation; user changes after that are preserved
    if (appliedModelForConvRef.current === activeConvId) return;
    appliedModelForConvRef.current = activeConvId;
    const convo = conversationStore.get(activeConvId);
    if (convo?.model) applyConvoModel(convo.model);
  }, [activeConvId, loadingModels, modelsByProvider, applyConvoModel]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingMessages]);

  const activeModel = useCustom
    ? customModel.trim()
    : selectedProvider && selectedModel
      ? `${selectedProvider}/${selectedModel}`
      : "";

  const newConversation = () => {
    abortRef.current?.abort();
    setActiveConvId(null);
    setAttachments([]);
    setError(null);
    setConversations(conversationStore.list());
  };

  const loadConversation = (id: string) => {
    abortRef.current?.abort();
    setActiveConvId(id);
    setAttachments([]);
    setShowHistory(false);
    setError(null);
  };

  const deleteConversation = (id: string) => {
    conversationStore.remove(id);
    if (activeConvId === id) setActiveConvId(null);
    setConversations(conversationStore.list());
  };

  const removeAttachment = (id: string) =>
    setAttachments((prev) => prev.filter((a) => a.id !== id));

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    files.forEach((file) => {
      const reader = new FileReader();
      const isImage = file.type.startsWith("image/");
      const isAudio = file.type.startsWith("audio/");
      reader.onload = (ev) => {
        const dataUrl = ev.target?.result as string;
        if (isImage) {
          setAttachments((prev) => [...prev, {
            id: crypto.randomUUID(),
            name: file.name,
            mediaType: "image",
            data: dataUrl,
            mimeType: file.type,
          }]);
        } else if (isAudio) {
          const base64 = dataUrl.split(",")[1] ?? "";
          const format = file.type.includes("wav") ? "wav"
            : file.type.includes("ogg") ? "ogg"
            : file.type.includes("webm") ? "webm"
            : "mp3";
          setAttachments((prev) => [...prev, {
            id: crypto.randomUUID(),
            name: file.name,
            mediaType: "audio",
            data: base64,
            mimeType: file.type,
            format,
          }]);
        }
      };
      reader.readAsDataURL(file);
    });
  };

  const canSend = (input.trim() !== "" || attachments.length > 0) && !!activeModel && !streaming;

  const send = async () => {
    const text = input.trim();
    if (!canSend) return;
    setError(null);
    setInput("");

    const userContent: string | ContentPart[] = attachments.length === 0
      ? text
      : [
          ...(text ? [{ type: "text" as const, text }] : []),
          ...attachments.map((att): ContentPart =>
            att.mediaType === "image"
              ? { type: "image_url", image_url: { url: att.data } }
              : { type: "input_audio", input_audio: { data: att.data, format: att.format ?? "mp3" } }
          ),
        ];
    setAttachments([]);

    const next: Message[] = [...messages, { role: "user", content: userContent }];
    const convId = activeConvId ?? conversationStore.newId();
    setActiveConvId(convId);
    const [provider] = activeModel.split("/");
    const now = Date.now();
    const convoBase: Conversation = activeConv ?? { id: convId, title: "", provider, model: activeModel, messages: [], createdAt: now, updatedAt: now };
    conversationStore.upsert({ ...convoBase, messages: next as StoredMessage[] });
    setConversations(conversationStore.list());

    const assistantIdx = next.length;
    let latestMessages: Message[] = [...next, { role: "assistant", content: "" }];

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
                  ? { ...m, content: (m.content as string) + delta }
                  : m
              );
              setStreamingMessages([...latestMessages]);
            }
          } catch {
            // non-JSON line, ignore
          }
        }
      }
    } catch (e: unknown) {
      if ((e as { name?: string }).name === "AbortError") {
        const last = latestMessages[latestMessages.length - 1];
        if (last?.role === "assistant" && last.content) {
          conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: latestMessages as StoredMessage[] });
          setConversations(conversationStore.list());
        }
        setStreamingMessages(null);
        return;
      } else {
        setError(String(e));
        const trimmed = latestMessages.slice(0, -1);
        conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: trimmed as StoredMessage[] });
        setConversations(conversationStore.list());
      }
    } finally {
      setStreaming(false);
      setStreamingMessages(null);
      abortRef.current = null;
    }
    const last = latestMessages[latestMessages.length - 1];
    if (last?.role === "assistant" && !last.content) {
      const trimmed = latestMessages.slice(0, -1);
      conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: trimmed as StoredMessage[] });
    } else {
      conversationStore.upsert({ ...convoBase, id: convId, model: activeModel, messages: latestMessages as StoredMessage[] });
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
  const storedMessages = activeConvId ? (conversationStore.get(activeConvId)?.messages ?? []) as Message[] : [];
  const currentMessages = streamingMessages ?? storedMessages;

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
          <button
            type="button"
            onClick={() => setShowHistory((v) => !v)}
            className={`text-xs px-2 py-1 rounded border whitespace-nowrap ${showHistory ? "border-blue-300 text-blue-600 bg-blue-50" : "border-gray-200 text-gray-400 hover:text-gray-600"}`}
            aria-label="Toggle conversation history"
          >
            ☰
          </button>

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
                className={`max-w-[80%] rounded-2xl px-4 py-2 text-sm leading-relaxed ${
                  msg.role === "user"
                    ? "bg-blue-500 text-white rounded-br-sm"
                    : "bg-gray-100 text-gray-800 rounded-bl-sm"
                }`}
              >
                {renderMessageContent(msg.content, streaming, i === currentMessages.length - 1)}
              </div>
            </div>
          ))}
          {error && (
            <div className="text-xs text-red-500 text-center py-1">{error}</div>
          )}
          <div ref={bottomRef} />
        </div>

        {/* Attachment previews */}
        {attachments.length > 0 && (
          <div className="px-4 pt-2 flex flex-wrap gap-2 border-t border-gray-100">
            {attachments.map((att) => (
              <div key={att.id} className="relative group flex-shrink-0">
                {att.mediaType === "image" ? (
                  <img
                    src={att.data}
                    alt={att.name}
                    className="w-14 h-14 object-cover rounded-lg border border-gray-200"
                  />
                ) : (
                  <div className="w-14 h-14 flex flex-col items-center justify-center rounded-lg border border-gray-200 bg-gray-50 text-gray-500 text-xs px-1 text-center overflow-hidden">
                    <span className="text-lg leading-none">♫</span>
                    <span className="mt-0.5 truncate w-full text-center">{att.name.split(".").pop()}</span>
                  </div>
                )}
                <button
                  type="button"
                  onClick={() => removeAttachment(att.id)}
                  className="absolute -top-1 -right-1 w-4 h-4 rounded-full bg-gray-600 text-white text-[10px] leading-none flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity"
                  aria-label={`Remove ${att.name}`}
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Input */}
        <div className="px-4 py-3 border-t border-gray-100 flex gap-2 items-end">
          <input
            ref={fileInputRef}
            type="file"
            accept={ACCEPTED_TYPES}
            multiple
            className="hidden"
            onChange={handleFileChange}
          />
          <button
            type="button"
            onClick={() => fileInputRef.current?.click()}
            className="flex-shrink-0 p-2 rounded-xl text-gray-400 hover:text-gray-600 hover:bg-gray-100 transition-colors"
            aria-label="Attach file"
          >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="w-4 h-4">
              <path fillRule="evenodd" d="M15.621 4.379a3 3 0 0 0-4.242 0l-7 7a1.5 1.5 0 0 0 2.122 2.121l7-7a1.5 1.5 0 0 0-2.121-2.121l-7 7a3 3 0 1 0 4.243 4.243l7-7a4.5 4.5 0 0 0-6.364-6.364l-7 7a6 6 0 0 0 8.485 8.486l7-7a1.5 1.5 0 0 0-2.122-2.122l-7 7a3 3 0 0 1-4.243-4.243l7-7a1.5 1.5 0 1 1 2.122 2.122l-7 7" clipRule="evenodd" />
            </svg>
          </button>
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
              disabled={!canSend}
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
