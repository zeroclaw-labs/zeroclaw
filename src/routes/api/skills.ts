import os from 'node:os'
import path from 'node:path'
import fs from 'node:fs/promises'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'

type SkillsTab = 'installed' | 'marketplace' | 'featured'
type SkillsSort = 'name' | 'category'

type SecurityRisk = {
  level: 'safe' | 'low' | 'medium' | 'high'
  flags: Array<string>
  score: number
}

type SkillSummary = {
  id: string
  slug: string
  name: string
  description: string
  author: string
  triggers: Array<string>
  tags: Array<string>
  homepage: string | null
  category: string
  icon: string
  content: string
  fileCount: number
  sourcePath: string
  installed: boolean
  enabled: boolean
  builtin?: boolean
  featuredGroup?: string
  security: SecurityRisk
}

type SkillIndexRecord = {
  id: string
  slug: string
  name: string
  description: string
  author: string
  triggers: Array<string>
  tags: Array<string>
  homepage: string | null
  category: string
  icon: string
  content: string
  sourcePath: string
  folderPath: string
  enabled: boolean
  builtin?: boolean
}

type CachedDataset = {
  builtAt: number
  items: Array<SkillIndexRecord>
}

type ParsedFrontmatter = {
  name: string
  description: string
  homepage: string | null
  triggers: Array<string>
  metadata: Record<string, unknown>
}

const CACHE_TTL_MS = 5 * 60 * 1000
const HOME_DIR = os.homedir()
const WORKSPACE_ROOT = path.join(HOME_DIR, '.openclaw', 'workspace')
const INSTALLED_ROOT = path.join(WORKSPACE_ROOT, 'skills')
// Resolve OpenClaw's built-in skills directory at runtime
let _builtinRoot: string | null = null
async function getBuiltinRoot(): Promise<string | null> {
  if (_builtinRoot !== null) return _builtinRoot || null

  const candidates = [
    // npm global (nvm)
    path.join(HOME_DIR, '.nvm', 'versions', 'node', process.version, 'lib', 'node_modules', 'openclaw', 'skills'),
    // npm global (system)
    path.join('/usr', 'local', 'lib', 'node_modules', 'openclaw', 'skills'),
    path.join('/usr', 'lib', 'node_modules', 'openclaw', 'skills'),
    // Docker / custom prefix
    path.join('/app', 'node_modules', 'openclaw', 'skills'),
    // Docker bundled skills (copied during build)
    path.join('/app', 'openclaw-skills'),
  ]

  // Also try `npm root -g` result
  try {
    const cp = await import('node:child_process')
    const globalRoot = cp.execSync('npm root -g', { encoding: 'utf8', timeout: 3000, stdio: ['pipe', 'pipe', 'pipe'] }).trim()
    candidates.unshift(path.join(globalRoot, 'openclaw', 'skills'))
  } catch {
    // npm not available (Docker etc)
  }

  for (const candidate of candidates) {
    try {
      await fs.access(candidate)
      _builtinRoot = candidate
      return candidate
    } catch {
      continue
    }
  }

  _builtinRoot = ''
  return null
}
const MARKETPLACE_ROOT = path.join(
  WORKSPACE_ROOT,
  'openclaw-skills-registry',
  'skills',
)

const KNOWN_CATEGORIES = [
  'All',
  'Web & Frontend',
  'Coding Agents',
  'Git & GitHub',
  'DevOps & Cloud',
  'Browser & Automation',
  'Image & Video',
  'Search & Research',
  'AI & LLMs',
  'Productivity',
  'Marketing & Sales',
  'Communication',
  'Data & Analytics',
  'Finance & Crypto',
] as const

const CATEGORY_ICONS: Record<string, string> = {
  'Web & Frontend': '🌐',
  'Coding Agents': '🧠',
  'Git & GitHub': '🌿',
  'DevOps & Cloud': '☁️',
  'Browser & Automation': '🤖',
  'Image & Video': '🎬',
  'Search & Research': '🔎',
  'AI & LLMs': '✨',
  Productivity: '⚡',
  'Marketing & Sales': '📈',
  Communication: '💬',
  'Data & Analytics': '📊',
  'Finance & Crypto': '💸',
}

const FEATURED_SKILLS: Array<{ id: string; group: string }> = [
  { id: 'dbalve/fast-io', group: 'Most Popular' },
  { id: 'okoddcat/gitflow', group: 'Most Popular' },
  { id: 'atomtanstudio/craft-do', group: 'Most Popular' },
  { id: 'bro3886/gtasks-cli', group: 'New This Week' },
  { id: 'saesak/openclaw-skill-gastown', group: 'New This Week' },
  { id: 'vvardhan14/pokerpal', group: 'New This Week' },
  { id: 'okoddcat/clawops', group: 'Developer Tools' },
  {
    id: 'veeramanikandanr48/docker-containerization',
    group: 'Developer Tools',
  },
  { id: 'veeramanikandanr48/azure-auth', group: 'Developer Tools' },
  { id: 'dbalve/fastio-skills', group: 'Productivity' },
  { id: 'gillberto1/moltwallet', group: 'Productivity' },
  { id: 'veeramanikandanr48/backtest-expert', group: 'Productivity' },
]

