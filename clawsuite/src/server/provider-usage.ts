/**
 * Provider Usage — polls each configured AI provider for real-time usage data.
 *
 * Follows OpenUsage's plugin pattern:
 * - Claude: OAuth credentials from ~/.claude/.credentials.json or keychain (auto-refresh)
 * - OpenAI: API key from env → /v1/dashboard/billing/usage
 * - OpenRouter: API key from env → /api/v1/auth/key
 * - Anthropic API: API key from env → header-based usage tracking
 */

import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import { execSync } from 'node:child_process'
import { homedir } from 'node:os'
import { join } from 'node:path'

// ── Types ────────────────────────────────────────────────────────────────────

export type ProviderStatus =
  | 'ok'
  | 'missing_credentials'
  | 'auth_expired'
  | 'error'

export type UsageLine = {
  type: 'progress' | 'text' | 'badge'
  label: string
  used?: number
  limit?: number
  format?: 'percent' | 'dollars' | 'tokens'
  value?: string
  color?: string
  resetsAt?: string
}

export type ProviderUsageResult = {
  provider: string
  displayName: string
  status: ProviderStatus
  message?: string
  plan?: string
  lines: UsageLine[]
  updatedAt: number
}

export type ProviderUsageResponse = {
  ok: boolean
  updatedAt: number
  providers: ProviderUsageResult[]
  error?: string
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function readNumber(v: unknown): number | undefined {
  if (typeof v === 'number' && Number.isFinite(v)) return v
  if (typeof v === 'string') {
    const n = Number(v)
    if (Number.isFinite(n)) return n
  }
  return undefined
}

function expandHome(p: string): string {
  return p.startsWith('~') ? join(homedir(), p.slice(1)) : p
}

// ── Claude OAuth ─────────────────────────────────────────────────────────────

const CLAUDE_CRED_FILE = '~/.claude/.credentials.json'
const CLAUDE_KEYCHAIN_SERVICE = 'Claude Code-credentials'
const CLAUDE_USAGE_URL = 'https://api.anthropic.com/api/oauth/usage'
const CLAUDE_REFRESH_URL = 'https://platform.claude.com/v1/oauth/token'
const CLAUDE_CLIENT_ID = '9d1c250a-e61b-44d9-88ed-5944d1962f5e'
const CLAUDE_SCOPES =
  'user:profile user:inference user:sessions:claude_code user:mcp_servers'
const REFRESH_BUFFER_MS = 5 * 60 * 1000

type ClaudeOAuth = {
  accessToken: string
  refreshToken?: string
  expiresAt?: number
  subscriptionType?: string
}

type ClaudeCredentials = {
  oauth: ClaudeOAuth
  source: 'file' | 'keychain'
  fullData: Record<string, unknown>
}

function tryParseKeychainHex(text: string): unknown | null {
  let hex = text.trim()
  if (hex.startsWith('0x') || hex.startsWith('0X')) hex = hex.slice(2)
  if (!hex || hex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(hex)) return null
  try {
    const bytes = Buffer.from(hex, 'hex')
    return JSON.parse(bytes.toString('utf-8'))
  } catch {
    return null
  }
}

function loadClaudeCredentials(): ClaudeCredentials | null {
  // Try file first
  const credPath = expandHome(CLAUDE_CRED_FILE)
  if (existsSync(credPath)) {
    try {
      const text = readFileSync(credPath, 'utf-8')
      const parsed = JSON.parse(text)
      const oauth = parsed?.claudeAiOauth
      if (oauth?.accessToken) {
        return { oauth, source: 'file', fullData: parsed }
      }
    } catch {
      /* continue to keychain */
    }
  }

  // Try macOS keychain
  if (process.platform === 'darwin') {
    try {
      const raw = execSync(
        `security find-generic-password -s "${CLAUDE_KEYCHAIN_SERVICE}" -w 2>/dev/null`,
        { encoding: 'utf-8', timeout: 5000 },
      ).trim()
      if (raw) {
        let parsed: Record<string, unknown> | null = null
        try {
          parsed = JSON.parse(raw)
        } catch {
          parsed = tryParseKeychainHex(raw) as Record<string, unknown> | null
        }
        const oauth = parsed?.claudeAiOauth as ClaudeOAuth | undefined
        if (oauth?.accessToken) {
          return { oauth, source: 'keychain', fullData: parsed! }
        }
      }
    } catch {
      /* no keychain entry */
    }
  }

  return null
}

function saveClaudeCredentials(creds: ClaudeCredentials): void {
  const text = JSON.stringify(creds.fullData)
  if (creds.source === 'file') {
    try {
      writeFileSync(expandHome(CLAUDE_CRED_FILE), text, 'utf-8')
    } catch {
      /* best effort */
    }
  } else if (creds.source === 'keychain' && process.platform === 'darwin') {
    try {
      execSync(
        `security add-generic-password -U -s "${CLAUDE_KEYCHAIN_SERVICE}" -w "${text.replace(/"/g, '\\"')}" 2>/dev/null`,
        { timeout: 5000 },
      )
    } catch {
      /* best effort */
    }
  }
}

async function refreshClaudeToken(
  creds: ClaudeCredentials,
): Promise<string | null> {
  if (!creds.oauth.refreshToken) return null

  const res = await fetch(CLAUDE_REFRESH_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      grant_type: 'refresh_token',
      refresh_token: creds.oauth.refreshToken,
      client_id: CLAUDE_CLIENT_ID,
      scope: CLAUDE_SCOPES,
    }),
  })

  if (res.status === 400 || res.status === 401) {
    const body = (await res.json().catch(() => null)) as Record<
      string,
      unknown
    > | null
    const errorCode = body?.error ?? body?.error_description
    if (errorCode === 'invalid_grant') {
      throw new Error('Claude session expired. Run `claude` to log in again.')
    }
    throw new Error('Claude token expired. Run `claude` to log in again.')
  }

  if (!res.ok) return null

  const body = (await res.json().catch(() => null)) as Record<
    string,
    unknown
  > | null
  if (!body?.access_token) return null

  creds.oauth.accessToken = body.access_token as string
  if (body.refresh_token)
    creds.oauth.refreshToken = body.refresh_token as string
  if (typeof body.expires_in === 'number') {
    creds.oauth.expiresAt = Date.now() + (body.expires_in as number) * 1000
  }
  creds.fullData.claudeAiOauth = creds.oauth
  saveClaudeCredentials(creds)

  return creds.oauth.accessToken
}

