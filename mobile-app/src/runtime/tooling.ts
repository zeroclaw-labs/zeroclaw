import { executeAndroidToolAction } from "../native/androidAgentBridge";
import type { IntegrationsConfig, MobileToolCapability, SecurityConfig } from "../state/mobileclaw";
import type { ToolCallDirective, ToolExecutionEvent } from "./types";

const HIGH_RISK_TOOLS = new Set<string>([
  "android_device.calls.start",
  "android_device.calls.incoming_hook",
  "android_device.sms.send",
  "android_device.sms.incoming_hook",
  "android_device.userdata.call_log",
  "android_device.userdata.sms_inbox",
  "android_device.contacts.read",
  "android_device.calendar.read_write",
  "android_device.notifications.read",
  "android_device.ui.tap",
  "android_device.ui.swipe",
  "android_device.ui.click_text",
  "android_device.ui.back",
  "android_device.ui.home",
  "android_device.ui.recents",
  "android_device.browser.open_session",
  "android_device.browser.navigate",
]);

const TOOL_ACTION_MAP: Record<string, string> = {
  "android_device.open_app": "launch_app",
  "android_device.list_apps": "list_apps",
  "android_device.open_url": "open_url",
  "android_device.open_settings": "open_settings",
  "android_device.notifications.read": "read_notifications",
  "android_device.notifications.post": "post_notification",
  "android_device.notifications.hook": "hook_notifications",
  "android_device.location.read": "get_location",
  "android_device.location.geofence": "manage_geofence",
  "android_device.sensor.accelerometer": "sensor_read",
  "android_device.sensor.gyroscope": "sensor_read",
  "android_device.sensor.magnetometer": "sensor_read",
  "android_device.sensor.battery": "get_battery",
  "android_device.sensor.network": "get_network",
  "android_device.camera.capture": "take_photo",
  "android_device.camera.scan_qr": "scan_qr",
  "android_device.microphone.record": "record_audio",
  "android_device.storage.files": "manage_files",
  "android_device.storage.files_full_access": "request_all_files_access",
  "android_device.storage.documents": "pick_document",
  "android_device.contacts.read": "read_contacts",
  "android_device.calendar.read_write": "read_calendar",
  "android_device.calls.start": "place_call",
  "android_device.calls.incoming_hook": "hook_incoming_call",
  "android_device.sms.send": "send_sms",
  "android_device.sms.incoming_hook": "hook_incoming_sms",
  "android_device.userdata.clipboard": "set_clipboard",
  "android_device.userdata.photos": "read_photos",
  "android_device.userdata.call_log": "read_call_log",
  "android_device.userdata.sms_inbox": "read_sms",
  "android_device.bluetooth.scan": "scan_bluetooth",
  "android_device.bluetooth.connect": "connect_bluetooth",
  "android_device.nfc.read": "read_nfc",
  "android_device.nfc.write": "write_nfc",
  "android_device.ui.automation_enable": "ui_automation_enable",
  "android_device.ui.automation_status": "ui_automation_status",
  "android_device.ui.tap": "ui_automation_tap",
  "android_device.ui.swipe": "ui_automation_swipe",
  "android_device.ui.click_text": "ui_automation_click_text",
  "android_device.ui.back": "ui_automation_back",
  "android_device.ui.home": "ui_automation_home",
  "android_device.ui.recents": "ui_automation_recents",
  "android_device.browser.open_session": "browser_open_session",
  "android_device.browser.navigate": "browser_navigate",
  "android_device.browser.state": "browser_state",
  "android_device.browser.fetch_page": "browser_fetch_page",
  hardware_board_info: "hardware_board_info",
  hardware_memory_map: "hardware_memory_map",
  hardware_memory_read: "hardware_memory_read",
};

