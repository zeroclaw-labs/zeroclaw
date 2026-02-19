import { useMemo } from 'react'
import type { AgentActivity } from '@/stores/chat-activity-store'
import { useChatActivityStore } from '@/stores/chat-activity-store'

export type OrchestratorState = AgentActivity

type OrchestratorInfo = {
  state: OrchestratorState
  label: string
}

const LABELS: Record<AgentActivity, string> = {
  idle: 'Idle',
  reading: 'Reading...',
  thinking: 'Thinking...',
  responding: 'Responding...',
  'tool-use': 'Using tools...',
  orchestrating: 'Orchestrating agents...',
}

export function useOrchestratorState(): OrchestratorInfo {
  const activity = useChatActivityStore((s) => s.activity)

  return useMemo(
    () => ({
      state: activity,
      label: LABELS[activity],
    }),
    [activity],
  )
}
