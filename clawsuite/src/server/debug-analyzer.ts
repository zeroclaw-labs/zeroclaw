import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

export type DebugAnalysis = {
  summary: string
  rootCause: string
  suggestedCommands: Array<{ command: string; description: string }>
  docsLink?: string
}

type ProviderName = 'gateway' | 'anthropic' | 'openai'

type ProviderConfig = {
  api?: string
  apiKey?: string
}

type OpenClawConfig = {
  models?: {
    providers?: {
      anthropic?: ProviderConfig
      openai?: ProviderConfig
    }
  }
}

type ResolvedProvider = {
  apiKey: string
  provider: ProviderName
}

type AnthropicResponse = {
  content?: Array<{
    text?: string
    type?: string
  }>
  error?: {
    message?: string
  }
}

type OpenAIChatResponse = {
  choices?: Array<{
    message?: {
      content?: string | null
    }
  }>
  error?: {
    message?: string
  }
}

const ANALYZER_SYSTEM_PROMPT =
  'You are a debugging assistant for OpenClaw. Analyze the error output and suggest fixes. Be concise. Return JSON with: summary, rootCause, suggestedCommands (array of {command: string, description: string}), docsLink (optional).'

const ANTHROPIC_URL = 'https://api.anthropic.com/v1/messages'
const OPENAI_URL = 'https://api.openai.com/v1/chat/completions'
const ANTHROPIC_MODEL = 'claude-sonnet-4-5-20250514'
const OPENAI_MODEL = 'gpt-4o-mini'

function getGatewayHttpUrl(path: string): string {
  const envUrl =
    process.env.CLAWDBOT_GATEWAY_URL?.trim() || 'ws://127.0.0.1:18789'
  try {
    const parsed = new URL(envUrl)
    parsed.protocol = parsed.protocol === 'wss:' ? 'https:' : 'http:'
    parsed.pathname = path
    return parsed.toString()
  } catch {
    return `http://127.0.0.1:18789${path}`
  }
}

// Gateway's OpenAI-compatible endpoint (works with any configured provider)
const GATEWAY_URL = getGatewayHttpUrl('/v1/chat/completions')
const MAX_PROMPT_CHARS = 14_000

