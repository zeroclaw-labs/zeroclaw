import AsyncStorage from "@react-native-async-storage/async-storage";

export type ProviderId = "ollama" | "openrouter" | "openai" | "anthropic" | "gemini" | "copilot";
export type AuthMode = "api_key" | "oauth_token";

export type AgentRuntimeConfig = {
  provider: ProviderId;
  model: string;
  apiUrl: string;
  apiKey: string;
  authMode: AuthMode;
  oauthAccessToken: string;
  oauthRefreshToken: string;
  oauthExpiresAtMs: number;
  accountId: string;
  enterpriseUrl: string;
  temperature: number;
  deepgramApiKey: string;
};

export type IntegrationsConfig = {
  telegramEnabled: boolean;
  telegramBotToken: string;
  telegramChatId: string;
  discordEnabled: boolean;
  discordBotToken: string;
  slackEnabled: boolean;
  slackBotToken: string;
  whatsappEnabled: boolean;
  whatsappAccessToken: string;
  composioEnabled: boolean;
  composioApiKey: string;
};

export type SecurityConfig = {
  requireApproval: boolean;
  highRiskActions: boolean;
  incomingCallHooks: boolean;
  includeCallerNumber: boolean;
};

export type MobileToolCapability = {
  id: string;
  title: string;
  detail: string;
  enabled: boolean;
};

const AGENT_KEY = "mobileclaw:agent-config:v1";
const INTEGRATIONS_KEY = "mobileclaw:integrations-config:v2";
const SECURITY_KEY = "mobileclaw:security-config:v1";
const DEVICE_TOOLS_KEY = "mobileclaw:device-tools:v2";

export const DEFAULT_AGENT_CONFIG: AgentRuntimeConfig = {
  provider: "openrouter",
  model: "minimax/minimax-m2.5",
  apiUrl: "https://openrouter.ai/api/v1",
  apiKey: "",
  authMode: "api_key",
  oauthAccessToken: "",
  oauthRefreshToken: "",
  oauthExpiresAtMs: 0,
  accountId: "",
  enterpriseUrl: "",
  temperature: 0.1,
  deepgramApiKey: "",
};

export const DEFAULT_INTEGRATIONS: IntegrationsConfig = {
  telegramEnabled: false,
  telegramBotToken: "",
  telegramChatId: "",
  discordEnabled: false,
  discordBotToken: "",
  slackEnabled: false,
  slackBotToken: "",
  whatsappEnabled: false,
  whatsappAccessToken: "",
  composioEnabled: false,
  composioApiKey: "",
};

export const DEFAULT_SECURITY: SecurityConfig = {
  requireApproval: false,
  highRiskActions: true,
  incomingCallHooks: true,
  includeCallerNumber: true,
};

