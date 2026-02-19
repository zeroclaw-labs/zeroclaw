/**
 * useAgentBehaviors — Manages the living office simulation loop.
 * Each agent gets independent activity cycles, break schedules, chat visits, and movement.
 */
import { useEffect, useRef, useCallback, useState } from 'react'
import { assignPersona, releasePersona } from '@/lib/agent-personas'
import type { SwarmSession } from '@/stores/agent-swarm-store'
import {
  type AgentActivity,
  type AgentBehaviorState,
  createBehaviorState,
  getBreakType,
  getExpression,
  getLocationForActivity,
  getRandomMessage,
  isAtTarget,
  lerpPosition,
  DESK_POSITIONS,
} from '@/components/agent-swarm/agent-behaviors'

const TICK_MS = 1000
const CODING_MIN_MS = 15_000
const CODING_MAX_MS = 30_000
const BREAK_MIN_MS = 5_000
const BREAK_MAX_MS = 12_000
const CHAT_VISIT_MIN_MS = 30_000
const CHAT_VISIT_MAX_MS = 60_000
const CHAT_BUBBLE_MS = 4_000
const CELEBRATE_MS = 5_000
const LERP_SPEED = 0.08 // Faster than default for visible movement

function randomBetween(min: number, max: number): number {
  return min + Math.floor(Math.random() * (max - min))
}

function getActivityEmoji(activity: AgentActivity): string {
  const map: Record<AgentActivity, string> = {
    idle: '🧍',
    walking: '🚶',
    coding: '💻',
    thinking: '💭',
    water_break: '💧',
    coffee_break: '☕',
    lunch: '🍕',
    meeting: '🤝',
    chatting: '💬',
    celebrating: '🎉',
    frustrated: '😤',
  }
  return map[activity]
}

export type AgentBehaviorView = AgentBehaviorState & {
  sessionKey: string
  personaName: string
  activityEmoji: string
  direction: 'left' | 'right'
  isWalking: boolean
}

