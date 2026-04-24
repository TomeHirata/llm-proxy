const STORAGE_KEY = "llmproxy-conversations";
const MAX_STORED = 50;

type ContentPart =
  | { type: "text"; text: string }
  | { type: "image_url"; image_url: { url: string } }
  | { type: "input_audio"; input_audio: { data: string; format: string } };

export interface Message {
  role: "user" | "assistant";
  content: string | ContentPart[];
}

export interface Conversation {
  id: string;
  title: string;
  provider: string;
  model: string;
  messages: Message[];
  createdAt: number;
  updatedAt: number;
}

function load(): Conversation[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    return JSON.parse(raw) as Conversation[];
  } catch {
    return [];
  }
}

function byRecency(convos: Conversation[]): Conversation[] {
  return [...convos].sort((a, b) => b.updatedAt - a.updatedAt);
}

function save(convos: Conversation[]): void {
  // Always trim by recency so we drop the oldest, regardless of input order.
  const candidates = byRecency(convos).slice(0, MAX_STORED);
  // Try persisting; if quota is exceeded, progressively drop more entries.
  for (let keep = candidates.length; keep > 0; keep--) {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(candidates.slice(0, keep)));
      return;
    } catch {
      // quota exceeded — try with fewer entries
    }
  }
}

function titleFrom(messages: Message[]): string {
  const first = messages.find((m) => m.role === "user");
  if (!first) return "New conversation";
  const text = typeof first.content === "string"
    ? first.content
    : first.content.find((p): p is { type: "text"; text: string } => p.type === "text")?.text ?? "";
  return text.slice(0, 60) + (text.length > 60 ? "…" : "");
}

export const conversationStore = {
  list(): Conversation[] {
    return byRecency(load());
  },

  get(id: string): Conversation | undefined {
    return load().find((c) => c.id === id);
  },

  upsert(convo: Conversation): void {
    const all = load().filter((c) => c.id !== convo.id);
    const updated = { ...convo, title: titleFrom(convo.messages), updatedAt: Date.now() };
    save([...all, updated]);
  },

  remove(id: string): void {
    save(load().filter((c) => c.id !== id));
  },

  newId(): string {
    return `conv-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  },
};
