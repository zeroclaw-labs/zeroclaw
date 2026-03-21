import { useState, useMemo, useCallback, useEffect } from "react";
import { t, type Locale } from "../lib/i18n";
import type { ChatSession } from "../lib/storage";
import { deriveChatTitle } from "../lib/storage";
import type { DeviceInfo, ToolInfo, ChannelInfo } from "../lib/api";
import { apiClient } from "../lib/api";
import { ChannelGuide } from "./ChannelGuide";

// ---------------------------------------------------------------------------
// LLM Model selector data
// ---------------------------------------------------------------------------
interface ModelEntry {
  id: string;
  label: string;
  tier: "Premium" | "Standard" | "Fast";
}

interface ModelGroup {
  provider: string;
  keyName: string;
  models: ModelEntry[];
}

const MODEL_GROUPS: ModelGroup[] = [
  {
    provider: "anthropic",
    keyName: "anthropic",
    models: [
      { id: "claude-opus-4-6", label: "Claude Opus 4.6", tier: "Premium" },
      { id: "claude-sonnet-4-6", label: "Claude Sonnet 4.6", tier: "Standard" },
      { id: "claude-haiku-4-5-20251001", label: "Claude Haiku 4.5", tier: "Fast" },
    ],
  },
  {
    provider: "openai",
    keyName: "openai",
    models: [
      { id: "gpt-4.1", label: "GPT-4.1", tier: "Premium" },
      { id: "gpt-4.1-mini", label: "GPT-4.1 Mini", tier: "Standard" },
      { id: "gpt-4.1-nano", label: "GPT-4.1 Nano", tier: "Fast" },
    ],
  },
  {
    provider: "gemini",
    keyName: "gemini",
    models: [
      { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro", tier: "Premium" },
      { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash", tier: "Standard" },
      { id: "gemini-3.1-flash-lite-preview", label: "Gemini 3.1 Flash Lite", tier: "Fast" },
    ],
  },
];

const PROVIDER_MAPPING: Record<string, string> = {
  anthropic: "claude",
  openai: "openai",
  gemini: "gemini",
};

interface SidebarProps {
  chats: ChatSession[];
  activeChatId: string | null;
  isOpen: boolean;
  locale: Locale;
  currentPage: string;
  devices: DeviceInfo[];
  channels: string[];
  channelsDetail: ChannelInfo[];
  tools: ToolInfo[];
  onNewChat: () => void;
  onSelectChat: (id: string) => void;
  onDeleteChat: (id: string) => void;
  onOpenSettings: () => void;
  onOpenInterpreter: () => void;
  onOpenDocument: () => void;
  onLogout: () => void;
  onToggle: () => void;
}

/** Pretty display names for channels */
const CHANNEL_DISPLAY_NAMES: Record<string, string> = {
  telegram: "Telegram",
  discord: "Discord",
  slack: "Slack",
  mattermost: "Mattermost",
  whatsapp: "WhatsApp",
  line: "LINE",
  kakao: "KakaoTalk",
  qq: "QQ",
  lark: "Lark",
  feishu: "Feishu",
  dingtalk: "DingTalk",
  matrix: "Matrix",
  signal: "Signal",
  irc: "IRC",
  email: "Email",
  github: "GitHub",
  nostr: "Nostr",
  imessage: "iMessage",
  bluebubbles: "BlueBubbles",
  linq: "Linq",
  wati: "WATI",
  nextcloud_talk: "Nextcloud Talk",
  napcat: "NapCat (QQ)",
  acp: "ACP",
  clawdtalk: "ClawdTalk",
  webhook: "Webhook",
};

/** Pretty display names for tools */
const TOOL_DISPLAY_NAMES: Record<string, string> = {
  shell: "Shell",
  process: "Process Manager",
  git_operations: "Git Operations",
  file_read: "File Read",
  file_write: "File Write",
  file_edit: "File Edit",
  apply_patch: "Apply Patch",
  glob_search: "Glob Search",
  content_search: "Content Search",
  browser: "Browser Automation",
  browser_open: "Browser Open",
  http_request: "HTTP Request",
  web_fetch: "Web Fetch",
  web_search_tool: "Web Search",
  memory_store: "Memory Store",
  memory_recall: "Memory Recall",
  memory_observe: "Memory Observe",
  memory_forget: "Memory Forget",
  pdf_read: "PDF Reader",
  docx_read: "DOCX Reader",
  document_process: "Hancom Document Viewer",
  pptx_read: "PPTX Reader",
  xlsx_read: "XLSX Reader",
  screenshot: "Screenshot",
  image_info: "Image Info",
  task_plan: "Task Planner",
  cron_list: "Cron List",
  cron_add: "Cron Add",
  cron_remove: "Cron Remove",
  cron_run: "Cron Run",
  cron_runs: "Cron History",
  cron_update: "Cron Update",
  bg_run: "Background Run",
  bg_status: "Background Status",
  subagent_spawn: "Sub-Agent Spawn",
  subagent_list: "Sub-Agent List",
  subagent_manage: "Sub-Agent Manage",
  delegate: "Delegate",
  delegate_coordination_status: "Delegation Status",
  wasm_module: "WASM Module",
  composio: "Composio",
  web_search_brave: "Web Search (Brave)",
  web_search_perplexity: "Web Search (Perplexity)",
  web_search_exa: "Web Search (Exa)",
  web_search_jina: "Web Search (Jina)",
  pushover: "Pushover",
  openclaw_migration: "OpenClaw Migration",
  manage_auth_profile: "Auth Profile",
  proxy_config: "Proxy Config",
  web_access_config: "Web Access Config",
  web_search_config: "Web Search Config",
  check_provider_quota: "Quota Check",
  switch_provider: "Switch Provider",
  estimate_quota_cost: "Quota Estimate",
  hardware_board_info: "Hardware Board Info",
  hardware_memory_map: "Hardware Memory Map",
  hardware_memory_read: "Hardware Memory Read",
  sop_list: "SOP List",
  sop_execute: "SOP Execute",
  sop_status: "SOP Status",
  sop_advance: "SOP Advance",
  sop_approve: "SOP Approve",
  state_get: "State Get",
  state_set: "State Set",
  model_routing_config: "Model Routing",
  channel_ack_config: "Channel Ack Config",
  schedule: "Scheduler",
};

/** Tools that require an API key, with display info */
interface ToolApiKeyInfo {
  toolId: string;         // key sent to backend
  displayName: string;    // shown in dropdown
  placeholder: string;    // input placeholder
}

const TOOLS_REQUIRING_API_KEY: ToolApiKeyInfo[] = [
  { toolId: "composio", displayName: "Composio", placeholder: "Composio API Key" },
  { toolId: "web_search_tool", displayName: "Web Search (Firecrawl/Tavily)", placeholder: "Firecrawl or Tavily API Key" },
  { toolId: "web_search_brave", displayName: "Web Search (Brave)", placeholder: "Brave Search API Key" },
  { toolId: "web_search_perplexity", displayName: "Web Search (Perplexity)", placeholder: "Perplexity API Key" },
  { toolId: "web_search_exa", displayName: "Web Search (Exa)", placeholder: "Exa API Key" },
  { toolId: "web_search_jina", displayName: "Web Search (Jina)", placeholder: "Jina API Key" },
  { toolId: "web_fetch", displayName: "Web Fetch (Firecrawl/Tavily)", placeholder: "Firecrawl or Tavily API Key" },
  { toolId: "pushover", displayName: "Pushover", placeholder: "Pushover Token" },
];

/** Set of tool names that need API keys (for sidebar label display) —
 *  must include ALL entries from TOOLS_REQUIRING_API_KEY */
const TOOLS_NEEDING_KEY = new Set(TOOLS_REQUIRING_API_KEY.map((t) => t.toolId));

/** Format a timestamp to a short time string (HH:MM) */
function formatTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Format a date for the separator label */
function formatDateLabel(ts: number, locale: Locale): string {
  const d = new Date(ts);
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const chatDay = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diffDays = Math.floor((today.getTime() - chatDay.getTime()) / 86400000);

  if (diffDays === 0) return locale === "ko" ? "오늘" : "Today";
  if (diffDays === 1) return locale === "ko" ? "어제" : "Yesterday";
  if (diffDays < 7) {
    return d.toLocaleDateString(locale === "ko" ? "ko-KR" : "en-US", { weekday: "long" });
  }
  return d.toLocaleDateString(locale === "ko" ? "ko-KR" : "en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** Get a date key for grouping (YYYY-MM-DD) */
function dateKey(ts: number): string {
  const d = new Date(ts);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** Generic/placeholder titles that should be replaced with derived ones */
const GENERIC_TITLES = ["New Chat", "MoA", "새 대화", "새로운 대화"];

/** Get effective display title for a chat */
function getChatDisplayTitle(chat: ChatSession): string {
  // If title was properly derived (not generic), use it
  if (chat.title && !GENERIC_TITLES.includes(chat.title)) return chat.title;
  // Try to derive from first user message
  const derived = deriveChatTitle(chat.messages);
  if (!GENERIC_TITLES.includes(derived)) return derived;
  // Fallback: show timestamp-based title
  const d = new Date(chat.createdAt);
  return `${d.toLocaleDateString()} ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
}

export function Sidebar({
  chats,
  activeChatId,
  isOpen,
  locale,
  currentPage,
  devices,
  channels: _channels,
  channelsDetail,
  tools,
  onNewChat,
  onSelectChat,
  onDeleteChat,
  onOpenSettings,
  onOpenInterpreter,
  onOpenDocument,
  onLogout,
  onToggle,
}: SidebarProps) {
  const [expandedSections, setExpandedSections] = useState<Record<string, boolean>>({
    devices: true,
    models: true,
    chats: true,
  });

  // Popup state for Channels and Tools
  const [showChannelsPopup, setShowChannelsPopup] = useState(false);
  const [showToolsPopup, setShowToolsPopup] = useState(false);

  // LLM Model selector state
  const [selectedModel, setSelectedModel] = useState<string>(
    () => localStorage.getItem("zeroclaw_llm_model") || "",
  );
  const [availableProviders, setAvailableProviders] = useState<Set<string>>(() => {
    const providers = new Set<string>();
    if (localStorage.getItem("zeroclaw_api_key_anthropic")) providers.add("anthropic");
    if (localStorage.getItem("zeroclaw_api_key_openai")) providers.add("openai");
    if (localStorage.getItem("zeroclaw_api_key_gemini")) providers.add("gemini");
    return providers;
  });

  // Re-check available providers periodically (in case keys change in Settings)
  useEffect(() => {
    const interval = setInterval(() => {
      const providers = new Set<string>();
      if (localStorage.getItem("zeroclaw_api_key_anthropic")) providers.add("anthropic");
      if (localStorage.getItem("zeroclaw_api_key_openai")) providers.add("openai");
      if (localStorage.getItem("zeroclaw_api_key_gemini")) providers.add("gemini");
      setAvailableProviders(providers);
    }, 3000);
    return () => clearInterval(interval);
  }, []);

  const handleSelectModel = useCallback((provider: string, modelId: string) => {
    const mappedProvider = PROVIDER_MAPPING[provider] || provider;
    localStorage.setItem("zeroclaw_llm_provider", mappedProvider);
    localStorage.setItem("zeroclaw_llm_model", modelId);
    setSelectedModel(modelId);
  }, []);

  // Determine which model groups to show
  const visibleModelGroups = useMemo(() => {
    if (availableProviders.size === 0) {
      // No keys set: show only Gemini 3.1 Flash Lite as default free tier
      return [{
        provider: "gemini",
        keyName: "gemini",
        models: [{ id: "gemini-3.1-flash-lite-preview", label: "Gemini 3.1 Flash Lite", tier: "Fast" as const }],
      }];
    }
    return MODEL_GROUPS.filter((g) => availableProviders.has(g.keyName));
  }, [availableProviders]);

  // Tool API key dropdown state
  const [, setShowToolKeyDropdown] = useState(false);
  const [selectedToolForKey, setSelectedToolForKey] = useState<string>("");
  const [toolKeyInput, setToolKeyInput] = useState("");
  const [toolKeySaving, setToolKeySaving] = useState(false);
  const [toolKeySaved, setToolKeySaved] = useState<string | null>(null);
  const [toolKeyError, setToolKeyError] = useState<string | null>(null);
  const [toolListOpen, setToolListOpen] = useState(false);
  const [configuredToolKeys, setConfiguredToolKeys] = useState<Set<string>>(() => {
    const set = new Set<string>();
    for (const info of TOOLS_REQUIRING_API_KEY) {
      if (apiClient.hasToolApiKey(info.toolId)) set.add(info.toolId);
    }
    return set;
  });

  const toggleSection = (key: string) => {
    setExpandedSections((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const handleDelete = (e: React.MouseEvent, id: string) => {
    e.stopPropagation();
    onDeleteChat(id);
  };

  const handleSaveToolKey = useCallback(async () => {
    if (!selectedToolForKey || !toolKeyInput.trim()) return;
    setToolKeySaving(true);
    setToolKeyError(null);
    try {
      await apiClient.saveToolApiKey(selectedToolForKey, toolKeyInput.trim());
      setConfiguredToolKeys((prev) => new Set([...prev, selectedToolForKey]));
      setToolKeySaved(selectedToolForKey);
      setToolKeyInput("");
      setTimeout(() => setToolKeySaved(null), 2000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Save failed";
      setToolKeyError(msg);
      setTimeout(() => setToolKeyError(null), 4000);
    } finally {
      setToolKeySaving(false);
    }
  }, [selectedToolForKey, toolKeyInput]);

  /** Check if a tool needs an API key and doesn't have one configured */
  const toolNeedsKey = useCallback(
    (toolName: string) => TOOLS_NEEDING_KEY.has(toolName) && !configuredToolKeys.has(toolName),
    [configuredToolKeys],
  );

  // Channel guide modal state
  const [guideChannel, setGuideChannel] = useState<string | null>(null);

  // Workspace connect state moved to Chat.tsx (action buttons row)

  // Merge API-key-requiring tools that aren't in the backend list, then sort A-Z
  const sortedTools = useMemo(() => {
    const existingIds = new Set(tools.map((t) => t.name));
    const merged = [...tools];
    for (const info of TOOLS_REQUIRING_API_KEY) {
      if (!existingIds.has(info.toolId)) {
        merged.push({
          name: info.toolId,
          description: info.displayName,
        });
      }
    }
    return merged.sort((a, b) => {
      const na = TOOL_DISPLAY_NAMES[a.name] ?? a.name;
      const nb = TOOL_DISPLAY_NAMES[b.name] ?? b.name;
      return na.localeCompare(nb);
    });
  }, [tools]);

  const sortedChannels = useMemo(
    () => [...channelsDetail].sort((a, b) => {
      const na = CHANNEL_DISPLAY_NAMES[a.name] || a.name;
      const nb = CHANNEL_DISPLAY_NAMES[b.name] || b.name;
      return na.localeCompare(nb);
    }),
    [channelsDetail],
  );

  /** Open the tool API key dropdown with a specific tool pre-selected */
  const openToolKeyFor = useCallback((toolId: string) => {
    setShowToolKeyDropdown(true);
    setSelectedToolForKey(toolId);
    setToolKeyInput("");
    setToolKeySaved(null);
    setToolKeyError(null);
    setToolListOpen(false);
    // Open the tools popup so the key dropdown is visible
    setShowToolsPopup(true);
  }, []);

  const onlineDevices = devices.filter((d) => d.is_online);

  // Sort chats by updatedAt descending and compute date groups
  const sortedChats = useMemo(() => {
    return [...chats].sort((a, b) => b.updatedAt - a.updatedAt);
  }, [chats]);

  return (
    <>
      <aside className={`sidebar ${isOpen ? "" : "closed"}`}>
        {/* Logo and New Chat */}
        <div className="sidebar-header">
          <div className="sidebar-logo">
            <div className="sidebar-logo-icon">ZC</div>
            <span className="sidebar-logo-text">{t("app_title", locale)}</span>
          </div>
          <button
            className="sidebar-new-chat-btn"
            onClick={onNewChat}
            title={t("new_chat", locale)}
          >
            +
          </button>
        </div>

        {/* Scrollable body with sections */}
        <div className="sidebar-body">

          {/* Devices section */}
          <div className="sidebar-section">
            <button
              className="sidebar-section-header"
              onClick={() => toggleSection("devices")}
            >
              <div className="sidebar-section-header-left">
                <svg className="sidebar-section-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <rect x="2" y="3" width="20" height="14" rx="2" ry="2" />
                  <line x1="8" y1="21" x2="16" y2="21" />
                  <line x1="12" y1="17" x2="12" y2="21" />
                </svg>
                <span>{t("sidebar_devices", locale)}</span>
                {onlineDevices.length > 0 && (
                  <span className="sidebar-section-badge">{onlineDevices.length}</span>
                )}
              </div>
              <svg
                className={`sidebar-section-chevron ${expandedSections.devices ? "expanded" : ""}`}
                width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
            {expandedSections.devices && (
              <div className="sidebar-section-content">
                {devices.length === 0 ? (
                  <div className="sidebar-section-empty">{t("sidebar_no_devices", locale)}</div>
                ) : (
                  devices.map((device) => (
                    <div key={device.device_id} className="sidebar-info-item">
                      <div className={`sidebar-status-dot ${device.is_online ? "online" : ""}`} />
                      <span className="sidebar-info-label">{device.device_name}</span>
                      {device.platform && (
                        <span className="sidebar-info-meta">{device.platform}</span>
                      )}
                    </div>
                  ))
                )}
              </div>
            )}
          </div>

          {/* Models section */}
          <div className="sidebar-section">
            <button
              className="sidebar-section-header"
              onClick={() => toggleSection("models")}
            >
              <div className="sidebar-section-header-left">
                <svg className="sidebar-section-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <circle cx="12" cy="12" r="3" />
                  <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                </svg>
                <span>{locale === "ko" ? "모델" : "Models"}</span>
              </div>
              <svg
                className={`sidebar-section-chevron ${expandedSections.models ? "expanded" : ""}`}
                width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
            {expandedSections.models && (
              <div className="sidebar-section-content">
                <div className="sidebar-model-list">
                  {visibleModelGroups.map((group) => (
                    <div key={group.provider}>
                      <div className="sidebar-model-provider">
                        {group.provider}
                      </div>
                      {group.models.map((model) => (
                        <button
                          key={model.id}
                          className={`sidebar-model-item ${selectedModel === model.id ? "active" : ""}`}
                          onClick={() => handleSelectModel(group.provider, model.id)}
                        >
                          <span>{model.label}</span>
                          <span className={`sidebar-model-tier ${model.tier.toLowerCase()}`}>
                            {model.tier}
                          </span>
                        </button>
                      ))}
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>

          {/* Channels button (opens popup) */}
          <div className="sidebar-section">
            <button
              className="sidebar-section-header"
              onClick={() => setShowChannelsPopup(true)}
            >
              <div className="sidebar-section-header-left">
                <svg className="sidebar-section-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
                </svg>
                <span>{t("sidebar_channels", locale)}</span>
                {channelsDetail.length > 0 && (
                  <span className="sidebar-section-badge">
                    {channelsDetail.filter((c) => c.enabled).length}/{channelsDetail.length}
                  </span>
                )}
              </div>
            </button>
          </div>

          {/* Tools button (opens popup) */}
          <div className="sidebar-section">
            <button
              className="sidebar-section-header"
              onClick={() => setShowToolsPopup(true)}
            >
              <div className="sidebar-section-header-left">
                <svg className="sidebar-section-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
                </svg>
                <span>{t("sidebar_tools", locale)}</span>
                {tools.length > 0 && (
                  <span className="sidebar-section-badge">{tools.length}</span>
                )}
              </div>
            </button>
          </div>

          {/* Interpreter nav item */}
          <div className="sidebar-section">
            <div
              className={`sidebar-chat-item ${currentPage === "interpreter" ? "active" : ""}`}
              onClick={onOpenInterpreter}
              style={{ margin: "4px 8px" }}
            >
              <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                  <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                  <line x1="12" y1="19" x2="12" y2="23" />
                  <line x1="8" y1="23" x2="16" y2="23" />
                </svg>
                <span className="sidebar-chat-title">{t("interpreter", locale)}</span>
              </span>
            </div>
          </div>

          {/* Document Editor nav item */}
          <div className="sidebar-section">
            <div
              className={`sidebar-chat-item ${currentPage === "document" ? "active" : ""}`}
              onClick={onOpenDocument}
              style={{ margin: "4px 8px" }}
            >
              <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                  <polyline points="14 2 14 8 20 8" />
                  <line x1="16" y1="13" x2="8" y2="13" />
                  <line x1="16" y1="17" x2="8" y2="17" />
                  <polyline points="10 9 9 9 8 9" />
                </svg>
                <span className="sidebar-chat-title">
                  {locale === "ko" ? "문서 편집기" : "Document Editor"}
                </span>
              </span>
            </div>
          </div>

          {/* Chats section */}
          <div className="sidebar-section sidebar-section-chats">
            <button
              className="sidebar-section-header"
              onClick={() => toggleSection("chats")}
            >
              <div className="sidebar-section-header-left">
                <svg className="sidebar-section-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z" />
                </svg>
                <span>{t("sidebar_chats", locale)}</span>
                {chats.length > 0 && (
                  <span className="sidebar-section-badge">{chats.length}</span>
                )}
              </div>
              <svg
                className={`sidebar-section-chevron ${expandedSections.chats ? "expanded" : ""}`}
                width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
            {expandedSections.chats && (
              <div className="sidebar-section-content sidebar-chats-list">
                {sortedChats.length === 0 ? (
                  <div className="sidebar-section-empty">{t("no_chats", locale)}</div>
                ) : (
                  sortedChats.map((chat, idx) => {
                    const chatDate = dateKey(chat.updatedAt);
                    const prevDate = idx > 0 ? dateKey(sortedChats[idx - 1].updatedAt) : null;
                    const showDateSep = idx === 0 || chatDate !== prevDate;
                    const displayTitle = getChatDisplayTitle(chat);
                    const msgCount = chat.messages.length;

                    return (
                      <div key={chat.id}>
                        {showDateSep && (
                          <div className="sidebar-date-separator">
                            <span>{formatDateLabel(chat.updatedAt, locale)}</span>
                          </div>
                        )}
                        <div
                          className={`sidebar-chat-item ${chat.id === activeChatId ? "active" : ""}`}
                          onClick={() => onSelectChat(chat.id)}
                        >
                          <div className="sidebar-chat-info">
                            <span className="sidebar-chat-title">{displayTitle}</span>
                            <div className="sidebar-chat-meta">
                              {msgCount > 0 && (
                                <span className="sidebar-chat-count">
                                  {msgCount} {locale === "ko" ? "메시지" : "msg"}
                                </span>
                              )}
                              <span className="sidebar-chat-time">
                                {formatTime(chat.updatedAt)}
                              </span>
                            </div>
                          </div>
                          <button
                            className="sidebar-chat-delete"
                            onClick={(e) => handleDelete(e, chat.id)}
                            title={t("delete_chat", locale)}
                          >
                            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                              <polyline points="3 6 5 6 21 6" />
                              <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                            </svg>
                          </button>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="sidebar-footer">
          <button
            className={`sidebar-footer-btn ${currentPage === "settings" ? "active" : ""}`}
            onClick={onOpenSettings}
          >
            <span className="icon">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="12" cy="12" r="3" />
                <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
              </svg>
            </span>
            {t("settings", locale)}
          </button>
          <button
            className="sidebar-footer-btn sidebar-logout-btn"
            onClick={onLogout}
          >
            <span className="icon">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                <polyline points="16 17 21 12 16 7" />
                <line x1="21" y1="12" x2="9" y2="12" />
              </svg>
            </span>
            {t("logout", locale)}
          </button>
        </div>
      </aside>

      {/* Mobile overlay */}
      {isOpen && <div className="sidebar-overlay" onClick={onToggle} />}

      {/* Channel setup guide modal */}
      {guideChannel && (
        <ChannelGuide
          channelName={guideChannel}
          locale={locale}
          onClose={() => setGuideChannel(null)}
        />
      )}

      {/* Channels popup */}
      {showChannelsPopup && (
        <div className="sidebar-popup-overlay" onClick={() => setShowChannelsPopup(false)}>
          <div className="sidebar-popup" onClick={(e) => e.stopPropagation()}>
            <div className="sidebar-popup-header">
              <h3>{t("sidebar_channels", locale)}</h3>
              <button onClick={() => setShowChannelsPopup(false)}>{"\u2715"}</button>
            </div>
            <div className="sidebar-popup-body">
              {sortedChannels.length === 0 ? (
                <div className="sidebar-section-empty">{t("sidebar_no_channels", locale)}</div>
              ) : (
                sortedChannels.map((ch) => (
                  <div key={ch.name} className="sidebar-info-item sidebar-channel-item">
                    <div className={`sidebar-status-dot ${ch.enabled ? "online" : ""}`} />
                    <span className="sidebar-info-label">
                      {CHANNEL_DISPLAY_NAMES[ch.name] || ch.name}
                    </span>
                    <button
                      className="sidebar-channel-guide-btn"
                      title={locale === "ko" ? "채널추가 안내" : "Setup Guide"}
                      onClick={(e) => {
                        e.stopPropagation();
                        setGuideChannel(ch.name);
                      }}
                    >
                      {locale === "ko" ? "채널추가 안내" : "Guide"}
                    </button>
                    <span className={`sidebar-channel-status ${ch.enabled ? "enabled" : "disabled"}`}>
                      {ch.enabled
                        ? (locale === "ko" ? "활성" : "ON")
                        : (locale === "ko" ? "비활성" : "OFF")}
                    </span>
                  </div>
                ))
              )}
              <div className="sidebar-channel-hint">
                {locale === "ko"
                  ? "채널 설정은 설정 페이지에서 변경할 수 있습니다"
                  : "Configure channels in Settings"}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Tools popup */}
      {showToolsPopup && (
        <div className="sidebar-popup-overlay" onClick={() => setShowToolsPopup(false)}>
          <div className="sidebar-popup" onClick={(e) => e.stopPropagation()}>
            <div className="sidebar-popup-header">
              <h3>{t("sidebar_tools", locale)}</h3>
              <button onClick={() => setShowToolsPopup(false)}>{"\u2715"}</button>
            </div>
            <div className="sidebar-popup-body">
              {/* Tool API Key Settings */}
              <div className="sidebar-tool-key-dropdown" style={{ marginBottom: 12 }}>
                <div className="sidebar-tool-key-dropdown-title">
                  {locale === "ko" ? "도구 API Key 설정" : "Tool API Key Settings"}
                </div>
                <div className="tool-key-selector">
                  <button
                    className="tool-key-selector-trigger"
                    onClick={() => setToolListOpen((prev) => !prev)}
                  >
                    <span className="tool-key-selector-label">
                      {selectedToolForKey
                        ? TOOLS_REQUIRING_API_KEY.find((ti) => ti.toolId === selectedToolForKey)?.displayName
                        : locale === "ko" ? "-- 도구 선택 --" : "-- Select Tool --"}
                    </span>
                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                      <polyline points="6 9 12 15 18 9" />
                    </svg>
                  </button>
                  {toolListOpen && (
                    <ul className="tool-key-selector-list">
                      {TOOLS_REQUIRING_API_KEY.map((info) => (
                        <li
                          key={info.toolId}
                          className={`tool-key-selector-item ${selectedToolForKey === info.toolId ? "selected" : ""}`}
                          onClick={() => {
                            setSelectedToolForKey(info.toolId);
                            setToolKeyInput("");
                            setToolKeySaved(null);
                            setToolKeyError(null);
                            setToolListOpen(false);
                          }}
                        >
                          <span className="tool-key-selector-item-name">{info.displayName}</span>
                          {configuredToolKeys.has(info.toolId) && (
                            <span className="tool-key-selector-item-check">{"\u2713"}</span>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
                {selectedToolForKey && (
                  <div className="sidebar-tool-key-input-row">
                    <input
                      type="password"
                      className="sidebar-tool-key-input"
                      placeholder={
                        TOOLS_REQUIRING_API_KEY.find((ti) => ti.toolId === selectedToolForKey)?.placeholder ?? "API Key"
                      }
                      value={toolKeyInput}
                      onChange={(e) => setToolKeyInput(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") handleSaveToolKey();
                      }}
                    />
                    <button
                      className="sidebar-tool-key-save-btn"
                      disabled={toolKeySaving || !toolKeyInput.trim()}
                      onClick={handleSaveToolKey}
                    >
                      {toolKeySaving
                        ? "..."
                        : toolKeySaved === selectedToolForKey
                          ? "\u2713"
                          : locale === "ko"
                            ? "저장"
                            : "Save"}
                    </button>
                  </div>
                )}
                {toolKeySaved && (
                  <div className="sidebar-tool-key-saved-msg">
                    {locale === "ko" ? "API Key가 저장되었습니다" : "API Key saved"}
                  </div>
                )}
                {toolKeyError && (
                  <div className="sidebar-tool-key-error-msg">
                    {locale === "ko" ? `저장 실패: ${toolKeyError}` : `Error: ${toolKeyError}`}
                  </div>
                )}
              </div>

              {/* Tool list */}
              {sortedTools.length === 0 ? (
                <div className="sidebar-section-empty">{t("sidebar_no_tools", locale)}</div>
              ) : (
                sortedTools.map((tool) => (
                  <div key={tool.name} className="sidebar-info-item" title={tool.description}>
                    <span className="sidebar-device-status active" />
                    <span className="sidebar-info-label">
                      {TOOL_DISPLAY_NAMES[tool.name] ?? tool.name}
                    </span>
                    {toolNeedsKey(tool.name) && (
                      <button
                        className="sidebar-tool-needs-key-btn"
                        onClick={(e) => {
                          e.stopPropagation();
                          openToolKeyFor(tool.name);
                        }}
                      >
                        {locale === "ko" ? "API Key 입력필요" : "API Key required"}
                      </button>
                    )}
                  </div>
                ))
              )}
            </div>
          </div>
        </div>
      )}
    </>
  );
}
