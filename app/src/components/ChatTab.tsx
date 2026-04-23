import { useEffect, useRef, useState } from "react";

const PROXY = "http://127.0.0.1:8080";

const PRESET_MODELS: Record<string, string[]> = {
  anthropic: [
    "anthropic/claude-sonnet-4-5",
    "anthropic/claude-opus-4-5",
    "anthropic/claude-haiku-4-5",
  ],
  openai: [
    "openai/gpt-4o",
    "openai/gpt-4o-mini",
    "openai/o3-mini",
  ],
  gemini: [
    "gemini/gemini-2.5-flash",
    "gemini/gemini-2.0-flash",
    "gemini/gemini-1.5-pro",
  ],
  mistral: [
    "mistral/mistral-large-latest",
    "mistral/mistral-small-latest",
  ],
  togetherai: [
    "togetherai/meta-llama/Llama-3.3-70B-Instruct-Turbo",
    "togetherai/mistralai/Mixtral-8x7B-Instruct-v0.1",
  ],
  bedrock: [
    "bedrock/anthropic.claude-3-5-sonnet-20241022-v2:0",
    "bedrock/amazon.nova-pro-v1:0",
  ],
  azure: [],
};

interface Message {
  role: "user" | "assistant";
  content: string;
}

interface Props {
  proxyOnline: boolean;
}

export default function ChatTab({ proxyOnline }: Props) {
  const [providers, setProviders] = useState<string[]>([]);
  const [model, setModel] = useState("");
  const [customModel, setCustomModel] = useState("");
  const [useCustom, setUseCustom] = useState(false);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<AbortController | null>(null);

  useEffect(() => {
    if (!proxyOnline) return;
    fetch(`${PROXY}/v1/models`)
      .then((r) => r.json())
      .then((j) => {
        const ids: string[] = j.data?.map((m: { id: string }) => m.id) ?? [];
        setProviders(ids);
        // Pick first preset model from first provider
        for (const id of ids) {
          const presets = PRESET_MODELS[id];
          if (presets?.length) {
            setModel(presets[0]);
            return;
          }
        }
        // Azure or unknown — fall back to custom
        if (ids.length > 0) {
          setUseCustom(true);
          setCustomModel(`${ids[0]}/`);
        }
      })
      .catch(() => {});
  }, [proxyOnline]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const activeModel = useCustom ? customModel.trim() : model;

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
      const res = await fetch(`${PROXY}/v1/chat/completions`, {
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
        // user cancelled
      } else {
        setError(String(e));
        setMessages((prev) => prev.slice(0, -1)); // remove empty assistant bubble
      }
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
  };

  const stop = () => {
    abortRef.current?.abort();
  };

  const clear = () => {
    abortRef.current?.abort();
    setMessages([]);
    setError(null);
  };

  if (!proxyOnline) {
    return (
      <div className="flex items-center justify-center h-full text-gray-400 text-sm">
        Start the proxy to use Chat.
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Model picker */}
      <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-100 bg-gray-50">
        {useCustom ? (
          <input
            className="flex-1 text-sm border border-gray-200 rounded px-2 py-1 font-mono focus:outline-none focus:ring-1 focus:ring-blue-300"
            placeholder="provider/model-id"
            value={customModel}
            onChange={(e) => setCustomModel(e.target.value)}
          />
        ) : (
          <select
            className="flex-1 text-sm border border-gray-200 rounded px-2 py-1 bg-white focus:outline-none focus:ring-1 focus:ring-blue-300"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          >
            {providers.flatMap((p) =>
              (PRESET_MODELS[p] ?? []).map((m) => (
                <option key={m} value={m}>{m}</option>
              ))
            )}
          </select>
        )}
        <button
          onClick={() => {
            setUseCustom((v) => !v);
            if (!useCustom) setCustomModel(activeModel);
          }}
          className="text-xs text-gray-400 hover:text-gray-600 px-1"
          title={useCustom ? "Use preset" : "Enter custom model"}
        >
          {useCustom ? "presets" : "custom"}
        </button>
        {messages.length > 0 && (
          <button
            onClick={clear}
            className="text-xs text-gray-400 hover:text-red-500 px-1"
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
            onClick={stop}
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
