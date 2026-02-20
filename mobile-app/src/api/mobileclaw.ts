import type { AgentRuntimeConfig } from "../state/mobileclaw";
import * as FileSystem from "expo-file-system/legacy";
import { Buffer } from "buffer";

export type ChatCompletionMessage = {
  role: "system" | "user" | "assistant";
  content: string;
};

type AgentPromptContext = {
  enabledTools?: string[];
  integrations?: string[];
};

function bearerFor(config: AgentRuntimeConfig): string {
  if (config.oauthAccessToken.trim()) return config.oauthAccessToken.trim();
  if (config.authMode === "oauth_token") return config.oauthAccessToken.trim();
  return config.apiKey.trim();
}

function apiKeyFor(config: AgentRuntimeConfig): string {
  if (config.authMode === "oauth_token") return "";
  return config.apiKey.trim();
}

async function readJsonResponse(res: Response): Promise<any> {
  const raw = await res.text();
  if (!raw) return null;
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    throw new Error(`Invalid JSON response: ${raw.slice(0, 300)}`);
  }
}

function extractOpenAiMessage(body: any): string {
  return String(body?.choices?.[0]?.message?.content || "").trim();
}

async function postJson(url: string, body: unknown, headers: Record<string, string>): Promise<any> {
  const res = await fetch(url, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  const json = await readJsonResponse(res);
  if (!res.ok) {
    const detail = json?.error?.message || json?.error || json?.detail;
    const errorMsg = typeof detail === "string" ? detail : `HTTP ${res.status}`;

    // Handle common authentication errors
    if (res.status === 401 || errorMsg.toLowerCase().includes("user not found")) {
      throw new Error("Invalid or expired API key. Please update your credentials in Settings.");
    }

    throw new Error(errorMsg);
  }
  return json;
}

function normalizeMessages(messages: ChatCompletionMessage[]): ChatCompletionMessage[] {
  return messages
    .map((m) => ({ role: m.role, content: String(m.content || "").trim() }))
    .filter((m) => m.content);
}

async function sendOllamaMessages(config: AgentRuntimeConfig, messages: ChatCompletionMessage[]): Promise<string> {
  const baseUrl = config.apiUrl.trim().replace(/\/$/, "") || "http://10.0.2.2:11434";
  const json = await postJson(
    `${baseUrl}/api/chat`,
    {
      model: config.model.trim(),
      stream: false,
      options: { temperature: Number.isFinite(config.temperature) ? config.temperature : 0.1 },
      messages: normalizeMessages(messages),
    },
    { "Content-Type": "application/json" },
  );

  return String(json?.message?.content || "").trim();
}

async function sendOpenAiSubscriptionCodex(config: AgentRuntimeConfig, messages: ChatCompletionMessage[]): Promise<string> {
  const token = bearerFor(config);
  if (!token) throw new Error("OAuth access token is required for OpenAI subscription mode.");

  const endpoint = config.apiUrl.trim() || "https://chatgpt.com/backend-api/codex/responses";
  const input = normalizeMessages(messages).map((m) => ({ role: m.role, content: m.content }));

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
  };

  if (config.accountId.trim()) {
    headers["ChatGPT-Account-Id"] = config.accountId.trim();
  }

  const json = await postJson(
    endpoint,
    {
      model: config.model.trim(),
      input,
    },
    headers,
  );

  const outputText = String(json?.output_text || "").trim();
  if (outputText) return outputText;
  return String(json?.output?.[0]?.content?.[0]?.text || "").trim();
}

