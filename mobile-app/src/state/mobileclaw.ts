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
};

const AGENT_KEY = "mobileclaw:agent-config:v1";
const INTEGRATIONS_KEY = "mobileclaw:integrations-config:v1";
const SECURITY_KEY = "mobileclaw:security-config:v1";

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
  requireApproval: true,
  highRiskActions: false,
};

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
