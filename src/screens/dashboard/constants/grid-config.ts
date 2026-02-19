/**
 * Dashboard widget registry.
 *
 * react-grid-layout has been removed; this now only defines
 * canonical widget ids plus default sizing metadata.
 */
export type WidgetId =
  | 'skills'
  | 'usage-meter'
  | 'tasks'
  | 'agent-status'
  | 'recent-sessions'
  | 'notifications'
  | 'activity-log'

export type WidgetRegistryEntry = {
  id: WidgetId
  defaultSize: 'medium' | 'large'
}

export const WIDGET_REGISTRY: Array<WidgetRegistryEntry> = [
  { id: 'usage-meter', defaultSize: 'large' },
  { id: 'agent-status', defaultSize: 'medium' },
  { id: 'recent-sessions', defaultSize: 'medium' },
  { id: 'tasks', defaultSize: 'medium' },
  { id: 'skills', defaultSize: 'medium' },
  { id: 'activity-log', defaultSize: 'large' },
  { id: 'notifications', defaultSize: 'medium' },
]
