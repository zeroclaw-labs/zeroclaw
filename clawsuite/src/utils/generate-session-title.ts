const DEFAULT_MAX_LENGTH = 40
const DEFAULT_MAX_WORDS = 6

type SessionCategory =
  | 'coding'
  | 'research'
  | 'config'
  | 'creative'
  | 'analysis'
  | 'chat'

const CATEGORY_KEYWORDS: Record<
  Exclude<SessionCategory, 'chat'>,
  Array<string>
> = {
  coding: [
    'code',
    'coding',
    'debug',
    'bug',
    'error',
    'stack trace',
    'typescript',
    'javascript',
    'react',
    'test',
    'build',
    'lint',
    'function',
    'api',
    'query',
  ],
  research: [
    'research',
    'source',
    'citation',
    'paper',
    'study',
    'docs',
    'documentation',
    'compare',
    'find',
    'search',
    'look up',
    'latest',
    'news',
  ],
  config: [
    'config',
    'configuration',
    'setting',
    'settings',
    'setup',
    'install',
    'environment',
    '.env',
    'deploy',
    'docker',
    'pipeline',
    'ci',
    'workflow',
    'permission',
  ],
  creative: [
    'creative',
    'brainstorm',
    'idea',
    'name',
    'naming',
    'story',
    'poem',
    'script',
    'copy',
    'rewrite',
    'draft',
  ],
  analysis: [
    'analyze',
    'analysis',
    'evaluate',
    'tradeoff',
    'trade-off',
    'pros',
    'cons',
    'performance',
    'metrics',
    'data',
    'summary',
    'summarize',
    'report',
  ],
}

