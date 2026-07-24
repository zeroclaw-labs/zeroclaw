export interface StatusResponse {
  version?: string;
  /** Dotted `<type>.<alias>` of the first configured model provider, or null
   *  when none is configured. "provider" alone is reserved — always qualify. */
  model_provider: string | null;
  model: string;
  temperature: number;
  uptime_seconds: number;
  /** RFC 3339 wall-clock of daemon start. Stable across the daemon's
   *  lifetime so the Logs page can default `since_ts` to "since daemon
   *  start" without a separate `/api/logs` round-trip. */
  daemon_started_at?: string;
  gateway_port: number;
  locale: string;
  memory_backend: string;
  paired: boolean;
  channels: Record<string, boolean>;
  health: HealthSnapshot;
  /** Self-process resource snapshot. Populated on Linux, macOS, Windows,
   * and FreeBSD via the `sysinfo` crate; on unsupported hosts
   * `rss_bytes = 0` and `cpu_percent = null`. */
  process?: ProcessStats;
  /** Whether the gateway is configured to poll for newer releases and show an
   *  update indicator (`gateway.check_updates`, default true). */
  check_updates?: boolean;
  /** Whether browser-triggered self-upgrade is enabled
   *  (`gateway.allow_self_upgrade`, default false). Gates the upgrade button. */
  allow_self_upgrade?: boolean;
  /** How a post-upgrade restart is achieved: `supervised` (systemd/launchd
   *  relaunches on exit), `self_respawn` (bare unix — the daemon detached-spawns
   *  the new binary), or `manual` (container / non-unix bare — no auto-restart). */
  restart_mode?: "supervised" | "self_respawn" | "manual";
  /** Command to show the operator for finishing an upgrade with a restart. */
  restart_hint?: string;
}

