export interface SessionMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: string;  // ISO 8601，便于 JSON 序列化到 localStorage
  toolCall?: {
    name: string;
    args: Record<string, unknown>;
    output?: string;
  };
}

export interface Session {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  messages: SessionMessage[];
  status: 'active' | 'archived';
}