const inMemoryCache: {
  installed?: CachedDataset
  marketplace?: CachedDataset
} = {}

async function pathExists(input: string): Promise<boolean> {
  try {
    await fs.access(input)
    return true
  } catch {
    return false
  }
}

function ensureInside(basePath: string, relativePath: string): string {
  const resolved = path.resolve(basePath, relativePath)
  const normalizedBase = path.resolve(basePath)
  if (
    resolved !== normalizedBase &&
    !resolved.startsWith(`${normalizedBase}${path.sep}`)
  ) {
    throw new Error('path traversal is not allowed')
  }
  return resolved
}

function slugify(input: string): string {
  const result = input
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/(^-|-$)+/g, '')
  return result.length > 0 ? result : 'skill'
}

function stripQuotes(input: string): string {
  const trimmed = input.trim()
  if (trimmed.length < 2) return trimmed
  const first = trimmed[0]
  const last = trimmed[trimmed.length - 1]
  if ((first === '"' && last === '"') || (first === "'" && last === "'")) {
    return trimmed.slice(1, -1)
  }
  return trimmed
}

function parseScalar(input: string): unknown {
  const value = stripQuotes(input)
  if (value === 'true') return true
  if (value === 'false') return false
  if (value === 'null') return null
  if (/^-?\d+(\.\d+)?$/.test(value)) {
    const parsedNumber = Number(value)
    if (!Number.isNaN(parsedNumber)) return parsedNumber
  }
  if (
    (value.startsWith('{') && value.endsWith('}')) ||
    (value.startsWith('[') && value.endsWith(']'))
  ) {
    try {
      return JSON.parse(value)
    } catch {
      return value
    }
  }
  return value
}

function collectIndentedBlock(
  lines: Array<string>,
  startIndex: number,
): {
  block: Array<string>
  nextIndex: number
} {
  const block: Array<string> = []
  let index = startIndex
  while (index < lines.length) {
    const line = lines[index]
    if (line.trim() === '') {
      block.push('')
      index += 1
      continue
    }
    if (!line.startsWith('  ')) break
    block.push(line.slice(2))
    index += 1
  }
  return { block, nextIndex: index }
}

function parseIndentedMap(block: Array<string>): Record<string, unknown> {
  const result: Record<string, unknown> = {}
  let index = 0
  while (index < block.length) {
    const line = block[index]
    if (line.trim() === '' || line.trim().startsWith('#')) {
      index += 1
      continue
    }

    const entryMatch = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/)
    if (!entryMatch) {
      index += 1
      continue
    }

    const key = entryMatch[1]
    const value = entryMatch[2]

    if (
      value === '' ||
      value === '|' ||
      value === '|-' ||
      value === '>' ||
      value === '>-'
    ) {
      const nested: Array<string> = []
      let nestedIndex = index + 1
      while (nestedIndex < block.length) {
        const nestedLine = block[nestedIndex]
        if (nestedLine.trim() === '') {
          nested.push('')
          nestedIndex += 1
          continue
        }
        if (!nestedLine.startsWith('  ')) break
        nested.push(nestedLine.slice(2))
        nestedIndex += 1
      }

      if (nested.length > 0 && nested[0].trim().startsWith('- ')) {
        result[key] = nested
          .map((item) => item.match(/^\s*-\s+(.*)$/)?.[1] ?? '')
          .map((item) => stripQuotes(item))
          .filter(Boolean)
      } else if (value === '>' || value === '>-') {
        result[key] = nested.join(' ').replace(/\s+/g, ' ').trim()
      } else {
        result[key] = nested.join('\n').trim()
      }

      index = nestedIndex
      continue
    }

    result[key] = parseScalar(value)
    index += 1
  }

  return result
}

function splitFrontmatter(markdown: string): {
  frontmatter: string
  content: string
} {
  const lines = markdown.replace(/^\uFEFF/, '').split(/\r?\n/)
  let index = 0

  while (index < lines.length) {
    const line = lines[index].trim()
    if (line === '') {
      index += 1
      continue
    }
    if (line.startsWith('<!--')) {
      while (index < lines.length && !lines[index].includes('-->')) {
        index += 1
      }
      index += 1
      continue
    }
    break
  }

  if (lines[index]?.trim() !== '---') {
    return { frontmatter: '', content: markdown }
  }

  let end = index + 1
  while (end < lines.length && lines[end].trim() !== '---') {
    end += 1
  }

  if (end >= lines.length) {
    return { frontmatter: '', content: markdown }
  }

  return {
    frontmatter: lines.slice(index + 1, end).join('\n'),
    content: lines
      .slice(end + 1)
      .join('\n')
      .trim(),
  }
}