function planLabel(subType?: string): string | undefined {
  if (!subType) return undefined
  const labels: Record<string, string> = {
    claude_pro: 'Pro',
    claude_team: 'Team',
    claude_enterprise: 'Enterprise',
    claude_max_5x: 'Max 5x',
    claude_max_20x: 'Max 20x',
    free: 'Free',
  }
  return labels[subType] ?? subType
}

export async function fetchClaudeUsage(): Promise<ProviderUsageResult> {
  const now = Date.now()
  const creds = loadClaudeCredentials()

  if (!creds) {
    return {
      provider: 'claude',
      displayName: 'Claude (OAuth)',
      status: 'missing_credentials',
      message: 'No Claude credentials found. Run `claude` to authenticate.',
      lines: [],
      updatedAt: now,
    }
  }

  let accessToken = creds.oauth.accessToken

  // Auto-refresh if expired or expiring soon
  if (
    creds.oauth.expiresAt &&
    now > creds.oauth.expiresAt - REFRESH_BUFFER_MS
  ) {
    try {
      const refreshed = await refreshClaudeToken(creds)
      if (refreshed) accessToken = refreshed
    } catch (e) {
      return {
        provider: 'claude',
        displayName: 'Claude (OAuth)',
        status: 'auth_expired',
        message: e instanceof Error ? e.message : String(e),
        lines: [],
        updatedAt: now,
      }
    }
  }

  // Fetch usage
  let res: Response
  try {
    res = await fetch(CLAUDE_USAGE_URL, {
      headers: {
        Authorization: `Bearer ${accessToken.trim()}`,
        Accept: 'application/json',
        'Content-Type': 'application/json',
        'anthropic-beta': 'oauth-2025-04-20',
        'User-Agent': 'OpenClaw-Studio',
      },
    })
  } catch (e) {
    return {
      provider: 'claude',
      displayName: 'Claude (OAuth)',
      status: 'error',
      message: `Request failed: ${e instanceof Error ? e.message : String(e)}`,
      lines: [],
      updatedAt: now,
    }
  }

  // If 401/403, try refresh once
  if (res.status === 401 || res.status === 403) {
    try {
      const refreshed = await refreshClaudeToken(creds)
      if (refreshed) {
        res = await fetch(CLAUDE_USAGE_URL, {
          headers: {
            Authorization: `Bearer ${refreshed.trim()}`,
            Accept: 'application/json',
            'Content-Type': 'application/json',
            'anthropic-beta': 'oauth-2025-04-20',
            'User-Agent': 'OpenClaw-Studio',
          },
        })
      }
    } catch (e) {
      return {
        provider: 'claude',
        displayName: 'Claude (OAuth)',
        status: 'auth_expired',
        message: e instanceof Error ? e.message : String(e),
        lines: [],
        updatedAt: now,
      }
    }
  }

  if (!res.ok) {
    return {
      provider: 'claude',
      displayName: 'Claude (OAuth)',
      status: 'error',
      message: `HTTP ${res.status}`,
      lines: [],
      updatedAt: now,
    }
  }

  const data = (await res.json().catch(() => null)) as Record<
    string,
    unknown
  > | null
  if (!data) {
    return {
      provider: 'claude',
      displayName: 'Claude (OAuth)',
      status: 'error',
      message: 'Invalid response',
      lines: [],
      updatedAt: now,
    }
  }

  const lines: UsageLine[] = []

  const fiveHour = data.five_hour as Record<string, unknown> | undefined
  if (fiveHour && typeof fiveHour.utilization === 'number') {
    lines.push({
      type: 'progress',
      label: 'Session (5h)',
      used: fiveHour.utilization as number,
      limit: 100,
      format: 'percent',
      resetsAt: fiveHour.resets_at
        ? new Date(fiveHour.resets_at as string).toISOString()
        : undefined,
    })
  }

  const sevenDay = data.seven_day as Record<string, unknown> | undefined
  if (sevenDay && typeof sevenDay.utilization === 'number') {
    lines.push({
      type: 'progress',
      label: 'Weekly',
      used: sevenDay.utilization as number,
      limit: 100,
      format: 'percent',
      resetsAt: sevenDay.resets_at
        ? new Date(sevenDay.resets_at as string).toISOString()
        : undefined,
    })
  }

  const sevenDaySonnet = data.seven_day_sonnet as
    | Record<string, unknown>
    | undefined
  if (sevenDaySonnet && typeof sevenDaySonnet.utilization === 'number') {
    lines.push({
      type: 'progress',
      label: 'Sonnet',
      used: sevenDaySonnet.utilization as number,
      limit: 100,
      format: 'percent',
      resetsAt: sevenDaySonnet.resets_at
        ? new Date(sevenDaySonnet.resets_at as string).toISOString()
        : undefined,
    })
  }

  const extraUsage = data.extra_usage as Record<string, unknown> | undefined
  if (extraUsage?.is_enabled) {
    // API returns values in cents — convert to dollars
    const usedCents = readNumber(extraUsage.used_credits)
    const limitCents = readNumber(extraUsage.monthly_limit)
    const used = usedCents !== undefined ? usedCents / 100 : undefined
    const limit = limitCents !== undefined ? limitCents / 100 : undefined
    if (used !== undefined && limit !== undefined && limit > 0) {
      lines.push({
        type: 'progress',
        label: 'Extra Usage',
        used,
        limit,
        format: 'dollars',
      })
    } else if (used !== undefined && used > 0) {
      lines.push({
        type: 'text',
        label: 'Extra Usage',
        value: `$${used.toFixed(2)}`,
      })
    }
  }

  if (lines.length === 0) {
    lines.push({
      type: 'badge',
      label: 'Status',
      value: 'No usage data',
      color: '#a3a3a3',
    })
  }

  return {
    provider: 'claude',
    displayName: 'Claude (OAuth)',
    status: 'ok',
    plan: planLabel(creds.oauth.subscriptionType),
    lines,
    updatedAt: now,
  }
}

