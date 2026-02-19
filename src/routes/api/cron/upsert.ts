import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayCronRpc, normalizeCronBool } from '@/server/cron'

function readString(value: unknown): string {
  if (typeof value !== 'string') return ''
  return value.trim()
}

function asRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {}
  }
  return value as Record<string, unknown>
}

function resolvePayloadJobId(payload: unknown): string | undefined {
  const row = asRecord(payload)
  const candidates = [row.jobId, row.id, row.key]
  for (const candidate of candidates) {
    if (typeof candidate === 'string' && candidate.trim().length > 0) {
      return candidate.trim()
    }
  }
  return undefined
}

function buildUpsertParams(
  body: Record<string, unknown>,
  jobId: string,
  enabled: boolean,
) {
  const name = readString(body.name)
  const schedule = readString(body.schedule)
  const payload = body.payload
  const deliveryConfig = body.deliveryConfig

  if (!jobId) {
    // cron.add format
    return {
      name,
      schedule: { kind: 'cron', expr: schedule },
      payload: payload || { kind: 'systemEvent', text: name },
      delivery: deliveryConfig || undefined,
      sessionTarget: 'main',
      enabled,
    }
  }

  // cron.update format
  return {
    jobId,
    patch: {
      name,
      schedule: { kind: 'cron', expr: schedule },
      payload: payload || undefined,
      delivery: deliveryConfig || undefined,
      enabled,
    },
  }
}

export const Route = createFileRoute('/api/cron/upsert')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const jobId = readString(body.jobId || body.id || body.key)
          const name = readString(body.name)
          const schedule = readString(
            body.schedule || body.cron || body.expression,
          )

          if (!name) {
            return json({ error: 'name is required' }, { status: 400 })
          }
          if (!schedule) {
            return json({ error: 'schedule is required' }, { status: 400 })
          }

          const enabled = normalizeCronBool(body.enabled, true)

          const methods = jobId
            ? ['cron.update']
            : ['cron.add']

          const payload = await gatewayCronRpc(
            methods,
            buildUpsertParams(body, jobId, enabled),
          )
          const resolvedJobId =
            resolvePayloadJobId(payload) ?? (jobId || undefined)

          return json({
            ok: true,
            payload,
            jobId: resolvedJobId,
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
      },
    },
  },
})