export interface ProcessStats {
  rss_bytes: number;
  /** Total system RAM in bytes. `0` on unsupported platforms; render the
   * RAM tile as `rss / total * 100%` when this is non-zero. */
  system_ram_total_bytes: number;
  /** Average CPU% across logical cores (0..100 * num_cpus). `null` on the
   * first sample after boot (no baseline) or on unsupported platforms. */
  cpu_percent: number | null;
  num_cpus: number;
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

export type OptionDomain =
  | "channel_refs"
  | "peer_targets"
  | "peer_groups"
  | "agent_aliases"
  | "tool_names"
  | "memory_categories";

export interface ToolSpec {
  name: string;
  description: string;
  parameters: any;
  output?: any;
  param_domains?: Record<string, OptionDomain>;
}

export interface CronDeliveryConfig {
  mode: string;
  channel?: string | null;
  to?: string | null;
  best_effort?: boolean;
}

export type CronSchedule =
  | { kind: "cron"; expr: string; tz?: string | null }
  | { kind: "at"; at: string }
  | { kind: "every"; every_ms: number };

export interface CronJob {
  id: string;
  name: string | null;
  expression: string;
  command: string;
  prompt: string | null;
  job_type: string;
  schedule: CronSchedule;
  enabled: boolean;
  delivery: CronDeliveryConfig;
  delete_after_run: boolean;
  uses_memory: boolean;
  session_target: string | null;
  model: string | null;
  allowed_tools: string[] | null;
  source: string | null;
  agent_alias: string;
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
  /** Stable enum-variant key (e.g. `"ToolsAutomation"`); use for grouping and
   *  filtering, not display. */
  category: string;
  /** Human-readable display label derived by the API from the category enum. */
  category_label: string;
  status: "Available" | "Active";
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
  /** Alias of the agent this entry was captured for (HashMap key in
   * `config.agents`). Populated by SQL-backed memory stores when the
   * agent is known at write time; `null` for older entries or backends
   * without per-agent attribution. */
  agent_alias: string | null;
}

export interface CostSummary {
  session_cost_usd: number;
  daily_cost_usd: number;
  monthly_cost_usd: number;
  total_tokens: number;
  request_count: number;
  by_model: Record<string, ModelStats>;
  /** Per-agent rollup. Empty when `[cost].track_per_agent = false` or
   * when no records carry an agent_alias. */
  by_agent: Record<string, AgentCostStats>;
}

export interface ModelStats {
  model: string;
  cost_usd: number;
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  request_count: number;
}

export interface AgentCostStats {
  agent_alias: string;
  cost_usd: number;
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  request_count: number;
}

export interface CliTool {
  name: string;
  path: string;
  version: string | null;
  category: string;
}

export interface Session {
  /** Display form: `gw_` stripped for gateway sessions, full composite for
   * channel-driven sessions. */
  session_id: string;
  /** Full DB key. Use this when calling DELETE / messages / abort
   * endpoints — `session_id` is for display only. */
  session_key: string;
  created_at: string;
  last_activity: string;
  message_count: number;
  name?: string;
  /** Alias of the agent that owned this session. `null` for legacy rows
   * with no attribution at all (channel_id null too). */
  agent_alias: string | null;
  /** Owning channel as `<type>.<alias>` for channel-driven sessions
   * (Discord, Matrix, …). `null` for gateway WebSocket sessions. */
  channel_id: string | null;
}

export type ChannelReadinessState = 'ready' | 'missing' | 'unknown';

export interface ChannelReadiness {
  enabled: ChannelReadinessState;
  bound_to_agent: ChannelReadinessState;
  authenticated: ChannelReadinessState;
  listening: ChannelReadinessState;
  requirements: string[];
  notes: string[];
}

export interface ChannelDetail {
  /** Composite `<type>.<alias>` identifier (v0.8.0). */
  name: string;
  /** Channel type as the schema emits it (kebab; e.g. `"discord"`). */
  type: string;
  /** Per-alias HashMap key (e.g. `"loneliness"`). */
  alias: string;
  /** Agent whose `channels` list contains `<type>.<alias>`, or `null`
   * when the block is orphaned. */
  owning_agent: string | null;
  enabled: boolean;
  status: "active" | "inactive" | "error";
  message_count: number;
  last_message_at: string | null;
  health: "healthy" | "degraded" | "down";
  /** Per-alias readiness breakdown (present when the gateway computes it). */
  readiness?: ChannelReadiness;
}

export interface SSEEvent {
  type: string;
  timestamp?: string;
  [key: string]: any;
}

export interface WsMessage {
  type:
    | "message"
    | "chunk"
    | "chunk_reset"
    | "thinking"
    | "tool_call"
    | "tool_result"
    | "done"
    | "error"
    | "session_start"
    | "connected"
    | "cron_result"
    | "approval_request"
    | "history_trimmed"
    | "aborted";
  content?: string;
  full_response?: string;
  name?: string;
  args?: any;
  output?: string;
  id?: string;
  message?: string;
  code?: string;
  session_id?: string;
  resumed?: boolean;
  message_count?: number;
  timestamp?: string;
  job_id?: string;
  success?: boolean;
  // Supervised-mode tool approval (server → client). See #6522.
  request_id?: string;
  tool?: string;
  arguments_summary?: string;
  timeout_secs?: number;
  dropped_messages?: number;
  kept_turns?: number;
  reason?: string;
  // Context window info (present on "done" frames). See #7311.
  max_context_tokens?: number;
  input_tokens?: number;
  output_tokens?: number;
  last_input_tokens?: number;
}

export type ApprovalDecision = "approve" | "deny" | "always";

export interface PendingApproval {
  requestId: string;
  toolName: string;
  argumentsSummary: string;
  timeoutSecs: number;
  /** Wall-clock millis when the request arrived; used to compute remaining time. */
  receivedAt: number;
}

/** Row from GET /api/sessions/{id}/messages */
export interface SessionMessageRow {
  role: string;
  content: string;
  /** RFC 3339 timestamp recorded when the row was persisted. `null` for
   * backends that don't stamp per-row timestamps (JSONL / in-memory). */
  created_at: string | null;
}

export interface SessionMessagesResponse {
  session_id: string;
  messages: SessionMessageRow[];
  session_persistence: boolean;
}

export interface TuiEntry {
  tui_id: string;
  connected_at: string;
  peer_label: string;
  transport: string;
}
