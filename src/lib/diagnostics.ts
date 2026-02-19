/**
 * Diagnostics bundle generation with redaction
 * Phase 2.4-001: Export diagnostics bundle (safe)
 */

// Patterns that indicate sensitive data - must be redacted
const SENSITIVE_PATTERNS = [
  /sk-[a-zA-Z0-9]{20,}/gi, // OpenAI API keys
  /sk-ant-[a-zA-Z0-9-]{20,}/gi, // Anthropic API keys
  /ghp_[a-zA-Z0-9]{36}/gi, // GitHub PATs
  /github_pat_[a-zA-Z0-9_]{10,}/gi, // GitHub fine-grained PATs (shorter min)
  /gho_[a-zA-Z0-9]{36}/gi, // GitHub OAuth tokens
  /glpat-[a-zA-Z0-9-]{20}/gi, // GitLab PATs
  /xox[baprs]-[a-zA-Z0-9-]{10,}/gi, // Slack tokens
  /Bearer\s+[a-zA-Z0-9._-]+/gi, // Bearer tokens
  /token[=:]\s*["']?[a-zA-Z0-9._-]{8,}["']?/gi, // Generic tokens (min 8 chars)
  /secret[=:]\s*["']?[a-zA-Z0-9._-]{8,}["']?/gi, // Generic secrets
  /password[=:]\s*["']?[^\s"']{4,}["']?/gi, // Passwords
  /api[_-]?key[=:]\s*["']?[a-zA-Z0-9._-]{10,}["']?/gi, // API keys
  /authorization[=:]\s*["']?[a-zA-Z0-9._-]+["']?/gi, // Auth headers
  /\/Users\/[^\/]+/g, // Full user paths (macOS)
  /\/home\/[^\/]+/g, // Full user paths (Linux)
  /C:\\Users\\[^\\]+/gi, // Full user paths (Windows)
]

// Redact sensitive patterns from text
export function redactSensitiveData(text: string): string {
  let redacted = text

  for (const pattern of SENSITIVE_PATTERNS) {
    redacted = redacted.replace(pattern, '[REDACTED]')
  }

  return redacted
}

// Redact object values recursively
export function redactObject<T extends Record<string, unknown>>(obj: T): T {
  const result = { ...obj } as Record<string, unknown>

  for (const key of Object.keys(result)) {
    const value = result[key]

    // Skip null/undefined
    if (value == null) continue

    // Check if key itself suggests sensitive data
    const lowerKey = key.toLowerCase()
    if (
      lowerKey.includes('token') ||
      lowerKey.includes('secret') ||
      lowerKey.includes('password') ||
      lowerKey.includes('apikey') ||
      lowerKey.includes('api_key') ||
      lowerKey.includes('bearer') ||
      lowerKey.includes('authorization') ||
      lowerKey.includes('credential')
    ) {
      result[key] = '[REDACTED]'
      continue
    }

    // Recurse into objects
    if (typeof value === 'object' && !Array.isArray(value)) {
      result[key] = redactObject(value as Record<string, unknown>)
      continue
    }

    // Recurse into arrays
    if (Array.isArray(value)) {
      result[key] = value.map((item) => {
        if (typeof item === 'string') return redactSensitiveData(item)
        if (typeof item === 'object' && item !== null) {
          return redactObject(item as Record<string, unknown>)
        }
        return item
      })
      continue
    }

    // Redact strings
    if (typeof value === 'string') {
      result[key] = redactSensitiveData(value)
    }
  }

  return result as T
}

// Extract just the folder name from a path (not full path)
export function extractFolderName(fullPath: string | null | undefined): string {
  if (!fullPath) return 'Not set'
  const parts = fullPath.replace(/\\/g, '/').split('/')
  return parts[parts.length - 1] || 'Unknown'
}

export type DiagnosticsBundle = {
  version: string
  generatedAt: string
  environment: {
    appVersion: string
    os: string
    nodeVersion: string
    userAgent: string
  }
  gateway: {
    status: 'connected' | 'disconnected' | 'unknown'
    url: string
    uptime: string | null
  }
  workspace: {
    folderName: string
  }
  providers: Array<{
    name: string
    status: 'active' | 'configured' | 'inactive'
    modelCount: number
  }>
  recentEvents: Array<{
    timestamp: string
    level: string
    title: string
    source: string
  }>
  debugEntries: Array<{
    timestamp: string
    suggestion: string
    triggeredBy: string
  }>
}

export const DIAGNOSTICS_BUNDLE_VERSION = '1.0.0'

export function generateBundleFilename(): string {
  const date = new Date().toISOString().slice(0, 10)
  const time = new Date().toISOString().slice(11, 19).replace(/:/g, '-')
  return `openclaw-diagnostics-${date}-${time}.json`
}

export function downloadBundle(bundle: DiagnosticsBundle): void {
  const json = JSON.stringify(bundle, null, 2)
  const blob = new Blob([json], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = generateBundleFilename()
  document.body.appendChild(link)
  link.click()
  document.body.removeChild(link)
  URL.revokeObjectURL(url)
}

export function buildGitHubIssueUrl(bundle: DiagnosticsBundle): string {
  const baseUrl = 'https://github.com/outsourc-e/clawsuite/issues/new'

  const title = encodeURIComponent('Bug: [Brief description]')

  const body = encodeURIComponent(`## Environment

- **App Version:** ${bundle.environment.appVersion}
- **OS:** ${bundle.environment.os}
- **Gateway Status:** ${bundle.gateway.status}

## What I was doing

[Describe what you were doing when the issue occurred]

## Expected behavior

[What did you expect to happen?]

## Actual behavior

[What actually happened?]

## Diagnostics Summary

\`\`\`json
${JSON.stringify(
  {
    version: bundle.version,
    generatedAt: bundle.generatedAt,
    gateway: bundle.gateway,
    providers: bundle.providers.map((p) => ({
      name: p.name,
      status: p.status,
    })),
    recentEventCount: bundle.recentEvents.length,
  },
  null,
  2,
)}
\`\`\`

## Additional context

[Add any other context about the problem here]
`)

  return `${baseUrl}?title=${title}&body=${body}`
}
