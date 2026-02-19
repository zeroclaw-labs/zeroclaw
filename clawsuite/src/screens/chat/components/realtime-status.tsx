import { useMemo } from 'react'
import {
  WifiConnected01Icon,
  WifiDisconnected01Icon,
  Loading03Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  useGatewayChatStore,
  type ConnectionState,
} from '../../../stores/gateway-chat-store'
import { cn } from '@/lib/utils'

type RealtimeStatusProps = {
  className?: string
  showLabel?: boolean
}

const STATUS_CONFIG: Record<
  ConnectionState,
  {
    icon: typeof WifiConnected01Icon
    label: string
    color: string
    animate?: boolean
  }
> = {
  connected: {
    icon: WifiConnected01Icon,
    label: 'Live',
    color: 'text-green-500',
  },
  connecting: {
    icon: Loading03Icon,
    label: 'Connecting...',
    color: 'text-yellow-500',
    animate: true,
  },
  disconnected: {
    icon: WifiDisconnected01Icon,
    label: 'Offline',
    color: 'text-neutral-400',
  },
  error: {
    icon: WifiDisconnected01Icon,
    label: 'Error',
    color: 'text-red-500',
  },
}

export function RealtimeStatus({
  className,
  showLabel = false,
}: RealtimeStatusProps) {
  const connectionState = useGatewayChatStore((s) => s.connectionState)

  const config = useMemo(() => {
    return STATUS_CONFIG[connectionState]
  }, [connectionState])

  return (
    <div
      className={cn(
        'flex items-center gap-1.5 text-xs',
        config.color,
        className,
      )}
      title={`Realtime: ${config.label}`}
    >
      <HugeiconsIcon
        icon={config.icon}
        size={14}
        strokeWidth={2}
        className={cn(config.animate && 'animate-spin')}
      />
      {showLabel && <span className="font-medium">{config.label}</span>}
    </div>
  )
}
