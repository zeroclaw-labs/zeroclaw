import { bundledLanguages } from 'shiki'

const LANGUAGE_ALIASES: Record<string, string> = {
  js: 'javascript',
  ts: 'typescript',
  tsx: 'tsx',
  jsx: 'jsx',
  typescriptreact: 'tsx',
  javascriptreact: 'jsx',
  react: 'jsx',
  sh: 'bash',
  shell: 'bash',
  yml: 'yaml',
  md: 'markdown',
  txt: 'text',
}

export function normalizeLanguage(language: string): string {
  const cleaned = language
    .trim()
    .toLowerCase()
    .replace(/^language-/, '')
    .replace(/^\[|\]$/g, '')
  const token = cleaned.split(/[\s,]+/)[0] || 'text'
  return LANGUAGE_ALIASES[token] ?? token
}

export function resolveLanguage(language: string): string {
  const normalized = normalizeLanguage(language)
  return normalized in bundledLanguages ? normalized : 'text'
}

export function formatLanguageName(language: string): string {
  const names: Record<string, string> = {
    bash: 'Bash',
    python: 'Python',
    javascript: 'JavaScript',
    typescript: 'TypeScript',
    tsx: 'TSX',
    jsx: 'JSX',
    json: 'JSON',
    html: 'HTML',
    css: 'CSS',
    sql: 'SQL',
    yaml: 'YAML',
    markdown: 'Markdown',
    text: 'Plain Text',
  }
  return names[language] || language.charAt(0).toUpperCase() + language.slice(1)
}