function parseFrontmatter(frontmatter: string): ParsedFrontmatter {
  const lines = frontmatter.split(/\r?\n/)
  const parsed: Record<string, unknown> = {}
  let index = 0

  while (index < lines.length) {
    const line = lines[index]
    if (line.trim() === '' || line.trim().startsWith('#')) {
      index += 1
      continue
    }

    const keyMatch = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/)
    if (!keyMatch) {
      index += 1
      continue
    }

    const key = keyMatch[1]
    const value = keyMatch[2]

    if (value === '|' || value === '|-' || value === '>' || value === '>-') {
      const { block, nextIndex } = collectIndentedBlock(lines, index + 1)
      parsed[key] =
        value === '>' || value === '>-'
          ? block.join(' ').replace(/\s+/g, ' ').trim()
          : block.join('\n').trim()
      index = nextIndex
      continue
    }

    if (value === '') {
      const { block, nextIndex } = collectIndentedBlock(lines, index + 1)
      if (
        block.length > 0 &&
        block.some((item) => item.trim().startsWith('- '))
      ) {
        parsed[key] = block
          .map((item) => item.match(/^\s*-\s+(.*)$/)?.[1] ?? '')
          .map((item) => stripQuotes(item))
          .filter(Boolean)
      } else {
        parsed[key] = parseIndentedMap(block)
      }
      index = nextIndex
      continue
    }

    parsed[key] = parseScalar(value)
    index += 1
  }

  const metadataRaw = parsed.metadata
  let metadata: Record<string, unknown> = {}
  if (
    metadataRaw &&
    typeof metadataRaw === 'object' &&
    !Array.isArray(metadataRaw)
  ) {
    metadata = metadataRaw as Record<string, unknown>
  }

  const triggersRaw = parsed.triggers
  const triggers = Array.isArray(triggersRaw)
    ? triggersRaw
        .map((item) => String(item).trim())
        .filter((item) => item.length > 0)
    : []

  const name = typeof parsed.name === 'string' ? parsed.name.trim() : ''
  const description =
    typeof parsed.description === 'string' ? parsed.description.trim() : ''
  const homepage =
    typeof parsed.homepage === 'string' && parsed.homepage.trim().length > 0
      ? parsed.homepage.trim()
      : null

  return {
    name,
    description,
    homepage,
    triggers,
    metadata,
  }
}

function findAuthor(
  metadata: Record<string, unknown>,
  frontmatter: string,
  ownerFallback: string,
): string {
  const directAuthor = metadata.author
  if (typeof directAuthor === 'string' && directAuthor.trim().length > 0) {
    return directAuthor.trim()
  }

  const frontmatterAuthor = frontmatter.match(/^\s+author:\s*(.+)$/m)?.[1]
  if (frontmatterAuthor) {
    return stripQuotes(frontmatterAuthor).trim()
  }

  return ownerFallback
}

function findTags(metadata: Record<string, unknown>): Array<string> {
  const tags = new Set<string>()

  const category = metadata.category
  if (typeof category === 'string' && category.trim().length > 0) {
    tags.add(category.trim())
  }

  const network = metadata.network
  if (typeof network === 'string' && network.trim().length > 0) {
    tags.add(network.trim())
  }

  const version = metadata.version
  if (typeof version === 'string' && version.trim().length > 0) {
    tags.add(`v${version.trim()}`)
  }

  return Array.from(tags).slice(0, 4)
}

function deriveCategory(
  metadata: Record<string, unknown>,
  searchableText: string,
): string {
  const metadataCategory =
    typeof metadata.category === 'string' ? metadata.category.toLowerCase() : ''
  const lowerText = searchableText.toLowerCase()

  if (
    metadataCategory.includes('frontend') ||
    /react|vue|svelte|css|tailwind|next\.js|nextjs/.test(lowerText)
  ) {
    return 'Web & Frontend'
  }
  if (
    metadataCategory.includes('devops') ||
    /docker|kubernetes|terraform|aws|gcp|azure|ci\/cd|k8s/.test(lowerText)
  ) {
    return 'DevOps & Cloud'
  }
  if (
    metadataCategory.includes('git') ||
    /git|github|gitlab|pull request|workflow/.test(lowerText)
  ) {
    return 'Git & GitHub'
  }
  if (
    metadataCategory.includes('automation') ||
    /browser|playwright|puppeteer|automation|scrape|selenium/.test(lowerText)
  ) {
    return 'Browser & Automation'
  }
  if (
    metadataCategory.includes('image') ||
    /image|video|photo|ffmpeg|render|media/.test(lowerText)
  ) {
    return 'Image & Video'
  }
  if (
    metadataCategory.includes('research') ||
    /search|research|docs|knowledge|rag|summari/.test(lowerText)
  ) {
    return 'Search & Research'
  }
  if (
    metadataCategory.includes('ai') ||
    /llm|agent|prompt|model|openai|anthropic|mcp/.test(lowerText)
  ) {
    return 'AI & LLMs'
  }
  if (
    metadataCategory.includes('marketing') ||
    /sales|crm|campaign|lead|seo|outreach/.test(lowerText)
  ) {
    return 'Marketing & Sales'
  }
  if (
    metadataCategory.includes('communication') ||
    /slack|discord|email|notion|telegram|chat/.test(lowerText)
  ) {
    return 'Communication'
  }
  if (
    metadataCategory.includes('finance') ||
    /wallet|crypto|trading|payment|invoice|ledger/.test(lowerText)
  ) {
    return 'Finance & Crypto'
  }
  if (
    metadataCategory.includes('data') ||
    /analytics|dashboard|metric|sql|data/.test(lowerText)
  ) {
    return 'Data & Analytics'
  }
  if (
    /coding|codegen|developer|build|lint|test|typescript|python/.test(lowerText)
  ) {
    return 'Coding Agents'
  }
  return 'Productivity'
}

