type Tombstone = {
  id: string
  expiresAt: number
}

const TOMBSTONE_TTL_MS = 8000
const tombstones = new Map<string, Tombstone>()

export function markSessionDeleted(id: string) {
  if (!id) return
  tombstones.set(id, { id, expiresAt: Date.now() + TOMBSTONE_TTL_MS })
}

export function clearSessionDeleted(id: string) {
  if (!id) return
  tombstones.delete(id)
}

export function filterSessionsWithTombstones<
  T extends { key: string; friendlyId: string },
>(sessions: Array<T>) {
  if (tombstones.size === 0) return sessions
  const now = Date.now()
  let changed = false
  const next = sessions.filter((session) => {
    const keyTombstone = tombstones.get(session.key)
    const friendlyTombstone = tombstones.get(session.friendlyId)
    const isExpired =
      (keyTombstone && keyTombstone.expiresAt <= now) ||
      (friendlyTombstone && friendlyTombstone.expiresAt <= now)
    if (isExpired) {
      if (keyTombstone && keyTombstone.expiresAt <= now) {
        tombstones.delete(session.key)
      }
      if (friendlyTombstone && friendlyTombstone.expiresAt <= now) {
        tombstones.delete(session.friendlyId)
      }
      return true
    }
    if (keyTombstone || friendlyTombstone) {
      changed = true
      return false
    }
    return true
  })
  // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
  return changed ? next : sessions
}