async function sendOpenAiCompatibleMessages(config: AgentRuntimeConfig, messages: ChatCompletionMessage[]): Promise<string> {
  const provider = config.provider.trim().toLowerCase();
  if (provider === "openai" && config.authMode === "oauth_token") {
    return sendOpenAiSubscriptionCodex(config, messages);
  }

  const baseUrl = (config.apiUrl.trim() ||
    (provider === "openrouter"
      ? "https://openrouter.ai/api/v1"
      : provider === "copilot"
        ? "https://api.githubcopilot.com"
        : "https://api.openai.com/v1")
  ).replace(/\/$/, "");

  const token = bearerFor(config);
  if (!token) {
    throw new Error(`Missing provider credentials for ${provider}.`);
  }

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
  };

  if (provider === "openrouter") {
    headers["HTTP-Referer"] = "https://mobileclaw.app";
    headers["X-Title"] = "MobileClaw";
  }

  if (provider === "copilot") {
    headers["Openai-Intent"] = "conversation-edits";
    headers["x-initiator"] = "user";
    if (config.enterpriseUrl.trim()) {
      headers["X-GitHub-Enterprise-Host"] = config.enterpriseUrl.trim();
    }
  }

  const json = await postJson(
    `${baseUrl}/chat/completions`,
    {
      model: config.model.trim(),
      temperature: Number.isFinite(config.temperature) ? config.temperature : 0.1,
      messages: normalizeMessages(messages),
    },
    headers,
  );

  return extractOpenAiMessage(json);
}

async function sendAnthropicMessages(config: AgentRuntimeConfig, messages: ChatCompletionMessage[]): Promise<string> {
  const baseUrl = (config.apiUrl.trim() || "https://api.anthropic.com/v1").replace(/\/$/, "");
  const oauthToken = bearerFor(config);
  const apiKey = apiKeyFor(config);
  if (!oauthToken && !apiKey) {
    throw new Error("Missing provider credentials for anthropic.");
  }

  const normalized = normalizeMessages(messages);
  const system = normalized
    .filter((m) => m.role === "system")
    .map((m) => m.content)
    .join("\n\n")
    .trim();
  const conversation = normalized
    .filter((m) => m.role !== "system")
    .map((m) => ({ role: m.role === "assistant" ? "assistant" : "user", content: m.content }));

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    "anthropic-version": "2023-06-01",
  };

  if (config.authMode === "oauth_token" && oauthToken) {
    headers.Authorization = `Bearer ${oauthToken}`;
    headers["anthropic-beta"] = "oauth-2025-04-20";
  } else {
    headers["x-api-key"] = apiKey;
  }

  const json = await postJson(
    `${baseUrl}/messages`,
    {
      model: config.model.trim(),
      max_tokens: 1024,
      temperature: Number.isFinite(config.temperature) ? config.temperature : 0.1,
      ...(system ? { system } : {}),
      messages: conversation,
    },
    headers,
  );

  return String(json?.content?.[0]?.text || "").trim();
}

async function sendGeminiMessages(config: AgentRuntimeConfig, messages: ChatCompletionMessage[]): Promise<string> {
  const baseUrl = (config.apiUrl.trim() || "https://generativelanguage.googleapis.com/v1beta").replace(/\/$/, "");
  const bearer = config.authMode === "oauth_token" ? bearerFor(config) : "";
  const apiKey = apiKeyFor(config);

  if (!bearer && !apiKey) {
    throw new Error("Missing provider credentials for gemini.");
  }

  const model = config.model.trim() || "gemini-1.5-pro";
  const prompt = normalizeMessages(messages)
    .map((m) => `${m.role.toUpperCase()}: ${m.content}`)
    .join("\n\n");

  const endpoint = bearer
    ? `${baseUrl}/models/${model}:generateContent`
    : `${baseUrl}/models/${model}:generateContent?key=${encodeURIComponent(apiKey)}`;

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  if (bearer) headers.Authorization = `Bearer ${bearer}`;

  const json = await postJson(
    endpoint,
    {
      contents: [{ parts: [{ text: prompt }] }],
      generationConfig: { temperature: Number.isFinite(config.temperature) ? config.temperature : 0.1 },
    },
    headers,
  );

  return String(json?.candidates?.[0]?.content?.parts?.[0]?.text || "").trim();
}