function deriveDescription(rawDescription: string, content: string): string {
  if (rawDescription.length > 0) {
    return rawDescription
  }

  const candidate = content
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find(
      (line) =>
        line.length > 0 && !line.startsWith('#') && !line.startsWith('>'),
    )

  if (!candidate) {
    return 'No description provided.'
  }

  return candidate.length > 280 ? `${candidate.slice(0, 277)}...` : candidate
}

async function collectInstalledSkillEntries(): Promise<
  Array<{ id: string; owner: string; folderPath: string }>
> {
  const entries: Array<{ id: string; owner: string; folderPath: string }> = []
  if (!(await pathExists(INSTALLED_ROOT))) {
    return entries
  }

  async function collectSkillFoldersRecursively(
    basePath: string,
  ): Promise<Array<string>> {
    const folders: Array<string> = []
    const stack: Array<string> = [basePath]

    while (stack.length > 0) {
      const current = stack.pop()
      if (!current) break

      const skillPath = path.join(current, 'SKILL.md')
      if (await pathExists(skillPath)) {
        folders.push(current)
      }

      const nestedDirs = await fs
        .readdir(current, { withFileTypes: true })
        .catch(() => [])
      for (const nested of nestedDirs) {
        if (!nested.isDirectory() || nested.name.startsWith('.')) continue
        stack.push(path.join(current, nested.name))
      }
    }

    return folders
  }

  const dirs = await fs.readdir(INSTALLED_ROOT, { withFileTypes: true })
  const seen = new Set<string>()

  for (const dir of dirs) {
    if (!dir.isDirectory() || dir.name.startsWith('.')) continue
    const nestedBase = path.join(INSTALLED_ROOT, dir.name)
    const skillFolders = await collectSkillFoldersRecursively(nestedBase)

    for (const folderPath of skillFolders) {
      const relativePath = path.relative(INSTALLED_ROOT, folderPath)
      if (!relativePath || relativePath.startsWith('..')) continue
      const id = relativePath.split(path.sep).join('/')
      if (seen.has(id)) continue

      seen.add(id)
      entries.push({
        id,
        owner: dir.name,
        folderPath,
      })
    }
  }

  return entries
}

async function collectBuiltinSkillEntries(): Promise<
  Array<{ id: string; owner: string; folderPath: string }>
> {
  const builtinRoot = await getBuiltinRoot()
  if (!builtinRoot) return []

  const entries: Array<{ id: string; owner: string; folderPath: string }> = []

  try {
    const dirs = await fs.readdir(builtinRoot, { withFileTypes: true })
    for (const dir of dirs) {
      if (!dir.isDirectory() || dir.name.startsWith('.')) continue
      const folderPath = path.join(builtinRoot, dir.name)
      const skillPath = path.join(folderPath, 'SKILL.md')
      try {
        await fs.access(skillPath)
        entries.push({
          id: `builtin/${dir.name}`,
          owner: 'openclaw',
          folderPath,
        })
      } catch {
        // No SKILL.md, skip
      }
    }
  } catch {
    // Can't read builtin dir
  }

  return entries
}

const REGISTRY_REPO = 'https://github.com/openclaw/skills.git'
const REGISTRY_PARENT = path.dirname(MARKETPLACE_ROOT)
let registryCloneAttempted = false

async function ensureMarketplaceRegistry(): Promise<boolean> {
  if (await pathExists(MARKETPLACE_ROOT)) return true
  if (registryCloneAttempted) return false
  registryCloneAttempted = true

  try {
    const cp = await import('node:child_process')
    await fs.mkdir(REGISTRY_PARENT, { recursive: true })
    cp.execSync(
      `git clone --depth 1 --single-branch ${REGISTRY_REPO} "${path.dirname(MARKETPLACE_ROOT)}"`,
      { timeout: 60_000, stdio: 'ignore' },
    )
    return await pathExists(MARKETPLACE_ROOT)
  } catch {
    return false
  }
}

async function collectMarketplaceSkillEntries(): Promise<
  Array<{ id: string; owner: string; folderPath: string }>
> {
  const entries: Array<{ id: string; owner: string; folderPath: string }> = []
  if (!(await ensureMarketplaceRegistry())) {
    return entries
  }

  const owners = await fs.readdir(MARKETPLACE_ROOT, { withFileTypes: true })
  for (const owner of owners) {
    if (!owner.isDirectory() || owner.name.startsWith('.')) continue
    const ownerPath = path.join(MARKETPLACE_ROOT, owner.name)
    const skills = await fs.readdir(ownerPath, { withFileTypes: true })
    for (const skill of skills) {
      if (!skill.isDirectory() || skill.name.startsWith('.')) continue
      const folderPath = path.join(ownerPath, skill.name)
      const skillPath = path.join(folderPath, 'SKILL.md')
      if (!(await pathExists(skillPath))) continue
      entries.push({
        id: `${owner.name}/${skill.name}`,
        owner: owner.name,
        folderPath,
      })
    }
  }

  return entries
}

// --- ClawHub API-based marketplace (fallback when git registry is unavailable) ---

const CLAWHUB_API_URL = 'https://clawhub.ai/api/v1/skills'
let apiMarketplaceCache: { builtAt: number; items: Array<SkillIndexRecord> } | null = null
const API_CACHE_TTL_MS = 10 * 60 * 1000

