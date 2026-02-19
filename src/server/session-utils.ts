import { gatewayRpc } from './gateway'

type SessionsResolveResponse = {
  ok?: boolean
  key?: string
}

type ResolveSessionKeyInput = {
  rawSessionKey?: string
  friendlyId?: string
  defaultKey?: string
}

type ResolveSessionResult = {
  sessionKey: string
  resolvedVia: 'raw' | 'friendly' | 'default'
}

export async function resolveSessionKey({
  rawSessionKey,
  friendlyId,
  defaultKey = 'main',
}: ResolveSessionKeyInput): Promise<ResolveSessionResult> {
  const trimmedRaw = rawSessionKey?.trim() ?? ''
  if (trimmedRaw.length > 0) {
    return { sessionKey: trimmedRaw, resolvedVia: 'raw' }
  }

  const trimmedFriendly = friendlyId?.trim() ?? ''
  if (trimmedFriendly.length > 0) {
    const resolved = await gatewayRpc<SessionsResolveResponse>(
      'sessions.resolve',
      {
        key: trimmedFriendly,
        includeUnknown: true,
        includeGlobal: true,
      },
    )
    const resolvedKey =
      typeof resolved.key === 'string' ? resolved.key.trim() : ''
    if (resolvedKey.length === 0) {
      throw new Error('session not found')
    }
    return { sessionKey: resolvedKey, resolvedVia: 'friendly' }
  }

  return { sessionKey: defaultKey, resolvedVia: 'default' }
}