export async function runAgentChat(
  config: AgentRuntimeConfig,
  messages: ChatCompletionMessage[],
): Promise<string> {
  const provider = config.provider.trim().toLowerCase();
  if (!config.model.trim()) {
    throw new Error("Model is required.");
  }

  switch (provider) {
    case "ollama":
      return sendOllamaMessages(config, messages);
    case "openrouter":
    case "openai":
    case "copilot":
      return sendOpenAiCompatibleMessages(config, messages);
    case "anthropic":
      return sendAnthropicMessages(config, messages);
    case "gemini":
      return sendGeminiMessages(config, messages);
    default:
      throw new Error(`Unsupported provider: ${provider}`);
  }
}

export async function sendAgentPrompt(
  prompt: string,
  config: AgentRuntimeConfig,
  context?: AgentPromptContext,
): Promise<string> {
  const toolList = (context?.enabledTools || []).join(", ");
  const integrationList = (context?.integrations || []).join(", ");
  const systemInstruction = [
    "You are MobileClaw Android agent.",
    toolList ? `Enabled tools: ${toolList}.` : "Enabled tools: none provided.",
    integrationList ? `Enabled integrations: ${integrationList}.` : "Enabled integrations: none.",
    "If user asks for device, android, hardware, or user-data actions, map to listed tool ids and return concrete tool id + parameters.",
  ].join(" ");

  return runAgentChat(config, [
    { role: "system", content: systemInstruction },
    { role: "user", content: prompt },
  ]);
}

export async function transcribeWithDeepgram(audioUri: string, apiKey: string): Promise<string> {
  const key = apiKey.trim();
  if (!key) throw new Error("Deepgram API key is required in Settings.");

  const form = new FormData();
  form.append("audio", { uri: audioUri, type: "audio/mp4", name: "recording.m4a" } as never);

  const res = await fetch("https://api.deepgram.com/v1/listen?model=nova-2&smart_format=true", {
    method: "POST",
    headers: {
      Authorization: `Token ${key}`,
    },
    body: form,
  });

  const json = (await res.json()) as {
    results?: { channels?: Array<{ alternatives?: Array<{ transcript?: string }> }> };
    err_msg?: string;
  };

  if (!res.ok) throw new Error(json.err_msg || `Deepgram HTTP ${res.status}`);
  return String(json.results?.channels?.[0]?.alternatives?.[0]?.transcript || "").trim();
}

export async function synthesizeSpeechWithDeepgram(text: string, apiKey: string): Promise<string> {
  const key = apiKey.trim();
  const content = text.trim();
  if (!key) throw new Error("Deepgram API key is required in Settings.");
  if (!content) throw new Error("Speech text is empty.");

  const response = await fetch("https://api.deepgram.com/v1/speak?model=aura-2-thalia-en", {
    method: "POST",
    headers: {
      Authorization: `Token ${key}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text: content }),
  });

  if (!response.ok) {
    const detail = await response.text();
    throw new Error(detail || `Deepgram TTS HTTP ${response.status}`);
  }

  const audioArrayBuffer = await response.arrayBuffer();
  const base64 = Buffer.from(audioArrayBuffer).toString("base64");
  const outputUri = `${FileSystem.cacheDirectory || ""}mobileclaw-tts-${Date.now()}.mp3`;
  if (!outputUri) {
    throw new Error("Cache directory unavailable for TTS playback");
  }

  await FileSystem.writeAsStringAsync(outputUri, base64, {
    encoding: FileSystem.EncodingType.Base64,
  });
  return outputUri;
}

export async function fetchOpenRouterModels(apiKey?: string): Promise<string[]> {
  const headers: Record<string, string> = {};
  if (apiKey?.trim()) {
    headers.Authorization = `Bearer ${apiKey.trim()}`;
  }

  const res = await fetch("https://openrouter.ai/api/v1/models", { headers });
  const data = (await res.json()) as {
    data?: Array<{ id?: string }>;
    error?: { message?: string };
  };

  if (!res.ok) {
    throw new Error(data.error?.message || `OpenRouter HTTP ${res.status}`);
  }

  const ids = (data.data || [])
    .map((m) => String(m.id || "").trim())
    .filter(Boolean)
    .sort((a, b) => a.localeCompare(b));

  return Array.from(new Set(ids));
}