// ── Codex (OpenAI ChatGPT/Codex CLI OAuth) ──────────────────────────────────

const CODEX_AUTH_PATH = '~/.codex/auth.json'
const CODEX_CLIENT_ID = 'app_EMoamEEZ73f0CkXaXp7hrann'
const CODEX_REFRESH_URL = 'https://auth.openai.com/oauth/token'
const CODEX_USAGE_URL = 'https://chatgpt.com/backend-api/wham/usage'
const CODEX_REFRESH_AGE_MS = 8 * 24 * 60 * 60 * 1000

type CodexAuth = {
  tokens?: {
    access_token?: string
    refresh_token?: string
    id_token?: string
    account_id?: string
  }
  last_refresh?: string
  OPENAI_API_KEY?: string
}

function loadCodexAuth(): CodexAuth | null {
  const authPath = expandHome(CODEX_AUTH_PATH)
  if (!existsSync(authPath)) return null
  try {
    return JSON.parse(readFileSync(authPath, 'utf-8'))
  } catch {
    return null
  }
}

function saveCodexAuth(auth: CodexAuth): void {
  try {
    writeFileSync(
      expandHome(CODEX_AUTH_PATH),
      JSON.stringify(auth, null, 2),
      'utf-8',
    )
  } catch {
    /* best effort */
  }
}

