import { useState } from 'react'
import type { FormEvent } from 'react'
import type { CronJob, CronJobUpsertInput } from './cron-types'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'

type CronJobFormProps = {
  mode: 'create' | 'edit'
  initialJob: CronJob | null
  pending: boolean
  error: string | null
  onSubmit: (payload: CronJobUpsertInput) => void
  onClose?: () => void
}

function stringifyJson(value: unknown): string {
  if (value == null) return ''
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

function parseOptionalJson(rawValue: string): {
  value?: unknown
  error?: string
} {
  const trimmed = rawValue.trim()
  if (!trimmed) return {}
  try {
    return { value: JSON.parse(trimmed) as unknown }
  } catch {
    return { error: 'Payload and delivery config must be valid JSON.' }
  }
}

export function CronJobForm({
  mode,
  initialJob,
  pending,
  error,
  onSubmit,
  onClose,
}: CronJobFormProps) {
  const [name, setName] = useState(initialJob?.name ?? '')
  const [schedule, setSchedule] = useState(initialJob?.schedule ?? '')
  const [description, setDescription] = useState(initialJob?.description ?? '')
  const [enabled, setEnabled] = useState(initialJob?.enabled ?? true)
  const [payloadInput, setPayloadInput] = useState(
    stringifyJson(initialJob?.payload),
  )
  const [deliveryConfigInput, setDeliveryConfigInput] = useState(
    stringifyJson(initialJob?.deliveryConfig),
  )
  const [localError, setLocalError] = useState<string | null>(null)

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setLocalError(null)

    const trimmedName = name.trim()
    const trimmedSchedule = schedule.trim()
    if (!trimmedName) {
      setLocalError('Name is required.')
      return
    }
    if (!trimmedSchedule) {
      setLocalError('Schedule is required.')
      return
    }

    const payloadResult = parseOptionalJson(payloadInput)
    if (payloadResult.error) {
      setLocalError(payloadResult.error)
      return
    }

    const deliveryConfigResult = parseOptionalJson(deliveryConfigInput)
    if (deliveryConfigResult.error) {
      setLocalError(deliveryConfigResult.error)
      return
    }

    onSubmit({
      jobId: initialJob?.id,
      name: trimmedName,
      schedule: trimmedSchedule,
      description: description.trim() || undefined,
      enabled,
      payload: payloadResult.value,
      deliveryConfig: deliveryConfigResult.value,
    })
  }

  return (
    <section className="rounded-2xl border border-primary-200 bg-primary-50/85 p-4 backdrop-blur-xl">
      <div className="mb-3">
        <h3 className="text-base font-medium text-ink text-balance">
          {mode === 'edit' ? 'Edit Cron Job' : 'Create Cron Job'}
        </h3>
        <p className="mt-1 text-sm text-primary-600 text-pretty">
          Save directly to gateway scheduler methods, then refresh the list.
        </p>
      </div>

      {error || localError ? (
        <p className="mb-3 rounded-lg border border-accent-500/40 bg-accent-500/10 px-3 py-2 text-sm text-accent-500 text-pretty">
          {localError ?? error}
        </p>
      ) : null}

      <form onSubmit={handleSubmit} className="space-y-3">
        <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          <label className="space-y-1">
            <span className="text-xs text-primary-600 tabular-nums">Name</span>
            <input
              value={name}
              onChange={function onChangeName(event) {
                setName(event.target.value)
              }}
              placeholder="Daily Digest"
              className="h-9 w-full rounded-lg border border-primary-200 bg-primary-100/60 px-3 text-sm text-primary-900 outline-none transition-colors focus:border-primary-400"
            />
          </label>

          <label className="space-y-1">
            <span className="text-xs text-primary-600 tabular-nums">
              Schedule
            </span>
            <input
              value={schedule}
              onChange={function onChangeSchedule(event) {
                setSchedule(event.target.value)
              }}
              placeholder="0 9 * * 1-5"
              className="h-9 w-full rounded-lg border border-primary-200 bg-primary-100/60 px-3 text-sm text-primary-900 outline-none transition-colors focus:border-primary-400 tabular-nums"
            />
          </label>
        </div>

        <label className="space-y-1">
          <span className="text-xs text-primary-600 tabular-nums">
            Description
          </span>
          <textarea
            value={description}
            onChange={function onChangeDescription(event) {
              setDescription(event.target.value)
            }}
            rows={2}
            placeholder="Optional job description"
            className="w-full rounded-lg border border-primary-200 bg-primary-100/60 px-3 py-2 text-sm text-primary-900 outline-none transition-colors focus:border-primary-400 text-pretty"
          />
        </label>

        <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          <label className="space-y-1">
            <span className="text-xs text-primary-600 tabular-nums">
              Payload JSON
            </span>
            <textarea
              value={payloadInput}
              onChange={function onChangePayload(event) {
                setPayloadInput(event.target.value)
              }}
              rows={4}
              placeholder='{"channel":"#ops"}'
              className="w-full rounded-lg border border-primary-200 bg-primary-100/60 px-3 py-2 text-xs text-primary-900 outline-none transition-colors focus:border-primary-400 tabular-nums"
            />
          </label>

          <label className="space-y-1">
            <span className="text-xs text-primary-600 tabular-nums">
              Delivery Config JSON
            </span>
            <textarea
              value={deliveryConfigInput}
              onChange={function onChangeDeliveryConfig(event) {
                setDeliveryConfigInput(event.target.value)
              }}
              rows={4}
              placeholder='{"provider":"slack"}'
              className="w-full rounded-lg border border-primary-200 bg-primary-100/60 px-3 py-2 text-xs text-primary-900 outline-none transition-colors focus:border-primary-400 tabular-nums"
            />
          </label>
        </div>

        <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-primary-200 bg-primary-100/50 px-3 py-2">
          <div className="flex items-center gap-2 text-sm text-primary-700">
            <Switch
              checked={enabled}
              onCheckedChange={function onCheckedChange(nextValue) {
                setEnabled(Boolean(nextValue))
              }}
            />
            <span className="tabular-nums">
              {enabled ? 'Enabled' : 'Disabled'}
            </span>
          </div>

          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              type="button"
              disabled={pending}
              onClick={function onClickClose() {
                onClose?.()
              }}
            >
              Cancel
            </Button>
            <Button
              size="sm"
              type="submit"
              disabled={pending}
              className="tabular-nums"
            >
              {pending
                ? 'Saving...'
                : mode === 'edit'
                  ? 'Save Changes'
                  : 'Create Job'}
            </Button>
          </div>
        </div>
      </form>
    </section>
  )
}