function formatLogDate(value: Date): string {
  const year = String(value.getFullYear())
  const month = String(value.getMonth() + 1).padStart(2, '0')
  const day = String(value.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

function toRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object') return null
  return value as Record<string, unknown>
}

function trimToPromptSize(value: string): string {
  if (value.length <= MAX_PROMPT_CHARS) return value
  return value.slice(value.length - MAX_PROMPT_CHARS)
}

function buildPrompt(terminalOutput: string, logContent: string): string {
  const terminalBlock = trimToPromptSize(terminalOutput.trim())
  const logBlock = trimToPromptSize(logContent.trim())

  return [
    'Terminal output:',
    terminalBlock || '(empty)',
    '',
    'OpenClaw logs (last 200 lines):',
    logBlock || '(empty)',
    '',
    'Return JSON only.',
  ].join('\n')
}

function readProviderApiKey(
  config: OpenClawConfig,
  provider: 'anthropic' | 'openai',
): string {
  const providerConfig = config.models?.providers?.[provider]
  if (!providerConfig) return ''

  if (
    typeof providerConfig.apiKey === 'string' &&
    providerConfig.apiKey.trim()
  ) {
    return providerConfig.apiKey.trim()
  }

  if (typeof providerConfig.api === 'string' && providerConfig.api.trim()) {
    return providerConfig.api.trim()
  }

  return ''
}

async function readOpenClawConfig(): Promise<OpenClawConfig | null> {
  const configPath = path.join(os.homedir(), '.openclaw', 'openclaw.json')

  try {
    const raw = await fs.readFile(configPath, 'utf8')
    return JSON.parse(raw) as OpenClawConfig
  } catch {
    return null
  }
}

async function resolveProvider(): Promise<ResolvedProvider | null> {
  // Try gateway first — works with any configured provider, uses gateway token
  try {
    const gwTokenEnv = process.env.CLAWDBOT_GATEWAY_TOKEN?.trim()
    if (gwTokenEnv) {
      // Quick probe to see if gateway is up
      const probe = await fetch(getGatewayHttpUrl('/health'), {
        signal: AbortSignal.timeout(1000),
      }).catch(() => null)
      if (probe?.ok) {
        return { provider: 'gateway', apiKey: gwTokenEnv }
      }
    }
  } catch {
    /* gateway not available, fall through */
  }

  const anthropicEnv = process.env.ANTHROPIC_API_KEY?.trim()
  if (anthropicEnv) {
    return { provider: 'anthropic', apiKey: anthropicEnv }
  }

  const openaiEnv = process.env.OPENAI_API_KEY?.trim()
  if (openaiEnv) {
    return { provider: 'openai', apiKey: openaiEnv }
  }

  const config = await readOpenClawConfig()
  if (!config) return null

  const anthropicApiKey = readProviderApiKey(config, 'anthropic')
  if (anthropicApiKey) {
    return { provider: 'anthropic', apiKey: anthropicApiKey }
  }

  const openaiApiKey = readProviderApiKey(config, 'openai')
  if (openaiApiKey) {
    return { provider: 'openai', apiKey: openaiApiKey }
  }

  return null
}

function extractJsonBody(content: string): string {
  const trimmed = content.trim()
  if (!trimmed) return '{}'

  const noFence = trimmed
    .replace(/^```(?:json)?\s*/i, '')
    .replace(/\s*```$/, '')
    .trim()

  const start = noFence.indexOf('{')
  const end = noFence.lastIndexOf('}')
  if (start === -1 || end === -1 || end < start) return noFence
  return noFence.slice(start, end + 1)
}

function normalizeAnalysis(rawValue: unknown): DebugAnalysis {
  const value = toRecord(rawValue)
  const rawCommands = value?.suggestedCommands
  const commandsInput = Array.isArray(rawCommands) ? rawCommands : []

  const suggestedCommands = commandsInput
    .map(function mapCommand(entry) {
      const commandEntry = toRecord(entry)
      if (!commandEntry) return null
      const command =
        typeof commandEntry.command === 'string'
          ? commandEntry.command.trim()
          : ''
      const description =
        typeof commandEntry.description === 'string'
          ? commandEntry.description.trim()
          : ''
      if (!command || !description) return null
      return { command, description }
    })
    .filter(function removeNulls(entry): entry is {
      command: string
      description: string
    } {
      return Boolean(entry)
    })

  const summary =
    typeof value?.summary === 'string' && value.summary.trim()
      ? value.summary.trim()
      : 'Unable to summarize the issue.'
  const rootCause =
    typeof value?.rootCause === 'string' && value.rootCause.trim()
      ? value.rootCause.trim()
      : 'Root cause could not be determined from the provided logs.'
  const docsLink =
    typeof value?.docsLink === 'string' && value.docsLink.trim()
      ? value.docsLink.trim()
      : undefined

  return {
    summary,
    rootCause,
    suggestedCommands,
    ...(docsLink ? { docsLink } : {}),
  }
}

function fallbackForNoApiKey(): DebugAnalysis {
  return {
    summary: 'No LLM API key is configured for debug analysis.',
    rootCause:
      'Neither ANTHROPIC_API_KEY nor OPENAI_API_KEY was found, and no provider API key was found in ~/.openclaw/openclaw.json.',
    suggestedCommands: [
      {
        command: 'export ANTHROPIC_API_KEY=your_key_here',
        description: 'Set an Anthropic key for this shell session.',
      },
      {
        command: 'export OPENAI_API_KEY=your_key_here',
        description: 'Or set an OpenAI key for this shell session.',
      },
    ],
  }
}

function maskApiKeys(value: string): string {
  const patterns = [
    /\bBearer\s+[A-Za-z0-9._~+/=-]{8,}\b/gi,
    /\bsk-[A-Za-z0-9_-]{8,}\b/gi,
    /\bkey-[A-Za-z0-9_-]{8,}\b/gi,
    /\b[A-Za-z0-9]{20,}\b/g,
  ]

  return patterns.reduce(function redact(masked, pattern) {
    return masked.replace(pattern, '[REDACTED]')
  }, value)
}

function fallbackForFailure(error: unknown): DebugAnalysis {
  const rawMessage = error instanceof Error ? error.message : String(error)
  const message = maskApiKeys(rawMessage)
  return {
    summary: 'Debug analysis request failed.',
    rootCause: message,
    suggestedCommands: [],
  }
}

async function callAnthropic(apiKey: string, prompt: string): Promise<string> {
  const response = await fetch(ANTHROPIC_URL, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'x-api-key': apiKey,
      'anthropic-version': '2023-06-01',
    },
    body: JSON.stringify({
      model: ANTHROPIC_MODEL,
      max_tokens: 1024,
      system: ANALYZER_SYSTEM_PROMPT,
      messages: [{ role: 'user', content: prompt }],
    }),
  })

  const payload = (await response.json().catch(() => ({}))) as AnthropicResponse
  if (!response.ok) {
    throw new Error(
      payload.error?.message || `Anthropic request failed (${response.status})`,
    )
  }

  const text = payload.content?.find(function findText(block) {
    return block.type === 'text' && typeof block.text === 'string'
  })?.text

  if (!text) {
    throw new Error('Anthropic response did not contain text output.')
  }

  return text
}