async function refreshCodexToken(auth: CodexAuth): Promise<string | null> {
  if (!auth.tokens?.refresh_token) return null

  const res = await fetch(CODEX_REFRESH_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: `grant_type=refresh_token&client_id=${encodeURIComponent(CODEX_CLIENT_ID)}&refresh_token=${encodeURIComponent(auth.tokens.refresh_token)}`,
  })

  if (res.status === 400 || res.status === 401) {
    const body = (await res.json().catch(() => null)) as Record<
      string,
      unknown
    > | null
    const code =
      (body?.error as Record<string, unknown>)?.code ??
      body?.error ??
      body?.code
    if (
      code === 'refresh_token_expired' ||
      code === 'refresh_token_reused' ||
      code === 'refresh_token_invalidated'
    ) {
      throw new Error('Codex session expired. Run `codex` to log in again.')
    }
    throw new Error('Codex token expired. Run `codex` to log in again.')
  }

  if (!res.ok) return null

  const body = (await res.json().catch(() => null)) as Record<
    string,
    unknown
  > | null
  if (!body?.access_token) return null

  auth.tokens!.access_token = body.access_token as string
  if (body.refresh_token)
    auth.tokens!.refresh_token = body.refresh_token as string
  if (body.id_token) auth.tokens!.id_token = body.id_token as string
  auth.last_refresh = new Date().toISOString()
  saveCodexAuth(auth)

  return auth.tokens!.access_token
}

function getResetsAtIso(
  nowSec: number,
  window?: Record<string, unknown>,
): string | undefined {
  if (!window) return undefined
  if (typeof window.reset_at === 'number')
    return new Date(window.reset_at * 1000).toISOString()
  if (typeof window.reset_after_seconds === 'number')
    return new Date(
      (nowSec + (window.reset_after_seconds as number)) * 1000,
    ).toISOString()
  return undefined
}

