/**
 * React hook for sound notifications in ClawSuite
 * Integrates with the agent swarm store to auto-play sounds on state changes
 */
import { useCallback, useEffect, useMemo, useRef } from 'react'

import type { SoundEvent } from '@/lib/sounds'
import type { SwarmSession } from '@/stores/agent-swarm-store'

import {
  getSoundVolume,
  isSoundEnabled,
  playAgentComplete,
  playAgentFailed,
  playAgentSpawned,
  playAlert,
  playChatComplete,
  playChatNotification,
  playSound,
  playThinking,
  setSoundEnabled,
  setSoundVolume,
} from '@/lib/sounds'
import { useSwarmStore } from '@/stores/agent-swarm-store'

interface UseSoundsOptions {
  /** Auto-play sounds when agent states change (default: true) */
  autoPlay?: boolean
  /** Throttle thinking sounds to once per interval in ms (default: 2000) */
  thinkingThrottleMs?: number
}

interface UseSoundsReturn {
  // Play functions
  playAgentSpawned: () => void
  playAgentComplete: () => void
  playAgentFailed: () => void
  playChatNotification: () => void
  playChatComplete: () => void
  playAlert: () => void
  playThinking: () => void
  playSound: (event: SoundEvent) => void

  // Control functions
  volume: number
  setVolume: (vol: number) => void
  enabled: boolean
  setEnabled: (enabled: boolean) => void
}

/**
 * Hook that provides sound functions and optionally auto-plays
 * based on agent swarm state changes.
 */
export function useSounds(options: UseSoundsOptions = {}): UseSoundsReturn {
  const { autoPlay = true, thinkingThrottleMs = 2000 } = options

  // Track previous session states to detect changes
  const prevSessionsRef = useRef<Map<string, SwarmSession['swarmStatus']>>(
    new Map(),
  )
  const lastThinkingSoundRef = useRef<number>(0)

  // Subscribe to swarm store
  const sessions = useSwarmStore((state) => state.sessions)

  // Detect state changes and play appropriate sounds
  useEffect(() => {
    if (!autoPlay) return

    const prevMap = prevSessionsRef.current
    const now = Date.now()
    let hasNewThinking = false

    for (const session of sessions) {
      const sessionId = String(session.key ?? session.friendlyId ?? '')
      const prevStatus = prevMap.get(sessionId)
      const currentStatus = session.swarmStatus

      // New session (spawned)
      if (!prevStatus && currentStatus === 'running') {
        playAgentSpawned()
      }
      // Status changed
      else if (prevStatus && prevStatus !== currentStatus) {
        switch (currentStatus) {
          case 'complete':
            playAgentComplete()
            break
          case 'failed':
            playAgentFailed()
            break
          case 'thinking':
            hasNewThinking = true
            break
        }
      }
      // Currently thinking (throttled)
      else if (currentStatus === 'thinking') {
        hasNewThinking = true
      }

      // Update tracking
      prevMap.set(sessionId, currentStatus)
    }

    // Play thinking sound (throttled)
    if (
      hasNewThinking &&
      now - lastThinkingSoundRef.current > thinkingThrottleMs
    ) {
      playThinking()
      lastThinkingSoundRef.current = now
    }

    // Clean up old sessions from tracking
    const currentIds = new Set(sessions.map((s) => s.id ?? s.key ?? ''))
    for (const id of prevMap.keys()) {
      if (!currentIds.has(id)) {
        prevMap.delete(id)
      }
    }
  }, [sessions, autoPlay, thinkingThrottleMs])

  // Stable callbacks
  const setVolume = useCallback((vol: number) => {
    setSoundVolume(vol)
  }, [])

  const setEnabled = useCallback((enabled: boolean) => {
    setSoundEnabled(enabled)
  }, [])

  // Return memoized object for stable reference
  return useMemo(
    () => ({
      // Play functions (stable references from module)
      playAgentSpawned,
      playAgentComplete,
      playAgentFailed,
      playChatNotification,
      playChatComplete,
      playAlert,
      playThinking,
      playSound,

      // Control
      volume: getSoundVolume(),
      setVolume,
      enabled: isSoundEnabled(),
      setEnabled,
    }),
    [setVolume, setEnabled],
  )
}

// Re-export types and functions for convenience
export type { SoundEvent }
export {
  playAgentSpawned,
  playAgentComplete,
  playAgentFailed,
  playChatNotification,
  playChatComplete,
  playAlert,
  playThinking,
  setSoundVolume,
  setSoundEnabled,
  isSoundEnabled,
  getSoundVolume,
  playSound,
}