async function fetchMarketplaceFromApi(): Promise<Array<SkillIndexRecord>> {
  const now = Date.now()
  if (apiMarketplaceCache && now - apiMarketplaceCache.builtAt < API_CACHE_TTL_MS) {
    return apiMarketplaceCache.items
  }

  type ApiSkillItem = {
    slug: string
    displayName: string
    summary: string
    tags?: Record<string, string>
    stats?: {
      downloads?: number
      stars?: number
      comments?: number
    }
    createdAt?: number
    updatedAt?: number
    latestVersion?: {
      version: string
      changelog?: string
    }
  }

  try {
    // Fetch all pages using cursor-based pagination
    const allItems: Array<ApiSkillItem> = []
    let cursor: string | undefined
    const MAX_PAGES = 10

    for (let page = 0; page < MAX_PAGES; page++) {
      const url = cursor
        ? `${CLAWHUB_API_URL}?limit=100&cursor=${encodeURIComponent(cursor)}`
        : `${CLAWHUB_API_URL}?limit=100`

      const response = await fetch(url, {
        signal: AbortSignal.timeout(15_000),
        headers: { Accept: 'application/json' },
      })

      if (!response.ok) break

      const data = (await response.json()) as {
        items?: Array<ApiSkillItem>
        nextCursor?: string
      }

      if (data.items?.length) {
        allItems.push(...data.items)
      }

      if (!data.nextCursor || !data.items?.length) break
      cursor = data.nextCursor
    }

    if (!allItems.length) return apiMarketplaceCache?.items ?? []

    const skills: Array<SkillIndexRecord> = allItems.map((item) => {
      const version = item.latestVersion?.version ?? item.tags?.latest ?? ''
      const downloads = item.stats?.downloads ?? 0

      return {
        id: `clawhub/${item.slug}`,
        slug: item.slug,
        name: item.displayName || item.slug,
        description: item.summary || '',
        author: 'ClawHub',
        triggers: [],
        tags: version ? [version] : [],
        homepage: `https://clawhub.ai/skills/${item.slug}`,
        category: deriveCategory({}, `${item.displayName} ${item.summary}`),
        icon: CATEGORY_ICONS[deriveCategory({}, `${item.displayName} ${item.summary}`)] || '🧩',
        content: item.summary || '',
        sourcePath: `clawhub://${item.slug}`,
        folderPath: '',
        enabled: true,
        _downloads: downloads,
        _stars: item.stats?.stars ?? 0,
      } as SkillIndexRecord & { _downloads: number; _stars: number }
    })

    apiMarketplaceCache = { builtAt: now, items: skills }
    return skills
  } catch {
    return apiMarketplaceCache?.items ?? []
  }
}

async function countFilesInFolder(folderPath: string): Promise<number> {
  let count = 0
  const stack = [folderPath]

  while (stack.length > 0) {
    const current = stack.pop()
    if (!current) break
    const entries = await fs.readdir(current, { withFileTypes: true })
    for (const entry of entries) {
      if (entry.name.startsWith('.')) continue
      const entryPath = path.join(current, entry.name)
      if (entry.isDirectory()) {
        stack.push(entryPath)
      } else if (entry.isFile()) {
        count += 1
      }
    }
  }

  return count
}

function invalidateSkillsCache(tab?: SkillsTab) {
  if (!tab) {
    delete inMemoryCache.installed
    delete inMemoryCache.marketplace
    return
  }

  if (tab === 'installed') {
    delete inMemoryCache.installed
    return
  }

  // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
  if (tab === 'marketplace' || tab === 'featured') {
    delete inMemoryCache.marketplace
  }
}

async function buildSkillsIndex(
  tab: Exclude<SkillsTab, 'featured'>,
): Promise<Array<SkillIndexRecord>> {
  // For marketplace: try git registry first, fall back to ClawHub API
  if (tab === 'marketplace') {
    const entries = await collectMarketplaceSkillEntries()
    if (entries.length === 0) {
      // No local registry — fetch from ClawHub API
      return await fetchMarketplaceFromApi()
    }
  }

  const entries =
    tab === 'installed'
      ? [
          ...(await collectInstalledSkillEntries()),
          ...(await collectBuiltinSkillEntries()),
        ]
      : await collectMarketplaceSkillEntries()

  const skills: Array<SkillIndexRecord> = []

  for (const entry of entries) {
    const skillPath = path.join(entry.folderPath, 'SKILL.md')
    const markdown = await fs.readFile(skillPath, 'utf8').catch(() => '')
    if (!markdown) continue

    const { frontmatter, content } = splitFrontmatter(markdown)
    const parsed = parseFrontmatter(frontmatter)

    const name = parsed.name || path.basename(entry.folderPath)
    const description = deriveDescription(parsed.description, content)
    const author = findAuthor(parsed.metadata, frontmatter, entry.owner)
    const tags = findTags(parsed.metadata)
    const searchableText = [
      name,
      description,
      author,
      parsed.triggers.join(' '),
      tags.join(' '),
      JSON.stringify(parsed.metadata),
      entry.id,
    ].join(' ')
    const category = deriveCategory(parsed.metadata, searchableText)

    skills.push({
      id: entry.id,
      slug: slugify(name),
      name,
      description,
      author,
      triggers: parsed.triggers,
      tags,
      homepage: parsed.homepage,
      category,
      icon: CATEGORY_ICONS[category] || '🧩',
      content,
      sourcePath: entry.folderPath,
      folderPath: entry.folderPath,
      enabled:
        tab === 'installed'
          ? !(await pathExists(path.join(entry.folderPath, '.disabled')))
          : true,
      builtin: entry.id.startsWith('builtin/'),
    })
  }

  return skills
}