export const DEFAULT_DEVICE_TOOLS: MobileToolCapability[] = [
  { id: "android_device.storage.files", title: "File Storage", detail: "Read/write local files", enabled: false },
  { id: "android_device.storage.files_full_access", title: "All Files Access", detail: "Request/manage full storage access", enabled: false },
  { id: "android_device.storage.documents", title: "Document Picker", detail: "Pick files with scoped access", enabled: false },
  { id: "android_device.open_app", title: "Launch App", detail: "Open installed package by id", enabled: false },
  { id: "android_device.list_apps", title: "List Installed Apps", detail: "Enumerate launchable package ids", enabled: false },
  { id: "android_device.open_url", title: "Open URL", detail: "Open https links via Android intent", enabled: false },
  { id: "android_device.open_settings", title: "Open Settings", detail: "Open Android system settings", enabled: false },
  { id: "android_device.notifications.read", title: "Read Notifications", detail: "Inspect notification feed", enabled: false },
  { id: "android_device.notifications.post", title: "Post Notification", detail: "Create local system notifications", enabled: false },
  { id: "android_device.notifications.hook", title: "Notification Hook", detail: "Receive notification listener events", enabled: false },
  { id: "android_device.location.read", title: "GPS Location", detail: "Read current location", enabled: false },
  { id: "android_device.location.geofence", title: "Geofencing", detail: "Background region enter/exit hooks", enabled: false },
  { id: "android_device.sensor.accelerometer", title: "Accelerometer", detail: "Read movement/tilt", enabled: false },
  { id: "android_device.sensor.gyroscope", title: "Gyroscope", detail: "Read rotation deltas", enabled: false },
  { id: "android_device.sensor.magnetometer", title: "Magnetometer", detail: "Read magnetic field/compass", enabled: false },
  { id: "android_device.sensor.battery", title: "Battery State", detail: "Read battery level and charging", enabled: false },
  { id: "android_device.sensor.network", title: "Network State", detail: "Read connectivity and transport", enabled: false },
  { id: "android_device.camera.capture", title: "Camera Capture", detail: "Capture photos/video", enabled: false },
  { id: "android_device.camera.scan_qr", title: "QR/Barcode Scan", detail: "Scan machine-readable codes", enabled: false },
  { id: "android_device.microphone.record", title: "Microphone", detail: "Capture audio input", enabled: false },
  { id: "android_device.contacts.read", title: "Contacts", detail: "Read contact cards", enabled: false },
  { id: "android_device.calendar.read_write", title: "Calendar", detail: "Read/create events", enabled: false },
  { id: "android_device.calls.start", title: "Calls", detail: "Start phone calls", enabled: false },
  { id: "android_device.calls.incoming_hook", title: "Incoming Call Hook", detail: "Notify agent on incoming calls", enabled: false },
  { id: "android_device.sms.send", title: "SMS", detail: "Send text messages", enabled: false },
  { id: "android_device.sms.incoming_hook", title: "Incoming SMS Hook", detail: "Notify agent on received SMS", enabled: false },
  { id: "android_device.userdata.clipboard", title: "Clipboard", detail: "Read/write clipboard", enabled: false },
  { id: "android_device.userdata.photos", title: "Photo Library", detail: "Read media files", enabled: false },
  { id: "android_device.userdata.call_log", title: "Call Log", detail: "Read recent calls", enabled: false },
  { id: "android_device.userdata.sms_inbox", title: "SMS Inbox", detail: "Read incoming SMS", enabled: false },
  { id: "android_device.bluetooth.scan", title: "Bluetooth LE Scan", detail: "Scan nearby BLE devices", enabled: false },
  { id: "android_device.bluetooth.connect", title: "Bluetooth Connect", detail: "Connect to known BLE device", enabled: false },
  { id: "android_device.nfc.read", title: "NFC Read", detail: "Read NFC tags with tap", enabled: false },
  { id: "android_device.nfc.write", title: "NFC Write", detail: "Write NFC tags", enabled: false },
  { id: "android_device.ui.automation_enable", title: "UI Automation Enable", detail: "Open accessibility settings for automation", enabled: false },
  { id: "android_device.ui.automation_status", title: "UI Automation Status", detail: "Check accessibility automation status", enabled: false },
  { id: "android_device.ui.tap", title: "UI Tap", detail: "Tap screen coordinates via accessibility", enabled: false },
  { id: "android_device.ui.swipe", title: "UI Swipe", detail: "Swipe on screen via accessibility", enabled: false },
  { id: "android_device.ui.click_text", title: "UI Click Text", detail: "Click visible text on screen", enabled: false },
  { id: "android_device.ui.back", title: "UI Back", detail: "Perform Android back action", enabled: false },
  { id: "android_device.ui.home", title: "UI Home", detail: "Go to Android home screen", enabled: false },
  { id: "android_device.ui.recents", title: "UI Recents", detail: "Open recents screen", enabled: false },
  { id: "android_device.browser.open_session", title: "Browser Open Session", detail: "Open in-app browser session", enabled: false },
  { id: "android_device.browser.navigate", title: "Browser Navigate", detail: "Navigate active in-app browser session", enabled: false },
  { id: "android_device.browser.state", title: "Browser State", detail: "Read current browser URL/title", enabled: false },
  { id: "android_device.browser.fetch_page", title: "Browser Fetch Page", detail: "Fetch page source text for browsing", enabled: false },
  { id: "hardware_board_info", title: "Hardware Board Info", detail: "Read connected board/chip info", enabled: false },
  { id: "hardware_memory_map", title: "Hardware Memory Map", detail: "Read flash/RAM ranges", enabled: false },
  { id: "hardware_memory_read", title: "Hardware Memory Read", detail: "Read memory addresses from board", enabled: false },
];

async function readJson<T>(key: string, fallback: T): Promise<T> {
  const raw = await AsyncStorage.getItem(key);
  if (!raw) return fallback;
  try {
    return { ...fallback, ...(JSON.parse(raw) as object) } as T;
  } catch {
    return fallback;
  }
}

export async function loadAgentConfig(): Promise<AgentRuntimeConfig> {
  return readJson(AGENT_KEY, DEFAULT_AGENT_CONFIG);
}

export async function saveAgentConfig(config: AgentRuntimeConfig): Promise<void> {
  await AsyncStorage.setItem(AGENT_KEY, JSON.stringify(config));
}

export async function loadIntegrationsConfig(): Promise<IntegrationsConfig> {
  return readJson(INTEGRATIONS_KEY, DEFAULT_INTEGRATIONS);
}

export async function saveIntegrationsConfig(config: IntegrationsConfig): Promise<void> {
  await AsyncStorage.setItem(INTEGRATIONS_KEY, JSON.stringify(config));
}

export async function loadSecurityConfig(): Promise<SecurityConfig> {
  return readJson(SECURITY_KEY, DEFAULT_SECURITY);
}

export async function saveSecurityConfig(config: SecurityConfig): Promise<void> {
  await AsyncStorage.setItem(SECURITY_KEY, JSON.stringify(config));
}

export async function loadDeviceToolsConfig(): Promise<MobileToolCapability[]> {
  const raw = await AsyncStorage.getItem(DEVICE_TOOLS_KEY);
  if (!raw) return DEFAULT_DEVICE_TOOLS;

  try {
    const parsed = JSON.parse(raw) as Array<Partial<MobileToolCapability>>;
    const map = new Map((Array.isArray(parsed) ? parsed : []).map((item) => [item.id, item]));
    return DEFAULT_DEVICE_TOOLS.map((item) => {
      const saved = map.get(item.id);
      return {
        ...item,
        enabled: typeof saved?.enabled === "boolean" ? saved.enabled : item.enabled,
      };
    });
  } catch {
    return DEFAULT_DEVICE_TOOLS;
  }
}

export async function saveDeviceToolsConfig(tools: MobileToolCapability[]): Promise<void> {
  await AsyncStorage.setItem(DEVICE_TOOLS_KEY, JSON.stringify(tools));
}
