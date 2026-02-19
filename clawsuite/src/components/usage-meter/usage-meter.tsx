'use client'

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { UsageDetailsModal } from './usage-details-modal'
import { ContextAlertModal } from './context-alert-modal'
import { DialogContent, DialogRoot } from '@/components/ui/dialog'
import {
  MenuContent,
  MenuItem,
  MenuRoot,
  MenuTrigger,
} from '@/components/ui/menu'
import { cn } from '@/lib/utils'
import { toast } from '@/components/ui/toast'
import { SEARCH_MODAL_EVENTS } from '@/hooks/use-search-modal'

const POLL_INTERVAL_MS = 10_000
const PROVIDER_POLL_INTERVAL_MS = 30_000
const STORAGE_KEY = 'clawsuite-usage-meter-alerts'
const STATS_VIEW_STORAGE_KEY = 'clawsuite-stats-view'
const THRESHOLDS = [50, 75, 90]

type StatsView = 'session' | 'provider' | 'cost' | 'agents'

const STATS_VIEW_LABELS: Record<StatsView, string> = {
  session: 'Session Stats',
  provider: 'Provider Usage',
  cost: 'Cost Breakdown',
  agents: 'Agent Activity',
}

const PREFERRED_PROVIDER_KEY = 'clawsuite-preferred-provider'

function getStoredPreferredProvider(): string | null {
  if (typeof window === 'undefined') return null
  try {
    return window.localStorage.getItem(PREFERRED_PROVIDER_KEY)
  } catch {
    return null
  }
}

function savePreferredProvider(provider: string) {
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(PREFERRED_PROVIDER_KEY, provider)
  } catch {
    /* ignore */
  }
}

function getStoredStatsView(): StatsView {
  if (typeof window === 'undefined') return 'session'
  try {
    const stored = window.localStorage.getItem(STATS_VIEW_STORAGE_KEY)
    if (stored && ['session', 'provider', 'cost', 'agents'].includes(stored)) {
      return stored as StatsView
    }
  } catch {
    /* ignore */
  }
  return 'session'
}

function saveStatsView(view: StatsView) {
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(STATS_VIEW_STORAGE_KEY, view)
  } catch {
    /* ignore */
  }
}

const MODEL_PRICING: Record<string, { input: number; output: number }> = {
  'gpt-4o': { input: 5, output: 15 },
  'gpt-4o-mini': { input: 0.15, output: 0.6 },
  'gpt-4.1': { input: 3, output: 15 },
  'gpt-4.1-mini': { input: 0.3, output: 1.2 },
  'gpt-4.1-nano': { input: 0.1, output: 0.4 },
  o1: { input: 15, output: 60 },
  'o1-mini': { input: 3, output: 12 },
  'o3-mini': { input: 1.1, output: 4.4 },
  'claude-3.5-sonnet': { input: 3, output: 15 },
  'claude-3.5-haiku': { input: 0.8, output: 4 },
  'claude-3-opus': { input: 15, output: 75 },
}

type UsageSummary = {
  inputTokens: number
  outputTokens: number
  contextPercent: number
  dailyCost: number
  models: Array<ModelUsage>
  sessions: Array<SessionUsage>
}

type ModelUsage = {
  model: string
  inputTokens: number
  outputTokens: number
  costUsd: number
}

type SessionUsage = {
  id: string
  model: string
  inputTokens: number
  outputTokens: number
  costUsd: number
  startedAt?: number
  updatedAt?: number
}

type SessionStatusResponse = {
  ok?: boolean
  payload?: unknown
  error?: string
}

type UsageLine = {
  type: 'progress' | 'text' | 'badge'
  label: string
  used?: number
  limit?: number
  format?: 'percent' | 'dollars' | 'tokens'
  value?: string
  color?: string
  resetsAt?: string
}

type ProviderUsageEntry = {
  provider: string
  displayName: string
  status: 'ok' | 'missing_credentials' | 'auth_expired' | 'error'
  message?: string
  plan?: string
  lines: Array<UsageLine>
  updatedAt: number
}

function getTodayKey() {
  const now = new Date()
  return now.toISOString().slice(0, 10)
}

function readNumber(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string') {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) return parsed
  }
  return 0
}

