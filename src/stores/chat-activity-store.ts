import { create } from 'zustand'

export type AgentActivity =
  | 'idle'
  | 'reading' // user sent a message, agent hasn't started responding
  | 'thinking' // waiting for first token
  | 'responding' // streaming response
  | 'tool-use' // executing a tool call
  | 'orchestrating' // subagents active

type ChatActivityState = {
  activity: AgentActivity
  /** Activity set by local chat UI */
  localActivity: AgentActivity
  /** Activity detected from gateway polling */
  gatewayActivity: AgentActivity
  /** Timestamp of last activity change */
  changedAt: number
  setLocalActivity: (activity: AgentActivity) => void
  setGatewayActivity: (activity: AgentActivity) => void
  /** Polling interval ref */
  _pollTimer: ReturnType<typeof setInterval> | null
  startGatewayPoll: () => void
  stopGatewayPoll: () => void
}

function resolveActivity(
  local: AgentActivity,
  gateway: AgentActivity,
): AgentActivity {
  // Local UI states take priority when active
  if (local !== 'idle') return local
  // Fall back to gateway-detected state
  return gateway
}

async function pollGatewayState(): Promise<AgentActivity> {
  try {
    const res = await fetch('/api/gateway/sessions')
    if (!res.ok) return 'idle'
    const data = await res.json()
    if (!data.ok) return 'idle'

    // sessions.list returns { sessions: [...] } or just an array
    const sessions: Array<Record<string, unknown>> = Array.isArray(
      data.data?.sessions,
    )
      ? data.data.sessions
      : Array.isArray(data.data)
        ? data.data
        : []

    if (sessions.length === 0) return 'idle'

    const now = Date.now()

    // Find main session (not a subagent)
    const mainSession = sessions.find(
      (s) =>
        typeof s === 'object' &&
        !String(s.key ?? s.id ?? '').includes('subagent:'),
    )

    // Check for active subagents
    const activeSubagents = sessions.filter((s) => {
      const key = String(s.key ?? s.id ?? '')
      if (!key.includes('subagent:')) return false
      const status = String(s.status ?? '').toLowerCase()
      return (
        status === 'running' || status === 'active' || status === 'thinking'
      )
    })

    if (activeSubagents.length > 0) {
      return 'orchestrating'
    }

    // Check main session activity based on timestamps
    if (mainSession) {
      const updatedAt =
        typeof mainSession.updatedAt === 'number'
          ? mainSession.updatedAt
          : typeof mainSession.lastMessageAt === 'number'
            ? mainSession.lastMessageAt
            : 0

      if (updatedAt > 0) {
        const staleness = now - updatedAt
        if (staleness < 5000) return 'responding'
        if (staleness < 15000) return 'thinking'
      }

      // Also check status field if available
      const status = String(mainSession.status ?? '').toLowerCase()
      if (status === 'responding' || status === 'streaming') return 'responding'
      if (status === 'thinking' || status === 'processing') return 'thinking'
      if (status === 'running' || status === 'active') return 'responding'
    }

    return 'idle'
  } catch {
    return 'idle'
  }
}

export const useChatActivityStore = create<ChatActivityState>((set, get) => ({
  activity: 'idle',
  localActivity: 'idle',
  gatewayActivity: 'idle',
  changedAt: Date.now(),
  _pollTimer: null,

  setLocalActivity: (localActivity) => {
    const state = get()
    const activity = resolveActivity(localActivity, state.gatewayActivity)
    if (state.localActivity !== localActivity || state.activity !== activity) {
      set({ localActivity, activity, changedAt: Date.now() })
    }
  },

  setGatewayActivity: (gatewayActivity) => {
    const state = get()
    const activity = resolveActivity(state.localActivity, gatewayActivity)
    if (
      state.gatewayActivity !== gatewayActivity ||
      state.activity !== activity
    ) {
      set({ gatewayActivity, activity, changedAt: Date.now() })
    }
  },

  startGatewayPoll: () => {
    const state = get()
    if (state._pollTimer) return
    const tick = async () => {
      const detected = await pollGatewayState()
      get().setGatewayActivity(detected)
    }
    void tick()
    const timer = setInterval(tick, 3000)
    set({ _pollTimer: timer })
  },

  stopGatewayPoll: () => {
    const timer = get()._pollTimer
    if (timer) {
      clearInterval(timer)
      set({ _pollTimer: null })
    }
  },
}))
