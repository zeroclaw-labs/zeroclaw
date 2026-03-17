import { useState, useMemo } from "react";
import { t, type Locale } from "../lib/i18n";
import type { ChatSession } from "../lib/storage";
import { deriveChatTitle } from "../lib/storage";
import type { DeviceInfo, ToolInfo, ChannelInfo } from "../lib/api";

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
  channels,
  channelsDetail,
  tools,
  onNewChat,
  onSelectChat,
  onDeleteChat,
  onOpenSettings,
  onOpenInterpreter,
  onOpenDocument,
  onToggle,
}: SidebarProps) {
  const [expandedSections, setExpandedSections] = useState<Record<string, boolean>>({
    devices: true,
    channels: true,
    tools: true,
    chats: true,
  });

  const toggleSection = (key: string) => {
    setExpandedSections((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const handleDelete = (e: React.MouseEvent, id: string) => {
    e.stopPropagation();
    onDeleteChat(id);
  };

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

          {/* Channels section */}
          <div className="sidebar-section">
            <button
              className="sidebar-section-header"
              onClick={() => toggleSection("channels")}
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
              <svg
                className={`sidebar-section-chevron ${expandedSections.channels ? "expanded" : ""}`}
                width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
            {expandedSections.channels && (
              <div className="sidebar-section-content">
                {channelsDetail.length === 0 ? (
                  <div className="sidebar-section-empty">{t("sidebar_no_channels", locale)}</div>
                ) : (
                  channelsDetail.map((ch) => (
                    <div key={ch.name} className="sidebar-info-item sidebar-channel-item">
                      <div className={`sidebar-status-dot ${ch.enabled ? "online" : ""}`} />
                      <span className="sidebar-info-label">
                        {CHANNEL_DISPLAY_NAMES[ch.name] || ch.name}
                      </span>
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
            )}
          </div>

          {/* Tools section */}
          <div className="sidebar-section">
            <button
              className="sidebar-section-header"
              onClick={() => toggleSection("tools")}
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
              <svg
                className={`sidebar-section-chevron ${expandedSections.tools ? "expanded" : ""}`}
                width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
            {expandedSections.tools && (
              <div className="sidebar-section-content">
                {tools.length === 0 ? (
                  <div className="sidebar-section-empty">{t("sidebar_no_tools", locale)}</div>
                ) : (
                  tools.map((tool) => (
                    <div key={tool.name} className="sidebar-info-item" title={tool.description}>
                      <span className="sidebar-device-status active" />
                      <span className="sidebar-info-label">
                        {TOOL_DISPLAY_NAMES[tool.name] ?? tool.name}
                      </span>
                    </div>
                  ))
                )}
              </div>
            )}
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
        </div>
      </aside>

      {/* Mobile overlay */}
      {isOpen && <div className="sidebar-overlay" onClick={onToggle} />}
    </>
  );
}