function readPercent(value: unknown): number {
  const num = readNumber(value)
  if (num <= 1 && num > 0) return num * 100
  return num
}

function resolvePricing(
  model: string,
): { input: number; output: number } | null {
  const key = model.trim().toLowerCase()
  return MODEL_PRICING[key] ?? null
}

function calculateCost(
  model: string,
  inputTokens: number,
  outputTokens: number,
): number {
  const pricing = resolvePricing(model)
  if (!pricing) return 0
  return (
    (inputTokens / 1_000_000) * pricing.input +
    (outputTokens / 1_000_000) * pricing.output
  )
}

function normalizeModelUsage(raw: unknown): Array<ModelUsage> {
  if (!raw) return []
  if (Array.isArray(raw)) {
    return raw
      .map((entry) => {
        if (!entry || typeof entry !== 'object') return null
        const model = String(entry.model ?? entry.id ?? '')
        if (!model) return null
        const inputTokens = readNumber(
          entry.inputTokens ??
            entry.input_tokens ??
            entry.promptTokens ??
            entry.prompt_tokens,
        )
        const outputTokens = readNumber(
          entry.outputTokens ??
            entry.output_tokens ??
            entry.completionTokens ??
            entry.completion_tokens,
        )
        const costProvided = readNumber(
          entry.costUsd ?? entry.cost ?? entry.usd,
        )
        const costUsd =
          costProvided > 0
            ? costProvided
            : calculateCost(model, inputTokens, outputTokens)
        return { model, inputTokens, outputTokens, costUsd }
      })
      .filter(Boolean) as Array<ModelUsage>
  }

  if (typeof raw === 'object') {
    return Object.entries(raw as Record<string, unknown>)
      .map(([model, data]) => {
        if (!data || typeof data !== 'object') return null
        const inputTokens = readNumber(
          (data as any).inputTokens ??
            (data as any).input_tokens ??
            (data as any).promptTokens ??
            (data as any).prompt_tokens,
        )
        const outputTokens = readNumber(
          (data as any).outputTokens ??
            (data as any).output_tokens ??
            (data as any).completionTokens ??
            (data as any).completion_tokens,
        )
        const costProvided = readNumber(
          (data as any).costUsd ?? (data as any).cost ?? (data as any).usd,
        )
        const costUsd =
          costProvided > 0
            ? costProvided
            : calculateCost(model, inputTokens, outputTokens)
        return { model, inputTokens, outputTokens, costUsd }
      })
      .filter(Boolean) as Array<ModelUsage>
  }

  return []
}

function normalizeSessions(raw: unknown): Array<SessionUsage> {
  if (!Array.isArray(raw)) return []
  return raw
    .map((entry) => {
      if (!entry || typeof entry !== 'object') return null
      const model = String(entry.model ?? entry.provider ?? '')
      const id = String(
        entry.id ?? entry.key ?? entry.sessionId ?? entry.sessionKey ?? '',
      )
      if (!id && !model) return null
      const inputTokens = readNumber(
        entry.inputTokens ??
          entry.input_tokens ??
          entry.promptTokens ??
          entry.prompt_tokens,
      )
      const outputTokens = readNumber(
        entry.outputTokens ??
          entry.output_tokens ??
          entry.completionTokens ??
          entry.completion_tokens,
      )
      const costProvided = readNumber(entry.costUsd ?? entry.cost ?? entry.usd)
      const costUsd =
        costProvided > 0
          ? costProvided
          : calculateCost(model, inputTokens, outputTokens)
      const startedAt = readNumber(
        entry.startedAt ??
          entry.started_at ??
          entry.createdAt ??
          entry.created_at,
      )
      const updatedAt = readNumber(
        entry.updatedAt ??
          entry.updated_at ??
          entry.lastUpdated ??
          entry.last_updated,
      )
      return {
        id: id || model || 'session',
        model: model || 'unknown',
        inputTokens,
        outputTokens,
        costUsd,
        startedAt: startedAt || undefined,
        updatedAt: updatedAt || undefined,
      }
    })
    .filter(Boolean) as Array<SessionUsage>
}