const NOISE_PREFIXES =
  /^(?:hey|hi|hello|ok(?:ay)?|so|well|please|kindly|um|uh|can you|could you|would you|will you|i want(?: to)?|i need(?: to)?|help me(?: with)?|let'?s|lets|also|just)\b[\s,:-]*/i

const STOP_WORDS = new Set([
  'a',
  'an',
  'the',
  'and',
  'or',
  'but',
  'if',
  'then',
  'else',
  'for',
  'to',
  'of',
  'in',
  'on',
  'at',
  'with',
  'from',
  'by',
  'this',
  'that',
  'these',
  'those',
  'it',
  'its',
  'is',
  'are',
  'was',
  'were',
  'be',
  'been',
  'being',
  'i',
  'me',
  'my',
  'we',
  'our',
  'you',
  'your',
  'he',
  'she',
  'they',
  'them',
  'can',
  'could',
  'would',
  'should',
  'will',
  'do',
  'does',
  'did',
  'just',
  'also',
  'please',
  'need',
  'want',
  'some',
  'any',
  'very',
  'really',
  'so',
  'too',
])

const UPPERCASE_TOKENS = new Set([
  'api',
  'ci',
  'css',
  'html',
  'json',
  'sdk',
  'sql',
  'ui',
  'ux',
])

const TOKEN_OVERRIDES: Record<string, string> = {
  typescript: 'TypeScript',
  javascript: 'JavaScript',
  nextjs: 'Next.js',
  react: 'React',
  tailwind: 'Tailwind',
}

const ACTION_PATTERNS: Array<{ pattern: RegExp; verb: string }> = [
  {
    pattern:
      /\b(?:fix|fixing|fixed|bug|bugs|error|errors|resolve|resolved|resolving)\b/,
    verb: 'Fix',
  },
  {
    pattern: /\b(?:debug|debugging|debugged|diagnose|diagnosing|diagnosed)\b/,
    verb: 'Debug',
  },
  {
    pattern: /\b(?:refactor|refactoring|refactored|cleanup|cleaning|cleaned)\b/,
    verb: 'Refactor',
  },
  {
    pattern:
      /\b(?:optimize|optimizing|optimized|optimise|optimising|performance)\b/,
    verb: 'Optimize',
  },
  {
    pattern:
      /\b(?:implement|implementing|implemented|build|building|create|creating|add|adding|write|writing)\b/,
    verb: 'Build',
  },
  {
    pattern: /\b(?:update|updating|updated|upgrade|upgrading|upgraded)\b/,
    verb: 'Update',
  },
  {
    pattern:
      /\b(?:test|testing|tested|verify|verifying|validate|validating|validated)\b/,
    verb: 'Test',
  },
  {
    pattern:
      /\b(?:analyze|analyzing|analyzed|analyse|analysing|analysis|evaluate|evaluating|investigate|investigating|review|reviewing)\b/,
    verb: 'Analyze',
  },
  { pattern: /\b(?:compare|comparing|comparison)\b/, verb: 'Compare' },
  {
    pattern:
      /\b(?:research|researching|search|searching|find|finding|lookup|look up)\b/,
    verb: 'Research',
  },
  {
    pattern:
      /\b(?:configure|config|configuration|setup|set up|install|deploy|deploying|deployed)\b/,
    verb: 'Configure',
  },
  {
    pattern:
      /\b(?:summarize|summarizing|summarized|summarise|summarising|summarised|summary)\b/,
    verb: 'Summarize',
  },
  {
    pattern:
      /\b(?:draft|drafting|drafted|brainstorm|brainstorming|rewrite|rewriting|name|naming)\b/,
    verb: 'Draft',
  },
  { pattern: /\b(?:explain|explaining|walkthrough)\b/, verb: 'Explain' },
]

const CATEGORY_DEFAULT_ACTION: Record<SessionCategory, string> = {
  coding: 'Fix',
  research: 'Research',
  config: 'Configure',
  creative: 'Draft',
  analysis: 'Analyze',
  chat: 'Discuss',
}

const CATEGORY_SUBJECT_FALLBACK: Record<SessionCategory, string> = {
  coding: 'Issue',
  research: 'Topic',
  config: 'Setup',
  creative: 'Idea',
  analysis: 'Results',
  chat: 'Chat',
}

export type SessionTitleSnippet = Array<{ role: string; text: string }>

function stripNoisePrefixes(text: string): string {
  let stripped = text.trim()
  let previous = ''
  while (stripped && stripped !== previous) {
    previous = stripped
    stripped = stripped.replace(NOISE_PREFIXES, '').trim()
  }
  return stripped
}

function cleanText(raw: string): string {
  let text = raw
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`[^`]*`/g, ' ')
    .replace(/\[(.*?)\]\((.*?)\)/g, '$1')
    .replace(/https?:\/\/\S+/g, ' ')
    .replace(/[#*`_~[\]()]/g, ' ')
    .replace(/[^\p{L}\p{N}\s-]/gu, ' ')
    .replace(/\s+/g, ' ')
    .trim()
  text = stripNoisePrefixes(text)
  return text
}

function normalizeToken(rawToken: string): string {
  return rawToken
    .toLowerCase()
    .replace(/^[^\p{L}\p{N}]+/gu, '')
    .replace(/[^\p{L}\p{N}]+$/gu, '')
    .replace(/-/g, '')
    .trim()
}

function tokenizeMeaningful(text: string): Array<string> {
  return text
    .split(/\s+/)
    .map(normalizeToken)
    .filter((token) => token.length > 1 && !/^\d+$/.test(token))
    .filter((token) => !STOP_WORDS.has(token))
}

function truncateToLength(text: string, maxLength: number): string {
  if (text.length <= maxLength) return text
  const clipped = text.slice(0, maxLength)
  const lastSpace = clipped.lastIndexOf(' ')
  if (lastSpace > 0) {
    return clipped.slice(0, lastSpace).trim()
  }
  return clipped.trim()
}

function formatToken(token: string): string {
  if (TOKEN_OVERRIDES[token]) return TOKEN_OVERRIDES[token]
  if (UPPERCASE_TOKENS.has(token)) return token.toUpperCase()
  return `${token.charAt(0).toUpperCase()}${token.slice(1)}`
}

