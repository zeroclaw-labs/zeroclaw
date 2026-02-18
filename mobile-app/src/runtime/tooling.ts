import { executeAndroidToolAction } from "../native/androidAgentBridge";
import type { MobileToolCapability, SecurityConfig } from "../state/mobileclaw";
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
  hardware_board_info: "hardware_board_info",
  hardware_memory_map: "hardware_memory_map",
  hardware_memory_read: "hardware_memory_read",
};

export function parseToolDirective(replyText: string): ToolCallDirective | null {
  const raw = String(replyText || "").trim();
  if (!raw) return null;

  const fencedMatch = raw.match(/```json\s*([\s\S]*?)```/i);
  const candidateJson = fencedMatch?.[1]?.trim() || raw;

  let parsed: any;
  try {
    parsed = JSON.parse(candidateJson);
  } catch {
    return null;
  }

  const type = String(parsed?.type || "").trim().toLowerCase();
  const tool = String(parsed?.tool || parsed?.tool_id || "").trim();
  const args = parsed?.arguments;

  if (type !== "tool_call" || !tool || typeof args !== "object" || args === null || Array.isArray(args)) {
    return null;
  }

  return {
    tool,
    arguments: args as Record<string, unknown>,
  };
}

function defaultPayloadForTool(tool: string, args: Record<string, unknown>): Record<string, unknown> {
  if (tool.startsWith("android_device.sensor.")) {
    const sensorFromId = tool.split(".").pop() || "accelerometer";
    return { sensor: args.sensor || sensorFromId };
  }

  if (tool === "android_device.storage.files") {
    return {
      scope: args.scope || "user",
      path: args.path || "",
      limit: args.limit || 200,
    };
  }

  return args;
}

export async function executeToolDirective(
  directive: ToolCallDirective,
  config: {
    tools: MobileToolCapability[];
    security: SecurityConfig;
  },
): Promise<ToolExecutionEvent> {
  if (directive.tool.startsWith("integration.")) {
    return {
      tool: directive.tool,
      status: "failed",
      detail: "Integration tools are available in ZeroClaw backend runtime and are not executed by mobile native bridge.",
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