export async function fetchCodexUsage(): Promise<ProviderUsageResult> {
  const now = Date.now()
  const auth = loadCodexAuth()

  if (!auth || !auth.tokens?.access_token) {
    return {
      provider: 'codex',
      displayName: 'Codex',
      status: 'missing_credentials',
      message: 'No Codex credentials found. Run `codex` to authenticate.',
      lines: [],
      updatedAt: now,
    }
  }

  let accessToken = auth.tokens.access_token

  // Refresh if stale
  const lastRefreshMs = auth.last_refresh
    ? new Date(auth.last_refresh).getTime()
    : 0
  if (now - lastRefreshMs > CODEX_REFRESH_AGE_MS) {
    try {
      const refreshed = await refreshCodexToken(auth)
      if (refreshed) accessToken = refreshed
    } catch (e) {
      return {
        provider: 'codex',
        displayName: 'Codex',
        status: 'auth_expired',
        message: e instanceof Error ? e.message : String(e),
        lines: [],
        updatedAt: now,
      }
    }
  }

  // Fetch usage
  const headers: Record<string, string> = {
    Authorization: `Bearer ${accessToken}`,
    Accept: 'application/json',
    'User-Agent': 'OpenClaw-Studio',
  }
  if (auth.tokens.account_id) {
    headers['ChatGPT-Account-Id'] = auth.tokens.account_id
  }

  let res: Response
  try {
    res = await fetch(CODEX_USAGE_URL, { headers })
  } catch (e) {
    return {
      provider: 'codex',
      displayName: 'Codex',
      status: 'error',
      message: `Request failed: ${e instanceof Error ? e.message : String(e)}`,
      lines: [],
      updatedAt: now,
    }
  }

  // Retry on auth failure
  if (res.status === 401 || res.status === 403) {
    try {
      const refreshed = await refreshCodexToken(auth)
      if (refreshed) {
        headers.Authorization = `Bearer ${refreshed}`
        res = await fetch(CODEX_USAGE_URL, { headers })
      }
    } catch (e) {
      return {
        provider: 'codex',
        displayName: 'Codex',
        status: 'auth_expired',
        message: e instanceof Error ? e.message : String(e),
        lines: [],
        updatedAt: now,
      }
    }
  }

  if (!res.ok) {
    return {
      provider: 'codex',
      displayName: 'Codex',
      status: 'error',
      message: `HTTP ${res.status}`,
      lines: [],
      updatedAt: now,
    }
  }

  const data = (await res.json().catch(() => null)) as Record<
    string,
    unknown
  > | null
  if (!data) {
    return {
      provider: 'codex',
      displayName: 'Codex',
      status: 'error',
      message: 'Invalid response',
      lines: [],
      updatedAt: now,
    }
  }

  const lines: UsageLine[] = []
  const nowSec = Math.floor(now / 1000)

  // Parse rate limits from headers and body
  const rateLimit = data.rate_limit as Record<string, unknown> | undefined
  const primaryWindow = rateLimit?.primary_window as
    | Record<string, unknown>
    | undefined
  const secondaryWindow = rateLimit?.secondary_window as
    | Record<string, unknown>
    | undefined
  const reviewRateLimit = data.code_review_rate_limit as
    | Record<string, unknown>
    | undefined
  const reviewWindow = reviewRateLimit?.primary_window as
    | Record<string, unknown>
    | undefined

  // Headers have the most accurate percent data
  const headerPrimary = readNumber(
    res.headers.get('x-codex-primary-used-percent'),
  )
  const headerSecondary = readNumber(
    res.headers.get('x-codex-secondary-used-percent'),
  )

  if (headerPrimary !== undefined) {
    lines.push({
      type: 'progress',
      label: 'Session',
      used: headerPrimary,
      limit: 100,
      format: 'percent',
      resetsAt: getResetsAtIso(nowSec, primaryWindow),
    })
  }
  if (headerSecondary !== undefined) {
    lines.push({
      type: 'progress',
      label: 'Weekly',
      used: headerSecondary,
      limit: 100,
      format: 'percent',
      resetsAt: getResetsAtIso(nowSec, secondaryWindow),
    })
  }

  // Fallback to body data
  if (lines.length === 0 && rateLimit) {
    if (primaryWindow && typeof primaryWindow.used_percent === 'number') {
      lines.push({
        type: 'progress',
        label: 'Session',
        used: primaryWindow.used_percent as number,
        limit: 100,
        format: 'percent',
        resetsAt: getResetsAtIso(nowSec, primaryWindow),
      })
    }
    if (secondaryWindow && typeof secondaryWindow.used_percent === 'number') {
      lines.push({
        type: 'progress',
        label: 'Weekly',
        used: secondaryWindow.used_percent as number,
        limit: 100,
        format: 'percent',
        resetsAt: getResetsAtIso(nowSec, secondaryWindow),
      })
    }
  }

  // Reviews
  if (reviewWindow && typeof reviewWindow.used_percent === 'number') {
    lines.push({
      type: 'progress',
      label: 'Reviews',
      used: reviewWindow.used_percent as number,
      limit: 100,
      format: 'percent',
      resetsAt: getResetsAtIso(nowSec, reviewWindow),
    })
  }

  // Credits
  const creditsHeader = readNumber(res.headers.get('x-codex-credits-balance'))
  const creditsData = (data.credits as Record<string, unknown>)?.balance
  const creditsRemaining = creditsHeader ?? readNumber(creditsData)
  if (creditsRemaining !== undefined) {
    const limit = 1000
    const used = Math.max(0, Math.min(limit, limit - creditsRemaining))
    lines.push({
      type: 'progress',
      label: 'Credits',
      used,
      limit,
      format: 'tokens',
    })
  }

  // Plan
  let plan: string | undefined
  if (data.plan_type) {
    const planLabels: Record<string, string> = {
      plus: 'Plus',
      pro: 'Pro',
      team: 'Team',
      enterprise: 'Enterprise',
      free: 'Free',
    }
    plan = planLabels[data.plan_type as string] ?? (data.plan_type as string)
  }

  if (lines.length === 0) {
    lines.push({
      type: 'badge',
      label: 'Status',
      value: 'No usage data',
      color: '#a3a3a3',
    })
  }

  return {
    provider: 'codex',
    displayName: 'Codex',
    status: 'ok',
    plan,
    lines,
    updatedAt: now,
  }
}

