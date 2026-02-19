import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { writeFile, readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { discoverGateway } from '../../server/gateway-discovery'

export const Route = createFileRoute('/api/gateway-discover')({
  server: {
    handlers: {
      /**
       * POST /api/gateway-discover
       *
       * Auto-discover local OpenClaw gateway, configure .env, and test connection.
       * Returns { ok, url, source, error } — if ok=true, gateway is ready to use.
       */
      POST: async () => {
        try {
          const result = await discoverGateway()

          if (!result.found) {
            return json({
              ok: false,
              error: result.error || 'No gateway found',
              portOpen: Boolean(result.url),
            })
          }

          // Write discovered config to .env so it persists across restarts
          const envPath = join(process.cwd(), '.env')
          let envContent = ''

          try {
            envContent = await readFile(envPath, 'utf-8')
          } catch {
            try {
              envContent = await readFile(join(process.cwd(), '.env.example'), 'utf-8')
            } catch {
              envContent = ''
            }
          }

          if (result.url) {
            if (envContent.match(/^CLAWDBOT_GATEWAY_URL=/m)) {
              envContent = envContent.replace(
                /^CLAWDBOT_GATEWAY_URL=.*/m,
                `CLAWDBOT_GATEWAY_URL=${result.url}`,
              )
            } else {
              envContent += `\nCLAWDBOT_GATEWAY_URL=${result.url}`
            }
          }

          if (result.token) {
            if (envContent.match(/^CLAWDBOT_GATEWAY_TOKEN=/m)) {
              envContent = envContent.replace(
                /^CLAWDBOT_GATEWAY_TOKEN=.*/m,
                `CLAWDBOT_GATEWAY_TOKEN=${result.token}`,
              )
            } else {
              envContent += `\nCLAWDBOT_GATEWAY_TOKEN=${result.token}`
            }
          }

          await writeFile(envPath, envContent, 'utf-8')

          // Now test the actual connection
          const { gatewayConnectCheck } = await import('../../server/gateway')
          try {
            await gatewayConnectCheck()
            return json({
              ok: true,
              url: result.url,
              source: result.source,
            })
          } catch (connErr) {
            return json({
              ok: false,
              url: result.url,
              source: result.source,
              error: `Found config but connection failed: ${connErr instanceof Error ? connErr.message : String(connErr)}`,
            })
          }
        } catch (err) {
          return json(
            { ok: false, error: err instanceof Error ? err.message : String(err) },
            { status: 500 },
          )
        }
      },
    },
  },
})
