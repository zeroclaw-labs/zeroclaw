import type { GatewayAttachment, GatewayMessage } from './types'

export type PendingSendPayload = {
  sessionKey: string
  friendlyId: string
  message: string
  attachments: Array<GatewayAttachment>
  optimisticMessage: GatewayMessage
}

let pendingSend: PendingSendPayload | null = null
let pendingGeneration = false
let recentSession: { friendlyId: string; at: number } | null = null

export function stashPendingSend(payload: PendingSendPayload) {
  pendingSend = payload
}

export function hasPendingSend() {
  return pendingSend !== null
}

export function setPendingGeneration(value: boolean) {
  pendingGeneration = value
}

export function hasPendingGeneration() {
  return pendingGeneration
}

export function resetPendingSend() {
  pendingSend = null
  pendingGeneration = false
}

export function clearPendingSendForSession(
  sessionKey: string,
  friendlyId: string,
) {
  if (!pendingSend) return
  if (sessionKey && pendingSend.sessionKey === sessionKey) {
    resetPendingSend()
    return
  }
  if (friendlyId && pendingSend.friendlyId === friendlyId) {
    resetPendingSend()
  }
}

export function setRecentSession(friendlyId: string) {
  recentSession = { friendlyId, at: Date.now() }
}

export function isRecentSession(friendlyId: string, maxAgeMs = 15000) {
  if (!recentSession) return false
  if (recentSession.friendlyId !== friendlyId) return false
  if (Date.now() - recentSession.at > maxAgeMs) return false
  return true
}

export function consumePendingSend(
  sessionKey: string,
  friendlyId?: string,
): PendingSendPayload | null {
  if (!pendingSend) return null
  if (sessionKey && pendingSend.sessionKey === sessionKey) {
    const payload = pendingSend
    pendingSend = null
    return payload
  }
  if (friendlyId && pendingSend.friendlyId === friendlyId) {
    const payload = pendingSend
    pendingSend = null
    return payload
  }
  return null
}
