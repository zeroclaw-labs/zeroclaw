// In Docker: nginx proxies /api/* -> zeroclaw:3000/*
// In dev: use VITE_API_URL or default to /api (works with vite proxy too)
const API_BASE = import.meta.env.VITE_API_URL ?? "/api";

export async function healthCheck() {
  const res = await fetch(`${API_BASE}/health`);
  return res.json();
}

export async function pair(code: string): Promise<{ paired: boolean; token?: string; error?: string }> {
  const res = await fetch(`${API_BASE}/pair`, {
    method: "POST",
    headers: { "X-Pairing-Code": code },
  });
  return res.json();
}

export async function sendMessage(
  message: string,
  token: string,
): Promise<{ response?: string; model?: string; error?: string }> {
  const res = await fetch(`${API_BASE}/webhook`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify({ message }),
  });
  return res.json();
}