// ── OpenAI (API Key) ─────────────────────────────────────────────────────────

export async function fetchOpenAIUsage(): Promise<ProviderUsageResult> {
  const now = Date.now()
  const apiKey = process.env.OPENAI_API_KEY?.trim()

  if (!apiKey) {
    return {
      provider: 'openai',
      displayName: 'OpenAI',
      status: 'missing_credentials',
      message: 'Missing OPENAI_API_KEY',
      lines: [],
      updatedAt: now,
    }
  }

  // Fetch organization subscription info
  try {
    const subRes = await fetch(
      'https://api.openai.com/v1/organization/usage/completions?start_time=' +
        Math.floor((now - 86400000 * 30) / 1000),
      {
        headers: { Authorization: `Bearer ${apiKey}` },
      },
    )

    if (subRes.status === 401 || subRes.status === 403) {
      // Fall back to simpler approach — just report the key is valid
      return {
        provider: 'openai',
        displayName: 'OpenAI',
        status: 'ok',
        lines: [
          {
            type: 'badge',
            label: 'Status',
            value: 'API key active',
            color: '#10b981',
          },
        ],
        updatedAt: now,
      }
    }

    if (!subRes.ok) {
      return {
        provider: 'openai',
        displayName: 'OpenAI',
        status: 'error',
        message: `HTTP ${subRes.status}`,
        lines: [],
        updatedAt: now,
      }
    }

    const payload = (await subRes.json().catch(() => null)) as Record<
      string,
      unknown
    > | null
    const lines: UsageLine[] = []

    // Parse usage buckets if available
    const data = payload?.data as Array<Record<string, unknown>> | undefined
    if (Array.isArray(data) && data.length > 0) {
      let totalInputTokens = 0
      let totalOutputTokens = 0
      for (const bucket of data) {
        const results = bucket.results as
          | Array<Record<string, unknown>>
          | undefined
        if (Array.isArray(results)) {
          for (const r of results) {
            totalInputTokens += readNumber(r.input_tokens) ?? 0
            totalOutputTokens += readNumber(r.output_tokens) ?? 0
          }
        }
      }
      if (totalInputTokens > 0 || totalOutputTokens > 0) {
        lines.push({
          type: 'text',
          label: 'Input (30d)',
          value: `${(totalInputTokens / 1_000_000).toFixed(2)}M tokens`,
        })
        lines.push({
          type: 'text',
          label: 'Output (30d)',
          value: `${(totalOutputTokens / 1_000_000).toFixed(2)}M tokens`,
        })
      }
    }

    if (lines.length === 0) {
      lines.push({
        type: 'badge',
        label: 'Status',
        value: 'Connected',
        color: '#10b981',
      })
    }

    return {
      provider: 'openai',
      displayName: 'OpenAI',
      status: 'ok',
      lines,
      updatedAt: now,
    }
  } catch (e) {
    return {
      provider: 'openai',
      displayName: 'OpenAI',
      status: 'error',
      message: e instanceof Error ? e.message : String(e),
      lines: [],
      updatedAt: now,
    }
  }
}

