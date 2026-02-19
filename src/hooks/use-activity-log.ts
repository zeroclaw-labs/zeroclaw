import { create } from 'zustand'

export type ActivityLogLevel = 'INFO' | 'WARN' | 'ERROR' | 'DEBUG'

export type ActivityLogEntry = {
  id: string
  timestamp: string
  source: string
  session: string
  message: string
  level: ActivityLogLevel
}

type LevelFilters = Record<ActivityLogLevel, boolean>

type ActivityLogState = {
  entries: Array<ActivityLogEntry>
  searchText: string
  sessionFilter: string
  autoScroll: boolean
  levelFilters: LevelFilters
  setSearchText: (value: string) => void
  setSessionFilter: (value: string) => void
  setAutoScroll: (value: boolean) => void
  toggleLevelFilter: (level: ActivityLogLevel) => void
  clearEntries: () => void
  appendMockEntry: () => void
}

const MOCK_SESSIONS = ['main', 'devops', 'perf', 'agent-lab', 'ui']
const MOCK_SOURCES = [
  'orchestrator',
  'gateway',
  'agent-runner',
  'api',
  'heartbeat',
]

const LEVELS: Array<ActivityLogLevel> = ['INFO', 'WARN', 'ERROR', 'DEBUG']

function buildMockMessage(
  level: ActivityLogLevel,
  source: string,
  session: string,
): string {
  if (source === 'heartbeat') {
    return `Heartbeat acknowledged for session "${session}".`
  }
  if (source === 'agent-runner') {
    if (level === 'ERROR') {
      return `Agent spawn failed for "${session}" due to missing tool permission.`
    }
    return `Spawned agent worker for "${session}" with retry-safe handoff.`
  }
  if (source === 'gateway') {
    if (level === 'WARN') {
      return `Stream reconnect for "${session}" took longer than expected.`
    }
    return `Gateway stream chunk delivered for "${session}".`
  }
  if (source === 'api') {
    if (level === 'ERROR') {
      return `POST /api/send-stream returned 500 for "${session}".`
    }
    return `POST /api/history completed for "${session}" in 42ms.`
  }
  if (level === 'DEBUG') {
    return `Orchestrator debug trace recorded for "${session}".`
  }
  return `Session "${session}" scheduled for next orchestration step.`
}

function createEntry(date: Date, index: number): ActivityLogEntry {
  const source = MOCK_SOURCES[index % MOCK_SOURCES.length]
  const session = MOCK_SESSIONS[index % MOCK_SESSIONS.length]
  const level = LEVELS[index % LEVELS.length]
  return {
    id: `${date.getTime()}-${index}`,
    timestamp: date.toISOString(),
    source,
    session,
    level,
    message: buildMockMessage(level, source, session),
  }
}

function _createSeedEntries(total: number): Array<ActivityLogEntry> {
  const now = Date.now()
  return Array.from({ length: total }, function createFromIndex(_, index) {
    const date = new Date(now - (total - index) * 6_000)
    return createEntry(date, index)
  })
}

export const useActivityLog = create<ActivityLogState>((set, get) => ({
  entries: [], // Real entries will come from Gateway event stream when implemented
  searchText: '',
  sessionFilter: 'all',
  autoScroll: true,
  levelFilters: {
    INFO: true,
    WARN: true,
    ERROR: true,
    DEBUG: true,
  },
  setSearchText: function setSearchText(value: string) {
    set({ searchText: value })
  },
  setSessionFilter: function setSessionFilter(value: string) {
    set({ sessionFilter: value })
  },
  setAutoScroll: function setAutoScroll(value: boolean) {
    set({ autoScroll: value })
  },
  toggleLevelFilter: function toggleLevelFilter(level: ActivityLogLevel) {
    set((state) => ({
      levelFilters: {
        ...state.levelFilters,
        [level]: !state.levelFilters[level],
      },
    }))
  },
  clearEntries: function clearEntries() {
    set({ entries: [] })
  },
  appendMockEntry: function appendMockEntry() {
    const current = get().entries
    const nextIndex = current.length + 1
    const nextEntry = createEntry(new Date(), nextIndex)
    set((state) => ({
      entries: [...state.entries, nextEntry],
    }))
  },
}))

// Preserve for debugging/testing
void _createSeedEntries
