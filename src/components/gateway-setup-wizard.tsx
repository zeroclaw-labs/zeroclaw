'use client'

import { useEffect } from 'react'
import { AnimatePresence, motion } from 'motion/react'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  CloudIcon,
  CheckmarkCircle02Icon,
  Alert02Icon,
} from '@hugeicons/core-free-icons'
import { cn } from '@/lib/utils'
import { useGatewaySetupStore } from '@/hooks/use-gateway-setup'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { ProviderSelectStep } from '@/components/onboarding/provider-select-step'

function GatewayStepContent() {
  const {
    gatewayUrl,
    gatewayToken,
    testStatus,
    testError,
    saving,
    setGatewayUrl,
    setGatewayToken,
    saveAndTest,
    proceed,
  } = useGatewaySetupStore()

  const handleSaveAndTest = async () => {
    const ok = await saveAndTest()
    if (ok) {
      setTimeout(() => proceed(), 800)
    }
  }

  const isBusy = testStatus === 'testing' || saving
  const canProceed = testStatus === 'success'

  return (
    <div className="w-full">
      <div className="mb-6 flex flex-col items-center text-center">
        <div className="mb-4 flex size-20 items-center justify-center rounded-2xl bg-gradient-to-br from-accent-500 to-accent-600 text-white shadow-lg">
          <HugeiconsIcon icon={CloudIcon} className="size-10" strokeWidth={1.5} />
        </div>
        <h2 className="mb-2 text-2xl font-semibold text-primary-900">
          Connect to Gateway
        </h2>
        <p className="max-w-md text-sm leading-relaxed text-primary-600">
          Enter your OpenClaw gateway URL and token. The token can be found by
          running:{' '}
          <code className="rounded bg-primary-100 px-1.5 py-0.5 text-xs font-medium">
            openclaw config get gateway.auth.token
          </code>
        </p>
      </div>

      <div className="space-y-4">
        <div>
          <label
            htmlFor="gateway-url"
            className="mb-1.5 block text-sm font-medium text-primary-900"
          >
            Gateway URL
          </label>
          <Input
            id="gateway-url"
            type="text"
            placeholder="ws://127.0.0.1:18789"
            value={gatewayUrl}
            onChange={(e) => setGatewayUrl(e.target.value)}
            className="h-10"
          />
          <p className="mt-1 text-xs text-primary-500">
            Default: ws://127.0.0.1:18789 for local OpenClaw (18790 for nanobot)
          </p>
        </div>

        <div>
          <label
            htmlFor="gateway-token"
            className="mb-1.5 block text-sm font-medium text-primary-900"
          >
            Gateway Token{' '}
            <span className="font-normal text-primary-400">(optional)</span>
          </label>
          <Input
            id="gateway-token"
            type="password"
            placeholder="Leave empty if no token is set"
            value={gatewayToken}
            onChange={(e) => setGatewayToken(e.target.value)}
            className="h-10"
          />
        </div>

        {testError && (
          <div className="flex items-start gap-2 rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-800">
            <HugeiconsIcon
              icon={Alert02Icon}
              className="mt-0.5 size-4 shrink-0"
              strokeWidth={2}
            />
            <div>
              <p>{testError}</p>
            </div>
          </div>
        )}

        {testStatus === 'success' && (
          <div className="flex items-start gap-2 rounded-lg border border-green-200 bg-green-50 p-3 text-sm text-green-800">
            <HugeiconsIcon
              icon={CheckmarkCircle02Icon}
              className="mt-0.5 size-4 shrink-0"
              strokeWidth={2}
            />
            <span>Connected to gateway!</span>
          </div>
        )}

        <div className="flex gap-3 pt-1">
          <Button
            variant="secondary"
            onClick={() => void handleSaveAndTest()}
            disabled={isBusy || !gatewayUrl.trim()}
            className="flex-1"
          >
            {saving
              ? 'Saving...'
              : testStatus === 'testing'
                ? 'Testing...'
                : 'Save & Test Connection'}
          </Button>
          <Button
            variant="default"
            onClick={proceed}
            disabled={!canProceed}
            className="flex-1 bg-accent-500 hover:bg-accent-600"
          >
            Continue
          </Button>
        </div>
      </div>
    </div>
  )
}

function ProviderStepContent() {
  const { skipProviderSetup, completeSetup } = useGatewaySetupStore()

  const handleProviderComplete = async (providerId: string, apiKey: string) => {
    // Save the provider config to the gateway
    try {
      await fetch('/api/gateway-config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ providerId, apiKey }),
      })
    } catch {
      // Non-blocking — user can configure later
    }
    completeSetup()
  }

  return (
    <ProviderSelectStep
      onComplete={handleProviderComplete}
      onSkip={skipProviderSetup}
    />
  )
}

export function GatewaySetupWizard() {
  const { isOpen, step, initialize } = useGatewaySetupStore()

  useEffect(() => {
    void initialize()
  }, [initialize])

  if (!isOpen) return null

  return (
    <AnimatePresence>
      {isOpen && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 }}
          className="fixed inset-0 z-[110] flex items-center justify-center bg-ink/80 backdrop-blur-sm"
        >
          <motion.div
            initial={{ opacity: 0, scale: 0.95, y: 20 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.95, y: 20 }}
            transition={{ type: 'spring', damping: 25, stiffness: 300 }}
            className="relative w-[min(620px,92vw)] min-w-[320px] overflow-hidden rounded-2xl border border-primary-200 bg-primary-50 shadow-2xl"
          >
            <div className="pointer-events-none absolute inset-0 bg-[radial-gradient(ellipse_at_top,_var(--tw-gradient-stops))] from-accent-500/5 via-transparent to-transparent" />

            <div className="relative px-8 pb-8 pt-8">
              {/* Step dots */}
              <div className="mb-6 flex items-center justify-center gap-2">
                {(['gateway', 'provider'] as const).map((s) => (
                  <div
                    key={s}
                    className={cn(
                      'size-2 rounded-full transition-colors',
                      step === s
                        ? 'bg-accent-500'
                        : 'bg-primary-300',
                    )}
                  />
                ))}
              </div>

              <AnimatePresence mode="wait">
                <motion.div
                  key={step}
                  initial={{ opacity: 0, x: 20 }}
                  animate={{ opacity: 1, x: 0 }}
                  exit={{ opacity: 0, x: -20 }}
                  transition={{ duration: 0.2 }}
                >
                  {step === 'gateway' && <GatewayStepContent />}
                  {step === 'provider' && <ProviderStepContent />}
                </motion.div>
              </AnimatePresence>
            </div>

            <div className="border-t border-primary-200 bg-primary-100/50 px-6 py-3">
              <p className="text-center text-xs text-primary-500">
                Need help?{' '}
                <a
                  href="https://docs.openclaw.ai"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-accent-600 underline hover:text-accent-700"
                >
                  Documentation
                </a>
              </p>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  )
}
