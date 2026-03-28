export interface StatusResponse {
  provider: string | null;
  model: string;
  temperature: number;
  uptime_seconds: number;
  gateway_port: number;
  locale: string;
  memory_backend: string;
  paired: boolean;
  channels: Record<string, boolean>;
  health: HealthSnapshot;
}

export interface HealthSnapshot {
  pid: number;
  updated_at: string;
  uptime_seconds: number;
  components: Record<string, ComponentHealth>;
}

export interface ComponentHealth {
  status: string;
  updated_at: string;
  last_ok: string | null;
  last_error: string | null;
  restart_count: number;
}

export interface ToolSpec {
  name: string;
  description: string;
  parameters: any;
}

export interface CronJob {
  id: string;
  name: string | null;
  expression: string;
  command: string;
  prompt: string | null;
  job_type: string;
  schedule: unknown;
  enabled: boolean;
  delivery: unknown;
  delete_after_run: boolean;
  created_at: string;
  next_run: string;
  last_run: string | null;
  last_status: string | null;
  last_output: string | null;
}

export interface CronRun {
  id: number;
  job_id: string;
  started_at: string;
  finished_at: string;
  status: string;
  output: string | null;
  duration_ms: number | null;
}

export interface Integration {
  name: string;
  description: string;
  category: string;
  status: 'Available' | 'Active' | 'ComingSoon';
}

export interface DiagResult {
  severity: 'ok' | 'warn' | 'error';
  category: string;
  message: string;
}

export interface MemoryEntry {
  id: string;
  key: string;
  content: string;
  category: string;
  timestamp: string;
  session_id: string | null;
  score: number | null;
}

export interface CostSummary {
  session_cost_usd: number;
  daily_cost_usd: number;
  monthly_cost_usd: number;
  total_tokens: number;
  request_count: number;
  by_model: Record<string, ModelStats>;
}

export interface ModelStats {
  model: string;
  cost_usd: number;
  total_tokens: number;
  request_count: number;
}

export interface CliTool {
  name: string;
  path: string;
  version: string | null;
  category: string;
}

export interface Session {
  session_id: string;
  created_at: string;
  last_activity: string;
  message_count: number;
  name?: string;
}

export interface ChannelDetail {
  name: string;
  type: string;
  enabled: boolean;
  status: 'active' | 'inactive' | 'error';
  health: 'healthy' | 'degraded' | 'down';
}

export interface SSEEvent {
  type: string;
  timestamp?: string;
  [key: string]: any;
}

export interface WsMessage {
  type:
    | 'message'
    | 'chunk'
    | 'chunk_reset'
    | 'thinking'
    | 'tool_call'
    | 'tool_result'
    | 'done'
    | 'error'
    | 'session_start'
    | 'connected'
    | 'history'
    | 'history_end';
  content?: string;
  full_response?: string;
  name?: string;
  args?: any;
  output?: string;
  message?: string;
  code?: string;
  role?: string;
  session_id?: string;
  resumed?: boolean;
  message_count?: number;
}

/** Row from GET /api/sessions/{id}/messages */
export interface SessionMessageRow {
  role: string;
  content: string;
}

export interface SessionMessagesResponse {
  session_id: string;
  messages: SessionMessageRow[];
  session_persistence: boolean;
}
