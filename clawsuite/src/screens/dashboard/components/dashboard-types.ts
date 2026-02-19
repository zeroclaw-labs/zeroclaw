import type { ComponentProps } from 'react'
import type { HugeiconsIcon } from '@hugeicons/react'

export type DashboardIcon = ComponentProps<typeof HugeiconsIcon>['icon']

export type QuickAction = {
  id: string
  label: string
  description: string
  to: '/new' | '/terminal' | '/skills' | '/files'
  icon: DashboardIcon
}

export type SystemStatus = {
  gateway: {
    connected: boolean
    checkedAtIso: string
  }
  uptimeSeconds: number
  currentModel: string
  sessionCount: number
}

export type CostDay = {
  dateIso: string
  amountUsd: number
}

export type RecentSession = {
  friendlyId: string
  title: string
  preview: string
  updatedAt: number
}

export type WeatherForecastDay = {
  id: string
  label: string
  highC: number
  lowC: number
  condition: string
  emoji: string
}

export type WeatherSnapshot = {
  location: string
  temperatureC: number
  condition: string
  emoji: string
  forecast: Array<WeatherForecastDay>
}

export type TodoTask = {
  id: string
  text: string
  completed: boolean
  source: 'local' | 'gateway'
}

export type DashboardNotification = {
  id: string
  label: string
  detail: string
  occurredAt: number
}

export type AgentStatusSummary = {
  connected: boolean
  model: string
  provider: string
  activeSessions: number
}
