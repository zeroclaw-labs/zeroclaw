import {
  ArrowLeft01Icon,
  Cancel01Icon,
  Copy01Icon,
  Link01Icon,
  Tick02Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useState } from 'react'
import {
  OPENCLAW_CONFIG_PATH,
  PROVIDER_CATALOG,
  buildConfigExample,
  getAuthTypeLabel,
  getProviderInfo,
} from '@/lib/provider-catalog'
import type { ProviderAuthType } from '@/lib/provider-catalog'
import { Button } from '@/components/ui/button'
import {
  DialogContent,
  DialogDescription,
  DialogRoot,
  DialogTitle,
} from '@/components/ui/dialog'
import { cn } from '@/lib/utils'
import { ProviderIcon } from './provider-icon'

type WizardStep = 'provider' | 'auth' | 'instructions' | 'verify'
type CopyState = 'idle' | 'copied' | 'failed'
type SaveState = 'idle' | 'saving' | 'saved' | 'error'

type ProviderWizardProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type StepItem = {
  id: WizardStep
  label: string
}

type AuthTypeMeta = {
  title: string
  description: string
}

const WIZARD_STEPS: Array<StepItem> = [
  { id: 'provider', label: 'Choose Provider' },
  { id: 'auth', label: 'Choose Auth' },
  { id: 'instructions', label: 'Config Instructions' },
  { id: 'verify', label: 'Verify' },
]

const AUTH_TYPE_ORDER: Array<ProviderAuthType> = [
  'api-key',
  'cli-token',
  'oauth',
  'local',
]

function getAuthTypeMeta(authType: ProviderAuthType): AuthTypeMeta {
  if (authType === 'api-key') {
    return {
      title: 'API Key',
      description: 'Paste your API key — saved directly to local config',
    }
  }

  if (authType === 'cli-token') {
    return {
      title: 'CLI Token',
      description:
        'Use your existing Claude CLI auth token (from Claude Code / claude.ai)',
    }
  }

  if (authType === 'oauth') {
    return {
      title: 'OAuth',
      description: 'Sign in via browser — launches OAuth flow automatically',
    }
  }

  return {
    title: 'Local',
    description: 'No auth needed (Ollama)',
  }
}

function getStepIndex(step: WizardStep): number {
  return WIZARD_STEPS.findIndex(function findStep(item) {
    return item.id === step
  })
}

