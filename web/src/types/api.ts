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
  command: string;
  next_run: string;
  last_run: string | null;
  last_status: string | null;
  enabled: boolean;
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

export interface SSEEvent {
  type: string;
  timestamp?: string;
  [key: string]: any;
}

export interface WsMessage {
  type: 'message' | 'chunk' | 'tool_call' | 'tool_result' | 'done' | 'error';
  content?: string;
  full_response?: string;
  name?: string;
  args?: any;
  output?: string;
  message?: string;
}
