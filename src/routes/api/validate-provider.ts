import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'

/**
 * POST /api/validate-provider
 *
 * Validates an API key by making a lightweight request to the provider's API.
 * Returns { ok: true } if valid, { ok: false, error: "..." } if not.
 */

type ValidateBody = {
  providerId?: string
  apiKey?: string
}

type ProviderConfig = {
  url: string
  headers: (key: string) => Record<string, string>
  body?: string
  method?: string
  successCheck: (status: number, data: unknown) => boolean
}

const PROVIDER_VALIDATORS: Record<string, ProviderConfig> = {
  anthropic: {
    // Hit the messages endpoint with a minimal request — will return 400 but proves auth works
    // Using a GET to /v1/models would be ideal but Anthropic doesn't have one
    // Instead we send a minimal message request — if we get 400 (bad request) that means auth passed
    url: 'https://api.anthropic.com/v1/messages',
    headers: (key) => ({
      'x-api-key': key,
      'anthropic-version': '2023-06-01',
      'content-type': 'application/json',
    }),
    body: JSON.stringify({
      model: 'claude-sonnet-4-5-20250514',
      max_tokens: 1,
      messages: [{ role: 'user', content: 'hi' }],
    }),
    method: 'POST',
    // 200 = valid (unlikely with max_tokens:1 but possible), 400 = valid auth but bad request
    // 401/403 = invalid key
    successCheck: (status) => status === 200 || status === 400 || status === 429,
  },
  openrouter: {
    url: 'https://openrouter.ai/api/v1/auth/key',
    headers: (key) => ({
      Authorization: `Bearer ${key}`,
    }),
    successCheck: (status) => status === 200,
  },
  google: {
    // Gemini API — list models to validate key
    url: 'https://generativelanguage.googleapis.com/v1/models',
    headers: (key) => ({
      'x-goog-api-key': key,
    }),
    successCheck: (status) => status === 200,
  },
  openai: {
    url: 'https://api.openai.com/v1/models',
    headers: (key) => ({
      Authorization: `Bearer ${key}`,
    }),
    successCheck: (status) => status === 200,
  },
  minimax: {
    url: 'https://api.minimaxi.chat/v1/models',
    headers: (key) => ({
      Authorization: `Bearer ${key}`,
    }),
    successCheck: (status) => status === 200,
  },
}

export const Route = createFileRoute('/api/validate-provider')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as ValidateBody

          if (!body.providerId || !body.apiKey) {
            return json({ ok: false, error: 'Missing provider or API key' }, { status: 400 })
          }

          const validator = PROVIDER_VALIDATORS[body.providerId]
          if (!validator) {
            return json({ ok: false, error: `Unknown provider: ${body.providerId}` }, { status: 400 })
          }

          const fetchOptions: RequestInit = {
            method: validator.method || 'GET',
            headers: validator.headers(body.apiKey),
            signal: AbortSignal.timeout(10000),
          }

          if (validator.body && fetchOptions.method === 'POST') {
            fetchOptions.body = validator.body
          }

          const response = await fetch(validator.url, fetchOptions)

          if (validator.successCheck(response.status, null)) {
            return json({ ok: true })
          }

          // Try to extract error message
          let errorMsg = `Invalid API key (HTTP ${response.status})`
          try {
            const data = (await response.json()) as { error?: { message?: string } }
            if (data.error?.message) {
              errorMsg = data.error.message
            }
          } catch {
            // ignore parse errors
          }

          return json({ ok: false, error: errorMsg })
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err)
          if (msg.includes('timeout') || msg.includes('abort')) {
            return json({ ok: false, error: 'Request timed out — check your connection' })
          }
          return json({ ok: false, error: `Validation error: ${msg}` }, { status: 500 })
        }
      },
    },
  },
})