function parseSessionStatus(payload: unknown): UsageSummary {
  const root = payload && typeof payload === 'object' ? (payload as any) : {}
  const usage = root.today ?? root.usage ?? root.summary ?? root.totals ?? root

  const tokensRoot = usage?.tokens ?? usage?.tokenUsage ?? usage
  const inputTokens = readNumber(
    tokensRoot?.inputTokens ??
      tokensRoot?.input_tokens ??
      tokensRoot?.promptTokens ??
      tokensRoot?.prompt_tokens ??
      usage?.inputTokens ??
      usage?.input_tokens ??
      usage?.promptTokens ??
      usage?.prompt_tokens,
  )
  const outputTokens = readNumber(
    tokensRoot?.outputTokens ??
      tokensRoot?.output_tokens ??
      tokensRoot?.completionTokens ??
      tokensRoot?.completion_tokens ??
      usage?.outputTokens ??
      usage?.output_tokens ??
      usage?.completionTokens ??
      usage?.completion_tokens,
  )
  const contextPercent = readPercent(
    usage?.contextPercent ??
      usage?.context_percent ??
      usage?.context ??
      root?.contextPercent ??
      root?.context_percent,
  )

  const modelUsage = normalizeModelUsage(
    root.models ?? root.modelUsage ?? root.usageByModel ?? usage?.models,
  )

  const sessions = normalizeSessions(root.sessions ?? root.history ?? [])

  const baseModel =
    String(root.model ?? usage?.model ?? '').trim() ||
    (modelUsage[0]?.model ?? 'unknown')
  const dailyCostProvided = readNumber(
    usage?.costUsd ??
      usage?.dailyCost ??
      usage?.cost ??
      root?.costUsd ??
      root?.dailyCost ??
      root?.cost,
  )
  const dailyCostFromModels = modelUsage.reduce(
    (sum, model) => sum + model.costUsd,
    0,
  )
  const dailyCostFromTokens =
    baseModel === 'unknown'
      ? 0
      : calculateCost(baseModel, inputTokens, outputTokens)
  const dailyCost =
    dailyCostProvided > 0
      ? dailyCostProvided
      : dailyCostFromModels > 0
        ? dailyCostFromModels
        : dailyCostFromTokens

  const models =
    modelUsage.length > 0
      ? modelUsage
      : baseModel !== 'unknown'
        ? [
            {
              model: baseModel,
              inputTokens,
              outputTokens,
              costUsd: dailyCost,
            },
          ]
        : []

  return {
    inputTokens,
    outputTokens,
    contextPercent,
    dailyCost,
    models,
    sessions,
  }
}

function formatTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}m`
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`
  return Math.round(value).toString()
}

function formatCurrency(value: number): string {
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    maximumFractionDigits: value >= 10 ? 2 : 3,
  }).format(value)
}

function getAlertState() {
  if (typeof window === 'undefined') {
    return { date: getTodayKey(), sent: {} as Record<number, boolean> }
  }
  const today = getTodayKey()
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return { date: today, sent: {} as Record<number, boolean> }
    const parsed = JSON.parse(raw) as {
      date?: string
      sent?: Record<number, boolean>
    }
    if (parsed.date !== today) {
      return { date: today, sent: {} as Record<number, boolean> }
    }
    return {
      date: today,
      sent: parsed.sent ?? ({} as Record<number, boolean>),
    }
  } catch {
    return { date: today, sent: {} as Record<number, boolean> }
  }
}

function saveAlertState(state: {
  date: string
  sent: Record<number, boolean>
}) {
  if (typeof window === 'undefined') return
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state))
}

type AgentActivity = {
  activeAgents: number
  totalSpawned: number
  totalAgentCost: number
}

