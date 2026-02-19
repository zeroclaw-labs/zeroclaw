/**
 * Gateway Auto-Discovery
 *
 * Automatically finds a local OpenClaw gateway by:
 * 1. Reading ~/.openclaw/openclaw.json for port + auth token
 * 2. Falling back to `openclaw config get` CLI commands
 * 3. Probing default port 18789
 *
 * This lets ClawSuite connect seamlessly without manual config.
 */

import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { homedir } from 'node:os'
import { exec as execCb } from 'node:child_process'
import { promisify } from 'node:util'

const execAsync = promisify(execCb)

export type DiscoveryResult = {
  found: boolean
  url?: string
  token?: string
  password?: string
  source?: 'config-file' | 'cli' | 'env' | 'none'
  error?: string
}

/**
 * Try to read gateway config from ~/.openclaw/openclaw.json
 */
async function discoverFromConfigFile(): Promise<DiscoveryResult> {
  try {
    const configPath = join(homedir(), '.openclaw', 'openclaw.json')
    const raw = await readFile(configPath, 'utf-8')
    const config = JSON.parse(raw) as {
      gateway?: {
        port?: number
        auth?: { token?: string; mode?: string }
      }
    }

    // The gateway config might be at top-level in the JSON or nested
    // Let's also try reading the gateway section via the structure we saw
    const port = config.gateway?.port
    const token = config.gateway?.auth?.token

    if (token) {
      const url = `ws://127.0.0.1:${port || 18789}`
      return { found: true, url, token, source: 'config-file' }
    }

    // Config file exists but no gateway token in it — try CLI
    return { found: false, source: 'none' }
  } catch {
    return { found: false, source: 'none' }
  }
}

/**
 * Try to get gateway config via `openclaw config get` CLI
 */
async function discoverFromCli(): Promise<DiscoveryResult> {
  try {
    const [tokenResult, portResult] = await Promise.allSettled([
      execAsync('openclaw config get gateway.auth.token', { timeout: 5000 }),
      execAsync('openclaw config get gateway.port', { timeout: 5000 }),
    ])

    const token =
      tokenResult.status === 'fulfilled'
        ? tokenResult.value.stdout.trim()
        : undefined
    const portStr =
      portResult.status === 'fulfilled'
        ? portResult.value.stdout.trim()
        : undefined

    if (token) {
      const port = portStr ? parseInt(portStr, 10) : 18789
      const url = `ws://127.0.0.1:${isNaN(port) ? 18789 : port}`
      return { found: true, url, token, source: 'cli' }
    }

    return { found: false, source: 'none' }
  } catch {
    return { found: false, source: 'none' }
  }
}

/**
 * Check if env vars are already set
 */
function discoverFromEnv(): DiscoveryResult {
  const url = process.env.CLAWDBOT_GATEWAY_URL?.trim()
  const token = process.env.CLAWDBOT_GATEWAY_TOKEN?.trim()
  const password = process.env.CLAWDBOT_GATEWAY_PASSWORD?.trim()

  if (token || password) {
    return {
      found: true,
      url: url || 'ws://127.0.0.1:18789',
      token: token || undefined,
      password: password || undefined,
      source: 'env',
    }
  }

  return { found: false, source: 'none' }
}

/**
 * Probe if a WebSocket server is listening on the given port
 */
async function probePort(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const net = require('node:net') as typeof import('node:net')
    const socket = net.createConnection({ host: '127.0.0.1', port }, () => {
      socket.destroy()
      resolve(true)
    })
    socket.on('error', () => resolve(false))
    socket.setTimeout(2000, () => {
      socket.destroy()
      resolve(false)
    })
  })
}

/**
 * Main discovery function. Tries all methods in order:
 * 1. Existing env vars (already configured)
 * 2. OpenClaw config file (~/.openclaw/openclaw.json)
 * 3. OpenClaw CLI (`openclaw config get`)
 *
 * If token is found, also writes it to process.env so the gateway client picks it up.
 */
export async function discoverGateway(): Promise<DiscoveryResult> {
  // 1. Check env vars first (user already configured)
  const envResult = discoverFromEnv()
  if (envResult.found) return envResult

  // 2. Try config file (fastest, no subprocess)
  const fileResult = await discoverFromConfigFile()
  if (fileResult.found) {
    // Apply to process.env so gateway client uses it
    if (fileResult.url) process.env.CLAWDBOT_GATEWAY_URL = fileResult.url
    if (fileResult.token) process.env.CLAWDBOT_GATEWAY_TOKEN = fileResult.token
    return fileResult
  }

  // 3. Try CLI (slower, but works if config file structure differs)
  const cliResult = await discoverFromCli()
  if (cliResult.found) {
    if (cliResult.url) process.env.CLAWDBOT_GATEWAY_URL = cliResult.url
    if (cliResult.token) process.env.CLAWDBOT_GATEWAY_TOKEN = cliResult.token
    return cliResult
  }

  // 4. Last resort: check if anything is listening on default port
  const portOpen = await probePort(18789)
  if (portOpen) {
    return {
      found: false,
      url: 'ws://127.0.0.1:18789',
      source: 'none',
      error: 'Gateway found on port 18789 but no auth token discovered. Please enter your token.',
    }
  }

  return {
    found: false,
    source: 'none',
    error: 'No local OpenClaw gateway found. Please start OpenClaw or enter connection details manually.',
  }
}
