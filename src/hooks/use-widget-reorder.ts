import { useCallback, useState } from 'react'

export type DashboardWidgetOrderId =
  | 'now-card'
  | 'metrics'
  | 'tasks'
  | 'usage'
  | 'skills'
  | 'activity'
  | 'agents'
  | 'sessions'
  | 'notifications'

const STORAGE_KEY = 'dashboard-widget-order'

export const DEFAULT_DASHBOARD_WIDGET_ORDER: Array<DashboardWidgetOrderId> = [
  'now-card',
  'metrics',
  'tasks',
  'usage',
  'skills',
  'activity',
  'agents',
  'sessions',
  'notifications',
]

const DEFAULT_ORDER_SET = new Set<DashboardWidgetOrderId>(
  DEFAULT_DASHBOARD_WIDGET_ORDER,
)

function normalizeOrder(value: unknown): Array<DashboardWidgetOrderId> {
  const seen = new Set<DashboardWidgetOrderId>()
  const ordered: Array<DashboardWidgetOrderId> = []

  if (Array.isArray(value)) {
    for (const entry of value) {
      if (typeof entry !== 'string') continue
      if (!DEFAULT_ORDER_SET.has(entry as DashboardWidgetOrderId)) continue
      const id = entry as DashboardWidgetOrderId
      if (seen.has(id)) continue
      seen.add(id)
      ordered.push(id)
    }
  }

  for (const id of DEFAULT_DASHBOARD_WIDGET_ORDER) {
    if (seen.has(id)) continue
    ordered.push(id)
  }

  return ordered
}

function loadWidgetOrder(): Array<DashboardWidgetOrderId> {
  if (typeof window === 'undefined') return [...DEFAULT_DASHBOARD_WIDGET_ORDER]

  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return [...DEFAULT_DASHBOARD_WIDGET_ORDER]
    return normalizeOrder(JSON.parse(raw))
  } catch {
    return [...DEFAULT_DASHBOARD_WIDGET_ORDER]
  }
}

function saveWidgetOrder(order: Array<DashboardWidgetOrderId>) {
  if (typeof window === 'undefined') return

  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(order))
  } catch {
    // Ignore storage write failures (private mode / quota).
  }
}

export function useWidgetReorder() {
  const [order, setOrder] = useState<Array<DashboardWidgetOrderId>>(
    loadWidgetOrder,
  )

  const moveWidget = useCallback((fromIndex: number, toIndex: number) => {
    setOrder((previousOrder) => {
      if (
        fromIndex < 0 ||
        toIndex < 0 ||
        fromIndex >= previousOrder.length ||
        toIndex >= previousOrder.length ||
        fromIndex === toIndex
      ) {
        return previousOrder
      }

      const nextOrder = [...previousOrder]
      const [moved] = nextOrder.splice(fromIndex, 1)
      if (!moved) return previousOrder
      nextOrder.splice(toIndex, 0, moved)
      saveWidgetOrder(nextOrder)
      return nextOrder
    })
  }, [])

  const resetOrder = useCallback(() => {
    const nextOrder = [...DEFAULT_DASHBOARD_WIDGET_ORDER]
    setOrder(nextOrder)
    saveWidgetOrder(nextOrder)
  }, [])

  return { order, moveWidget, resetOrder }
}