async function getCachedSkills(
  tab: Exclude<SkillsTab, 'featured'>,
): Promise<Array<SkillIndexRecord>> {
  const now = Date.now()
  const current = inMemoryCache[tab]
  if (current && now - current.builtAt < CACHE_TTL_MS) {
    return current.items
  }

  const items = await buildSkillsIndex(tab)
  inMemoryCache[tab] = {
    builtAt: now,
    items,
  }
  return items
}

function makeInstalledLookup(items: Array<SkillIndexRecord>): Set<string> {
  const lookup = new Set<string>()
  for (const item of items) {
    lookup.add(item.name.toLowerCase())
    lookup.add(item.slug.toLowerCase())
    lookup.add(item.id.toLowerCase())
    lookup.add(path.basename(item.id).toLowerCase())
  }
  return lookup
}

function matchesSearch(skill: SkillIndexRecord, search: string): boolean {
  if (!search) return true
  const haystack = [
    skill.name,
    skill.description,
    skill.author,
    skill.category,
    skill.triggers.join(' '),
    skill.tags.join(' '),
    skill.id,
  ]
    .join(' ')
    .toLowerCase()
  return haystack.includes(search.toLowerCase())
}

function sortSkills(
  items: Array<SkillIndexRecord>,
  sort: SkillsSort,
): Array<SkillIndexRecord> {
  const cloned = [...items]
  if (sort === 'category') {
    cloned.sort((a, b) => {
      const categoryCompare = a.category.localeCompare(b.category)
      if (categoryCompare !== 0) return categoryCompare
      return a.name.localeCompare(b.name)
    })
    return cloned
  }

  cloned.sort((a, b) => a.name.localeCompare(b.name))
  return cloned
}

// ── Security Scanner ──────────────────────────────────────────────────────