export function parseToolDirective(replyText: string): ToolCallDirective | null {
  const raw = String(replyText || "").trim();
  if (!raw) return null;

  const cleaned = raw
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/gi, "")
    .replace(/<system-reminder>[\s\S]*$/gi, "")
    .trim();

  const invokeMatch = cleaned.match(/<invoke\s+name\s*=\s*"([^"]+)"\s*>/i);
  if (invokeMatch?.[1]) {
    return {
      tool: invokeMatch[1].trim(),
      arguments: {},
    };
  }

  const taggedToolCallMatch = cleaned.match(/\[TOOL_CALL\]\s*([\s\S]*?)\s*\[\/TOOL_CALL\]/i);
  if (taggedToolCallMatch?.[1]) {
    const taggedBody = taggedToolCallMatch[1].trim();
    try {
      const taggedParsed = JSON.parse(taggedBody) as { tool?: string; arguments?: Record<string, unknown> };
      if (typeof taggedParsed.tool === "string" && taggedParsed.tool.trim()) {
        return {
          tool: taggedParsed.tool.trim(),
          arguments:
            taggedParsed.arguments && typeof taggedParsed.arguments === "object" && !Array.isArray(taggedParsed.arguments)
              ? taggedParsed.arguments
              : {},
        };
      }
    } catch {
      // fall through to other parsers
    }
  }

  const parseCandidate = (candidate: string): ToolCallDirective | null => {
    let parsed: any;
    try {
      parsed = JSON.parse(candidate);
    } catch {
      return null;
    }

    const type = String(parsed?.type || "").trim().toLowerCase();
    const tool = String(parsed?.tool || parsed?.tool_id || "").trim();
    const args = parsed?.arguments;
    const looksLikeToolCallWithoutType = !type && !!tool;
    if ((!looksLikeToolCallWithoutType && type !== "tool_call") || !tool || typeof args !== "object" || args === null || Array.isArray(args)) {
      return null;
    }

    return {
      tool,
      arguments: args as Record<string, unknown>,
    };
  };

  const direct = parseCandidate(cleaned);
  if (direct) return direct;

  const fencedBlocks = [...cleaned.matchAll(/```json\s*([\s\S]*?)```/gi)].map((match) => match[1]?.trim()).filter(Boolean) as string[];
  for (const block of fencedBlocks) {
    const parsed = parseCandidate(block);
    if (parsed) return parsed;
  }

  const candidates: string[] = [];
  let depth = 0;
  let start = -1;
  let inString = false;
  let escaped = false;
  for (let i = 0; i < cleaned.length; i += 1) {
    const ch = cleaned[i];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }

    if (ch === '"') {
      inString = true;
      continue;
    }
    if (ch === "{") {
      if (depth === 0) start = i;
      depth += 1;
      continue;
    }
    if (ch === "}") {
      if (depth > 0) depth -= 1;
      if (depth === 0 && start >= 0) {
        candidates.push(cleaned.slice(start, i + 1));
        start = -1;
      }
    }
  }

  for (const candidate of candidates) {
    const parsed = parseCandidate(candidate);
    if (parsed) return parsed;
  }

  return null;
}

function defaultPayloadForTool(tool: string, args: Record<string, unknown>): Record<string, unknown> {
  if (tool.startsWith("android_device.sensor.")) {
    const sensorFromId = tool.split(".").pop() || "accelerometer";
    return { sensor: args.sensor || sensorFromId };
  }

  if (tool === "android_device.storage.files") {
    const rawPath = String(args.path || "").trim();
    const normalizedPath = rawPath
      .replace(/^\/sdcard\/?/i, "")
      .replace(/^\/storage\/emulated\/0\/?/i, "");

    return {
      scope: args.scope || "user",
      path: normalizedPath,
      limit: args.limit || 200,
    };
  }

  if (tool === "android_device.calls.start") {
    const to =
      (typeof args.to === "string" && args.to.trim()) ||
      (typeof args.phone === "string" && args.phone.trim()) ||
      (typeof args.number === "string" && args.number.trim()) ||
      "";
    return { to };
  }

  if (tool === "android_device.sms.send") {
    const to =
      (typeof args.to === "string" && args.to.trim()) ||
      (typeof args.phone === "string" && args.phone.trim()) ||
      (typeof args.number === "string" && args.number.trim()) ||
      "";
    const body =
      (typeof args.body === "string" && args.body.trim()) ||
      (typeof args.text === "string" && args.text.trim()) ||
      (typeof args.message === "string" && args.message.trim()) ||
      (typeof args.content === "string" && args.content.trim()) ||
      "";
    return { to, body };
  }

  if (tool === "android_device.camera.capture") {
    const lens =
      (typeof args.lens === "string" && args.lens.trim()) ||
      (typeof args.camera === "string" && args.camera.trim()) ||
      (typeof args.facing === "string" && args.facing.trim()) ||
      "rear";
    return { lens };
  }

  return args;
}

function extractMessageText(args: Record<string, unknown>): string {
  const text =
    (typeof args.text === "string" && args.text.trim()) ||
    (typeof args.message === "string" && args.message.trim()) ||
    (typeof args.body === "string" && args.body.trim()) ||
    "";
  return text;
}

