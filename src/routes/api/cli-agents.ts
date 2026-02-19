import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'

const execFileAsync = promisify(execFile)

const NAME_ADJECTIVES = [
  'amber',
  'brisk',
  'calm',
  'daring',
  'ember',
  'frost',
  'golden',
  'harbor',
  'ivory',
  'jade',
  'lunar',
  'misty',
  'nova',
  'quiet',
  'raven',
  'solar',
  'swift',
  'tidal',
  'vivid',
  'wild',
]

const NAME_NOUNS = [
  'anchor',
  'bloom',
  'canyon',
  'drift',
  'engine',
  'falcon',
  'forge',
  'glade',
  'harbor',
  'isle',
  'journey',
  'knoll',
  'meadow',
  'nexus',
  'orbit',
  'peak',
  'ridge',
  'signal',
  'summit',
  'trail',
]

type CliAgentStatus = 'running' | 'finished'

type AgentProcess = {
  pid: number
  stat: string
  command: string
}

type CliAgent = {
  pid: number
  name: string
  task: string
  runtimeSeconds: number
  status: CliAgentStatus
}

function parsePsAuxOutput(output: string): Array<AgentProcess> {
  const lines = output.split('\n')
  const entries: Array<AgentProcess> = []

  for (const line of lines.slice(1)) {
    const trimmed = line.trim()
    if (!trimmed) continue

    const columns = trimmed.split(/\s+/)
    if (columns.length < 11) continue

    const pid = Number.parseInt(columns[1] ?? '', 10)
    if (!Number.isFinite(pid)) continue

    const stat = columns[7] ?? ''
    const command = columns.slice(10).join(' ')
    if (!command) continue
    if (!command.toLowerCase().includes('codex')) continue

    entries.push({ pid, stat, command })
  }

  return entries
}

function parseElapsedToSeconds(elapsed: string): number {
  const value = elapsed.trim()
  if (!value) return 0

  let daySeconds = 0
  let clock = value

  if (value.includes('-')) {
    const [daysPart, timePart] = value.split('-', 2)
    const days = Number.parseInt(daysPart, 10)
    if (Number.isFinite(days) && days > 0) {
      daySeconds = days * 24 * 60 * 60
    }
    clock = timePart
  }

  const segments = clock.split(':').map(function parseSegment(segment) {
    return Number.parseInt(segment, 10)
  })

  if (
    segments.some(function hasInvalid(valuePart) {
      return !Number.isFinite(valuePart)
    })
  ) {
    return daySeconds
  }

  if (segments.length === 3) {
    const [hours, minutes, seconds] = segments
    return daySeconds + hours * 60 * 60 + minutes * 60 + seconds
  }

  if (segments.length === 2) {
    const [minutes, seconds] = segments
    return daySeconds + minutes * 60 + seconds
  }

  if (segments.length === 1) {
    return daySeconds + segments[0]
  }

  return daySeconds
}

async function readRuntimeByPid(
  pids: Array<number>,
): Promise<Map<number, number>> {
  const runtimeByPid = new Map<number, number>()
  if (!pids.length) return runtimeByPid

  try {
    const { stdout } = await execFileAsync('ps', [
      '-o',
      'pid=,etime=',
      '-p',
      pids.join(','),
    ])

    for (const line of stdout.split('\n')) {
      const trimmed = line.trim()
      if (!trimmed) continue

      const match = trimmed.match(/^(\d+)\s+(\S+)$/)
      if (!match) continue

      const pid = Number.parseInt(match[1], 10)
      const runtimeSeconds = parseElapsedToSeconds(match[2])

      if (Number.isFinite(pid) && Number.isFinite(runtimeSeconds)) {
        runtimeByPid.set(pid, runtimeSeconds)
      }
    }
  } catch {
    return runtimeByPid
  }

  return runtimeByPid
}