const SECURITY_PATTERNS: Array<{
  pattern: RegExp
  flag: string
  weight: number
}> = [
  // High risk
  { pattern: /\bsudo\b/i, flag: 'Uses sudo/root access', weight: 30 },
  { pattern: /\brm\s+-rf?\b/i, flag: 'Deletes files (rm)', weight: 25 },
  { pattern: /\beval\b.*\(/i, flag: 'Uses eval()', weight: 25 },
  { pattern: /\bexec\b.*\(/i, flag: 'Executes shell commands', weight: 20 },
  { pattern: /\bchild_process\b/i, flag: 'Spawns child processes', weight: 20 },
  {
    pattern: /\bProcess\.Start\b/i,
    flag: 'Starts system processes',
    weight: 20,
  },
  { pattern: /\bos\.system\b/i, flag: 'Runs OS commands', weight: 20 },
  { pattern: /\bsubprocess\b/i, flag: 'Runs subprocesses', weight: 20 },
  // Medium risk
  {
    pattern: /\bcurl\b.*https?:/i,
    flag: 'Makes HTTP requests (curl)',
    weight: 15,
  },
  { pattern: /\bwget\b/i, flag: 'Downloads files (wget)', weight: 15 },
  { pattern: /\bfetch\s*\(/i, flag: 'Makes network requests', weight: 10 },
  {
    pattern: /\brequests?\.(get|post|put|delete)\b/i,
    flag: 'Makes HTTP requests',
    weight: 10,
  },
  { pattern: /\bapi[_-]?key\b/i, flag: 'Handles API keys', weight: 10 },
  {
    pattern: /\b(secret|token|password|credential)\b/i,
    flag: 'Handles secrets/credentials',
    weight: 10,
  },
  {
    pattern: /\bfs\.(write|unlink|rm|rmdir)\b/i,
    flag: 'Writes/deletes files',
    weight: 10,
  },
  { pattern: /\bchmod\b/i, flag: 'Changes file permissions', weight: 10 },
  // Low risk
  {
    pattern: /\bfs\.(read|readFile|readdir)\b/i,
    flag: 'Reads files',
    weight: 3,
  },
  {
    pattern: /\benv\b.*\b(HOME|PATH|USER)\b/i,
    flag: 'Reads environment variables',
    weight: 3,
  },
  { pattern: /\binstall\b/i, flag: 'Installs packages', weight: 5 },
  { pattern: /\bnpm\b.*\binstall\b/i, flag: 'Runs npm install', weight: 5 },
  { pattern: /\bpip\b.*\binstall\b/i, flag: 'Runs pip install', weight: 5 },
]

function scanSkillSecurity(
  content: string,
  allFileContents?: string,
): SecurityRisk {
  const textToScan = allFileContents
    ? `${content}\n${allFileContents}`
    : content
  const flags: Array<string> = []
  let score = 0
  const seen = new Set<string>()

  for (const rule of SECURITY_PATTERNS) {
    if (rule.pattern.test(textToScan) && !seen.has(rule.flag)) {
      seen.add(rule.flag)
      flags.push(rule.flag)
      score += rule.weight
    }
  }

  let level: SecurityRisk['level'] = 'safe'
  if (score >= 40) level = 'high'
  else if (score >= 20) level = 'medium'
  else if (score > 0) level = 'low'

  return { level, flags, score }
}

async function scanSkillFolder(
  folderPath: string,
  skillContent: string,
): Promise<SecurityRisk> {
  // Scan SKILL.md content + all script files in the folder
  let allContent = ''
  try {
    const stack = [folderPath]
    const scriptExtensions = new Set([
      '.sh',
      '.py',
      '.js',
      '.ts',
      '.mjs',
      '.ps1',
      '.bat',
      '.cmd',
      '.rb',
    ])
    while (stack.length > 0) {
      const dir = stack.pop()
      if (!dir) break
      const entries = await fs
        .readdir(dir, { withFileTypes: true })
        .catch(() => [])
      for (const entry of entries) {
        if (entry.name.startsWith('.')) continue
        const fullPath = path.join(dir, entry.name)
        if (entry.isDirectory()) {
          stack.push(fullPath)
        } else if (entry.isFile()) {
          const ext = path.extname(entry.name).toLowerCase()
          if (
            scriptExtensions.has(ext) ||
            entry.name === 'Makefile' ||
            entry.name === 'Dockerfile'
          ) {
            const fileContent = await fs
              .readFile(fullPath, 'utf8')
              .catch(() => '')
            if (fileContent.length < 50_000) {
              // Skip huge files
              allContent += `\n${fileContent}`
            }
          }
        }
      }
    }
  } catch {
    // Ignore scan errors
  }

  return scanSkillSecurity(skillContent, allContent)
}

// ── Inflate ───────────────────────────────────────────────────────────────

async function inflateSkillSummaries(
  items: Array<SkillIndexRecord>,
  installedLookup: Set<string>,
  options?: { includeFileCount?: boolean },
): Promise<Array<SkillSummary>> {
  const includeFileCount = options?.includeFileCount !== false
  const output: Array<SkillSummary> = []
  for (const item of items) {
    const fileCount = includeFileCount
      ? await countFilesInFolder(item.folderPath)
      : 0
    const installed =
      installedLookup.has(item.name.toLowerCase()) ||
      installedLookup.has(item.slug.toLowerCase()) ||
      installedLookup.has(item.id.toLowerCase()) ||
      installedLookup.has(path.basename(item.id).toLowerCase())

    const security = await scanSkillFolder(item.folderPath, item.content)

    output.push({
      id: item.id,
      slug: item.slug,
      name: item.name,
      description: item.description,
      author: item.author,
      triggers: item.triggers,
      tags: item.tags,
      homepage: item.homepage,
      category: item.category,
      icon: item.icon,
      content: item.content,
      fileCount,
      sourcePath: item.sourcePath,
      installed,
      enabled: item.enabled,
      builtin: item.builtin || false,
      security,
    })
  }

  return output
}

async function getFeaturedSkills(
  marketplaceItems: Array<SkillIndexRecord>,
  installedLookup: Set<string>,
): Promise<Array<SkillSummary>> {
  const byId = new Map<string, SkillIndexRecord>()
  for (const item of marketplaceItems) {
    byId.set(item.id, item)
  }

  const picked: Array<{ item: SkillIndexRecord; group: string }> = []
  for (const featured of FEATURED_SKILLS) {
    const match = byId.get(featured.id)
    if (match) {
      picked.push({ item: match, group: featured.group })
    }
  }

  if (picked.length < 10) {
    for (const item of marketplaceItems) {
      if (picked.find((entry) => entry.item.id === item.id)) continue
      picked.push({ item, group: 'Most Popular' })
      if (picked.length >= 12) break
    }
  }

  const summaries = await inflateSkillSummaries(
    picked.map((entry) => entry.item),
    installedLookup,
  )

  return summaries.map((summary, index) => ({
    ...summary,
    featuredGroup: picked[index]?.group || 'Most Popular',
  }))
}

async function installMarketplaceSkill(skillId: string) {
  if (skillId.trim().length === 0) {
    throw new Error('skill id is required')
  }

  const sourcePath = ensureInside(MARKETPLACE_ROOT, skillId)
  const sourceSkillMd = path.join(sourcePath, 'SKILL.md')
  if (!(await pathExists(sourceSkillMd))) {
    throw new Error('skill was not found in marketplace registry')
  }

  await fs.mkdir(INSTALLED_ROOT, { recursive: true })

  const markdown = await fs.readFile(sourceSkillMd, 'utf8')
  const parsed = parseFrontmatter(splitFrontmatter(markdown).frontmatter)
  const destinationBase = slugify(parsed.name || path.basename(sourcePath))

  let destinationPath = path.join(INSTALLED_ROOT, destinationBase)
  let sequence = 2
  while (await pathExists(destinationPath)) {
    destinationPath = path.join(
      INSTALLED_ROOT,
      `${destinationBase}-${sequence}`,
    )
    sequence += 1
  }

  await fs.cp(sourcePath, destinationPath, { recursive: true })
}

async function resolveInstalledSkillPath(
  skillId: string,
): Promise<string | null> {
  const normalizedSkillId = skillId.trim().toLowerCase()
  if (!normalizedSkillId) return null

  const directPath = ensureInside(INSTALLED_ROOT, skillId)
  if (await pathExists(directPath)) {
    return directPath
  }

  const installedItems = await getCachedSkills('installed')
  const directMatch = installedItems.find((item) => {
    const candidates = [
      item.id.toLowerCase(),
      item.slug.toLowerCase(),
      item.name.toLowerCase(),
      path.basename(item.id).toLowerCase(),
      path.basename(item.folderPath).toLowerCase(),
    ]
    return candidates.includes(normalizedSkillId)
  })
  if (directMatch) {
    return directMatch.folderPath
  }

  const marketplaceMatchId = skillId.trim().toLowerCase()
  if (!marketplaceMatchId.includes('/')) return null

  const marketplaceItems = await getCachedSkills('marketplace')
  const marketplaceSkill = marketplaceItems.find(
    (item) => item.id.toLowerCase() === marketplaceMatchId,
  )
  if (!marketplaceSkill) return null

  const byMarketplaceFingerprint = installedItems.find((item) => {
    return (
      item.name.toLowerCase() === marketplaceSkill.name.toLowerCase() ||
      item.slug.toLowerCase() === marketplaceSkill.slug.toLowerCase()
    )
  })
  return byMarketplaceFingerprint?.folderPath || null
}

async function uninstallInstalledSkill(skillId: string) {
  if (skillId.trim().length === 0) {
    throw new Error('skill id is required')
  }

  const targetPath = await resolveInstalledSkillPath(skillId)
  if (!targetPath) {
    throw new Error('installed skill was not found')
  }

  await fs.rm(targetPath, { recursive: true, force: true })
}

async function toggleInstalledSkill(skillId: string, enabled: boolean) {
  if (skillId.trim().length === 0) {
    throw new Error('skill id is required')
  }

  const targetPath = await resolveInstalledSkillPath(skillId)
  if (!targetPath) {
    throw new Error('installed skill was not found')
  }
  const markerPath = path.join(targetPath, '.disabled')

  if (enabled) {
    await fs.rm(markerPath, { force: true })
    return
  }

  await fs.writeFile(markerPath, 'disabled\n', 'utf8')
}

export const Route = createFileRoute('/api/skills')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        try {
          const url = new URL(request.url)
          const tabParam = url.searchParams.get('tab')
          const tab: SkillsTab =
            tabParam === 'installed' ||
            tabParam === 'marketplace' ||
            tabParam === 'featured'
              ? tabParam
              : 'marketplace'

          const rawSearch = (url.searchParams.get('search') || '').trim()
          const category = (url.searchParams.get('category') || 'All').trim()
          const summaryMode = (url.searchParams.get('summary') || '').trim()
          const sortParam = (url.searchParams.get('sort') || 'name').trim()
          const sort: SkillsSort =
            sortParam === 'category' || sortParam === 'name'
              ? sortParam
              : 'name'
          const page = Math.max(1, Number(url.searchParams.get('page') || '1'))
          const limit = Math.min(
            60,
            Math.max(1, Number(url.searchParams.get('limit') || '30')),
          )

          const installedItems = await getCachedSkills('installed')
          const installedLookup = makeInstalledLookup(installedItems)

          if (tab === 'featured') {
            const marketplaceItems = await getCachedSkills('marketplace')
            const featuredSkills = await getFeaturedSkills(
              marketplaceItems,
              installedLookup,
            )
            return json({
              skills: featuredSkills,
              total: featuredSkills.length,
              page: 1,
              categories: KNOWN_CATEGORIES,
            })
          }

          const sourceItems = await getCachedSkills(tab)
          const filtered = sortSkills(
            sourceItems.filter((skill) => {
              if (!matchesSearch(skill, rawSearch)) return false
              if (category !== 'All' && skill.category !== category)
                return false
              return true
            }),
            sort,
          )

          const total = filtered.length
          const start = (page - 1) * limit
          const paged = filtered.slice(start, start + limit)

          const skills = await inflateSkillSummaries(paged, installedLookup, {
            includeFileCount: summaryMode !== 'search',
          })

          return json({
            skills,
            total,
            page,
            categories: KNOWN_CATEGORIES,
          })
        } catch (err) {
          return json(
            { error: err instanceof Error ? err.message : String(err) },
            { status: 500 },
          )
        }
      },
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const action =
            typeof body.action === 'string' ? body.action.trim() : ''
          const skillId =
            typeof body.skillId === 'string' ? body.skillId.trim() : ''

          if (action === 'install') {
            await installMarketplaceSkill(skillId)
            invalidateSkillsCache('installed')
            return json({ ok: true })
          }

          if (action === 'uninstall') {
            await uninstallInstalledSkill(skillId)
            invalidateSkillsCache('installed')
            return json({ ok: true })
          }

          if (action === 'toggle') {
            const enabled = Boolean(body.enabled)
            await toggleInstalledSkill(skillId, enabled)
            invalidateSkillsCache('installed')
            return json({ ok: true })
          }

          return json({ error: 'unsupported action' }, { status: 400 })
        } catch (err) {
          return json(
            { error: err instanceof Error ? err.message : String(err) },
            { status: 500 },
          )
        }
      },
    },
  },
})
