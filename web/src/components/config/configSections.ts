import {
  Settings,
  Eye,
  ShieldCheck,
  Box,
  Gauge,
  FileText,
  KeyRound,
  OctagonAlert,
  Filter,
  Globe,
  Server,
  Container,
  RefreshCw,
  Clock,
  Bot,
  Sparkles,
  Heart,
  Timer,
  Target,
  MessageCircle,
  Send,
  Hash,
  Database,
  Router,
  Search,
  Network,
  UserCircle,
  DollarSign,
  Mic,
  BookOpen,
  Puzzle,
  Lock,
  ArrowLeftRight,
  Cpu,
  Plug,
  Webhook,
  Users,
  Image,
  HardDrive,
  Play,
} from 'lucide-react';
import type { SectionDef } from './types';

export const CONFIG_SECTIONS: SectionDef[] = [
  // ── General ───────────────────────────────────────────────────────
  {
    path: '',
    category: 'general',
    title: 'General',
    description: 'Top-level model and provider settings',
    icon: Settings,
    fields: [
      { key: 'api_key', label: 'API Key', type: 'password', sensitive: true, description: 'e.g. sk-abc123... or key-xyz...' },
      { key: 'api_url', label: 'API URL', type: 'text', description: 'e.g. https://api.openai.com/v1' },
      { key: 'default_provider', label: 'Default Provider', type: 'text', description: 'e.g. openrouter, openai, anthropic', defaultValue: 'openrouter' },
      { key: 'provider_api', label: 'Provider API Mode', type: 'select', options: [
        { value: 'openai-chat-completions', label: 'OpenAI Chat Completions' },
        { value: 'openai-responses', label: 'OpenAI Responses' },
      ]},
      { key: 'default_model', label: 'Default Model', type: 'text', description: 'e.g. anthropic/claude-sonnet-4.6', defaultValue: 'anthropic/claude-sonnet-4.6' },
      { key: 'default_temperature', label: 'Temperature', type: 'number', min: 0, max: 2, step: 0.1, defaultValue: 0.7, description: 'Default: 0.7 (range 0.0–2.0)' },
      { key: 'model_support_vision', label: 'Model Supports Vision', type: 'toggle', description: 'Whether the model supports image inputs' },
    ],
  },

  // ── Provider ──────────────────────────────────────────────────────
  {
    path: 'provider',
    category: 'general',
    title: 'Provider',
    description: 'Provider-specific settings',
    icon: Cpu,
    defaultCollapsed: true,
    fields: [
      { key: 'reasoning_level', label: 'Reasoning Level', type: 'text', description: 'e.g. low, medium, high' },
    ],
  },

  // ── Observability ─────────────────────────────────────────────────
  {
    path: 'observability',
    category: 'advanced',
    title: 'Observability',
    description: 'Tracing, metrics, and telemetry',
    icon: Eye,
    defaultCollapsed: true,
    fields: [
      { key: 'backend', label: 'Backend', type: 'select', defaultValue: 'none', options: [
        { value: 'none', label: 'None' },
        { value: 'otlp', label: 'OpenTelemetry (OTLP)' },
      ]},
      { key: 'otel_endpoint', label: 'OTEL Endpoint', type: 'text', description: 'e.g. http://localhost:4317' },
      { key: 'otel_service_name', label: 'OTEL Service Name', type: 'text', description: 'e.g. zeroclaw-prod' },
      { key: 'runtime_trace_mode', label: 'Runtime Trace Mode', type: 'select', defaultValue: 'none', options: [
        { value: 'none', label: 'None' },
        { value: 'file', label: 'File' },
      ]},
      { key: 'runtime_trace_path', label: 'Runtime Trace Path', type: 'text', defaultValue: 'state/runtime-trace.jsonl', description: 'Default: state/runtime-trace.jsonl' },
      { key: 'runtime_trace_max_entries', label: 'Max Trace Entries', type: 'number', min: 1, defaultValue: 200, description: 'Default: 200' },
    ],
  },

  // ── Autonomy ──────────────────────────────────────────────────────
  {
    path: 'autonomy',
    category: 'advanced',
    title: 'Autonomy',
    description: 'Agent autonomy level, action limits, and tool permissions',
    icon: ShieldCheck,
    fields: [
      { key: 'level', label: 'Autonomy Level', type: 'select', defaultValue: 'supervised', options: [
        { value: 'read_only', label: 'Read Only' },
        { value: 'supervised', label: 'Supervised' },
        { value: 'full', label: 'Full' },
      ]},
      { key: 'workspace_only', label: 'Workspace Only', type: 'toggle', defaultValue: true, description: 'Restrict actions to workspace directory' },
      { key: 'max_actions_per_hour', label: 'Max Actions / Hour', type: 'number', min: 1, defaultValue: 20, description: 'Default: 20' },
      { key: 'max_cost_per_day_cents', label: 'Max Cost / Day (cents)', type: 'number', min: 0, defaultValue: 500, description: 'Default: 500 (= $5.00)' },
      { key: 'require_approval_for_medium_risk', label: 'Require Approval for Medium Risk', type: 'toggle', defaultValue: true },
      { key: 'block_high_risk_commands', label: 'Block High Risk Commands', type: 'toggle', defaultValue: true },
      { key: 'allowed_commands', label: 'Allowed Commands', type: 'tag-list', tagPlaceholder: 'Add command (e.g. git, npm, cargo)' },
      { key: 'forbidden_paths', label: 'Forbidden Paths', type: 'tag-list', tagPlaceholder: 'Add path (e.g. /etc, ~/.ssh)' },
      { key: 'auto_approve', label: 'Auto-Approve Tools', type: 'tag-list', tagPlaceholder: 'e.g. file_read, memory_recall' },
      { key: 'always_ask', label: 'Always Ask Tools', type: 'tag-list', tagPlaceholder: 'e.g. shell, file_write' },
      { key: 'allowed_roots', label: 'Allowed Roots', type: 'tag-list', tagPlaceholder: 'e.g. /home/user/projects' },
      { key: 'shell_env_passthrough', label: 'Shell Env Passthrough', type: 'tag-list', tagPlaceholder: 'e.g. PATH, HOME, EDITOR' },
    ],
  },

  // ── Security: Sandbox ─────────────────────────────────────────────
  {
    path: 'security.sandbox',
    category: 'security',
    title: 'Security: Sandbox',
    description: 'Process sandboxing backend',
    icon: Box,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', description: 'Enable sandboxing (auto-detect if unset)' },
      { key: 'backend', label: 'Backend', type: 'select', defaultValue: 'auto', options: [
        { value: 'auto', label: 'Auto' },
        { value: 'landlock', label: 'Landlock' },
        { value: 'firejail', label: 'Firejail' },
        { value: 'bubblewrap', label: 'Bubblewrap' },
        { value: 'docker', label: 'Docker' },
        { value: 'none', label: 'None' },
      ]},
      { key: 'firejail_args', label: 'Firejail Extra Args', type: 'tag-list', tagPlaceholder: 'e.g. --net=none, --private' },
    ],
  },

  // ── Security: Resources ───────────────────────────────────────────
  {
    path: 'security.resources',
    category: 'security',
    title: 'Security: Resource Limits',
    description: 'Memory, CPU, and subprocess limits',
    icon: Gauge,
    defaultCollapsed: true,
    fields: [
      { key: 'max_memory_mb', label: 'Max Memory (MB)', type: 'number', min: 1, defaultValue: 512, description: 'Default: 512' },
      { key: 'max_cpu_time_seconds', label: 'Max CPU Time (s)', type: 'number', min: 1, defaultValue: 60, description: 'Default: 60' },
      { key: 'max_subprocesses', label: 'Max Subprocesses', type: 'number', min: 1, defaultValue: 10, description: 'Default: 10' },
      { key: 'memory_monitoring', label: 'Memory Monitoring', type: 'toggle', defaultValue: true },
    ],
  },

  // ── Security: Audit ───────────────────────────────────────────────
  {
    path: 'security.audit',
    category: 'security',
    title: 'Security: Audit',
    description: 'Audit logging configuration',
    icon: FileText,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'log_path', label: 'Log Path', type: 'text', defaultValue: 'audit.log', description: 'Default: audit.log' },
      { key: 'max_size_mb', label: 'Max Size (MB)', type: 'number', min: 1, defaultValue: 100, description: 'Default: 100' },
      { key: 'sign_events', label: 'Sign Events', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Security: OTP ─────────────────────────────────────────────────
  {
    path: 'security.otp',
    category: 'security',
    title: 'Security: OTP',
    description: 'One-time password challenge settings',
    icon: KeyRound,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'method', label: 'Method', type: 'select', defaultValue: 'totp', options: [
        { value: 'totp', label: 'TOTP' },
        { value: 'pairing', label: 'Pairing' },
        { value: 'cli-prompt', label: 'CLI Prompt' },
      ]},
      { key: 'token_ttl_secs', label: 'Token TTL (s)', type: 'number', min: 1, defaultValue: 30, description: 'Default: 30' },
      { key: 'cache_valid_secs', label: 'Cache Valid (s)', type: 'number', min: 0, defaultValue: 300, description: 'Default: 300 (5 min)' },
      { key: 'gated_actions', label: 'Gated Actions', type: 'tag-list', tagPlaceholder: 'e.g. shell, file_write, browser_open' },
      { key: 'gated_domains', label: 'Gated Domains', type: 'tag-list', tagPlaceholder: 'e.g. example.com' },
      { key: 'challenge_delivery', label: 'Challenge Delivery', type: 'select', defaultValue: 'dm', options: [
        { value: 'dm', label: 'Direct Message' },
        { value: 'thread', label: 'Thread' },
        { value: 'ephemeral', label: 'Ephemeral' },
      ]},
      { key: 'challenge_timeout_secs', label: 'Challenge Timeout (s)', type: 'number', min: 1, defaultValue: 120, description: 'Default: 120 (2 min)' },
      { key: 'challenge_max_attempts', label: 'Max Attempts', type: 'number', min: 1, max: 10, defaultValue: 3, description: 'Default: 3 (max 10)' },
    ],
  },

  // ── Security: E-Stop ──────────────────────────────────────────────
  {
    path: 'security.estop',
    category: 'security',
    title: 'Security: Emergency Stop',
    description: 'Emergency stop configuration',
    icon: OctagonAlert,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'state_file', label: 'State File', type: 'text', defaultValue: '~/.zeroclaw/estop-state.json', description: 'Default: ~/.zeroclaw/estop-state.json' },
      { key: 'require_otp_to_resume', label: 'Require OTP to Resume', type: 'toggle', defaultValue: true },
    ],
  },

  // ── Security: Perplexity Filter ───────────────────────────────────
  {
    path: 'security.perplexity_filter',
    category: 'security',
    title: 'Security: Perplexity Filter',
    description: 'Adversarial suffix detection',
    icon: Filter,
    defaultCollapsed: true,
    fields: [
      { key: 'enable_perplexity_filter', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'perplexity_threshold', label: 'Perplexity Threshold', type: 'number', min: 0, step: 0.1, defaultValue: 18.0, description: 'Default: 18.0' },
      { key: 'suffix_window_chars', label: 'Suffix Window (chars)', type: 'number', min: 1, defaultValue: 64, description: 'Default: 64' },
      { key: 'min_prompt_chars', label: 'Min Prompt (chars)', type: 'number', min: 1, defaultValue: 32, description: 'Default: 32' },
      { key: 'symbol_ratio_threshold', label: 'Symbol Ratio Threshold', type: 'number', min: 0, max: 1, step: 0.01, defaultValue: 0.20, description: 'Default: 0.20 (range 0–1)' },
    ],
  },

  // ── Security: URL Access ──────────────────────────────────────────
  {
    path: 'security.url_access',
    category: 'security',
    title: 'Security: URL Access',
    description: 'Network access controls for URLs',
    icon: Globe,
    defaultCollapsed: true,
    fields: [
      { key: 'block_private_ip', label: 'Block Private IPs', type: 'toggle', defaultValue: true },
      { key: 'allow_loopback', label: 'Allow Loopback', type: 'toggle', defaultValue: false },
      { key: 'allow_cidrs', label: 'Allowed CIDRs', type: 'tag-list', tagPlaceholder: 'e.g. 10.0.0.0/8, 192.168.0.0/16' },
      { key: 'allow_domains', label: 'Allowed Domains', type: 'tag-list', tagPlaceholder: 'e.g. api.example.com' },
    ],
  },

  // ── Runtime ───────────────────────────────────────────────────────
  {
    path: 'runtime',
    category: 'runtime',
    title: 'Runtime',
    description: 'Runtime execution environment',
    icon: Server,
    defaultCollapsed: true,
    fields: [
      { key: 'kind', label: 'Kind', type: 'select', defaultValue: 'native', options: [
        { value: 'native', label: 'Native' },
        { value: 'docker', label: 'Docker' },
        { value: 'wasm', label: 'WASM' },
      ]},
      { key: 'reasoning_enabled', label: 'Reasoning Enabled', type: 'toggle', description: 'Enable model reasoning mode' },
    ],
  },

  // ── Runtime: Docker ───────────────────────────────────────────────
  {
    path: 'runtime.docker',
    category: 'runtime',
    title: 'Runtime: Docker',
    description: 'Docker container runtime settings',
    icon: Container,
    defaultCollapsed: true,
    fields: [
      { key: 'image', label: 'Image', type: 'text', defaultValue: 'alpine:3.20', description: 'e.g. alpine:3.20, ubuntu:22.04' },
      { key: 'network', label: 'Network', type: 'text', defaultValue: 'none', description: 'e.g. none, bridge, host' },
      { key: 'memory_limit_mb', label: 'Memory Limit (MB)', type: 'number', min: 1, defaultValue: 512, description: 'Default: 512' },
      { key: 'cpu_limit', label: 'CPU Limit', type: 'number', min: 0.1, step: 0.1, defaultValue: 1.0, description: 'Default: 1.0 (number of cores)' },
      { key: 'read_only_rootfs', label: 'Read-Only Root FS', type: 'toggle', defaultValue: true },
      { key: 'mount_workspace', label: 'Mount Workspace', type: 'toggle', defaultValue: true },
      { key: 'allowed_workspace_roots', label: 'Allowed Workspace Roots', type: 'tag-list', tagPlaceholder: 'e.g. /home/user/projects' },
    ],
  },

  // ── Research ──────────────────────────────────────────────────────
  {
    path: 'research',
    category: 'runtime',
    title: 'Research',
    description: 'Research phase configuration',
    icon: BookOpen,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'trigger', label: 'Trigger', type: 'select', defaultValue: 'never', options: [
        { value: 'never', label: 'Never' },
        { value: 'always', label: 'Always' },
        { value: 'keywords', label: 'Keywords' },
        { value: 'length', label: 'Message Length' },
        { value: 'question', label: 'Question Mark' },
      ]},
      { key: 'keywords', label: 'Trigger Keywords', type: 'tag-list', tagPlaceholder: 'e.g. find, search, investigate' },
      { key: 'min_message_length', label: 'Min Message Length', type: 'number', min: 1, defaultValue: 50, description: 'Default: 50 characters' },
      { key: 'max_iterations', label: 'Max Iterations', type: 'number', min: 1, defaultValue: 5, description: 'Default: 5' },
      { key: 'show_progress', label: 'Show Progress', type: 'toggle', defaultValue: true },
    ],
  },

  // ── Reliability ───────────────────────────────────────────────────
  {
    path: 'reliability',
    category: 'advanced',
    title: 'Reliability',
    description: 'Retry, fallback, and backoff settings',
    icon: RefreshCw,
    defaultCollapsed: true,
    fields: [
      { key: 'provider_retries', label: 'Provider Retries', type: 'number', min: 0, defaultValue: 2, description: 'Default: 2' },
      { key: 'provider_backoff_ms', label: 'Backoff (ms)', type: 'number', min: 0, defaultValue: 500, description: 'Default: 500' },
      { key: 'fallback_providers', label: 'Fallback Providers', type: 'tag-list', tagPlaceholder: 'e.g. openai, anthropic' },
      { key: 'api_keys', label: 'Fallback API Keys', type: 'tag-list', tagPlaceholder: 'Add fallback API key' },
      { key: 'channel_initial_backoff_secs', label: 'Channel Initial Backoff (s)', type: 'number', min: 1, defaultValue: 2, description: 'Default: 2' },
      { key: 'channel_max_backoff_secs', label: 'Channel Max Backoff (s)', type: 'number', min: 1, defaultValue: 60, description: 'Default: 60' },
      { key: 'scheduler_poll_secs', label: 'Scheduler Poll (s)', type: 'number', min: 1, defaultValue: 15, description: 'Default: 15' },
      { key: 'scheduler_retries', label: 'Scheduler Retries', type: 'number', min: 0, defaultValue: 2, description: 'Default: 2' },
    ],
  },

  // ── Scheduler ─────────────────────────────────────────────────────
  {
    path: 'scheduler',
    category: 'advanced',
    title: 'Scheduler',
    description: 'Task scheduler settings',
    icon: Clock,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'max_tasks', label: 'Max Tasks', type: 'number', min: 1, defaultValue: 64, description: 'Default: 64' },
      { key: 'max_concurrent', label: 'Max Concurrent', type: 'number', min: 1, defaultValue: 4, description: 'Default: 4' },
    ],
  },

  // ── Agent ─────────────────────────────────────────────────────────
  {
    path: 'agent',
    category: 'general',
    title: 'Agent',
    description: 'Agent orchestration settings',
    icon: Bot,
    fields: [
      { key: 'compact_context', label: 'Compact Context', type: 'toggle', defaultValue: true, description: 'Compress long conversation context' },
      { key: 'max_tool_iterations', label: 'Max Tool Iterations', type: 'number', min: 1, defaultValue: 20, description: 'Default: 20' },
      { key: 'max_history_messages', label: 'Max History Messages', type: 'number', min: 1, defaultValue: 50, description: 'Default: 50' },
      { key: 'parallel_tools', label: 'Parallel Tools', type: 'toggle', defaultValue: false, description: 'Execute tools in parallel when possible' },
      { key: 'tool_dispatcher', label: 'Tool Dispatcher', type: 'select', defaultValue: 'auto', options: [
        { value: 'auto', label: 'Auto' },
        { value: 'sequential', label: 'Sequential' },
        { value: 'parallel', label: 'Parallel' },
      ]},
    ],
  },

  // ── Skills ────────────────────────────────────────────────────────
  {
    path: 'skills',
    category: 'advanced',
    title: 'Skills',
    description: 'Open skills and script execution',
    icon: Sparkles,
    defaultCollapsed: true,
    fields: [
      { key: 'open_skills_enabled', label: 'Open Skills Enabled', type: 'toggle', defaultValue: false },
      { key: 'open_skills_dir', label: 'Open Skills Directory', type: 'text', description: 'e.g. ./skills or /opt/zeroclaw/skills' },
      { key: 'allow_scripts', label: 'Allow Scripts', type: 'toggle', defaultValue: false },
      { key: 'prompt_injection_mode', label: 'Prompt Injection Mode', type: 'select', defaultValue: 'full', options: [
        { value: 'full', label: 'Full' },
        { value: 'compact', label: 'Compact' },
      ]},
      { key: 'clawhub_token', label: 'ClawHub Token', type: 'password', sensitive: true, description: 'e.g. clh_abc123...' },
    ],
  },

  // ── Heartbeat ─────────────────────────────────────────────────────
  {
    path: 'heartbeat',
    category: 'advanced',
    title: 'Heartbeat',
    description: 'Periodic heartbeat messages',
    icon: Heart,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'interval_minutes', label: 'Interval (min)', type: 'number', min: 1, defaultValue: 30, description: 'Default: 30' },
      { key: 'message', label: 'Message', type: 'text', description: 'e.g. Agent is alive and running' },
      { key: 'target', label: 'Target Channel', type: 'text', description: 'e.g. telegram, discord, slack' },
      { key: 'to', label: 'Recipient', type: 'text', description: 'e.g. channel ID or username' },
    ],
  },

  // ── Cron ──────────────────────────────────────────────────────────
  {
    path: 'cron',
    category: 'advanced',
    title: 'Cron',
    description: 'Cron job settings',
    icon: Timer,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'max_run_history', label: 'Max Run History', type: 'number', min: 1, defaultValue: 50, description: 'Default: 50' },
    ],
  },

  // ── Goal Loop ─────────────────────────────────────────────────────
  {
    path: 'goal_loop',
    category: 'advanced',
    title: 'Goal Loop',
    description: 'Autonomous goal pursuit loop',
    icon: Target,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'interval_minutes', label: 'Interval (min)', type: 'number', min: 1, defaultValue: 10, description: 'Default: 10' },
      { key: 'step_timeout_secs', label: 'Step Timeout (s)', type: 'number', min: 1, defaultValue: 120, description: 'Default: 120 (2 min)' },
      { key: 'max_steps_per_cycle', label: 'Max Steps / Cycle', type: 'number', min: 1, defaultValue: 3, description: 'Default: 3' },
      { key: 'channel', label: 'Channel', type: 'text', description: 'e.g. telegram, discord' },
      { key: 'target', label: 'Target', type: 'text', description: 'e.g. channel ID or user ID' },
    ],
  },

  // ── Channels Config ───────────────────────────────────────────────
  {
    path: 'channels_config',
    category: 'channels',
    title: 'Channels',
    description: 'Channel transport settings',
    icon: MessageCircle,
    fields: [
      { key: 'cli', label: 'CLI Enabled', type: 'toggle', defaultValue: true },
      { key: 'message_timeout_secs', label: 'Message Timeout (s)', type: 'number', min: 1, defaultValue: 300, description: 'Default: 300 (5 min)' },
    ],
  },

  // ── Telegram ──────────────────────────────────────────────────────
  {
    path: 'channels_config.telegram',
    category: 'channels',
    title: 'Telegram',
    description: 'Telegram bot channel',
    icon: Send,
    defaultCollapsed: true,
    fields: [
      { key: 'bot_token', label: 'Bot Token', type: 'password', sensitive: true, description: 'e.g. 123456:ABC-DEF1234ghIkl-zyx57W2v' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. 123456789 or @username' },
      { key: 'stream_mode', label: 'Stream Mode', type: 'select', defaultValue: 'off', options: [
        { value: 'off', label: 'Off' },
        { value: 'partial', label: 'Partial' },
      ]},
      { key: 'draft_update_interval_ms', label: 'Draft Update Interval (ms)', type: 'number', min: 100, defaultValue: 1000, description: 'Default: 1000' },
      { key: 'mention_only', label: 'Mention Only', type: 'toggle', defaultValue: false },
      { key: 'interrupt_on_new_message', label: 'Interrupt on New Message', type: 'toggle', defaultValue: false },
      { key: 'base_url', label: 'Custom Base URL', type: 'text', description: 'e.g. https://api.telegram.org' },
    ],
  },

  // ── Discord ───────────────────────────────────────────────────────
  {
    path: 'channels_config.discord',
    category: 'channels',
    title: 'Discord',
    description: 'Discord bot channel',
    icon: Hash,
    defaultCollapsed: true,
    fields: [
      { key: 'bot_token', label: 'Bot Token', type: 'password', sensitive: true, description: 'e.g. MTIzNDU2Nzg5.AbCdEf...' },
      { key: 'guild_id', label: 'Guild ID', type: 'text', description: 'e.g. 123456789012345678' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. 123456789012345678' },
      { key: 'listen_to_bots', label: 'Listen to Bots', type: 'toggle', defaultValue: false },
      { key: 'mention_only', label: 'Mention Only', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Slack ─────────────────────────────────────────────────────────
  {
    path: 'channels_config.slack',
    category: 'channels',
    title: 'Slack',
    description: 'Slack bot channel',
    icon: Hash,
    defaultCollapsed: true,
    fields: [
      { key: 'bot_token', label: 'Bot Token', type: 'password', sensitive: true, description: 'e.g. xoxb-1234567890-...' },
      { key: 'app_token', label: 'App Token', type: 'password', sensitive: true, description: 'e.g. xapp-1-...' },
      { key: 'channel_id', label: 'Channel ID', type: 'text', description: 'e.g. C0123456789' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. U0123456789' },
    ],
  },

  // ── Mattermost ────────────────────────────────────────────────────
  {
    path: 'channels_config.mattermost',
    category: 'channels',
    title: 'Mattermost',
    description: 'Mattermost bot channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'url', label: 'Server URL', type: 'text', description: 'e.g. https://mattermost.example.com' },
      { key: 'bot_token', label: 'Bot Token', type: 'password', sensitive: true, description: 'e.g. abc123def456...' },
      { key: 'channel_id', label: 'Channel ID', type: 'text', description: 'e.g. abc123def456ghi789' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. user_id or username' },
      { key: 'mention_only', label: 'Mention Only', type: 'toggle' },
    ],
  },

  // ── Matrix ────────────────────────────────────────────────────────
  {
    path: 'channels_config.matrix',
    category: 'channels',
    title: 'Matrix',
    description: 'Matrix chat channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'homeserver', label: 'Homeserver URL', type: 'text', description: 'e.g. https://matrix.org' },
      { key: 'access_token', label: 'Access Token', type: 'password', sensitive: true, description: 'e.g. syt_abc123...' },
      { key: 'user_id', label: 'User ID', type: 'text', description: 'e.g. @bot:matrix.org' },
      { key: 'room_id', label: 'Room ID', type: 'text', description: 'e.g. !abc123:matrix.org' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. @user:matrix.org' },
      { key: 'mention_only', label: 'Mention Only', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Signal ────────────────────────────────────────────────────────
  {
    path: 'channels_config.signal',
    category: 'channels',
    title: 'Signal',
    description: 'Signal messaging channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'http_url', label: 'HTTP URL', type: 'text', description: 'e.g. http://localhost:8080' },
      { key: 'account', label: 'Account', type: 'text', description: 'e.g. +1234567890' },
      { key: 'group_id', label: 'Group ID', type: 'text', description: 'e.g. base64-encoded group ID' },
      { key: 'allowed_from', label: 'Allowed From', type: 'tag-list', tagPlaceholder: 'e.g. +1234567890' },
      { key: 'ignore_attachments', label: 'Ignore Attachments', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Nostr ─────────────────────────────────────────────────────────
  {
    path: 'channels_config.nostr',
    category: 'channels',
    title: 'Nostr',
    description: 'Nostr relay channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'private_key', label: 'Private Key', type: 'password', sensitive: true, description: 'e.g. nsec1abc123...' },
      { key: 'relays', label: 'Relays', type: 'tag-list', tagPlaceholder: 'e.g. wss://relay.damus.io' },
      { key: 'allowed_pubkeys', label: 'Allowed Pubkeys', type: 'tag-list', tagPlaceholder: 'e.g. npub1abc123...' },
    ],
  },

  // ── Lark ──────────────────────────────────────────────────────────
  {
    path: 'channels_config.lark',
    category: 'channels',
    title: 'Lark',
    description: 'Lark/Feishu bot channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'app_id', label: 'App ID', type: 'text', description: 'e.g. cli_abc123...' },
      { key: 'app_secret', label: 'App Secret', type: 'password', sensitive: true, description: 'e.g. abc123def456...' },
      { key: 'encrypt_key', label: 'Encrypt Key', type: 'password', sensitive: true, description: 'Lark event encryption key' },
      { key: 'verification_token', label: 'Verification Token', type: 'password', sensitive: true, description: 'Lark event verification token' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. ou_abc123...' },
      { key: 'mention_only', label: 'Mention Only', type: 'toggle', defaultValue: false },
      { key: 'receive_mode', label: 'Receive Mode', type: 'select', defaultValue: 'websocket', options: [
        { value: 'websocket', label: 'WebSocket' },
        { value: 'webhook', label: 'Webhook' },
      ]},
      { key: 'port', label: 'Webhook Port', type: 'number', min: 1, max: 65535, description: 'e.g. 8080 (range 1–65535)' },
    ],
  },

  // ── DingTalk ──────────────────────────────────────────────────────
  {
    path: 'channels_config.dingtalk',
    category: 'channels',
    title: 'DingTalk',
    description: 'DingTalk bot channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'client_id', label: 'Client ID', type: 'text', description: 'e.g. dingabc123...' },
      { key: 'client_secret', label: 'Client Secret', type: 'password', sensitive: true, description: 'e.g. abc123def456...' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. user_id' },
    ],
  },

  // ── QQ ────────────────────────────────────────────────────────────
  {
    path: 'channels_config.qq',
    category: 'channels',
    title: 'QQ',
    description: 'QQ bot channel',
    icon: MessageCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'app_id', label: 'App ID', type: 'text', description: 'e.g. 102012345' },
      { key: 'app_secret', label: 'App Secret', type: 'password', sensitive: true, description: 'e.g. abc123def456...' },
      { key: 'allowed_users', label: 'Allowed Users', type: 'tag-list', tagPlaceholder: 'e.g. QQ user ID' },
      { key: 'receive_mode', label: 'Receive Mode', type: 'select', defaultValue: 'webhook', options: [
        { value: 'websocket', label: 'WebSocket' },
        { value: 'webhook', label: 'Webhook' },
      ]},
      { key: 'environment', label: 'Environment', type: 'select', defaultValue: 'production', options: [
        { value: 'production', label: 'Production' },
        { value: 'sandbox', label: 'Sandbox' },
      ]},
    ],
  },

  // ── Memory ────────────────────────────────────────────────────────
  {
    path: 'memory',
    category: 'memory',
    title: 'Memory',
    description: 'Memory backend and embedding settings',
    icon: Database,
    fields: [
      { key: 'backend', label: 'Backend', type: 'select', defaultValue: 'sqlite', options: [
        { value: 'sqlite', label: 'SQLite' },
        { value: 'markdown', label: 'Markdown' },
      ]},
      { key: 'auto_save', label: 'Auto Save', type: 'toggle', defaultValue: true },
      { key: 'hygiene_enabled', label: 'Hygiene Enabled', type: 'toggle', defaultValue: true },
      { key: 'archive_after_days', label: 'Archive After (days)', type: 'number', min: 1, defaultValue: 7, description: 'Default: 7' },
      { key: 'purge_after_days', label: 'Purge After (days)', type: 'number', min: 1, defaultValue: 30, description: 'Default: 30' },
      { key: 'conversation_retention_days', label: 'Conversation Retention (days)', type: 'number', min: 1, defaultValue: 30, description: 'Default: 30' },
      { key: 'embedding_provider', label: 'Embedding Provider', type: 'text', defaultValue: 'none', description: 'e.g. none, openai, local' },
      { key: 'embedding_model', label: 'Embedding Model', type: 'text', defaultValue: 'text-embedding-3-small', description: 'e.g. text-embedding-3-small' },
      { key: 'embedding_dimensions', label: 'Embedding Dimensions', type: 'number', min: 1, defaultValue: 1536, description: 'Default: 1536' },
      { key: 'vector_weight', label: 'Vector Weight', type: 'number', min: 0, max: 1, step: 0.1, defaultValue: 0.7, description: 'Default: 0.7 (range 0–1)' },
      { key: 'keyword_weight', label: 'Keyword Weight', type: 'number', min: 0, max: 1, step: 0.1, defaultValue: 0.3, description: 'Default: 0.3 (range 0–1)' },
      { key: 'min_relevance_score', label: 'Min Relevance Score', type: 'number', min: 0, max: 1, step: 0.05, defaultValue: 0.4, description: 'Default: 0.4 (range 0–1)' },
      { key: 'response_cache_enabled', label: 'Response Cache', type: 'toggle', defaultValue: false },
      { key: 'response_cache_ttl_minutes', label: 'Cache TTL (min)', type: 'number', min: 1, defaultValue: 60, description: 'Default: 60' },
      { key: 'snapshot_enabled', label: 'Snapshots', type: 'toggle', defaultValue: false },
      { key: 'auto_hydrate', label: 'Auto Hydrate', type: 'toggle', defaultValue: true },
    ],
  },

  // ── Memory: Qdrant ────────────────────────────────────────────────
  {
    path: 'memory.qdrant',
    category: 'memory',
    title: 'Memory: Qdrant',
    description: 'Qdrant vector database connection',
    icon: Database,
    defaultCollapsed: true,
    fields: [
      { key: 'url', label: 'URL', type: 'text', description: 'e.g. http://localhost:6334' },
      { key: 'collection', label: 'Collection', type: 'text', defaultValue: 'zeroclaw_memories', description: 'Default: zeroclaw_memories' },
      { key: 'api_key', label: 'API Key', type: 'password', sensitive: true, description: 'Qdrant Cloud API key' },
    ],
  },

  // ── Gateway ───────────────────────────────────────────────────────
  {
    path: 'gateway',
    category: 'network',
    title: 'Gateway',
    description: 'HTTP gateway and webhook server',
    icon: Router,
    fields: [
      { key: 'port', label: 'Port', type: 'number', min: 1, max: 65535, defaultValue: 42617, description: 'Default: 42617 (range 1–65535)' },
      { key: 'host', label: 'Host', type: 'text', defaultValue: '127.0.0.1', description: 'e.g. 127.0.0.1 or 0.0.0.0' },
      { key: 'require_pairing', label: 'Require Pairing', type: 'toggle', defaultValue: true },
      { key: 'allow_public_bind', label: 'Allow Public Bind', type: 'toggle', defaultValue: false },
      { key: 'pair_rate_limit_per_minute', label: 'Pair Rate Limit / min', type: 'number', min: 1, defaultValue: 10, description: 'Default: 10' },
      { key: 'webhook_rate_limit_per_minute', label: 'Webhook Rate Limit / min', type: 'number', min: 1, defaultValue: 60, description: 'Default: 60' },
      { key: 'trust_forwarded_headers', label: 'Trust Forwarded Headers', type: 'toggle', defaultValue: false },
      { key: 'idempotency_ttl_secs', label: 'Idempotency TTL (s)', type: 'number', min: 1, defaultValue: 300, description: 'Default: 300 (5 min)' },
    ],
  },

  // ── Gateway: Node Control ─────────────────────────────────────────
  {
    path: 'gateway.node_control',
    category: 'network',
    title: 'Gateway: Node Control',
    description: 'Multi-node control plane',
    icon: Router,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'auth_token', label: 'Auth Token', type: 'password', sensitive: true, description: 'Shared secret for node auth' },
      { key: 'allowed_node_ids', label: 'Allowed Node IDs', type: 'tag-list', tagPlaceholder: 'e.g. node-1, node-us-east' },
    ],
  },

  // ── Browser ───────────────────────────────────────────────────────
  {
    path: 'browser',
    category: 'tools',
    title: 'Browser',
    description: 'Browser automation settings',
    icon: Globe,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'allowed_domains', label: 'Allowed Domains', type: 'tag-list', tagPlaceholder: 'e.g. example.com, docs.rs' },
      { key: 'session_name', label: 'Session Name', type: 'text', description: 'e.g. default, research' },
      { key: 'backend', label: 'Backend', type: 'select', defaultValue: 'agent_browser', options: [
        { value: 'agent_browser', label: 'Agent Browser' },
        { value: 'native', label: 'Native' },
        { value: 'computer_use', label: 'Computer Use' },
      ]},
      { key: 'native_headless', label: 'Native Headless', type: 'toggle', defaultValue: true },
      { key: 'native_webdriver_url', label: 'WebDriver URL', type: 'text', defaultValue: 'http://127.0.0.1:9515', description: 'Default: http://127.0.0.1:9515' },
    ],
  },

  // ── HTTP Request ──────────────────────────────────────────────────
  {
    path: 'http_request',
    category: 'tools',
    title: 'HTTP Request',
    description: 'HTTP request tool settings',
    icon: Globe,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'allowed_domains', label: 'Allowed Domains', type: 'tag-list', tagPlaceholder: 'e.g. api.example.com' },
      { key: 'max_response_size', label: 'Max Response Size (bytes)', type: 'number', min: 1, defaultValue: 1000000, description: 'Default: 1000000 (1 MB)' },
      { key: 'timeout_secs', label: 'Timeout (s)', type: 'number', min: 1, defaultValue: 30, description: 'Default: 30' },
      { key: 'user_agent', label: 'User Agent', type: 'text', defaultValue: 'ZeroClaw/1.0', description: 'Default: ZeroClaw/1.0' },
    ],
  },

  // ── Web Fetch ─────────────────────────────────────────────────────
  {
    path: 'web_fetch',
    category: 'tools',
    title: 'Web Fetch',
    description: 'Web page fetching and conversion',
    icon: Globe,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'provider', label: 'Provider', type: 'text', defaultValue: 'fast_html2md', description: 'e.g. fast_html2md, firecrawl' },
      { key: 'api_key', label: 'API Key', type: 'password', sensitive: true, description: 'Provider API key (if required)' },
      { key: 'api_url', label: 'API URL', type: 'text', description: 'e.g. https://api.firecrawl.dev/v1' },
      { key: 'allowed_domains', label: 'Allowed Domains', type: 'tag-list', tagPlaceholder: 'e.g. * (all) or example.com' },
      { key: 'blocked_domains', label: 'Blocked Domains', type: 'tag-list', tagPlaceholder: 'e.g. malware.example.com' },
      { key: 'max_response_size', label: 'Max Response Size (bytes)', type: 'number', min: 1, defaultValue: 500000, description: 'Default: 500000 (500 KB)' },
      { key: 'timeout_secs', label: 'Timeout (s)', type: 'number', min: 1, defaultValue: 30, description: 'Default: 30' },
      { key: 'user_agent', label: 'User Agent', type: 'text', defaultValue: 'ZeroClaw/1.0', description: 'Default: ZeroClaw/1.0' },
    ],
  },

  // ── Web Search ────────────────────────────────────────────────────
  {
    path: 'web_search',
    category: 'tools',
    title: 'Web Search',
    description: 'Web search tool settings',
    icon: Search,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'provider', label: 'Provider', type: 'select', defaultValue: 'duckduckgo', options: [
        { value: 'duckduckgo', label: 'DuckDuckGo' },
        { value: 'brave', label: 'Brave' },
        { value: 'tavily', label: 'Tavily' },
        { value: 'serper', label: 'Serper' },
      ]},
      { key: 'api_key', label: 'API Key', type: 'password', sensitive: true, description: 'Search provider API key' },
      { key: 'api_url', label: 'API URL', type: 'text', description: 'e.g. https://api.search.brave.com' },
      { key: 'brave_api_key', label: 'Brave API Key', type: 'password', sensitive: true, description: 'Brave Search API key' },
      { key: 'max_results', label: 'Max Results', type: 'number', min: 1, defaultValue: 5, description: 'Default: 5' },
      { key: 'timeout_secs', label: 'Timeout (s)', type: 'number', min: 1, defaultValue: 15, description: 'Default: 15' },
    ],
  },

  // ── Proxy ─────────────────────────────────────────────────────────
  {
    path: 'proxy',
    category: 'network',
    title: 'Proxy',
    description: 'Network proxy settings',
    icon: Network,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'http_proxy', label: 'HTTP Proxy', type: 'text', description: 'e.g. http://proxy.example.com:8080' },
      { key: 'https_proxy', label: 'HTTPS Proxy', type: 'text', description: 'e.g. http://proxy.example.com:8443' },
      { key: 'all_proxy', label: 'All Proxy', type: 'text', description: 'e.g. socks5://proxy.example.com:1080' },
      { key: 'no_proxy', label: 'No Proxy', type: 'tag-list', tagPlaceholder: 'e.g. localhost, 127.0.0.1, .internal' },
      { key: 'scope', label: 'Scope', type: 'select', defaultValue: 'zeroclaw', options: [
        { value: 'environment', label: 'Environment' },
        { value: 'zeroclaw', label: 'ZeroClaw Only' },
        { value: 'services', label: 'Services' },
      ]},
      { key: 'services', label: 'Proxy Services', type: 'tag-list', tagPlaceholder: 'e.g. openai, anthropic' },
    ],
  },

  // ── Identity ──────────────────────────────────────────────────────
  {
    path: 'identity',
    category: 'advanced',
    title: 'Identity',
    description: 'Agent identity format',
    icon: UserCircle,
    defaultCollapsed: true,
    fields: [
      { key: 'format', label: 'Format', type: 'text', defaultValue: 'openclaw', description: 'e.g. openclaw, aieos' },
      { key: 'aieos_path', label: 'AIEOS Path', type: 'text', description: 'e.g. ./identity.aieos' },
      { key: 'aieos_inline', label: 'AIEOS Inline', type: 'text', description: 'Inline AIEOS identity string' },
    ],
  },

  // ── Cost ──────────────────────────────────────────────────────────
  {
    path: 'cost',
    category: 'advanced',
    title: 'Cost',
    description: 'Cost tracking and spending limits',
    icon: DollarSign,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'daily_limit_usd', label: 'Daily Limit (USD)', type: 'number', min: 0, step: 0.01, defaultValue: 10.0, description: 'Default: 10.00' },
      { key: 'monthly_limit_usd', label: 'Monthly Limit (USD)', type: 'number', min: 0, step: 0.01, defaultValue: 100.0, description: 'Default: 100.00' },
      { key: 'warn_at_percent', label: 'Warn at (%)', type: 'number', min: 0, max: 100, defaultValue: 80, description: 'Default: 80 (range 0–100)' },
      { key: 'allow_override', label: 'Allow Override', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Transcription ─────────────────────────────────────────────────
  {
    path: 'transcription',
    category: 'advanced',
    title: 'Transcription',
    description: 'Audio transcription settings',
    icon: Mic,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'api_url', label: 'API URL', type: 'text', defaultValue: 'https://api.groq.com/openai/v1/audio/transcriptions', description: 'Default: Groq Whisper endpoint' },
      { key: 'model', label: 'Model', type: 'text', defaultValue: 'whisper-large-v3-turbo', description: 'e.g. whisper-large-v3-turbo' },
      { key: 'language', label: 'Language', type: 'text', description: 'e.g. en, ja, zh, fr' },
      { key: 'max_duration_secs', label: 'Max Duration (s)', type: 'number', min: 1, defaultValue: 120, description: 'Default: 120 (2 min)' },
    ],
  },

  // ── Composio ──────────────────────────────────────────────────────
  {
    path: 'composio',
    category: 'advanced',
    title: 'Composio',
    description: 'Composio integration',
    icon: Puzzle,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'api_key', label: 'API Key', type: 'password', sensitive: true, description: 'Composio API key' },
      { key: 'entity_id', label: 'Entity ID', type: 'text', defaultValue: 'default', description: 'Default: default' },
    ],
  },

  // ── Secrets ───────────────────────────────────────────────────────
  {
    path: 'secrets',
    category: 'advanced',
    title: 'Secrets',
    description: 'Secret storage encryption',
    icon: Lock,
    defaultCollapsed: true,
    fields: [
      { key: 'encrypt', label: 'Encrypt', type: 'toggle', defaultValue: true },
    ],
  },

  // ── Tunnel ────────────────────────────────────────────────────────
  {
    path: 'tunnel',
    category: 'network',
    title: 'Tunnel',
    description: 'Tunnel provider for exposing gateway',
    icon: ArrowLeftRight,
    defaultCollapsed: true,
    fields: [
      { key: 'provider', label: 'Provider', type: 'select', defaultValue: 'none', options: [
        { value: 'none', label: 'None' },
        { value: 'cloudflare', label: 'Cloudflare' },
        { value: 'tailscale', label: 'Tailscale' },
        { value: 'ngrok', label: 'ngrok' },
        { value: 'custom', label: 'Custom' },
      ]},
    ],
  },

  // ── Hardware ──────────────────────────────────────────────────────
  {
    path: 'hardware',
    category: 'advanced',
    title: 'Hardware',
    description: 'Hardware integration settings',
    icon: Cpu,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'transport', label: 'Transport', type: 'select', defaultValue: 'None', options: [
        { value: 'None', label: 'None' },
        { value: 'Native', label: 'Native' },
        { value: 'Serial', label: 'Serial' },
        { value: 'Probe', label: 'Probe' },
      ]},
      { key: 'serial_port', label: 'Serial Port', type: 'text', description: 'e.g. /dev/ttyUSB0 or COM3' },
      { key: 'baud_rate', label: 'Baud Rate', type: 'number', min: 1, defaultValue: 115200, description: 'Default: 115200' },
      { key: 'probe_target', label: 'Probe Target', type: 'text', description: 'e.g. STM32F411CEUx' },
    ],
  },

  // ── Peripherals ───────────────────────────────────────────────────
  {
    path: 'peripherals',
    category: 'advanced',
    title: 'Peripherals',
    description: 'Hardware peripheral boards',
    icon: Cpu,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'datasheet_dir', label: 'Datasheet Directory', type: 'text', description: 'e.g. ./datasheets' },
    ],
  },

  // ── MCP ───────────────────────────────────────────────────────────
  {
    path: 'mcp',
    category: 'advanced',
    title: 'MCP',
    description: 'Model Context Protocol servers',
    icon: Plug,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Wasm ──────────────────────────────────────────────────────────
  {
    path: 'wasm',
    category: 'runtime',
    title: 'WASM Plugins',
    description: 'WebAssembly plugin engine',
    icon: Play,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'memory_limit_mb', label: 'Memory Limit (MB)', type: 'number', min: 1, defaultValue: 64, description: 'Default: 64' },
      { key: 'fuel_limit', label: 'Fuel Limit', type: 'number', min: 1, defaultValue: 1000000000, description: 'Default: 1000000000' },
      { key: 'registry_url', label: 'Registry URL', type: 'text', defaultValue: 'https://zeromarket.vercel.app/api', description: 'Default: ZeroMarket registry' },
    ],
  },

  // ── Multimodal ────────────────────────────────────────────────────
  {
    path: 'multimodal',
    category: 'advanced',
    title: 'Multimodal',
    description: 'Image and multimodal input settings',
    icon: Image,
    defaultCollapsed: true,
    fields: [
      { key: 'max_images', label: 'Max Images', type: 'number', min: 1, defaultValue: 4, description: 'Default: 4' },
      { key: 'max_image_size_mb', label: 'Max Image Size (MB)', type: 'number', min: 1, defaultValue: 5, description: 'Default: 5' },
      { key: 'allow_remote_fetch', label: 'Allow Remote Fetch', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Storage ───────────────────────────────────────────────────────
  {
    path: 'storage.provider.config',
    category: 'memory',
    title: 'Storage',
    description: 'External storage provider settings',
    icon: HardDrive,
    defaultCollapsed: true,
    fields: [
      { key: 'provider', label: 'Provider', type: 'text', description: 'e.g. postgres, mysql' },
      { key: 'db_url', label: 'Database URL', type: 'password', sensitive: true, description: 'e.g. postgres://user:pass@host:5432/db' },
      { key: 'schema', label: 'Schema', type: 'text', defaultValue: 'public', description: 'Default: public' },
      { key: 'table', label: 'Table', type: 'text', defaultValue: 'memories', description: 'Default: memories' },
      { key: 'tls', label: 'TLS', type: 'toggle', defaultValue: false },
    ],
  },

  // ── Hooks ─────────────────────────────────────────────────────────
  {
    path: 'hooks',
    category: 'advanced',
    title: 'Hooks',
    description: 'Lifecycle hooks',
    icon: Webhook,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
    ],
  },

  // ── Plugins ───────────────────────────────────────────────────────
  {
    path: 'plugins',
    category: 'advanced',
    title: 'Plugins',
    description: 'Plugin system settings',
    icon: Puzzle,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'allow', label: 'Allow List', type: 'tag-list', tagPlaceholder: 'e.g. my-plugin, tools-extra' },
      { key: 'deny', label: 'Deny List', type: 'tag-list', tagPlaceholder: 'e.g. untrusted-plugin' },
      { key: 'load_paths', label: 'Load Paths', type: 'tag-list', tagPlaceholder: 'e.g. ./plugins, /opt/zeroclaw/plugins' },
    ],
  },

  // ── Coordination ──────────────────────────────────────────────────
  {
    path: 'coordination',
    category: 'advanced',
    title: 'Coordination',
    description: 'Multi-agent coordination',
    icon: Users,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: true },
      { key: 'lead_agent', label: 'Lead Agent', type: 'text', defaultValue: 'delegate-lead', description: 'Default: delegate-lead' },
      { key: 'max_inbox_messages_per_agent', label: 'Max Inbox Messages', type: 'number', min: 1, defaultValue: 256, description: 'Default: 256' },
      { key: 'max_dead_letters', label: 'Max Dead Letters', type: 'number', min: 1, defaultValue: 256, description: 'Default: 256' },
      { key: 'max_context_entries', label: 'Max Context Entries', type: 'number', min: 1, defaultValue: 512, description: 'Default: 512' },
    ],
  },

  // ── Agents IPC ────────────────────────────────────────────────────
  {
    path: 'agents_ipc',
    category: 'advanced',
    title: 'Agents IPC',
    description: 'Inter-process agent communication',
    icon: Users,
    defaultCollapsed: true,
    fields: [
      { key: 'enabled', label: 'Enabled', type: 'toggle', defaultValue: false },
      { key: 'db_path', label: 'Database Path', type: 'text', defaultValue: '~/.zeroclaw/agents.db', description: 'Default: ~/.zeroclaw/agents.db' },
      { key: 'staleness_secs', label: 'Staleness (s)', type: 'number', min: 1, defaultValue: 300, description: 'Default: 300 (5 min)' },
    ],
  },
];
