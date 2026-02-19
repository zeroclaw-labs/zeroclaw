import { useCallback, useEffect, useMemo, useState } from 'react'
import { Editor } from '@monaco-editor/react'
import {
  DialogClose,
  DialogContent,
  DialogRoot,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'

const LANGUAGE_MAP: Record<string, string> = {
  ts: 'typescript',
  tsx: 'typescript',
  js: 'javascript',
  jsx: 'javascript',
  json: 'json',
  md: 'markdown',
  css: 'css',
  html: 'html',
  yml: 'yaml',
  yaml: 'yaml',
  env: 'dotenv',
}

function getExtension(path: string) {
  const parts = path.split('.')
  return parts.length > 1 ? parts.pop()!.toLowerCase() : ''
}

function isImageFile(path: string) {
  return ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg'].includes(
    getExtension(path),
  )
}

function isTextFile(path: string) {
  return !isImageFile(path)
}

type FilePreviewDialogProps = {
  path: string | null
  onClose: () => void
  onSaved: () => void
}

export default function FilePreviewDialog({
  path,
  onClose,
  onSaved,
}: FilePreviewDialogProps) {
  const [loading, setLoading] = useState(false)
  const [content, setContent] = useState('')
  const [dataUrl, setDataUrl] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [dirty, setDirty] = useState(false)

  const language = useMemo(() => {
    if (!path) return 'plaintext'
    const ext = getExtension(path)
    return LANGUAGE_MAP[ext] || 'plaintext'
  }, [path])

  const loadFile = useCallback(async () => {
    if (!path) return
    setLoading(true)
    setError(null)
    try {
      const res = await fetch(
        `/api/files?action=read&path=${encodeURIComponent(path)}`,
      )
      if (!res.ok) throw new Error('Failed to read file')
      const data = (await res.json()) as {
        type: 'text' | 'image'
        content: string
      }
      if (data.type === 'image') {
        setDataUrl(data.content)
        setContent('')
      } else {
        setContent(data.content)
        setDataUrl('')
      }
      setDirty(false)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [path])

  useEffect(() => {
    if (path) void loadFile()
  }, [loadFile, path])

  const handleSave = useCallback(async () => {
    if (!path) return
    await fetch('/api/files', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        action: 'write',
        path,
        content,
      }),
    })
    setDirty(false)
    onSaved()
  }, [content, onSaved, path])

  return (
    <DialogRoot
      open={Boolean(path)}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent className="w-[min(900px,96vw)]">
        <div className="p-5 border-b border-primary-200 flex items-center justify-between">
          <DialogTitle className="text-base font-semibold">
            {path || 'File'}
          </DialogTitle>
          <div className="flex gap-2">
            {isTextFile(path || '') ? (
              <Button onClick={handleSave} disabled={!dirty || loading}>
                Save
              </Button>
            ) : null}
            <DialogClose render={<Button variant="outline">Close</Button>} />
          </div>
        </div>

        <div className="p-4">
          {loading ? (
            <div className="text-sm text-primary-500">Loading…</div>
          ) : error ? (
            <div className="text-sm text-red-600">{error}</div>
          ) : path && isImageFile(path) ? (
            <div className="flex items-center justify-center">
              {dataUrl ? (
                <img
                  src={dataUrl}
                  alt={path}
                  className="max-h-[60vh] max-w-full rounded-lg border border-primary-200"
                />
              ) : null}
            </div>
          ) : (
            <div className="h-[60vh]">
              <Editor
                value={content}
                language={language}
                theme="vs-dark"
                onChange={(value) => {
                  setContent(value || '')
                  setDirty(true)
                }}
                options={{
                  minimap: { enabled: false },
                  fontSize: 13,
                  scrollBeyondLastLine: false,
                  wordWrap: 'on',
                }}
              />
            </div>
          )}
        </div>
      </DialogContent>
    </DialogRoot>
  )
}