export function useAgentBehaviors(
  sessions: Array<SwarmSession>,
): Map<string, AgentBehaviorView> {
  const statesRef = useRef<Map<string, AgentBehaviorState>>(new Map())
  const deskAssignments = useRef<Map<string, number>>(new Map())
  const nextDesk = useRef(0)
  const lastChatVisit = useRef(Date.now())
  const nextChatVisitAt = useRef(
    Date.now() + randomBetween(CHAT_VISIT_MIN_MS, CHAT_VISIT_MAX_MS),
  )
  const [, setTick] = useState(0) // Force re-render

  const getOrCreateState = useCallback((key: string): AgentBehaviorState => {
    let state = statesRef.current.get(key)
    if (!state) {
      let deskIdx = deskAssignments.current.get(key)
      if (deskIdx === undefined) {
        // Find next desk not already taken by another active agent
        const takenDesks = new Set(deskAssignments.current.values())
        let found = false
        for (let i = 0; i < DESK_POSITIONS.length; i++) {
          const candidate = (nextDesk.current + i) % DESK_POSITIONS.length
          if (!takenDesks.has(candidate)) {
            deskIdx = candidate
            nextDesk.current = candidate + 1
            found = true
            break
          }
        }
        if (!found) {
          deskIdx = nextDesk.current % DESK_POSITIONS.length
          nextDesk.current++
        }
        deskAssignments.current.set(key, deskIdx!)
      }
      state = createBehaviorState(deskIdx!)
      statesRef.current.set(key, state)
    }
    return state
  }, [])

  const getDeskIndex = useCallback((key: string): number => {
    return deskAssignments.current.get(key) ?? 0
  }, [])

  // Main simulation tick
  useEffect(() => {
    const interval = setInterval(() => {
      const now = Date.now()
      const activeKeys = new Set(
        sessions.map((s) => s.key ?? s.friendlyId ?? ''),
      )

      // Clean up stale agents + release their personas
      for (const key of statesRef.current.keys()) {
        if (!activeKeys.has(key)) {
          statesRef.current.delete(key)
          deskAssignments.current.delete(key)
          releasePersona(key)
        }
      }

      // Update each agent
      for (const session of sessions) {
        const key = session.key ?? session.friendlyId ?? ''
        if (!key) continue

        const state = getOrCreateState(key)
        const deskIdx = getDeskIndex(key)
        const elapsed = now - state.activityStartTime

        // Handle swarmStatus transitions
        if (
          session.swarmStatus === 'complete' &&
          state.activity !== 'celebrating'
        ) {
          state.activity = 'celebrating'
          state.activityStartTime = now
          state.chatMessage = getRandomMessage('complete')
          state.expression = getExpression('celebrating')
          state.targetPosition = { ...state.deskPosition }
        } else if (
          session.swarmStatus === 'failed' &&
          state.activity !== 'frustrated'
        ) {
          state.activity = 'frustrated'
          state.activityStartTime = now
          state.chatMessage = getRandomMessage('failed')
          state.expression = getExpression('frustrated')
          state.targetPosition = { ...state.deskPosition }
        } else if (
          session.swarmStatus === 'thinking' &&
          state.activity !== 'thinking' &&
          state.activity !== 'walking'
        ) {
          state.activity = 'thinking'
          state.activityStartTime = now
          state.chatMessage = '💭 thinking...'
          state.expression = getExpression('thinking')
          state.targetPosition = { ...state.deskPosition }
        } else if (session.swarmStatus === 'running') {
          // Running agents cycle: coding → break → coding
          if (state.activity === 'idle') {
            // Just spawned — run to desk
            state.activity = 'walking'
            state.targetPosition = { ...state.deskPosition }
            state.activityStartTime = now
            state.chatMessage = null
          } else if (
            state.activity === 'coding' &&
            elapsed > randomBetween(CODING_MIN_MS, CODING_MAX_MS)
          ) {
            // Time for a break
            const breakType = getBreakType()
            state.activity = 'walking'
            state.targetPosition = getLocationForActivity(breakType, deskIdx)
            state.activityStartTime = now
            state.chatMessage = getRandomMessage('break')
            // Store intended break type in chatTarget temporarily
            state.chatTarget = breakType
          } else if (state.activity === 'coding' && Math.random() < 0.15) {
            // Random work chat bubble
            if (!state.chatMessage) {
              state.chatMessage = getRandomMessage('working')
              setTimeout(() => {
                const s = statesRef.current.get(key)
                if (s) s.chatMessage = null
              }, CHAT_BUBBLE_MS)
            }
          } else if (
            (state.activity === 'water_break' ||
              state.activity === 'coffee_break' ||
              state.activity === 'lunch' ||
              state.activity === 'meeting') &&
            elapsed > randomBetween(BREAK_MIN_MS, BREAK_MAX_MS)
          ) {
            // Break over, walk back to desk
            state.activity = 'walking'
            state.targetPosition = { ...state.deskPosition }
            state.activityStartTime = now
            state.chatMessage = null
            state.chatTarget = null
          }
        }

        // Celebrating/frustrated timeout → idle
        if (state.activity === 'celebrating' && elapsed > CELEBRATE_MS) {
          state.activity = 'idle'
          state.chatMessage = null
          state.expression = getExpression('idle')
        }
        if (state.activity === 'frustrated' && elapsed > CELEBRATE_MS) {
          state.activity = 'idle'
          state.chatMessage = null
          state.expression = getExpression('idle')
        }

        // Walking — move toward target
        if (state.activity === 'walking') {
          state.position = lerpPosition(
            state.position,
            state.targetPosition,
            LERP_SPEED,
          )

          if (isAtTarget(state.position, state.targetPosition, 2)) {
            // Arrived at destination
            state.position = { ...state.targetPosition }

            if (
              Math.abs(state.targetPosition.x - state.deskPosition.x) < 2 &&
              Math.abs(state.targetPosition.y - state.deskPosition.y) < 2
            ) {
              // Back at desk — start coding
              state.activity = 'coding'
              state.expression = getExpression('coding')
              state.activityStartTime = now
              state.chatMessage = null
            } else {
              // At break location
              const breakType =
                (state.chatTarget as AgentActivity) ?? 'water_break'
              state.activity = breakType
              state.expression = getExpression(breakType)
              state.activityStartTime = now
            }
          }
        }

        // Update expression based on current activity
        if (state.activity !== 'walking') {
          state.expression = getExpression(state.activity)
        }

        // Clear chat bubbles after timeout
        if (state.chatMessage) {
          // Auto-clear after 4s for non-persistent messages
          if (
            state.activity !== 'thinking' &&
            state.activity !== 'celebrating' &&
            state.activity !== 'frustrated'
          ) {
            if (elapsed > CHAT_BUBBLE_MS && state.activity !== 'walking') {
              // Only clear if it's been showing for a while
            }
          }
        }
      }

      // Cross-agent chat visits
      if (now > nextChatVisitAt.current) {
        const runningSessions = sessions.filter(
          (s) => s.swarmStatus === 'running',
        )
        if (runningSessions.length >= 2) {
          const idx1 = Math.floor(Math.random() * runningSessions.length)
          let idx2 = Math.floor(Math.random() * (runningSessions.length - 1))
          if (idx2 >= idx1) idx2++

          const session1 = runningSessions[idx1]
          const session2 = runningSessions[idx2]
          if (session1 && session2) {
            const key1 = session1.key ?? session1.friendlyId ?? ''
            const key2 = session2.key ?? session2.friendlyId ?? ''
            const state1 = statesRef.current.get(key1)
            const state2 = statesRef.current.get(key2)

            if (
              state1 &&
              state2 &&
              state1.activity === 'coding' &&
              state2.activity === 'coding'
            ) {
              const persona2 = assignPersona(
                key2,
                session2.task ?? session2.label ?? '',
              )
              // Agent 1 walks to agent 2's desk
              state1.activity = 'walking'
              state1.targetPosition = {
                x: state2.deskPosition.x + 5,
                y: state2.deskPosition.y + 3,
              }
              state1.chatTarget = key2
              state1.activityStartTime = now

              // Both get chat bubbles
              const msg = getRandomMessage('chatting').replace(
                '{name}',
                persona2.name,
              )
              state1.chatMessage = msg
              state2.chatMessage = getRandomMessage('chatting').replace(
                '{name}',
                assignPersona(key1, session1.task ?? session1.label ?? '').name,
              )

              // Clear bubbles after delay
              setTimeout(() => {
                const s1 = statesRef.current.get(key1)
                const s2 = statesRef.current.get(key2)
                if (s1) {
                  s1.chatMessage = null
                  // Walk back to own desk
                  if (s1.activity !== 'walking') {
                    s1.activity = 'walking'
                    s1.targetPosition = { ...s1.deskPosition }
                    s1.chatTarget = null
                  }
                }
                if (s2) s2.chatMessage = null
              }, CHAT_BUBBLE_MS + 2000)
            }
          }
        }
        lastChatVisit.current = now
        nextChatVisitAt.current =
          now + randomBetween(CHAT_VISIT_MIN_MS, CHAT_VISIT_MAX_MS)
      }

      // Force re-render
      setTick((t) => t + 1)
    }, TICK_MS)

    return () => clearInterval(interval)
  }, [sessions, getOrCreateState, getDeskIndex])

  // Build view map
  const viewMap = new Map<string, AgentBehaviorView>()
  for (const session of sessions) {
    const key = session.key ?? session.friendlyId ?? ''
    if (!key) continue

    const state = statesRef.current.get(key) ?? getOrCreateState(key)
    const persona = assignPersona(
      key,
      session.task ?? session.initialMessage ?? session.label ?? '',
    )

    const dx = state.targetPosition.x - state.position.x
    const direction: 'left' | 'right' = dx < -0.5 ? 'left' : 'right'
    const isWalking =
      state.activity === 'walking' &&
      !isAtTarget(state.position, state.targetPosition, 2)

    viewMap.set(key, {
      ...state,
      sessionKey: key,
      personaName: persona.name,
      activityEmoji: getActivityEmoji(state.activity),
      direction,
      isWalking,
    })
  }

  return viewMap
}

export { getActivityEmoji }