export function ProviderWizard({ open, onOpenChange }: ProviderWizardProps) {
  const [step, setStep] = useState<WizardStep>('provider')
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(
    null,
  )
  const [selectedAuthType, setSelectedAuthType] =
    useState<ProviderAuthType | null>(null)
  const [copyState, setCopyState] = useState<CopyState>('idle')
  const [saveState, setSaveState] = useState<SaveState>('idle')
  const [saveError, setSaveError] = useState('')
  const [apiKeyInput, setApiKeyInput] = useState('')
  const [showManualSnippet, setShowManualSnippet] = useState(false)
  const [verificationMessage, setVerificationMessage] = useState('')

  const currentStepIndex = getStepIndex(step)
  const selectedProvider = selectedProviderId
    ? getProviderInfo(selectedProviderId)
    : null
  const configExample =
    selectedProvider && selectedAuthType
      ? buildConfigExample(selectedProvider, selectedAuthType)
      : ''

  function resetState() {
    setStep('provider')
    setSelectedProviderId(null)
    setSelectedAuthType(null)
    setCopyState('idle')
    setSaveState('idle')
    setSaveError('')
    setApiKeyInput('')
    setShowManualSnippet(false)
    setVerificationMessage('')
  }

  function handleDialogOpenChange(nextOpen: boolean) {
    onOpenChange(nextOpen)
    if (!nextOpen) {
      resetState()
    }
  }

  function handleSelectProvider(providerId: string) {
    setSelectedProviderId(providerId)
    setSelectedAuthType(null)
    setCopyState('idle')
    setVerificationMessage('')
    setStep('auth')
  }

  function handleChooseAuthType(authType: ProviderAuthType) {
    setSelectedAuthType(authType)
    setCopyState('idle')
    setVerificationMessage('')
    setStep('instructions')
  }

  async function handleCopyConfig() {
    if (!configExample) return

    try {
      await navigator.clipboard.writeText(configExample)
      setCopyState('copied')
    } catch {
      setCopyState('failed')
    }
  }

  async function handleSaveApiKey() {
    if (!selectedProvider || !apiKeyInput.trim()) return

    setSaveState('saving')
    setSaveError('')

    const profileKey = `${selectedProvider.id}:default`
    const patch = {
      auth: {
        profiles: {
          [profileKey]: {
            provider: selectedProvider.id,
            apiKey: apiKeyInput.trim(),
          },
        },
      },
    }

    try {
      const res = await fetch('/api/config-patch', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          raw: JSON.stringify(patch, null, 2),
          reason: `Studio: add ${selectedProvider.name} API key`,
        }),
      })

      const data = (await res.json()) as { ok: boolean; error?: string }

      if (!data.ok) {
        setSaveState('error')
        setSaveError(data.error || 'Failed to save config')
        return
      }

      setSaveState('saved')
      setVerificationMessage(
        `${selectedProvider.name} API key saved to config. Gateway will restart to apply changes.`,
      )
      setStep('verify')
    } catch (err) {
      setSaveState('error')
      setSaveError(err instanceof Error ? err.message : 'Network error')
    }
  }

  // Keep for potential future use with manual config verification
  void function _handleStartVerification() {
    setVerificationMessage(
      'Verification not yet implemented — restart Gateway to apply changes.',
    )
    setStep('verify')
  }

  function handleDone() {
    onOpenChange(false)
    resetState()
  }

  return (
    <DialogRoot open={open} onOpenChange={handleDialogOpenChange}>
      <DialogContent className="left-auto right-0 top-0 h-[100dvh] w-screen translate-x-0 translate-y-0 overflow-hidden rounded-none border-primary-200 bg-primary-50/95 backdrop-blur-sm duration-300 ease-out sm:w-[min(860px,100vw)] sm:rounded-l-2xl data-[state=open]:scale-100 data-[state=closed]:scale-100 data-[state=open]:translate-x-0 data-[state=closed]:translate-x-full">
        <div className="flex h-full min-h-0 flex-col">
          <div className="border-b border-primary-200 p-4 sm:p-5">
            <div className="flex items-start justify-between gap-4">
              <div className="space-y-1">
                <DialogTitle className="text-balance">
                  Provider Setup Wizard
                </DialogTitle>
                <DialogDescription className="text-pretty">
                  Add provider credentials safely. API keys stay local in your
                  OpenClaw config file and are never sent to Studio.
                </DialogDescription>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                onClick={function onClose() {
                  handleDialogOpenChange(false)
                }}
                aria-label="Close provider setup wizard"
              >
                <HugeiconsIcon
                  icon={Cancel01Icon}
                  size={20}
                  strokeWidth={1.5}
                />
              </Button>
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4 sm:px-5 sm:pb-5">
            <ol className="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-4">
              {WIZARD_STEPS.map(function mapStep(item, index) {
                const isComplete = index < currentStepIndex
                const isCurrent = index === currentStepIndex

                return (
                  <li
                    key={item.id}
                    className={cn(
                      'rounded-xl border px-2.5 py-2',
                      isCurrent && 'border-primary-400 bg-primary-100/70',
                      isComplete && 'border-green-500/30 bg-green-500/10',
                      !isCurrent &&
                        !isComplete &&
                        'border-primary-200 bg-primary-50',
                    )}
                  >
                    <div className="flex items-center gap-2">
                      <span
                        className={cn(
                          'inline-flex size-5 items-center justify-center rounded-full border text-xs font-medium tabular-nums',
                          isCurrent && 'border-primary-500 text-primary-800',
                          isComplete && 'border-green-500/40 text-green-600',
                          !isCurrent &&
                            !isComplete &&
                            'border-primary-300 text-primary-600',
                        )}
                      >
                        {index + 1}
                      </span>
                      <span className="truncate text-xs font-medium text-primary-800">
                        {item.label}
                      </span>
                    </div>
                  </li>
                )
              })}
            </ol>

            {step === 'provider' ? (
              <section className="mt-5">
                <h3 className="text-base font-medium text-primary-900 text-balance">
                  Step 1: Choose Provider
                </h3>
                <p className="mt-1 text-sm text-primary-600 text-pretty">
                  Select the provider you want to configure.
                </p>

                <div className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                  {PROVIDER_CATALOG.map(function mapProvider(provider) {
                    return (
                      <button
                        key={provider.id}
                        type="button"
                        onClick={function onSelectProvider() {
                          handleSelectProvider(provider.id)
                        }}
                        className="rounded-2xl border border-primary-200 bg-primary-50/70 p-3 text-left transition-colors hover:border-primary-400 hover:bg-primary-100/70"
                      >
                        <div className="flex items-center gap-2.5">
                          <span className="inline-flex size-9 items-center justify-center rounded-xl border border-primary-200 bg-primary-100/70">
                            <ProviderIcon providerId={provider.id} />
                          </span>
                          <h4 className="text-sm font-medium text-primary-900 text-balance">
                            {provider.name}
                          </h4>
                        </div>

                        <p className="mt-2 text-xs text-primary-600 text-pretty line-clamp-2">
                          {provider.description}
                        </p>

                        <div className="mt-3 flex flex-wrap gap-1.5">
                          {provider.authTypes.map(function mapAuth(authType) {
                            return (
                              <span
                                key={authType}
                                className="rounded-full border border-primary-300 bg-primary-100 px-2 py-0.5 text-xs text-primary-700"
                              >
                                {getAuthTypeLabel(authType)}
                              </span>
                            )
                          })}
                        </div>
                      </button>
                    )
                  })}
                </div>
              </section>
            ) : null}

            {step === 'auth' && selectedProvider ? (
              <section className="mt-5">
                <h3 className="text-base font-medium text-primary-900 text-balance">
                  Step 2: Choose Auth Type
                </h3>
                <p className="mt-1 text-sm text-primary-600 text-pretty">
                  {selectedProvider.name} supports{' '}
                  {selectedProvider.authTypes
                    .map(function mapAuthType(authType) {
                      return getAuthTypeLabel(authType)
                    })
                    .join(', ')}
                  .
                </p>

                <div className="mt-3 rounded-xl border border-primary-200 bg-primary-100/70 px-3 py-2">
                  <p className="text-xs text-primary-700 text-pretty">
                    Config file path:{' '}
                    <code className="font-mono">{OPENCLAW_CONFIG_PATH}</code>
                  </p>
                </div>

                <div className="mt-4 grid gap-3 sm:grid-cols-3">
                  {AUTH_TYPE_ORDER.map(function mapAuthType(authType) {
                    const meta = getAuthTypeMeta(authType)
                    const supported =
                      selectedProvider.authTypes.includes(authType)

                    return (
                      <button
                        key={authType}
                        type="button"
                        disabled={!supported}
                        onClick={function onChooseAuthType() {
                          handleChooseAuthType(authType)
                        }}
                        className={cn(
                          'rounded-2xl border p-3 text-left transition-colors',
                          supported
                            ? 'border-primary-200 bg-primary-50/70 hover:border-primary-400 hover:bg-primary-100/80'
                            : 'cursor-not-allowed border-primary-200 bg-primary-50/40 opacity-50',
                        )}
                      >
                        <h4 className="text-sm font-medium text-primary-900 text-balance">
                          {meta.title}
                        </h4>
                        <p className="mt-1 text-xs text-primary-600 text-pretty">
                          {meta.description}
                        </p>
                        {!supported ? (
                          <p className="mt-2 text-xs text-primary-500 text-pretty">
                            Not supported by {selectedProvider.name}.
                          </p>
                        ) : null}
                      </button>
                    )
                  })}
                </div>

                <div className="mt-5 flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={function onBack() {
                      setStep('provider')
                    }}
                  >
                    <HugeiconsIcon
                      icon={ArrowLeft01Icon}
                      size={20}
                      strokeWidth={1.5}
                    />
                    Back
                  </Button>
                </div>
              </section>
            ) : null}

            {step === 'instructions' && selectedProvider && selectedAuthType ? (
              <section className="mt-5">
                <h3 className="text-base font-medium text-primary-900 text-balance">
                  Step 3: Add API Key
                </h3>

                {selectedAuthType === 'oauth' ? (
                  <>
                    <p className="mt-1 text-sm text-primary-600 text-pretty">
                      This will run{' '}
                      <code className="font-mono text-primary-800">
                        openclaw configure
                      </code>{' '}
                      in the terminal to start the OAuth flow. A browser window
                      will open for you to sign in with Google.
                    </p>

                    <div className="mt-4 flex flex-col gap-3">
                      <Button
                        size="sm"
                        onClick={function onLaunchOAuth() {
                          // Open the terminal route and trigger the command
                          window.open('/terminal', '_blank')
                          setVerificationMessage(
                            'Run "openclaw configure" in the terminal and select Google OAuth when prompted. ' +
                              'A browser window will open for sign-in. Once complete, the gateway will restart automatically.',
                          )
                          setStep('verify')
                        }}
                      >
                        Open Terminal
                      </Button>

                      <div className="rounded-xl border border-primary-200 bg-primary-100/70 px-3 py-2">
                        <p className="text-xs text-primary-700 text-pretty">
                          In the terminal, run:
                        </p>
                        <pre className="mt-1 rounded-lg bg-primary-200/60 px-2 py-1.5 text-xs font-mono text-primary-900">
                          openclaw configure
                        </pre>
                        <p className="mt-1.5 text-xs text-primary-600 text-pretty">
                          Select <strong>Google Antigravity</strong> →{' '}
                          <strong>OAuth</strong>. A browser tab will open for
                          Google sign-in.
                        </p>
                      </div>
                    </div>
                  </>
                ) : selectedAuthType === 'cli-token' ? (
                  <>
                    <p className="mt-1 text-sm text-primary-600 text-pretty">
                      If you have Claude Code or the Claude CLI installed,
                      OpenClaw can use the same auth token. Run the configure
                      command to detect and import it automatically.
                    </p>

                    <div className="mt-4 flex flex-col gap-3">
                      <Button
                        size="sm"
                        onClick={function onLaunchCLI() {
                          window.open('/terminal', '_blank')
                          setVerificationMessage(
                            'Run "openclaw configure" in the terminal and select Anthropic → CLI Token. ' +
                              'It will detect your Claude CLI credentials and import them automatically.',
                          )
                          setStep('verify')
                        }}
                      >
                        Open Terminal
                      </Button>

                      <div className="rounded-xl border border-primary-200 bg-primary-100/70 px-3 py-2">
                        <p className="text-xs text-primary-700 text-pretty">
                          In the terminal, run:
                        </p>
                        <pre className="mt-1 rounded-lg bg-primary-200/60 px-2 py-1.5 text-xs font-mono text-primary-900">
                          openclaw configure
                        </pre>
                        <p className="mt-1.5 text-xs text-primary-600 text-pretty">
                          Select <strong>Anthropic</strong> →{' '}
                          <strong>Setup Token (Claude CLI)</strong>. It will
                          detect your existing Claude credentials from{' '}
                          <code className="font-mono">~/.claude/</code>.
                        </p>
                      </div>

                      <div className="rounded-xl border border-amber-200 bg-amber-50/70 px-3 py-2">
                        <p className="text-xs text-amber-800 text-pretty">
                          <strong>Requires:</strong> Claude Code or Claude CLI
                          must be installed and authenticated first. Run{' '}
                          <code className="font-mono">claude</code> in terminal
                          to verify.
                        </p>
                      </div>
                    </div>
                  </>
                ) : selectedAuthType === 'api-key' ? (
                  <>
                    <p className="mt-1 text-sm text-primary-600 text-pretty">
                      Paste your {selectedProvider.name} API key below. It will
                      be saved directly to your local config file.
                    </p>

                    <div className="mt-4 flex flex-col gap-3">
                      <div className="flex gap-2">
                        <input
                          type="password"
                          value={apiKeyInput}
                          onChange={function onInputChange(e) {
                            setApiKeyInput(e.target.value)
                          }}
                          placeholder={`sk-... or your ${selectedProvider.name} API key`}
                          className="flex-1 rounded-xl border border-primary-300 bg-white px-3 py-2 text-sm text-primary-900 placeholder:text-primary-400 focus:border-accent-400 focus:outline-none focus:ring-1 focus:ring-accent-400/50"
                          autoFocus
                        />
                        <Button
                          size="sm"
                          disabled={
                            !apiKeyInput.trim() || saveState === 'saving'
                          }
                          onClick={function onSave() {
                            void handleSaveApiKey()
                          }}
                        >
                          {saveState === 'saving'
                            ? 'Saving…'
                            : saveState === 'saved'
                              ? 'Saved ✓'
                              : 'Save & Connect'}
                        </Button>
                      </div>

                      {saveState === 'error' ? (
                        <p className="text-xs text-red-600">{saveError}</p>
                      ) : null}

                      {saveState === 'saved' ? (
                        <p className="text-xs text-green-600">
                          <HugeiconsIcon
                            icon={Tick02Icon}
                            size={14}
                            strokeWidth={1.5}
                            className="inline mr-1"
                          />
                          Key saved! Gateway is restarting to apply changes.
                        </p>
                      ) : null}
                    </div>

                    <a
                      href={selectedProvider.docsUrl}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="mt-3 inline-flex items-center gap-1 text-sm text-primary-700 underline decoration-primary-400 hover:text-primary-900"
                    >
                      <HugeiconsIcon
                        icon={Link01Icon}
                        size={20}
                        strokeWidth={1.5}
                      />
                      Get your {selectedProvider.name} API key
                    </a>

                    <div className="mt-4 rounded-xl border border-primary-200 bg-primary-100/70 px-3 py-2">
                      <p className="text-xs text-primary-700 text-pretty">
                        API keys are stored locally in{' '}
                        <code className="font-mono">
                          {OPENCLAW_CONFIG_PATH}
                        </code>
                        , never sent to Studio.
                      </p>
                    </div>

                    {/* Manual fallback */}
                    <button
                      type="button"
                      onClick={function toggleManual() {
                        setShowManualSnippet(!showManualSnippet)
                      }}
                      className="mt-3 text-xs text-primary-500 hover:text-primary-700 underline"
                    >
                      {showManualSnippet ? 'Hide' : 'Show'} manual config
                      snippet
                    </button>

                    {showManualSnippet ? (
                      <div className="mt-2">
                        <div className="flex items-center gap-2 mb-2">
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={function onCopyConfig() {
                              void handleCopyConfig()
                            }}
                          >
                            <HugeiconsIcon
                              icon={Copy01Icon}
                              size={20}
                              strokeWidth={1.5}
                            />
                            Copy snippet
                          </Button>
                          {copyState === 'copied' ? (
                            <span className="inline-flex items-center gap-1 text-xs text-green-600">
                              <HugeiconsIcon
                                icon={Tick02Icon}
                                size={20}
                                strokeWidth={1.5}
                              />
                              Copied
                            </span>
                          ) : null}
                        </div>
                        <pre className="overflow-x-auto rounded-2xl border border-primary-200 bg-primary-100/80 p-3 text-xs text-primary-900">
                          <code className="font-mono tabular-nums">
                            {configExample}
                          </code>
                        </pre>
                      </div>
                    ) : null}
                  </>
                ) : (
                  <>
                    <p className="mt-1 text-sm text-primary-600 text-pretty">
                      No additional configuration needed. Just ensure the
                      service is running locally.
                    </p>
                  </>
                )}

                <div className="mt-5 flex flex-wrap items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={function onBack() {
                      setStep('auth')
                    }}
                  >
                    <HugeiconsIcon
                      icon={ArrowLeft01Icon}
                      size={20}
                      strokeWidth={1.5}
                    />
                    Back
                  </Button>
                  {selectedAuthType === 'local' ? (
                    <Button
                      size="sm"
                      onClick={function onDone() {
                        handleDone()
                      }}
                    >
                      Done
                    </Button>
                  ) : null}
                </div>
              </section>
            ) : null}

            {step === 'verify' ? (
              <section className="mt-5">
                <h3 className="text-base font-medium text-primary-900 text-balance">
                  Step 4: Verify (Stub)
                </h3>
                <div className="mt-3 rounded-2xl border border-primary-200 bg-primary-100/70 p-4">
                  <p className="text-sm font-medium text-primary-900 text-balance">
                    Checking connection...
                  </p>
                  <p className="mt-1 text-sm text-primary-600 text-pretty">
                    {verificationMessage ||
                      'Verification not yet implemented — restart Gateway to apply changes.'}
                  </p>
                </div>

                <div className="mt-5 flex flex-wrap items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={function onBack() {
                      setStep('instructions')
                    }}
                  >
                    <HugeiconsIcon
                      icon={ArrowLeft01Icon}
                      size={20}
                      strokeWidth={1.5}
                    />
                    Back
                  </Button>
                  <Button
                    size="sm"
                    onClick={function onDone() {
                      handleDone()
                    }}
                  >
                    Done
                  </Button>
                </div>
              </section>
            ) : null}
          </div>
        </div>
      </DialogContent>
    </DialogRoot>
  )
}
