import { URL, fileURLToPath } from 'node:url'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { resolve } from 'node:path'

// devtools removed
import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
// nitro plugin removed (tanstackStart handles server runtime)
import { defineConfig, loadEnv } from 'vite'
import viteTsConfigPaths from 'vite-tsconfig-paths'

const config = defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const gatewayUrl = env.CLAWDBOT_GATEWAY_URL?.trim() || 'ws://127.0.0.1:18789'

  // Allow access from Tailscale, LAN, or custom domains via env var
  // e.g. CLAWSUITE_ALLOWED_HOSTS=my-server.tail1234.ts.net,192.168.1.50
  const allowedHosts: string[] | true = env.CLAWSUITE_ALLOWED_HOSTS?.trim()
    ? env.CLAWSUITE_ALLOWED_HOSTS.split(',')
        .map((h) => h.trim())
        .filter(Boolean)
    : []
  let proxyTarget = 'http://127.0.0.1:18789'

  try {
    const parsed = new URL(gatewayUrl)
    parsed.protocol = parsed.protocol === 'wss:' ? 'https:' : 'http:'
    parsed.pathname = ''
    proxyTarget = parsed.toString().replace(/\/$/, '')
  } catch {
    // fallback
  }

  return {
    define: {
      // Note: Do NOT set 'process.env': {} here — TanStack Start uses environment-based
      // builds where isSsrBuild is unreliable. Blanket process.env replacement breaks
      // server-side code in Docker (kills runtime env var access).
      // Client-side process.env is handled per-environment below.
    },
    resolve: {
      alias: {
        '@': fileURLToPath(new URL('./src', import.meta.url)),
      },
    },
    ssr: {
      external: [
        'playwright',
        'playwright-core',
        'playwright-extra',
        'puppeteer-extra-plugin-stealth',
      ],
    },
    optimizeDeps: {
      exclude: [
        'playwright',
        'playwright-core',
        'playwright-extra',
        'puppeteer-extra-plugin-stealth',
      ],
    },
    server: {
      // Force IPv4 — 'localhost' resolves to ::1 (IPv6) on Windows, breaking gateway connectivity
      host: allowedHosts.length > 0 ? '0.0.0.0' : '127.0.0.1',
      allowedHosts: allowedHosts.length > 0 ? [...allowedHosts, '127.0.0.1', 'localhost'] : ['127.0.0.1', 'localhost'],
      proxy: {
        // WebSocket proxy: clients connect to /ws-gateway on the ClawSuite
        // server (any IP/port), which internally forwards to the local gateway.
        // This means phone/LAN/Docker users never need to reach port 18789 directly.
        '/ws-gateway': {
          target: proxyTarget,
          changeOrigin: false,
          ws: true,
          rewrite: (path) => path.replace(/^\/ws-gateway/, ''),
        },
        // REST API proxy: all /api/gateway/* calls proxied through ClawSuite server
        '/api/gateway-proxy': {
          target: proxyTarget,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/api\/gateway-proxy/, ''),
        },
        '/gateway-ui': {
          target: proxyTarget,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/gateway-ui/, ''),
          ws: true,
          configure: (proxy) => {
            proxy.on('proxyRes', (_proxyRes) => {
              // Strip iframe-blocking headers so we can embed
              delete _proxyRes.headers['x-frame-options']
              delete _proxyRes.headers['content-security-policy']
            })
          },
        },
      },
    },
    plugins: [
      // devtools(),
      // this is the plugin that enables path aliases
      viteTsConfigPaths({
        projects: ['./tsconfig.json'],
      }),
      tailwindcss(),
      tanstackStart(),
      viteReact(),
      // Client-only: replace process.env references in client bundles
      // Server bundles must keep real process.env for Docker runtime env vars
      {
        name: 'client-process-env',
        enforce: 'pre',
        transform(code, _id) {
          const envName = this.environment?.name
          if (envName !== 'client') return null
          if (!code.includes('process.env') && !code.includes('process.platform')) return null

          // Replace specific env vars first, then the generic fallback
          let result = code
          result = result.replace(/process\.env\.CLAWDBOT_GATEWAY_URL/g, JSON.stringify(gatewayUrl))
          result = result.replace(/process\.env\.CLAWDBOT_GATEWAY_TOKEN/g, JSON.stringify(env.CLAWDBOT_GATEWAY_TOKEN || ''))
          result = result.replace(/process\.env\.NODE_ENV/g, JSON.stringify(mode))
          result = result.replace(/process\.env/g, '{}')
          result = result.replace(/process\.platform/g, '"browser"')
          return result
        },
      },
      // Copy pty-helper.py into the server assets directory after build
      {
        name: 'copy-pty-helper',
        closeBundle() {
          const src = resolve('src/server/pty-helper.py')
          const destDir = resolve('dist/server/assets')
          const dest = resolve(destDir, 'pty-helper.py')
          if (existsSync(src)) {
            mkdirSync(destDir, { recursive: true })
            copyFileSync(src, dest)
          }
        },
      },
    ],
  }
})

export default config
