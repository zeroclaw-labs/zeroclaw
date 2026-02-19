'use client'

import { useQuery } from '@tanstack/react-query'

async function pingGateway(): Promise<boolean> {
  try {
    const response = await fetch('/api/ping', {
      signal: AbortSignal.timeout(5000),
    })
    if (!response.ok) return false
    const data = (await response.json()) as { ok?: boolean }
    return Boolean(data.ok)
  } catch {
    return false
  }
}

/**
 * Minimal dot-only status indicator (no text).
 * Shows green (connected), yellow (connecting), or red (offline).
 */
export function GatewayStatusDot() {
  const { data: isConnected, isLoading } = useQuery({
    queryKey: ['gateway', 'ping'],
    queryFn: pingGateway,
    refetchInterval: 15_000,
    retry: false,
  })

  const dotColor = isLoading
    ? 'bg-yellow-400'
    : isConnected
      ? 'bg-emerald-400'
      : 'bg-red-400'

  const label = isLoading
    ? 'Connecting...'
    : isConnected
      ? 'Connected'
      : 'Offline'

  return (
    <span className="relative flex h-2 w-2 shrink-0" title={`Gateway: ${label}`}>
      {isConnected && (
        <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400/40" />
      )}
      <span className={`relative inline-flex h-2 w-2 rounded-full ${dotColor}`} />
    </span>
  )
}

export function GatewayStatusIndicator({
  collapsed,
  inline,
}: {
  collapsed?: boolean
  inline?: boolean
}) {
  const { data: isConnected, isLoading } = useQuery({
    queryKey: ['gateway', 'ping'],
    queryFn: pingGateway,
    refetchInterval: 15_000,
    retry: false,
  })

  const dotColor = isLoading
    ? 'bg-yellow-400'
    : isConnected
      ? 'bg-emerald-400'
      : 'bg-red-400'

  const pulseColor = isLoading
    ? 'bg-yellow-400/40'
    : isConnected
      ? 'bg-emerald-400/40'
      : 'bg-red-400/40'

  const label = isLoading
    ? 'Connecting...'
    : isConnected
      ? 'Connected'
      : 'Offline'

  if (inline) {
    return (
      <span className="flex items-center gap-1.5" title={`Gateway ${label}`}>
        <span className="relative flex h-1.5 w-1.5 shrink-0">
          {(isLoading || isConnected) && (
            <span
              className={`absolute inline-flex h-full w-full animate-ping rounded-full ${pulseColor}`}
            />
          )}
          <span
            className={`relative inline-flex h-1.5 w-1.5 rounded-full ${dotColor}`}
          />
        </span>
        <span className="text-[10px] text-primary-400 dark:text-gray-500">{label}</span>
      </span>
    )
  }

  return (
    <div className="flex items-center gap-2 px-2 py-1.5" title={`Gateway ${label}`}>
      <span className="relative flex h-2 w-2 shrink-0">
        {(isLoading || isConnected) && (
          <span
            className={`absolute inline-flex h-full w-full animate-ping rounded-full ${pulseColor}`}
          />
        )}
        <span
          className={`relative inline-flex h-2 w-2 rounded-full ${dotColor}`}
        />
      </span>
      {!collapsed && (
        <span className="truncate text-[11px] text-primary-500 dark:text-gray-400">{label}</span>
      )}
    </div>
  )
}