function detectCategory(snippet: SessionTitleSnippet): SessionCategory {
  const combined = snippet
    .map((message) => cleanText(message.text).toLowerCase())
    .join(' ')
  if (!combined) return 'chat'

  let bestCategory: SessionCategory = 'chat'
  let bestScore = 0
  const orderedCategories = [
    'coding',
    'research',
    'config',
    'analysis',
    'creative',
  ] as const

  for (const category of orderedCategories) {
    const keywords = CATEGORY_KEYWORDS[category]
    let score = 0
    for (const keyword of keywords) {
      if (combined.includes(keyword)) score += 1
    }
    if (score > bestScore) {
      bestScore = score
      bestCategory = category
    }
  }

  return bestScore > 0 ? bestCategory : 'chat'
}

function primaryCandidate(snippet: SessionTitleSnippet): string {
  const firstUser = snippet.find((message) => message.role === 'user')
  if (firstUser?.text) return cleanText(firstUser.text)
  const firstAssistant = snippet.find((message) => message.role === 'assistant')
  if (firstAssistant?.text) return cleanText(firstAssistant.text)
  return ''
}

function scoreContextTokens(snippet: SessionTitleSnippet): Array<string> {
  const scores = new Map<string, number>()
  for (const message of snippet) {
    if (message.role !== 'user' && message.role !== 'assistant') continue
    const cleaned = cleanText(message.text)
    if (!cleaned) continue
    const tokens = tokenizeMeaningful(cleaned)
    const roleWeight = message.role === 'user' ? 3 : 2
    for (let index = 0; index < tokens.length; index += 1) {
      const token = tokens[index]
      const earlyBonus = index < 5 ? 1 : 0
      const score = (scores.get(token) ?? 0) + roleWeight + earlyBonus
      scores.set(token, score)
    }
  }

  return [...scores.entries()]
    .sort((a, b) => b[1] - a[1] || b[0].length - a[0].length)
    .map(([token]) => token)
}

function detectAction(
  snippet: SessionTitleSnippet,
  category: SessionCategory,
): string {
  const firstUser = snippet.find((message) => message.role === 'user')
  const firstUserText = firstUser ? cleanText(firstUser.text).toLowerCase() : ''
  const userText = snippet
    .filter((message) => message.role === 'user')
    .map((message) => cleanText(message.text).toLowerCase())
    .join(' ')
  const allText = snippet
    .map((message) => cleanText(message.text).toLowerCase())
    .join(' ')
  const candidates = [firstUserText, userText, allText].filter(Boolean)

  for (const text of candidates) {
    for (const actionPattern of ACTION_PATTERNS) {
      if (actionPattern.pattern.test(text)) {
        return actionPattern.verb
      }
    }
  }

  return CATEGORY_DEFAULT_ACTION[category]
}

function selectFocusTokens(
  primaryTokens: Array<string>,
  contextTokens: Array<string>,
  maxTokens: number,
): Array<string> {
  const selected: Array<string> = []
  const combined = [...primaryTokens, ...contextTokens]
  for (const token of combined) {
    if (selected.length >= maxTokens) break
    if (selected.includes(token)) continue
    // Keep action tokens if they're from primaryTokens (the user's actual words)
    selected.push(token)
  }
  return selected
}

type GenerateSessionTitleOptions = {
  maxLength?: number
  maxWords?: number
}

export function generateSessionTitle(
  snippet: SessionTitleSnippet,
  options: GenerateSessionTitleOptions = {},
): string {
  const maxLength = options.maxLength ?? DEFAULT_MAX_LENGTH
  const maxWords = options.maxWords ?? DEFAULT_MAX_WORDS

  const category = detectCategory(snippet)
  const primary = primaryCandidate(snippet)
  const titleTokens = tokenizeMeaningful(primary)
  const contextTokens = scoreContextTokens(snippet)
  const action = detectAction(snippet, category)
  const maxFocusTokens = Math.max(1, maxWords - 1)
  const focusTokens = selectFocusTokens(
    titleTokens,
    contextTokens,
    maxFocusTokens,
  )
  const subjectTokens =
    focusTokens.length > 0
      ? focusTokens
      : [normalizeToken(CATEGORY_SUBJECT_FALLBACK[category])]
  const coreTokens = [action, ...subjectTokens]
  const truncatedCoreTokens = coreTokens.slice(0, maxWords)
  const title = truncatedCoreTokens.map(formatToken).join(' ')
  return truncateToLength(title, maxLength)
}
