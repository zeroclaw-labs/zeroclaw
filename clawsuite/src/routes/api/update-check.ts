import { execSync } from 'node:child_process'
import path from 'node:path'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'

/**
 * Checks if the local repo is behind the remote.
 * Returns local commit, remote commit, and whether an update is available.
 */

const CHECK_COOLDOWN_MS = 5 * 60 * 1000 // 5 min cache
let lastCheck: { at: number; result: UpdateCheckResult } | null = null

type CommitEntry = {
  hash: string
  subject: string
  date: string
}

type UpdateCheckResult = {
  updateAvailable: boolean
  localVersion: string
  remoteVersion: string
  localCommit: string
  remoteCommit: string
  localDate: string
  remoteDate: string
  behindBy: number
  repoPath: string
  changelog: Array<CommitEntry>
}

let gitAvailable: boolean | null = null

function isGitAvailable(): boolean {
  if (gitAvailable !== null) return gitAvailable
  try {
    execSync('git --version', { timeout: 5_000, stdio: 'pipe' })
    gitAvailable = true
  } catch {
    gitAvailable = false
  }
  return gitAvailable
}

function runGit(args: string, cwd: string): string {
  if (!isGitAvailable()) return ''
  try {
    return execSync(`git ${args}`, {
      cwd,
      timeout: 15_000,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim()
  } catch {
    return ''
  }
}

function readPackageVersion(repoPath: string): string {
  try {
    const pkg = require(path.join(repoPath, 'package.json')) as {
      version?: string
    }
    return pkg.version || '0.0.0'
  } catch {
    return '0.0.0'
  }
}

function buildVersionLabel(baseVersion: string, commit: string): string {
  return `${baseVersion} (${commit})`
}

function detectRemote(repoPath: string): string {
  // Prefer 'production' or 'studio' remote (Eric's fork), fall back to 'origin'
  const remotes = runGit('remote', repoPath)
    .split('\n')
    .map((r) => r.trim())
    .filter(Boolean)
  if (remotes.includes('production')) return 'production'
  if (remotes.includes('studio')) return 'studio'
  return 'origin'
}

function checkForUpdates(): UpdateCheckResult {
  const repoPath = path.resolve(process.cwd())
  const pkgVersion = readPackageVersion(repoPath)
  const remote = detectRemote(repoPath)

  // Fetch latest from remote (quiet, won't fail if offline)
  runGit(`fetch ${remote} --quiet`, repoPath)

  const currentBranch =
    runGit('rev-parse --abbrev-ref HEAD', repoPath) || 'main'
  const localCommit = runGit('rev-parse --short HEAD', repoPath)
  const localDate = runGit('log -1 --format=%ci', repoPath)

  const remoteRef = `${remote}/${currentBranch}`
  const remoteCommit = runGit(`rev-parse --short ${remoteRef}`, repoPath)
  const remoteDate = runGit(`log -1 --format=%ci ${remoteRef}`, repoPath)

  // Count commits behind
  const behindCount = runGit(`rev-list --count HEAD..${remoteRef}`, repoPath)
  const behindBy = parseInt(behindCount, 10) || 0

  // Build version labels
  const localVersion = buildVersionLabel(pkgVersion, localCommit)
  const remoteVersion =
    behindBy > 0 ? buildVersionLabel(pkgVersion, remoteCommit) : localVersion

  // Get changelog (up to 20 commits)
  const changelog: Array<CommitEntry> = []
  if (behindBy > 0) {
    const logOutput = runGit(
      `log HEAD..${remoteRef} --format=%h||%s||%ci -n 20`,
      repoPath,
    )
    for (const line of logOutput.split('\n')) {
      if (!line.trim()) continue
      const [hash, subject, date] = line.split('||')
      if (hash && subject) {
        changelog.push({
          hash: hash.trim(),
          subject: subject.trim(),
          date: (date || '').trim(),
        })
      }
    }
  }

  return {
    updateAvailable: behindBy > 0,
    localVersion,
    remoteVersion,
    localCommit,
    remoteCommit: remoteCommit || localCommit,
    localDate,
    remoteDate: remoteDate || localDate,
    behindBy,
    repoPath,
    changelog,
  }
}

function runUpdate(): { ok: boolean; output: string } {
  if (!isGitAvailable()) {
    return { ok: false, output: 'git is not available in this environment' }
  }

  const repoPath = path.resolve(process.cwd())
  const remote = detectRemote(repoPath)
  const currentBranch =
    runGit('rev-parse --abbrev-ref HEAD', repoPath) || 'main'

  try {
    // Pull latest — use merge (not rebase) to avoid conflict hell with forked repos
    const pullOutput = execSync(`git pull ${remote} ${currentBranch}`, {
      cwd: repoPath,
      timeout: 30_000,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim()

    // Install deps
    const installOutput = execSync('npm install --prefer-offline', {
      cwd: repoPath,
      timeout: 120_000,
      encoding: 'utf8',
    }).trim()

    // Clear update cache so next check shows up-to-date
    lastCheck = null

    return {
      ok: true,
      output: `${pullOutput}\n\n${installOutput}`,
    }
  } catch (err) {
    return {
      ok: false,
      output: err instanceof Error ? err.message : String(err),
    }
  }
}

export const Route = createFileRoute('/api/update-check')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const now = Date.now()
          if (lastCheck && now - lastCheck.at < CHECK_COOLDOWN_MS) {
            return json(lastCheck.result)
          }

          const result = checkForUpdates()
          lastCheck = { at: now, result }
          return json(result)
        } catch (err) {
          return json(
            { error: err instanceof Error ? err.message : String(err) },
            { status: 500 },
          )
        }
      },
      POST: async () => {
        try {
          const result = runUpdate()
          return json(result)
        } catch (err) {
          return json(
            {
              ok: false,
              output: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
