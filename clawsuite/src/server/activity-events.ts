import EventEmitter from 'node:events'
import type { ActivityEvent } from '../types/activity-event'

const MAX_ACTIVITY_EVENTS = 100
const ACTIVITY_EVENT_NAME = 'activity'

type ActivityEventCallback = (event: ActivityEvent) => void

const activityEmitter = new EventEmitter()
const activityBuffer: Array<ActivityEvent> = []

activityEmitter.setMaxListeners(0)

export function pushEvent(event: ActivityEvent) {
  activityBuffer.push(event)
  if (activityBuffer.length > MAX_ACTIVITY_EVENTS) {
    activityBuffer.shift()
  }
  activityEmitter.emit(ACTIVITY_EVENT_NAME, event)
}

export function getRecentEvents(count = 50): Array<ActivityEvent> {
  const normalizedCount = Number.isFinite(count)
    ? Math.max(1, Math.min(MAX_ACTIVITY_EVENTS, Math.floor(count)))
    : 50

  if (activityBuffer.length <= normalizedCount) {
    return [...activityBuffer]
  }

  return activityBuffer.slice(activityBuffer.length - normalizedCount)
}

export function onEvent(callback: ActivityEventCallback) {
  activityEmitter.on(ACTIVITY_EVENT_NAME, callback)
}

export function offEvent(callback: ActivityEventCallback) {
  activityEmitter.off(ACTIVITY_EVENT_NAME, callback)
}
