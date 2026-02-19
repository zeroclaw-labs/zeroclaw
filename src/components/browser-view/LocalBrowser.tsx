import { HugeiconsIcon } from '@hugeicons/react'
import {
  Loading03Icon,
  Cancel01Icon,
  GlobeIcon,
  AiChat02Icon,
  SentIcon,
  ComputerTerminal01Icon,
} from '@hugeicons/core-free-icons'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { Button } from '@/components/ui/button'

type BrowserState = {
  running: boolean
  url: string
  title: string
}

export function LocalBrowser() {
  const navigateTo = useNavigate()
  const [status, setStatus] = useState<BrowserState>({
    running: false,
    url: '',
    title: '',
  })
  const [thumbnail, setThumbnail] = useState<string | null>(null)
  const [launching, setLaunching] = useState(false)
  const [closing, setClosing] = useState(false)
  const [agentPrompt, setAgentPrompt] = useState('')
  const [handingOff, setHandingOff] = useState(false)
  const [urlInput, setUrlInput] = useState('')
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null)

  // Poll stream server for status + thumbnail
  const pollStatus = useCallback(async () => {
    try {
      const res = await fetch('http://localhost:9223', {
        signal: AbortSignal.timeout(2000),
      })
      if (!res.ok) {
        setStatus({ running: false, url: '', title: '' })
        return
      }
      const data = (await res.json()) as Record<string, unknown>
      const running = Boolean(data.running)
      const url = String(data.url || '')
      const title = String(data.title || '')
      setStatus({ running, url, title })
      if (url && !document.activeElement?.classList.contains('url-input')) {
        setUrlInput(url)
      }

      // Get thumbnail screenshot
      if (running) {
        const ssRes = await fetch('http://localhost:9223', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ action: 'screenshot' }),
          signal: AbortSignal.timeout(3000),
        })
        if (ssRes.ok) {
          const ssData = (await ssRes.json()) as Record<string, unknown>
          if (ssData.screenshot) setThumbnail(String(ssData.screenshot))
        }
      }
    } catch {
      setStatus({ running: false, url: '', title: '' })
    }
  }, [])

  useEffect(() => {
    pollStatus()
    pollRef.current = setInterval(pollStatus, 3000)
    return () => {
      if (pollRef.current) clearInterval(pollRef.current)
    }
  }, [pollStatus])

  // Send action to stream server
  const sendAction = useCallback(
    async (action: string, params?: Record<string, unknown>) => {
      try {
        const res = await fetch('http://localhost:9223', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ action, ...params }),
          signal: AbortSignal.timeout(10000),
        })
        return res.ok ? await res.json() : null
      } catch {
        return null
      }
    },
    [],
  )

  const handleLaunch = useCallback(async () => {
    setLaunching(true)
    // First ensure stream server is up
    await fetch('/api/browser', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action: 'stream-start' }),
    }).catch(() => {})
    // Small delay for server startup
    await new Promise((r) => setTimeout(r, 1000))
    await sendAction('launch')
    setTimeout(() => {
      setLaunching(false)
      pollStatus()
    }, 2000)
  }, [sendAction, pollStatus])

  const handleClose = useCallback(async () => {
    setClosing(true)
    await sendAction('close')
    setStatus({ running: false, url: '', title: '' })
    setThumbnail(null)
    setClosing(false)
  }, [sendAction])

  const handleNavigate = useCallback(
    async (e?: React.FormEvent) => {
      e?.preventDefault()
      let url = urlInput.trim()
      if (!url) return
      if (!url.match(/^https?:\/\//)) url = `https://${url}`
      await sendAction('navigate', { url })
      setTimeout(pollStatus, 500)
    },
    [urlInput, sendAction, pollStatus],
  )

  // Agent handoff
  async function handleHandoff() {
    if (!agentPrompt.trim() && !status.url) return
    setHandingOff(true)
    try {
      const content = (await sendAction('content')) as {
        url?: string
        title?: string
        text?: string
      } | null
      const instruction = agentPrompt.trim() || 'Help me with this page.'
      const pageUrl = content?.url || status.url
      const pageTitle = content?.title || status.title
      const pageText = (content?.text || '').slice(0, 3000)
      // Clean summary for display — short and readable
      const contextMsg = [
        `🌐 **Browser Handoff**`,
        `**${pageTitle || 'Untitled'}** — [${pageUrl}](${pageUrl})`,
        `**Task:** ${instruction}`,
      ].join('\n')

      // Send clean message + hidden context
      const fullMessage = `${contextMsg}\n\n<details><summary>Page context</summary>\n\n${pageText.slice(0, 1500)}\n\n</details>\n\nBrowser API: \`POST http://localhost:9223\` with \`{ action, ...params }\``
      const sendRes = await fetch('/api/send', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          sessionKey: '',
          friendlyId: 'new',
          message: fullMessage,
        }),
      })
      const result = (await sendRes.json()) as { friendlyId?: string }
      setAgentPrompt('')
      if (result.friendlyId) {
        void navigateTo({
          to: '/chat/$sessionKey',
          params: { sessionKey: result.friendlyId },
        })
      }
    } catch {
    } finally {
      setHandingOff(false)
    }
  }

  // ── Not running — launch screen ─────────────────────────────
  if (!status.running && !launching) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-6 p-8">
        <div className="flex size-20 items-center justify-center rounded-2xl bg-accent-500/10">
          <HugeiconsIcon
            icon={GlobeIcon}
            size={40}
            strokeWidth={1.5}
            className="text-accent-500"
          />
        </div>
        <div className="text-center max-w-lg">
          <h2 className="text-2xl font-semibold text-ink">Browser</h2>
          <p className="mt-3 text-sm text-primary-500 leading-relaxed">
            Launch a real Chromium window. Browse any site, log in to your
            accounts, then hand control to your AI agent.
          </p>
        </div>
        <Button onClick={handleLaunch} size="lg" className="gap-2.5 px-6">
          <HugeiconsIcon icon={ComputerTerminal01Icon} size={18} /> Launch
          Browser
        </Button>
        <div className="mt-2 grid grid-cols-3 gap-3 max-w-md text-center">
          <div className="rounded-xl border border-primary-200 bg-primary-50/50 p-3">
            <p className="text-lg mb-1">🔐</p>
            <p className="text-[11px] font-medium text-ink">You Log In</p>
          </div>
          <div className="rounded-xl border border-primary-200 bg-primary-50/50 p-3">
            <p className="text-lg mb-1">🤖</p>
            <p className="text-[11px] font-medium text-ink">Agent Takes Over</p>
          </div>
          <div className="rounded-xl border border-primary-200 bg-primary-50/50 p-3">
            <p className="text-lg mb-1">🍪</p>
            <p className="text-[11px] font-medium text-ink">Session Persists</p>
          </div>
        </div>
      </div>
    )
  }

  // ── Launching ────────────────────────────────────────────────
  if (launching) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 p-8">
        <HugeiconsIcon
          icon={Loading03Icon}
          size={32}
          className="animate-spin text-accent-500"
        />
        <p className="text-sm text-primary-500">Launching Chromium...</p>
        <p className="text-xs text-primary-400">
          A browser window will open on your desktop
        </p>
      </div>
    )
  }

  // ── Running — compact control panel ───────────────────────────
  return (
    <div className="flex h-full flex-col">
      {/* Compact header: status + URL + close */}
      <div className="flex items-center gap-2 border-b border-primary-200 bg-primary-50/80 px-3 py-1.5 shrink-0">
        <div className="size-2 rounded-full bg-green-500 animate-pulse shrink-0" />
        <form
          onSubmit={handleNavigate}
          className="flex-1 min-w-0 flex items-center"
        >
          <input
            type="text"
            value={urlInput}
            onChange={(e) => setUrlInput(e.target.value)}
            placeholder="Enter URL..."
            className="url-input flex-1 bg-transparent text-[12px] text-primary-600 placeholder:text-primary-400 focus:outline-none truncate"
          />
        </form>
        <button
          type="button"
          onClick={handleClose}
          disabled={closing}
          className="rounded p-1 text-primary-400 hover:text-red-500 hover:bg-red-50 transition-colors"
          title="Close browser"
        >
          <HugeiconsIcon icon={Cancel01Icon} size={12} />
        </button>
      </div>

      {/* Agent handoff — right at the top */}
      <div className="border-b border-primary-200 bg-surface px-3 py-2 shrink-0">
        <form
          onSubmit={(e) => {
            e.preventDefault()
            handleHandoff()
          }}
          className="flex items-center gap-1.5"
        >
          <HugeiconsIcon
            icon={AiChat02Icon}
            size={14}
            className="text-accent-500 shrink-0"
          />
          <input
            type="text"
            value={agentPrompt}
            onChange={(e) => setAgentPrompt(e.target.value)}
            placeholder="Tell the agent what to do on this page..."
            className="flex-1 rounded border border-primary-200 bg-primary-50 px-2.5 py-1.5 text-[12px] text-ink placeholder:text-primary-400 focus:border-accent-500 focus:outline-none"
          />
          <Button
            type="submit"
            disabled={handingOff}
            className="gap-1 bg-accent-500 hover:bg-accent-400 text-[11px] px-2.5 h-7"
            size="sm"
          >
            {handingOff ? (
              <HugeiconsIcon
                icon={Loading03Icon}
                size={12}
                className="animate-spin"
              />
            ) : (
              <HugeiconsIcon icon={SentIcon} size={12} />
            )}
            Hand to Agent
          </Button>
        </form>
      </div>

      {/* Thumbnail + status */}
      <div className="flex-1 min-h-0 overflow-y-auto p-3">
        <div className="rounded-lg border border-primary-200 overflow-hidden bg-white">
          {thumbnail ? (
            <img
              src={thumbnail}
              alt="Browser"
              className="w-full object-contain max-h-[300px]"
            />
          ) : (
            <div className="flex h-32 items-center justify-center bg-primary-50">
              <p className="text-[11px] text-primary-400">
                Waiting for screenshot...
              </p>
            </div>
          )}
        </div>
        <p className="mt-2 text-[11px] text-primary-500 truncate px-1">
          {status.title || 'Browser window active on your desktop'}
        </p>
      </div>
    </div>
  )
}