export function UsageMeter() {
  const [usage, setUsage] = useState<UsageSummary>(() =>
    parseSessionStatus(null),
  )
  const [error, setError] = useState<string | null>(null)
  const [providerUsage, setProviderUsage] = useState<Array<ProviderUsageEntry>>(
    [],
  )
  const [providerError, setProviderError] = useState<string | null>(null)
  const [providerUpdatedAt, setProviderUpdatedAt] = useState<number | null>(
    null,
  )
  const [open, setOpen] = useState(false)
  const [statsView, setStatsView] = useState<StatsView>(getStoredStatsView)
  const [agentActivity] = useState<AgentActivity>({
    activeAgents: 0,
    totalSpawned: 0,
    totalAgentCost: 0,
  })
  const [contextAlert, setContextAlert] = useState<{
    open: boolean
    threshold: number
  }>({ open: false, threshold: 0 })
  const alertStateRef = useRef(getAlertState())

  const refresh = useCallback(async () => {
    try {
      const res = await fetch('/api/session-status')
      if (!res.ok) {
        const data = await res.json().catch(() => null)
        throw new Error(
          data?.error || data?.message || res.statusText || 'Request failed',
        )
      }
      const data = (await res.json()) as SessionStatusResponse
      const payload = data.payload ?? data
      const parsed = parseSessionStatus(payload)
      setUsage(parsed)
      setError(null)
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err)
      setError(errorMessage)
      toast('Failed to fetch usage data', { type: 'error' })
    }
  }, [])

  const refreshProviders = useCallback(async () => {
    try {
      const res = await fetch('/api/provider-usage')
      const data = (await res.json().catch(() => null)) as {
        ok?: boolean
        providers?: Array<ProviderUsageEntry>
        updatedAt?: number
        error?: string
      } | null

      if (!res.ok || data?.ok === false) {
        throw new Error(data?.error || res.statusText || 'Request failed')
      }

      setProviderUsage(data?.providers ?? [])
      setProviderUpdatedAt(data?.updatedAt ?? Date.now())
      setProviderError(null)
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err)
      setProviderError(errorMessage)
      toast('Failed to fetch provider usage', { type: 'error' })
    }
  }, [])

  // Agent activity API doesn't exist yet — disabled to prevent 404 spam
  const refreshAgentActivity = useCallback(async () => {}, [])

  useEffect(() => {
    let active = true
    void refresh()
    const interval = window.setInterval(() => {
      if (!active) return
      void refresh()
    }, POLL_INTERVAL_MS)
    return () => {
      active = false
      window.clearInterval(interval)
    }
  }, [refresh])

  useEffect(() => {
    let active = true
    void refreshProviders()
    const interval = window.setInterval(() => {
      if (!active) return
      void refreshProviders()
    }, PROVIDER_POLL_INTERVAL_MS)
    return () => {
      active = false
      window.clearInterval(interval)
    }
  }, [refreshProviders])

  useEffect(() => {
    let active = true
    void refreshAgentActivity()
    const interval = window.setInterval(() => {
      if (!active) return
      void refreshAgentActivity()
    }, POLL_INTERVAL_MS)
    return () => {
      active = false
      window.clearInterval(interval)
    }
  }, [refreshAgentActivity])

  useEffect(() => {
    if (typeof window === 'undefined') return
    const current = usage.contextPercent
    if (!Number.isFinite(current)) return
    const state = alertStateRef.current
    if (state.date !== getTodayKey()) {
      state.date = getTodayKey()
      state.sent = {}
    }
    const eligible = THRESHOLDS.filter((threshold) => current >= threshold)
    if (eligible.length === 0) return
    for (const threshold of eligible) {
      if (state.sent[threshold]) continue
      state.sent[threshold] = true
      saveAlertState(state)
      // Show in-app modal instead of browser notification
      setContextAlert({ open: true, threshold })
      break // Only show one alert at a time
    }
  }, [usage.contextPercent])

  useEffect(() => {
    function handleOpenUsageFromSearch() {
      setOpen(true)
    }

    window.addEventListener(
      SEARCH_MODAL_EVENTS.OPEN_USAGE,
      handleOpenUsageFromSearch,
    )
    return () => {
      window.removeEventListener(
        SEARCH_MODAL_EVENTS.OPEN_USAGE,
        handleOpenUsageFromSearch,
      )
    }
  }, [])

  // Find the preferred provider for the status bar display
  const [preferredProvider, setPreferredProvider] = useState<string | null>(
    getStoredPreferredProvider,
  )
  const primaryProvider = useMemo(() => {
    if (preferredProvider) {
      const preferred = providerUsage.find(
        (p) =>
          p.provider === preferredProvider &&
          p.status === 'ok' &&
          p.lines.length > 0,
      )
      if (preferred) return preferred
    }
    return providerUsage.find((p) => p.status === 'ok' && p.lines.length > 0)
  }, [providerUsage, preferredProvider])
  const providerProgressLines =
    primaryProvider?.lines.filter((l) => l.type === 'progress') ?? []

  // Aggregate provider tokens
  const providerTokens = useMemo(() => {
    const byProvider: Record<string, { input: number; output: number }> = {}
    providerUsage.forEach((p) => {
      if (p.status !== 'ok') return
      const tokenLines = p.lines.filter(
        (l) => l.format === 'tokens' && l.used !== undefined,
      )
      const total = tokenLines.reduce((sum, l) => sum + (l.used ?? 0), 0)
      if (total > 0) {
        byProvider[p.displayName.split(' ')[0]] = { input: total, output: 0 }
      }
    })
    return byProvider
  }, [providerUsage])

  // Compute pill color based on context percent or agent activity
  const alertTone = (() => {
    if (statsView === 'agents') {
      if (agentActivity.activeAgents > 5)
        return 'text-amber-600 bg-amber-100 border-amber-200'
      if (agentActivity.activeAgents > 0)
        return 'text-emerald-600 bg-emerald-100 border-emerald-200'
      return 'text-primary-600 bg-primary-50 border-primary-200'
    }
    if (statsView === 'provider' && primaryProvider) {
      const allProgress = primaryProvider.lines.filter(
        (l) =>
          l.type === 'progress' &&
          l.format === 'percent' &&
          l.used !== undefined,
      )
      const maxPct = allProgress.reduce(
        (max, l) => Math.max(max, l.used ?? 0),
        0,
      )
      if (maxPct >= 75) return 'text-red-600 bg-red-100 border-red-200'
      if (maxPct >= 50) return 'text-amber-600 bg-amber-100 border-amber-200'
      return 'text-emerald-600 bg-emerald-100 border-emerald-200'
    }
    const value = usage.contextPercent
    if (value >= 75) return 'text-red-600 bg-red-100 border-red-200'
    if (value >= 50) return 'text-amber-600 bg-amber-100 border-amber-200'
    return 'text-amber-600 bg-amber-100 border-amber-200'
  })()

  const handleSetPreferredProvider = useCallback((provider: string) => {
    setPreferredProvider(provider)
    savePreferredProvider(provider)
  }, [])

  const detailProps = useMemo(
    () => ({
      usage,
      error,
      providerUsage,
      providerError,
      providerUpdatedAt,
      onRefreshProviders: refreshProviders,
      preferredProvider,
      onSetPreferredProvider: handleSetPreferredProvider,
    }),
    [
      error,
      providerError,
      providerUpdatedAt,
      providerUsage,
      usage,
      refreshProviders,
      preferredProvider,
      handleSetPreferredProvider,
    ],
  )

  const handleStatsViewChange = (view: StatsView) => {
    setStatsView(view)
    saveStatsView(view)
  }

  // Render pill content based on stats view
  const renderPillContent = () => {
    switch (statsView) {
      case 'session':
        return (
          <>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                In
              </span>
              <span>{formatTokens(usage.inputTokens)}</span>
            </div>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                Out
              </span>
              <span>{formatTokens(usage.outputTokens)}</span>
            </div>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                Ctx
              </span>
              <span>{Math.round(usage.contextPercent)}%</span>
            </div>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                Cost
              </span>
              <span>{formatCurrency(usage.dailyCost)}</span>
            </div>
          </>
        )

      case 'provider': {
        if (primaryProvider) {
          return (
            <>
              <div className="flex items-center gap-1">
                <span className="text-[10px] uppercase tracking-wide text-primary-600">
                  {primaryProvider.displayName.split(' ')[0]}
                </span>
                {primaryProvider.plan && (
                  <span className="text-[9px] uppercase text-primary-500">
                    {primaryProvider.plan}
                  </span>
                )}
              </div>
              {providerProgressLines.slice(0, 3).map((line, i) => (
                <div
                  key={`${line.label}-${i}`}
                  className="flex items-center gap-1"
                >
                  <span className="text-[10px] uppercase tracking-wide text-primary-600">
                    {line.label
                      .replace('Session (5h)', 'Sess')
                      .replace('Weekly', 'Wk')
                      .replace('Sonnet', 'Son')}
                  </span>
                  <span>
                    {line.format === 'dollars' && line.used !== undefined
                      ? `$${line.used >= 1000 ? `${(line.used / 1000).toFixed(1)}k` : line.used.toFixed(0)}`
                      : line.used !== undefined
                        ? `${Math.round(line.used)}%`
                        : '—'}
                  </span>
                </div>
              ))}
            </>
          )
        }
        // Fallback: show aggregated tokens by provider
        const providers = Object.entries(providerTokens)
        if (providers.length > 0) {
          return (
            <>
              {providers.slice(0, 3).map(([name, tokens]) => (
                <div key={name} className="flex items-center gap-1">
                  <span className="text-[10px] uppercase tracking-wide text-primary-600">
                    {name}
                  </span>
                  <span>{formatTokens(tokens.input)}</span>
                </div>
              ))}
            </>
          )
        }
        return (
          <span className="text-[10px] text-primary-500">No provider data</span>
        )
      }

      case 'cost': {
        if (usage.models.length > 0) {
          const sortedModels = [...usage.models].sort(
            (a, b) => b.costUsd - a.costUsd,
          )
          return (
            <>
              {sortedModels.slice(0, 3).map((model) => (
                <div key={model.model} className="flex items-center gap-1">
                  <span className="text-[10px] uppercase tracking-wide text-primary-600">
                    {model.model
                      .replace('claude-', '')
                      .replace('gpt-', '')
                      .slice(0, 8)}
                  </span>
                  <span>{formatCurrency(model.costUsd)}</span>
                </div>
              ))}
              {usage.models.length > 3 && (
                <span className="text-[10px] text-primary-500">
                  +{usage.models.length - 3}
                </span>
              )}
            </>
          )
        }
        return (
          <div className="flex items-center gap-1">
            <span className="text-[10px] uppercase tracking-wide text-primary-600">
              Total
            </span>
            <span>{formatCurrency(usage.dailyCost)}</span>
          </div>
        )
      }

      case 'agents':
        return (
          <>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                Active
              </span>
              <span>{agentActivity.activeAgents}</span>
            </div>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                Spawned
              </span>
              <span>{agentActivity.totalSpawned}</span>
            </div>
            <div className="flex items-center gap-1">
              <span className="text-[10px] uppercase tracking-wide text-primary-600">
                Cost
              </span>
              <span>{formatCurrency(agentActivity.totalAgentCost)}</span>
            </div>
          </>
        )

      default:
        return null
    }
  }

  return (
    <>
      <MenuRoot>
        <MenuTrigger
          className={cn(
            'ml-auto rounded-full border px-3 py-1 text-xs font-medium',
            'flex items-center gap-3 transition hover:bg-primary-100 cursor-pointer',
            alertTone,
          )}
          data-tour="usage-meter"
        >
          <span className="text-[9px] uppercase tracking-widest text-primary-500 opacity-75">
            {STATS_VIEW_LABELS[statsView].split(' ')[0]}
          </span>
          <span className="text-primary-300">|</span>
          {renderPillContent()}
        </MenuTrigger>
        <MenuContent align="end" className="min-w-[180px]">
          {(['session', 'provider', 'cost', 'agents'] as const).map((view) => (
            <MenuItem
              key={view}
              onClick={() => handleStatsViewChange(view)}
              className={cn(
                statsView === view && 'bg-amber-100 text-amber-800',
              )}
            >
              <span className="flex-1">{STATS_VIEW_LABELS[view]}</span>
              {statsView === view && <span className="text-amber-600">✓</span>}
            </MenuItem>
          ))}
          <div className="my-1 h-px bg-primary-100" />
          <MenuItem onClick={() => setOpen(true)}>View Details…</MenuItem>
        </MenuContent>
      </MenuRoot>

      <DialogRoot open={open} onOpenChange={setOpen}>
        <DialogContent className="w-[min(720px,94vw)]">
          <UsageDetailsModal {...detailProps} />
        </DialogContent>
      </DialogRoot>

      <ContextAlertModal
        open={contextAlert.open}
        onClose={() => setContextAlert({ open: false, threshold: 0 })}
        threshold={contextAlert.threshold}
        contextPercent={usage.contextPercent}
      />
    </>
  )
}
