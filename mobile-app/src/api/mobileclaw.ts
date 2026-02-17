import type { AgentRuntimeConfig } from "../state/mobileclaw";

function bearerFor(config: AgentRuntimeConfig): string {
  if (config.oauthAccessToken.trim()) return config.oauthAccessToken.trim();
  if (config.authMode === "oauth_token") return config.oauthAccessToken.trim();
  return config.apiKey.trim();
}

export async function sendAgentPrompt(prompt: string, config: AgentRuntimeConfig): Promise<string> {
  const baseUrl = config.apiUrl.trim().replace(/\/$/, "");
  const model = config.model.trim();
  const temperature = Number.isFinite(config.temperature) ? config.temperature : 0.1;

  if (!baseUrl || !model) {
    throw new Error("Provider endpoint and model are required.");
  }

  const token = bearerFor(config);
  if (config.provider !== "ollama" && !token) {
    throw new Error("Missing provider credentials. Add API key or OAuth token in Settings.");
  }

  if (config.provider === "ollama") {
    const res = await fetch(`${baseUrl}/api/generate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model, prompt, stream: false, options: { temperature } }),
    });
    const data = (await res.json()) as { response?: string; error?: string };
    if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
    return String(data.response || "");
  }

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
  };

  if (config.provider === "anthropic") {
    headers["x-api-key"] = token;
    headers["anthropic-version"] = "2023-06-01";
    delete headers.Authorization;
  }

  if (config.provider === "openrouter") {
    headers["HTTP-Referer"] = "https://mobileclaw.app";
    headers["X-Title"] = "MobileClaw";
  }

  const endpoint = `${baseUrl}/chat/completions`;
  const res = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify({
      model,
      temperature,
      messages: [{ role: "user", content: prompt }],
    }),
  });

  const body = (await res.json()) as {
    choices?: Array<{ message?: { content?: string } }>;
    error?: { message?: string };
  };

  if (!res.ok) {
    throw new Error(body.error?.message || `HTTP ${res.status}`);
  }

  return String(body.choices?.[0]?.message?.content || "");
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
