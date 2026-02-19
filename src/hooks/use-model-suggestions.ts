/**
 * Phase 4.1: Smart Model Suggestions
 *
 * Client-side heuristics to suggest cheaper/better models
 * Opt-in only (requires settings toggle)
 */
import { useEffect, useState } from 'react'
import { useSettings } from './use-settings'

type ModelTier = 'budget' | 'balanced' | 'premium'

type Suggestion = {
  currentModel: string
  suggestedModel: string
  reason: string
  costImpact?: string
}

// Provider-specific model tiers (fallback when cost metadata unavailable)
const MODEL_TIERS: Record<string, Record<ModelTier, string[]>> = {
  anthropic: {
    budget: ['claude-3-5-haiku', 'claude-haiku'],
    balanced: ['claude-3-5-sonnet', 'claude-sonnet-4-5'],
    premium: ['claude-opus-4', 'claude-opus-4-5', 'claude-opus-4-6'],
  },
  openai: {
    budget: ['gpt-4o-mini'],
    balanced: ['gpt-4o', 'gpt-5.2-codex'],
    premium: ['o1', 'o1-preview'],
  },
  google: {
    budget: ['gemini-2.5-flash', 'gemini-1.5-flash'],
    balanced: ['gemini-2.5-pro', 'gemini-1.5-pro'],
    premium: ['gemini-2.0-flash-thinking'],
  },
}

type Message = {
  role: string
  content: string
  [key: string]: unknown
}

type SessionDismissal = {
  sessionKey: string
  timestamp: number
}

const GLOBAL_COOLDOWN_MS = 5 * 60 * 1000 // 5 minutes
const _AUTO_DISMISS_MS = 15 * 1000 // 15 seconds

function getModelTier(modelId: string): ModelTier {
  const normalized = modelId.toLowerCase()

  for (const tiers of Object.values(MODEL_TIERS)) {
    if (tiers.budget.some((m) => normalized.includes(m.toLowerCase())))
      return 'budget'
    if (tiers.balanced.some((m) => normalized.includes(m.toLowerCase())))
      return 'balanced'
    if (tiers.premium.some((m) => normalized.includes(m.toLowerCase())))
      return 'premium'
  }

  return 'balanced' // default
}

function getProvider(modelId: string): string | null {
  const normalized = modelId.toLowerCase()
  if (normalized.includes('claude')) return 'anthropic'
  if (normalized.includes('gpt') || normalized.includes('o1')) return 'openai'
  if (normalized.includes('gemini')) return 'google'
  return null
}

function findModelInTier(
  provider: string,
  tier: ModelTier,
  availableModels: string[],
): string | null {
  const providerTiers = MODEL_TIERS[provider]
  if (!providerTiers) return null

  const candidates = providerTiers[tier] || []
  for (const candidate of candidates) {
    const match = availableModels.find((m) =>
      m.toLowerCase().includes(candidate.toLowerCase()),
    )
    if (match) return match
  }

  return null
}

function isSimpleTask(messages: Message[]): boolean {
  const recent = messages.slice(-3)
  if (recent.length < 3) return false

  return recent.every((m) => {
    const content = String(m.content || '')
    return (
      content.length < 200 &&
      !content.includes('```') &&
      !content.match(/debug|error|fix|refactor|architect/i)
    )
  })
}

function isComplexTask(message: Message): boolean {
  const content = String(message.content || '')
  return (
    content.length > 500 ||
    content.includes('```') ||
    content.match(/architecture|design|debug|refactor|optimize|plan/i) !== null
  )
}

function getLastShownTimestamp(): number {
  try {
    const stored = localStorage.getItem('modelSuggestionLastShown')
    return stored ? parseInt(stored, 10) : 0
  } catch {
    return 0
  }
}

function setLastShownTimestamp() {
  try {
    localStorage.setItem('modelSuggestionLastShown', String(Date.now()))
  } catch {
    // Ignore
  }
}

function getSessionDismissals(): SessionDismissal[] {
  try {
    const stored = localStorage.getItem('modelSuggestionSessionDismissals')
    return stored ? JSON.parse(stored) : []
  } catch {
    return []
  }
}

