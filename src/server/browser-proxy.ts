/**
 * Local web proxy that strips iframe-blocking headers (X-Frame-Options, CSP).
 * Allows any website to be embedded in an iframe inside ClawSuite.
 * Only runs locally — not exposed to the internet.
 */

import http from 'node:http'
import https from 'node:https'
import { URL } from 'node:url'

const PROXY_PORT = 9222
const STRIP_HEADERS = [
  'x-frame-options',
  'content-security-policy',
  'content-security-policy-report-only',
  'permissions-policy',
  'cross-origin-opener-policy',
  'cross-origin-embedder-policy',
  'cross-origin-resource-policy',
]

let proxyServer: http.Server | null = null
let currentTargetOrigin = ''

function isValidUrl(url: string): boolean {
  try {
    const parsed = new URL(url)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
  } catch {
    return false
  }
}

export function getProxyPort(): number {
  return PROXY_PORT
}

export function getProxyUrl(): string {
  return `http://localhost:${PROXY_PORT}`
}

export function getCurrentTarget(): string {
  return currentTargetOrigin
}

export async function startProxy(): Promise<{ port: number; url: string }> {
  if (proxyServer) {
    return { port: PROXY_PORT, url: getProxyUrl() }
  }

  return new Promise((resolve, reject) => {
    proxyServer = http.createServer((clientReq, clientRes) => {
      // The target URL is passed via the "x-proxy-url" header or query param
      const reqUrl = new URL(
        clientReq.url || '/',
        `http://localhost:${PROXY_PORT}`,
      )
      const targetUrl =
        reqUrl.searchParams.get('url') ||
        (clientReq.headers['x-proxy-url'] as string)

      // Handle CORS preflight
      if (clientReq.method === 'OPTIONS') {
        clientRes.writeHead(200, {
          'Access-Control-Allow-Origin': '*',
          'Access-Control-Allow-Methods': 'GET, POST, PUT, DELETE, OPTIONS',
          'Access-Control-Allow-Headers': '*',
          'Access-Control-Max-Age': '86400',
        })
        clientRes.end()
        return
      }

      // Navigate endpoint — set the target for iframe
      if (reqUrl.pathname === '/__proxy__/navigate') {
        const url = reqUrl.searchParams.get('url') || ''
        if (url && isValidUrl(url)) {
          currentTargetOrigin = url
          clientRes.writeHead(200, {
            'Content-Type': 'application/json',
            'Access-Control-Allow-Origin': '*',
          })
          clientRes.end(JSON.stringify({ ok: true, url }))
        } else {
          clientRes.writeHead(400, { 'Content-Type': 'application/json' })
          clientRes.end(JSON.stringify({ error: 'Invalid URL' }))
        }
        return
      }

      // Status endpoint
      if (reqUrl.pathname === '/__proxy__/status') {
        clientRes.writeHead(200, {
          'Content-Type': 'application/json',
          'Access-Control-Allow-Origin': '*',
        })
        clientRes.end(
          JSON.stringify({
            running: true,
            target: currentTargetOrigin,
            port: PROXY_PORT,
          }),
        )
        return
      }

      // Proxy the actual page
      let fullUrl = ''
      if (targetUrl && isValidUrl(targetUrl)) {
        fullUrl = targetUrl
      } else if (currentTargetOrigin) {
        // Relative path — resolve against current target
        try {
          const base = new URL(currentTargetOrigin)
          fullUrl = new URL(
            reqUrl.pathname + reqUrl.search,
            base.origin,
          ).toString()
        } catch {
          clientRes.writeHead(400)
          clientRes.end('Bad request')
          return
        }
      } else {
        // No target set — show instructions
        clientRes.writeHead(200, { 'Content-Type': 'text/html' })
        clientRes.end(
          `<html><body style="display:flex;align-items:center;justify-content:center;height:100vh;font-family:system-ui;color:#666"><p>Enter a URL above to start browsing</p></body></html>`,
        )
        return
      }

      // Fetch the target
      const parsed = new URL(fullUrl)
      const transport = parsed.protocol === 'https:' ? https : http

      const proxyReq = transport.request(
        fullUrl,
        {
          method: clientReq.method,
          headers: {
            ...clientReq.headers,
            host: parsed.host,
            origin: parsed.origin,
            referer: parsed.origin + '/',
            'accept-encoding': 'identity', // Request uncompressed so we can rewrite HTML
          },
          rejectUnauthorized: false,
        },
        (proxyRes) => {
          // Strip iframe-blocking headers
          const headers: Record<string, string | string[]> = {}
          for (const [key, value] of Object.entries(proxyRes.headers)) {
            if (
              !STRIP_HEADERS.includes(key.toLowerCase()) &&
              value !== undefined
            ) {
              headers[key] = value as string | string[]
            }
          }

          // Add permissive CORS
          headers['access-control-allow-origin'] = '*'
          headers['access-control-allow-credentials'] = 'true'

          // Rewrite Location headers for redirects
          if (headers['location'] && typeof headers['location'] === 'string') {
            const loc = headers['location']
            if (loc.startsWith('http')) {
              headers['location'] = `/?url=${encodeURIComponent(loc)}`
            }
          }

          const contentType = (headers['content-type'] || '') as string
          const isHtml = contentType.includes('text/html')

          if (isHtml) {
            // Buffer HTML to inject <base> tag so relative URLs resolve to the real origin
            const chunks: Buffer[] = []
            proxyRes.on('data', (chunk: Buffer) => chunks.push(chunk))
            proxyRes.on('end', () => {
              let html = Buffer.concat(chunks).toString('utf8')

              // Inject <base> tag + navigation interceptor
              const proxyOrigin = `http://localhost:${PROXY_PORT}`
              const baseTag = `<base href="${parsed.origin}/">
<script>
// Intercept link clicks to route through proxy
document.addEventListener('click', function(e) {
  var a = e.target.closest('a');
  if (a && a.href && a.href.startsWith('http') && !a.href.startsWith('${proxyOrigin}')) {
    e.preventDefault();
    window.location.href = '${proxyOrigin}/?url=' + encodeURIComponent(a.href);
  }
}, true);
// Intercept form submissions
document.addEventListener('submit', function(e) {
  var form = e.target;
  if (form.action && form.action.startsWith('http') && !form.action.startsWith('${proxyOrigin}')) {
    form.action = '${proxyOrigin}/?url=' + encodeURIComponent(form.action);
  }
}, true);
// Notify parent of URL changes
try { window.parent.postMessage({ type: 'proxy-navigate', url: window.location.href }, '*'); } catch(e) {}
</script>`
              if (html.includes('<head>')) {
                html = html.replace('<head>', `<head>${baseTag}`)
              } else if (html.includes('<HEAD>')) {
                html = html.replace('<HEAD>', `<HEAD>${baseTag}`)
              } else if (html.includes('<html')) {
                html = html.replace(
                  /(<html[^>]*>)/i,
                  `$1<head>${baseTag}</head>`,
                )
              } else {
                html = baseTag + html
              }

              // Remove content-length since we modified the body
              delete headers['content-length']
              // Remove content-encoding since we decoded it
              delete headers['content-encoding']
              delete headers['transfer-encoding']

              clientRes.writeHead(proxyRes.statusCode || 200, headers)
              clientRes.end(html)
            })
          } else {
            clientRes.writeHead(proxyRes.statusCode || 200, headers)
            proxyRes.pipe(clientRes)
          }
        },
      )

      proxyReq.on('error', (err) => {
        clientRes.writeHead(502, { 'Content-Type': 'text/html' })
        clientRes.end(
          `<html><body style="display:flex;align-items:center;justify-content:center;height:100vh;font-family:system-ui;color:#666"><p>Failed to load page: ${err.message}</p></body></html>`,
        )
      })

      clientReq.pipe(proxyReq)
    })

    proxyServer.listen(PROXY_PORT, '127.0.0.1', () => {
      resolve({ port: PROXY_PORT, url: getProxyUrl() })
    })

    proxyServer.on('error', (err) => {
      if ((err as any).code === 'EADDRINUSE') {
        // Port already in use — proxy likely already running
        resolve({ port: PROXY_PORT, url: getProxyUrl() })
      } else {
        reject(err)
      }
    })
  })
}

export async function stopProxy(): Promise<void> {
  if (proxyServer) {
    proxyServer.close()
    proxyServer = null
  }
  currentTargetOrigin = ''
}
