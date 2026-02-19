import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'

type VersionCheckResult = {
  currentVersion: string
  latestVersion: string
  updateAvailable: boolean
  installType: 'git' | 'npm' | 'unknown'
}

let versionCache: { checkedAt: number; result: VersionCheckResult } | null = null
const CACHE_TTL_MS = 15 * 60 * 1000 // 15 minutes

async function checkOpenClawVersion(): Promise<VersionCheckResult> {
  const now = Date.now()
  if (versionCache && now - versionCache.checkedAt < CACHE_TTL_MS) {
    return versionCache.result
  }

  let currentVersion = 'unknown'
  let installType: 'git' | 'npm' | 'unknown' = 'unknown'

  // Try to get version from gateway status RPC
  try {
    const statusResult = await gatewayRpc<Record<string, unknown>>('status')
    if (typeof statusResult?.version === 'string') {
      currentVersion = statusResult.version
    }
    if (typeof statusResult?.installType === 'string') {
      installType = statusResult.installType as 'git' | 'npm' | 'unknown'
    }
  } catch {
    // fallback below
  }

  if (currentVersion === 'unknown') {
    try {
      const { execSync } = await import('node:child_process')
      currentVersion = execSync('openclaw --version', {
        timeout: 5_000,
        encoding: 'utf8',
        stdio: ['pipe', 'pipe', 'pipe'],
      }).trim()
    } catch {
      // Can't determine version
    }
  }

  // Detect install type if gateway didn't tell us
  if (installType === 'unknown') {
    try {
      const { execSync } = await import('node:child_process')
      const clawPath = execSync('which openclaw', {
        timeout: 5_000,
        encoding: 'utf8',
        stdio: ['pipe', 'pipe', 'pipe'],
      }).trim()
      if (clawPath.includes('node_modules')) {
        installType = 'npm'
      } else {
        const { existsSync } = await import('node:fs')
        const { dirname, join } = await import('node:path')
        let dir = dirname(clawPath)
        for (let i = 0; i < 5; i++) {
          if (existsSync(join(dir, '.git'))) {
            installType = 'git'
            break
          }
          const parent = dirname(dir)
          if (parent === dir) break
          dir = parent
        }
        if (installType === 'unknown') installType = 'npm'
      }
    } catch {
      installType = 'npm' // Safe default — prevents accidental restart
    }
  }

  // Check npm registry for latest version
  let latestVersion = currentVersion
  try {
    const res = await fetch('https://registry.npmjs.org/openclaw/latest', {
      signal: AbortSignal.timeout(10_000),
      headers: { Accept: 'application/json' },
    })
    if (res.ok) {
      const data = (await res.json()) as { version?: string }
      if (data.version) latestVersion = data.version
    }
  } catch {
    // Can't check registry — assume up to date
  }

  const updateAvailable =
    currentVersion !== 'unknown' &&
    latestVersion !== currentVersion &&
    latestVersion !== 'unknown'

  const result: VersionCheckResult = {
    currentVersion,
    latestVersion,
    updateAvailable,
    installType,
  }

  versionCache = { checkedAt: now, result }
  return result
}

export const Route = createFileRoute('/api/openclaw-update')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const result = await checkOpenClawVersion()
          return json({ ok: true, ...result })
        } catch (err) {
          return json(
            { ok: false, error: err instanceof Error ? err.message : String(err) },
            { status: 500 },
          )
        }
      },

      POST: async () => {
        try {
          // Re-check install type before attempting update
          const check = await checkOpenClawVersion()
          if (check.installType === 'npm') {
            return json({
              ok: false,
              error: 'OpenClaw is installed via npm. Update with: npm install -g openclaw@latest',
              installType: 'npm',
            })
          }

          const result = await gatewayRpc<{ ok: boolean; error?: string }>(
            'update.run',
            {},
          )

          versionCache = null

          if (result?.ok === false) {
            return json({ ok: false, error: result.error || 'Update failed' })
          }

          return json({ ok: true, message: 'OpenClaw update initiated. Gateway will restart.' })
        } catch (err) {
          const errMsg = err instanceof Error ? err.message : String(err)
          if (errMsg.includes('close') || errMsg.includes('disconnect') || errMsg.includes('ECONNRESET')) {
            versionCache = null
            return json({ ok: true, message: 'OpenClaw is restarting with the update.' })
          }
          return json(
            { ok: false, error: errMsg },
            { status: 500 },
          )
        }
      },
    },
  },
})
