const STORAGE_KEY = "llmproxy-conversations";
const MAX_STORED = 50;

export interface Message {
  role: "user" | "assistant";
  content: string;
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

function save(convos: Conversation[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(convos));
  } catch {
    // storage quota exceeded — drop oldest
    const trimmed = convos.slice(-MAX_STORED);
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    } catch {
      // ignore
    }
  }
}

function titleFrom(messages: Message[]): string {
  const first = messages.find((m) => m.role === "user");
  if (!first) return "New conversation";
  return first.content.slice(0, 60) + (first.content.length > 60 ? "…" : "");
}

export const conversationStore = {
  list(): Conversation[] {
    return load().sort((a, b) => b.updatedAt - a.updatedAt);
  },

  get(id: string): Conversation | undefined {
    return load().find((c) => c.id === id);
  },

  upsert(convo: Conversation): void {
    const all = load().filter((c) => c.id !== convo.id);
    // Recompute title from messages
    const updated = { ...convo, title: titleFrom(convo.messages), updatedAt: Date.now() };
    save([...all, updated].slice(-MAX_STORED));
  },

  remove(id: string): void {
    save(load().filter((c) => c.id !== id));
  },

  newId(): string {
    return `conv-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  },
};
