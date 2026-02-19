/**
 * Widget metadata — structural prep for future widget management.
 * NO UI built yet. This formalizes widget scope and visibility.
 */
export type WidgetScope = 'dashboard' | 'workspace'
export type WidgetTier = 'primary' | 'secondary' | 'demo'

export type WidgetMeta = {
  id: string
  label: string
  scope: WidgetScope
  tier: WidgetTier
  defaultVisible: boolean
}

export const WIDGET_META: WidgetMeta[] = [
  {
    id: 'skills',
    label: 'Skills',
    scope: 'dashboard',
    tier: 'primary',
    defaultVisible: true,
  },
  {
    id: 'agent-status',
    label: 'Active Agents',
    scope: 'dashboard',
    tier: 'primary',
    defaultVisible: true,
  },
  {
    id: 'usage-meter',
    label: 'Usage Meter',
    scope: 'dashboard',
    tier: 'primary',
    defaultVisible: true,
  },
  {
    id: 'recent-sessions',
    label: 'Recent Sessions',
    scope: 'dashboard',
    tier: 'secondary',
    defaultVisible: true,
  },
  {
    id: 'activity-log',
    label: 'Activity Log',
    scope: 'dashboard',
    tier: 'secondary',
    defaultVisible: true,
  },
  {
    id: 'notifications',
    label: 'Notifications',
    scope: 'dashboard',
    tier: 'secondary',
    defaultVisible: true,
  },
  {
    id: 'tasks',
    label: 'Tasks',
    scope: 'dashboard',
    tier: 'demo',
    defaultVisible: true,
  },
]