async function callOpenAI(apiKey: string, prompt: string): Promise<string> {
  const response = await fetch(OPENAI_URL, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${apiKey}`,
    },
    body: JSON.stringify({
      model: OPENAI_MODEL,
      max_tokens: 1024,
      messages: [
        { role: 'system', content: ANALYZER_SYSTEM_PROMPT },
        { role: 'user', content: prompt },
      ],
    }),
  })

  const payload = (await response
    .json()
    .catch(() => ({}))) as OpenAIChatResponse
  if (!response.ok) {
    throw new Error(
      payload.error?.message || `OpenAI request failed (${response.status})`,
    )
  }

  const text = payload.choices?.[0]?.message?.content
  if (!text) {
    throw new Error('OpenAI response did not contain message content.')
  }

  return text
}

async function callGateway(token: string, prompt: string): Promise<string> {
  const response = await fetch(GATEWAY_URL, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify({
      max_tokens: 1024,
      messages: [
        { role: 'system', content: ANALYZER_SYSTEM_PROMPT },
        { role: 'user', content: prompt },
      ],
    }),
    signal: AbortSignal.timeout(30_000),
  })

  const payload = (await response
    .json()
    .catch(() => ({}))) as OpenAIChatResponse
  if (!response.ok) {
    throw new Error(
      payload.error?.message || `Gateway request failed (${response.status})`,
    )
  }

  const text = payload.choices?.[0]?.message?.content
  if (!text) {
    throw new Error('Gateway response did not contain message content.')
  }

  return text
}

function parseModelResponse(modelOutput: string): DebugAnalysis {
  const jsonBody = extractJsonBody(modelOutput)
  const parsed = JSON.parse(jsonBody) as unknown
  return normalizeAnalysis(parsed)
}

export async function analyzeError(
  terminalOutput: string,
  logContent: string,
): Promise<DebugAnalysis> {
  const provider = await resolveProvider()
  if (!provider) {
    return fallbackForNoApiKey()
  }

  const prompt = buildPrompt(terminalOutput, logContent)

  try {
    let modelOutput: string
    if (provider.provider === 'gateway') {
      modelOutput = await callGateway(provider.apiKey, prompt)
    } else if (provider.provider === 'anthropic') {
      modelOutput = await callAnthropic(provider.apiKey, prompt)
    } else {
      modelOutput = await callOpenAI(provider.apiKey, prompt)
    }

    return parseModelResponse(modelOutput)
  } catch (error) {
    return fallbackForFailure(error)
  }
}

export async function readOpenClawLogs(): Promise<string> {
  const date = formatLogDate(new Date())
  const logPath = path.join('/tmp', 'openclaw', `openclaw-${date}.log`)

  try {
    const content = await fs.readFile(logPath, 'utf8')
    const lines = content.split(/\r?\n/)
    return lines.slice(-200).join('\n').trim()
  } catch {
    return ''
  }
}