// ── OpenRouter ───────────────────────────────────────────────────────────────

export async function fetchOpenRouterUsage(): Promise<ProviderUsageResult> {
  const now = Date.now()
  const apiKey = process.env.OPENROUTER_API_KEY?.trim()

  if (!apiKey) {
    return {
      provider: 'openrouter',
      displayName: 'OpenRouter',
      status: 'missing_credentials',
      message: 'Missing OPENROUTER_API_KEY',
      lines: [],
      updatedAt: now,
    }
  }

  try {
    const res = await fetch('https://openrouter.ai/api/v1/auth/key', {
      headers: { Authorization: `Bearer ${apiKey}` },
    })

    if (!res.ok) {
      return {
        provider: 'openrouter',
        displayName: 'OpenRouter',
        status: 'error',
        message: `HTTP ${res.status}`,
        lines: [],
        updatedAt: now,
      }
    }

    const payload = (await res.json().catch(() => ({}))) as Record<
      string,
      unknown
    >
    const data = (payload?.data ?? payload) as Record<string, unknown>
    const usage = (data?.usage ?? {}) as Record<string, unknown>

    const costUsd =
      readNumber(usage?.cost ?? data?.cost ?? data?.usage_cost) ?? 0
    const limitUsd = readNumber(data?.limit ?? data?.spend_limit)

    const lines: UsageLine[] = []

    if (limitUsd && limitUsd > 0) {
      lines.push({
        type: 'progress',
        label: 'Spend',
        used: costUsd,
        limit: limitUsd,
        format: 'dollars',
      })
    } else {
      lines.push({
        type: 'text',
        label: 'Spend',
        value: `$${costUsd.toFixed(2)}`,
      })
    }

    const inputTokens =
      readNumber(usage?.prompt_tokens ?? usage?.input_tokens) ?? 0
    const outputTokens =
      readNumber(usage?.completion_tokens ?? usage?.output_tokens) ?? 0
    if (inputTokens > 0 || outputTokens > 0) {
      lines.push({
        type: 'text',
        label: 'Tokens',
        value: `${((inputTokens + outputTokens) / 1_000_000).toFixed(2)}M total`,
      })
    }

    return {
      provider: 'openrouter',
      displayName: 'OpenRouter',
      status: 'ok',
      lines,
      updatedAt: now,
    }
  } catch (e) {
    return {
      provider: 'openrouter',
      displayName: 'OpenRouter',
      status: 'error',
      message: e instanceof Error ? e.message : String(e),
      lines: [],
      updatedAt: now,
    }
  }
}

// ── Aggregate ────────────────────────────────────────────────────────────────

const CACHE_TTL_MS = 30_000
let cache: { timestamp: number; payload: ProviderUsageResponse } | undefined

export async function getProviderUsage(
  force = false,
): Promise<ProviderUsageResponse> {
  const now = Date.now()
  if (!force && cache && now - cache.timestamp < CACHE_TTL_MS) {
    return cache.payload
  }

  const results = await Promise.allSettled([
    fetchClaudeUsage(),
    fetchCodexUsage(),
    fetchOpenAIUsage(),
    fetchOpenRouterUsage(),
  ])

  const providers: ProviderUsageResult[] = results.map((r, i) => {
    if (r.status === 'fulfilled') return r.value
    const names = ['Claude (OAuth)', 'Codex', 'OpenAI', 'OpenRouter']
    const ids = ['claude', 'codex', 'openai', 'openrouter']
    return {
      provider: ids[i],
      displayName: names[i],
      status: 'error' as const,
      message: r.reason instanceof Error ? r.reason.message : String(r.reason),
      lines: [],
      updatedAt: now,
    }
  })

  // Show all providers — unconfigured ones display setup instructions
  const activeProviders = providers

  const payload: ProviderUsageResponse = {
    ok: true,
    updatedAt: now,
    providers: activeProviders,
  }

  cache = { timestamp: now, payload }
  return payload
}