async function executeTelegramSendMessage(
  directive: ToolCallDirective,
  integrations: IntegrationsConfig,
): Promise<ToolExecutionEvent> {
  if (!integrations.telegramEnabled) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: "Telegram integration is disabled in Integrations screen.",
    };
  }

  const botToken = integrations.telegramBotToken.trim();
  let chatId = integrations.telegramChatId.trim();
  if (!botToken) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: "Telegram bot token is missing. Open Integrations and add bot token first.",
    };
  }

  if (!chatId) {
    try {
      const updatesResponse = await fetch(`https://api.telegram.org/bot${botToken}/getUpdates`);
      const updatesPayload = (await updatesResponse.json()) as {
        ok?: boolean;
        result?: Array<{ message?: { chat?: { id?: number | string } } }>;
      };
      const updates = Array.isArray(updatesPayload.result) ? updatesPayload.result : [];
      const latest = [...updates]
        .reverse()
        .map((update) => update.message?.chat?.id)
        .find((id) => id !== undefined && id !== null);
      if (latest !== undefined && latest !== null) {
        chatId = String(latest);
      }
    } catch {
      // Keep fallback behavior below.
    }
  }

  if (!chatId) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: "Telegram chat is not detected yet. Send any message to your bot, then retry.",
    };
  }

  const text = extractMessageText(directive.arguments);
  if (!text) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: "Telegram message text is empty. Provide the message text.",
    };
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 8000);
  try {
    const response = await fetch(`https://api.telegram.org/bot${botToken}/sendMessage`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        chat_id: chatId,
        text,
      }),
      signal: controller.signal,
    });

    const payload = (await response.json()) as {
      ok?: boolean;
      description?: string;
      result?: { message_id?: number };
    };

    if (!response.ok || !payload.ok) {
      const detail = payload.description ? `: ${payload.description}` : "";
      return {
        tool: directive.tool,
        status: "failed",
        detail: `Telegram API request failed${detail}`,
      };
    }

    return {
      tool: directive.tool,
      status: "executed",
      detail: "Telegram message sent successfully.",
      output: payload.result ?? null,
    };
  } catch (error) {
    return {
      tool: directive.tool,
      status: "failed",
      detail: error instanceof Error ? error.message : "Telegram send failed.",
    };
  } finally {
    clearTimeout(timer);
  }
}

function integrationBlockedEvent(tool: string, enabled: boolean, configured: boolean, detail: string): ToolExecutionEvent {
  if (!enabled) {
    return {
      tool,
      status: "blocked",
      detail: `${detail} integration is disabled in Integrations screen.`,
    };
  }
  if (!configured) {
    return {
      tool,
      status: "blocked",
      detail: `${detail} integration is enabled but incomplete. Finish setup in Integrations screen first.`,
    };
  }
  return {
    tool,
    status: "failed",
    detail: `${detail} execution from mobile chat is not implemented yet. Keep ZeroClaw backend runtime healthy and use ${detail} channel inbound for now.`,
  };
}

export async function executeToolDirective(
  directive: ToolCallDirective,
  config: {
    tools: MobileToolCapability[];
    integrations: IntegrationsConfig;
    security: SecurityConfig;
  },
): Promise<ToolExecutionEvent> {
  if (directive.tool.startsWith("integration.")) {
    if (directive.tool === "integration.telegram.send_message") {
      return executeTelegramSendMessage(directive, config.integrations);
    }
    if (directive.tool === "integration.discord.send_message") {
      return integrationBlockedEvent(
        directive.tool,
        config.integrations.discordEnabled,
        Boolean(config.integrations.discordBotToken.trim()),
        "Discord",
      );
    }
    if (directive.tool === "integration.slack.send_message") {
      return integrationBlockedEvent(
        directive.tool,
        config.integrations.slackEnabled,
        Boolean(config.integrations.slackBotToken.trim()),
        "Slack",
      );
    }
    if (directive.tool === "integration.whatsapp.send_message") {
      return integrationBlockedEvent(
        directive.tool,
        config.integrations.whatsappEnabled,
        Boolean(config.integrations.whatsappAccessToken.trim()),
        "WhatsApp",
      );
    }
    if (directive.tool === "integration.composio.invoke_action") {
      return integrationBlockedEvent(
        directive.tool,
        config.integrations.composioEnabled,
        Boolean(config.integrations.composioApiKey.trim()),
        "Composio",
      );
    }
    return {
      tool: directive.tool,
      status: "failed",
      detail: `Unsupported integration tool: ${directive.tool}`,
    };
  }

  const enabledTool = config.tools.find((tool) => tool.id === directive.tool);
  if (!enabledTool || !enabledTool.enabled) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: `Tool is disabled by policy: ${directive.tool}`,
    };
  }

  if (HIGH_RISK_TOOLS.has(directive.tool) && !config.security.highRiskActions) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: "High-risk actions are disabled in Security settings.",
    };
  }

  if (HIGH_RISK_TOOLS.has(directive.tool) && config.security.requireApproval) {
    return {
      tool: directive.tool,
      status: "blocked",
      detail: "Action requires explicit approval and was blocked.",
    };
  }

  const action = TOOL_ACTION_MAP[directive.tool];
  if (!action) {
    return {
      tool: directive.tool,
      status: "failed",
      detail: "Tool is not yet mapped to Android native execution.",
    };
  }

  try {
    const payload = defaultPayloadForTool(directive.tool, directive.arguments);
    if (
      directive.tool === "android_device.calls.start" ||
      directive.tool === "android_device.sms.send" ||
      directive.tool === "android_device.camera.capture"
    ) {
      payload.direct = config.security.directExecution;
    }
    const output = await executeAndroidToolAction(action, payload);
    return {
      tool: directive.tool,
      status: "executed",
      detail: "Tool executed successfully.",
      output,
    };
  } catch (error) {
    return {
      tool: directive.tool,
      status: "failed",
      detail: error instanceof Error ? error.message : "Tool execution failed.",
    };
  }
}