function addSessionDismissal(sessionKey: string) {
  try {
    const dismissals = getSessionDismissals()
    dismissals.push({ sessionKey, timestamp: Date.now() })
    localStorage.setItem(
      'modelSuggestionSessionDismissals',
      JSON.stringify(dismissals),
    )
  } catch {
    // Ignore
  }
}

function isSessionDismissed(sessionKey: string): boolean {
  const dismissals = getSessionDismissals()
  return dismissals.some((d) => d.sessionKey === sessionKey)
}

export function useModelSuggestions({
  currentModel,
  sessionKey,
  messages,
  availableModels,
}: {
  currentModel: string
  sessionKey: string
  messages: Message[]
  availableModels: string[]
}) {
  const { settings } = useSettings()
  const [suggestion, setSuggestion] = useState<Suggestion | null>(null)

  useEffect(() => {
    // Feature disabled
    if (!settings.smartSuggestionsEnabled) {
      setSuggestion(null)
      return
    }

    // Fail closed: no suggestions if current model unknown
    if (!currentModel || currentModel.trim() === '') {
      setSuggestion(null)
      return
    }

    // Global cooldown active
    const lastShown = getLastShownTimestamp()
    if (Date.now() - lastShown < GLOBAL_COOLDOWN_MS) {
      return
    }

    // Session dismissed
    if (isSessionDismissed(sessionKey)) {
      return
    }

    // Not enough messages
    if (messages.length < 3) {
      return
    }

    const currentTier = getModelTier(currentModel)
    const provider = getProvider(currentModel)

    if (!provider) {
      return
    }

    // Check for downgrade opportunity (simple tasks on expensive model)
    if (
      (currentTier === 'balanced' || currentTier === 'premium') &&
      isSimpleTask(messages)
    ) {
      // Phase 4.2: Prefer user's preferred budget model
      let cheaperModel: string | null = null

      if (
        settings.preferredBudgetModel &&
        availableModels.includes(settings.preferredBudgetModel)
      ) {
        cheaperModel = settings.preferredBudgetModel
      } else {
        cheaperModel = findModelInTier(provider, 'budget', availableModels)
      }

      if (cheaperModel && cheaperModel !== currentModel) {
        setSuggestion({
          currentModel,
          suggestedModel: cheaperModel,
          reason: 'This chat seems lightweight',
          costImpact: 'Save ~80% per message',
        })
        setLastShownTimestamp()
        return
      }
    }

    // Check for upgrade opportunity (complex task on weak model)
    // Phase 4.2: Skip if "Only suggest cheaper" is enabled
    if (!settings.onlySuggestCheaper) {
      const lastMessage = messages[messages.length - 1]
      if (lastMessage && isComplexTask(lastMessage)) {
        let targetTier: ModelTier | null = null

        if (currentTier === 'budget') targetTier = 'balanced'
        else if (currentTier === 'balanced') targetTier = 'premium'

        if (targetTier) {
          // Phase 4.2: Prefer user's preferred premium model
          let betterModel: string | null = null

          if (
            settings.preferredPremiumModel &&
            availableModels.includes(settings.preferredPremiumModel)
          ) {
            betterModel = settings.preferredPremiumModel
          } else {
            betterModel = findModelInTier(provider, targetTier, availableModels)
          }

          if (betterModel && betterModel !== currentModel) {
            setSuggestion({
              currentModel,
              suggestedModel: betterModel,
              reason: 'This looks complex',
              costImpact:
                targetTier === 'premium'
                  ? 'Better quality (2x cost)'
                  : 'Better quality',
            })
            setLastShownTimestamp()
            return
          }
        }
      }
    }
  }, [
    currentModel,
    sessionKey,
    messages,
    availableModels,
    settings.smartSuggestionsEnabled,
  ])

  const dismiss = () => {
    setSuggestion(null)
  }

  const dismissForSession = () => {
    addSessionDismissal(sessionKey)
    setSuggestion(null)
  }

  return {
    suggestion,
    dismiss,
    dismissForSession,
  }
}

// Preserve for future auto-dismiss feature
void _AUTO_DISMISS_MS