function stripQuotes(value: string): string {
  const trimmed = value.trim()
  const hasDoubleQuotes = trimmed.startsWith('"') && trimmed.endsWith('"')
  const hasSingleQuotes = trimmed.startsWith("'") && trimmed.endsWith("'")
  if (hasDoubleQuotes || hasSingleQuotes) {
    return trimmed.slice(1, -1)
  }
  return trimmed
}

function normalizeTask(value: string): string {
  const normalized = value.trim().replace(/\s+/g, ' ')
  return normalized || 'No task description'
}

function tokenizeCommand(command: string): Array<string> {
  const tokens = command.match(/"[^"]*"|'[^']*'|\S+/g)
  return tokens ?? []
}

function extractTaskFromCommand(command: string): string {
  const tokens = tokenizeCommand(command)
  if (!tokens.length) return 'No task description'

  const codexIndex = tokens.findIndex(function isCodexToken(token) {
    return stripQuotes(token).toLowerCase().includes('codex')
  })

  const args = codexIndex >= 0 ? tokens.slice(codexIndex + 1) : tokens

  for (let i = 0; i < args.length; i += 1) {
    const current = stripQuotes(args[i] ?? '')
    if (!current) continue

    if (current.startsWith('--task=')) {
      return normalizeTask(current.slice('--task='.length))
    }

    if (current.startsWith('--prompt=')) {
      return normalizeTask(current.slice('--prompt='.length))
    }

    if (
      current === '--task' ||
      current === '--prompt' ||
      current === '-t' ||
      current === '-p'
    ) {
      const next = stripQuotes(args[i + 1] ?? '')
      if (next) return normalizeTask(next)
    }
  }

  const meaningfulParts: Array<string> = []

  for (const arg of args) {
    const part = stripQuotes(arg)
    if (!part) continue
    if (part.startsWith('-')) continue
    meaningfulParts.push(part)
  }

  if (!meaningfulParts.length) {
    return 'No task description'
  }

  return normalizeTask(meaningfulParts.join(' '))
}

function hashPid(pid: number): number {
  let value = pid | 0
  value = Math.imul(value ^ 0x45d9f3b, 0x45d9f3b)
  value ^= value >>> 16
  return value >>> 0
}

function createAgentName(pid: number): string {
  const hash = hashPid(pid)
  const adjective = NAME_ADJECTIVES[hash % NAME_ADJECTIVES.length]
  const nounIndex =
    Math.floor(hash / NAME_ADJECTIVES.length) % NAME_NOUNS.length
  const noun = NAME_NOUNS[nounIndex]
  return `${adjective}-${noun}`
}

function resolveStatus(stat: string): CliAgentStatus {
  if (stat.includes('Z') || stat.includes('T') || stat.includes('X')) {
    return 'finished'
  }
  return 'running'
}

function toCliAgent(
  processEntry: AgentProcess,
  runtimeByPid: Map<number, number>,
): CliAgent {
  return {
    pid: processEntry.pid,
    name: createAgentName(processEntry.pid),
    task: extractTaskFromCommand(processEntry.command),
    runtimeSeconds: runtimeByPid.get(processEntry.pid) ?? 0,
    status: resolveStatus(processEntry.stat),
  }
}

export const Route = createFileRoute('/api/cli-agents')({
  server: {
    handlers: {
      GET: async function getCliAgents() {
        try {
          const { stdout } = await execFileAsync('ps', ['aux'])
          const processes = parsePsAuxOutput(stdout)
          const runtimeByPid = await readRuntimeByPid(
            processes.map(function getPid(entry) {
              return entry.pid
            }),
          )

          const agents = processes
            .map(function mapToAgent(entry) {
              return toCliAgent(entry, runtimeByPid)
            })
            .sort(function sortAgents(a, b) {
              if (a.runtimeSeconds !== b.runtimeSeconds) {
                return b.runtimeSeconds - a.runtimeSeconds
              }
              return a.pid - b.pid
            })

          return json({ agents })
        } catch (error) {
          return json(
            {
              agents: [],
              error: error instanceof Error ? error.message : String(error),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
