export type MemoryViewerFile = {
  name: string
  path: string
  size: number
  modifiedAt: string
  source: 'api' | 'mock'
  isRootMemory: boolean
  isDaily: boolean
  dateGroup: string | null
}

export type MemoryFileGroup = {
  id: string
  label: string
  files: Array<MemoryViewerFile>
}

export type MemorySearchResult = {
  path: string
  line: number
  snippet: string
}
