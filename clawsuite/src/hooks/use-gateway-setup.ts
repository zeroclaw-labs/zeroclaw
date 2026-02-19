import { create } from 'zustand'

const SETUP_STORAGE_KEY = 'clawsuite-gateway-configured'

type GatewaySetupState = {
  isOpen: boolean
  step: 'gateway' | 'provider' | 'complete'
  gatewayUrl: string
  gatewayToken: string
  testStatus: 'idle' | 'testing' | 'success' | 'error'
  testError: string | null
  saving: boolean
  _initialized: boolean
  initialize: () => Promise<void>
  setGatewayUrl: (url: string) => void
  setGatewayToken: (token: string) => void
  /** Save URL/token to server .env, then test connection */
  saveAndTest: () => Promise<boolean>
  /** Just test current server connection (no save) */
  testConnection: () => Promise<boolean>
  proceed: () => void
  skipProviderSetup: () => void
  completeSetup: () => void
  reset: () => void
  open: () => void
}

async function pingGateway(): Promise<{ ok: boolean; error?: string }> {
  try {
    const response = await fetch('/api/ping', {
      signal: AbortSignal.timeout(8000),
    })
    const data = (await response.json()) as { ok?: boolean; error?: string }
    return { ok: Boolean(data.ok), error: data.error }
  } catch {
    return { ok: false, error: 'Could not reach ClawSuite server' }
  }
}

async function fetchCurrentConfig(): Promise<{ url: string; hasToken: boolean }> {
  try {
    const response = await fetch('/api/gateway-config', {
      signal: AbortSignal.timeout(5000),
    })
    const data = (await response.json()) as { url?: string; hasToken?: boolean }
    return { url: data.url || 'ws://127.0.0.1:18789', hasToken: Boolean(data.hasToken) }
  } catch {
    return { url: 'ws://127.0.0.1:18789', hasToken: false }
  }
}

async function saveConfig(url: string, token: string): Promise<{ ok: boolean; error?: string }> {
  try {
    const response = await fetch('/api/gateway-config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, token }),
      signal: AbortSignal.timeout(15000),
    })
    const data = (await response.json()) as { ok?: boolean; connected?: boolean; error?: string }
    if (data.ok && data.connected === false) {
      return { ok: true, error: 'Config saved. Reconnecting to gateway...' }
    }
    return { ok: Boolean(data.ok), error: data.error }
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : 'Failed to save config' }
  }
}

export const useGatewaySetupStore = create<GatewaySetupState>((set, get) => ({
  isOpen: false,
  step: 'gateway',
  gatewayUrl: 'ws://127.0.0.1:18789',
  gatewayToken: '',
  testStatus: 'idle',
  testError: null,
  saving: false,
  _initialized: false,

  initialize: async () => {
    if (get()._initialized) return
    set({ _initialized: true })
    if (typeof window === 'undefined') return

    try {
      const configured = localStorage.getItem(SETUP_STORAGE_KEY) === 'true'

      // Debug: ?wizard=provider or ?wizard=gateway forces the wizard open
      const params = new URLSearchParams(window.location.search)
      const forceWizard = params.get('wizard')
      if (forceWizard) {
        const step = forceWizard === 'provider' ? 'provider' : 'gateway'
        set({ isOpen: true, step, gatewayUrl: 'ws://127.0.0.1:18789' })
        return
      }

      // Check if gateway is already working
      const { ok } = await pingGateway()
      if (ok) {
        localStorage.setItem(SETUP_STORAGE_KEY, 'true')
        return
      }

      if (configured) {
        // Was configured but now down — banner handles this, not wizard
        return
      }

      // First run + gateway not working → try auto-discovery
      try {
        const discoverRes = await fetch('/api/gateway-discover', {
          method: 'POST',
          signal: AbortSignal.timeout(15000),
        })
        const discoverData = (await discoverRes.json()) as {
          ok?: boolean
          url?: string
          source?: string
          error?: string
        }

        if (discoverData.ok) {
          // Auto-discovery succeeded — connected!
          localStorage.setItem(SETUP_STORAGE_KEY, 'true')
          // Skip gateway step, go straight to provider setup
          set({
            isOpen: true,
            step: 'provider',
            gatewayUrl: discoverData.url || 'ws://127.0.0.1:18789',
            testStatus: 'success',
          })
          return
        }
      } catch {
        // Auto-discovery failed, fall through to manual wizard
      }

      // Auto-discovery failed → show manual wizard
      const config = await fetchCurrentConfig()
      set({
        isOpen: true,
        step: 'gateway',
        gatewayUrl: config.url,
        gatewayToken: '', // Don't pre-fill token for security
      })
    } catch {
      // Ignore init errors
    }
  },

  setGatewayUrl: (url) => set({ gatewayUrl: url, testStatus: 'idle', testError: null }),
  setGatewayToken: (token) => set({ gatewayToken: token, testStatus: 'idle', testError: null }),

  saveAndTest: async () => {
    const { gatewayUrl, gatewayToken } = get()
    set({ saving: true, testStatus: 'testing', testError: null })

    // 1. Save to .env via server API
    const saveResult = await saveConfig(gatewayUrl, gatewayToken)
    if (!saveResult.ok) {
      set({
        saving: false,
        testStatus: 'error',
        testError: saveResult.error || 'Failed to save configuration',
      })
      return false
    }

    // 2. Brief delay for server to pick up new env vars
    await new Promise((r) => setTimeout(r, 500))

    // 3. Test connection via /api/ping
    const { ok, error } = await pingGateway()
    set({ saving: false })

    if (ok) {
      set({ testStatus: 'success', testError: null })
      return true
    }

    set({
      testStatus: 'error',
      testError: error || 'Gateway not reachable after saving config. You may need to restart ClawSuite.',
    })
    return false
  },

  testConnection: async () => {
    set({ testStatus: 'testing', testError: null })
    const { ok, error } = await pingGateway()
    if (ok) {
      set({ testStatus: 'success', testError: null })
      return true
    }
    set({ testStatus: 'error', testError: error || 'Gateway not reachable' })
    return false
  },

  proceed: () => set({ step: 'provider' }),

  skipProviderSetup: () => {
    localStorage.setItem(SETUP_STORAGE_KEY, 'true')
    set({ isOpen: false, step: 'complete' })
  },

  completeSetup: () => {
    localStorage.setItem(SETUP_STORAGE_KEY, 'true')
    set({ isOpen: false, step: 'complete' })
  },

  reset: () => {
    localStorage.removeItem(SETUP_STORAGE_KEY)
    set({
      isOpen: true,
      step: 'gateway',
      gatewayUrl: 'ws://127.0.0.1:18789',
      gatewayToken: '',
      testStatus: 'idle',
      testError: null,
    })
  },

  open: async () => {
    const config = await fetchCurrentConfig()
    set({
      isOpen: true,
      step: 'gateway',
      gatewayUrl: config.url,
      gatewayToken: '',
      testStatus: 'idle',
      testError: null,
    })
  },
}))
