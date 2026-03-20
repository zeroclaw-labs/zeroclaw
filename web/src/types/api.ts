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
  status: "Available" | "Active" | "ComingSoon";
}

export interface DiagResult {
  severity: "ok" | "warn" | "error";
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
  type: "message" | "chunk" | "tool_call" | "tool_result" | "done" | "error";
  content?: string;
  full_response?: string;
  name?: string;
  args?: any;
  output?: string;
  message?: string;
}

// ---------------------------------------------------------------------------
// Graph / Memory OS types
// ---------------------------------------------------------------------------

export interface GraphNode {
  id: string;
  key: string;
  content: string;
  category: string;
  score: number | null;
  timestamp: string;
}

export interface GraphHotNode {
  id: string;
  key: string;
  content: string;
  heat: number;
  category: string;
}

export interface GraphStats {
  total_nodes: number;
  backend: string;
  healthy: boolean;
}

export interface GraphSearchResult {
  results: GraphNode[];
  count: number;
  query: string;
}

export interface GraphHotResponse {
  nodes: GraphHotNode[];
  count: number;
  threshold: number;
}

export interface GraphNodesResponse {
  nodes: GraphNode[];
  count: number;
}

export interface GraphBudget {
  daily_cost_usd: number;
  total_tokens: number;
  backend: string;
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

export interface Skill {
  name: string;
  description: string;
  version: string;
  author: string | null;
  tags: string[];
  tools: SkillTool[];
  prompts: string[];
}

export interface SkillTool {
  name: string;
  description: string;
  kind: string;
  command: string;
}

// ---------------------------------------------------------------------------
// MCP Servers
// ---------------------------------------------------------------------------

export interface McpServer {
  name: string;
  transport: string;
  url: string | null;
  command: string;
  args: string[];
  env: Record<string, string>;
  headers: Record<string, string>;
  tool_timeout_secs: number | null;
}

export interface McpServerInput {
  name: string;
  transport: string;
  url?: string;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  headers?: Record<string, string>;
  tool_timeout_secs?: number;
}

// ---------------------------------------------------------------------------
// Channel Config Schema (for dynamic form rendering)
// ---------------------------------------------------------------------------

export type FieldType =
  | "text"
  | "password"
  | "number"
  | "boolean"
  | "string_array"
  | "select";

export interface ChannelFieldSpec {
  name: string;
  label: string;
  type: FieldType;
  required: boolean;
  placeholder?: string;
  default_value?: unknown;
  options?: string[];
  help_text?: string;
}

export interface ChannelSchema {
  channel_key: string;
  display_name: string;
  description?: string;
  fields: ChannelFieldSpec[];
}
