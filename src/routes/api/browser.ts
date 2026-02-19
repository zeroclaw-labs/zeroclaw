import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import {
  launchBrowser,
  closeBrowser,
  navigate,
  typeText,
  pressKey,
  goBack,
  goForward,
  refresh,
  scrollPage,
  getScreenshot,
  getPageContent,
  cdpMouseClick,
} from '../../server/browser-session'
import {
  startProxy,
  stopProxy,
  getProxyUrl,
  getCurrentTarget,
} from '../../server/browser-proxy'
import { startBrowserStream } from '../../server/browser-stream'

export const Route = createFileRoute('/api/browser')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        const url = new URL(request.url)
        const action = url.searchParams.get('action') || 'status'

        if (action === 'status' || action === 'proxy-status') {
          try {
            return json({
              ok: true,
              proxyUrl: getProxyUrl(),
              target: getCurrentTarget(),
            })
          } catch (err) {
            return json(
              {
                ok: false,
                error: err instanceof Error ? err.message : String(err),
              },
              { status: 500 },
            )
          }
        }

        return json(
          { error: `Unsupported GET action: ${action}` },
          { status: 400 },
        )
      },

      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >
          const action =
            typeof body.action === 'string' ? body.action.trim() : ''

          switch (action) {
            case 'launch': {
              const state = await launchBrowser()
              return json({ ok: true, ...state })
            }

            case 'close': {
              await closeBrowser()
              return json({ ok: true, running: false })
            }

            case 'navigate': {
              const url = typeof body.url === 'string' ? body.url.trim() : ''
              if (!url)
                return json({ error: 'url is required' }, { status: 400 })
              const state = await navigate(url)
              return json({ ok: true, ...state })
            }

            case 'click': {
              const x = typeof body.x === 'number' ? body.x : 0
              const y = typeof body.y === 'number' ? body.y : 0
              const state = await cdpMouseClick(x, y)
              return json({ ok: true, ...state })
            }

            case 'type': {
              const text = typeof body.text === 'string' ? body.text : ''
              const submit = body.submit === true
              const state = await typeText(text, submit)
              return json({ ok: true, ...state })
            }

            case 'press': {
              const key = typeof body.key === 'string' ? body.key : ''
              if (!key)
                return json({ error: 'key is required' }, { status: 400 })
              const state = await pressKey(key)
              return json({ ok: true, ...state })
            }

            case 'back': {
              const state = await goBack()
              return json({ ok: true, ...state })
            }

            case 'forward': {
              const state = await goForward()
              return json({ ok: true, ...state })
            }

            case 'refresh': {
              const state = await refresh()
              return json({ ok: true, ...state })
            }

            case 'scroll': {
              const direction = body.direction === 'up' ? 'up' : 'down'
              const amount = typeof body.amount === 'number' ? body.amount : 400
              const state = await scrollPage(direction, amount)
              return json({ ok: true, ...state })
            }

            case 'screenshot': {
              const state = await getScreenshot()
              return json({ ok: true, ...state })
            }

            case 'content': {
              const content = await getPageContent()
              return json({ ok: true, ...content })
            }

            // Proxy mode — iframe-based browsing
            case 'proxy-start': {
              const result = await startProxy()
              return json({ ok: true, ...result })
            }

            case 'proxy-stop': {
              await stopProxy()
              return json({ ok: true })
            }

            case 'proxy-navigate': {
              const url = typeof body.url === 'string' ? body.url.trim() : ''
              if (!url) return json({ error: 'url required' }, { status: 400 })
              let normalizedUrl = url
              if (!normalizedUrl.match(/^https?:\/\//))
                normalizedUrl = `https://${normalizedUrl}`
              // Navigate the proxy
              const proxyUrl = getProxyUrl()
              await fetch(
                `${proxyUrl}/__proxy__/navigate?url=${encodeURIComponent(normalizedUrl)}`,
              )
              return json({
                ok: true,
                proxyUrl,
                iframeSrc: `${proxyUrl}/?url=${encodeURIComponent(normalizedUrl)}`,
                url: normalizedUrl,
              })
            }

            case 'proxy-status': {
              return json({
                ok: true,
                proxyUrl: getProxyUrl(),
                target: getCurrentTarget(),
              })
            }

            case 'stream-start': {
              const result = await startBrowserStream()
              return json({
                ok: true,
                wsUrl: `ws://localhost:${result.port}`,
                ...result,
              })
            }

            default:
              return json(
                { error: `Unknown action: ${action}` },
                { status: 400 },
              )
          }
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
