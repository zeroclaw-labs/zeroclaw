'use client'

import { HugeiconsIcon } from '@hugeicons/react'
import {
  Cancel01Icon,
  CheckmarkCircle02Icon,
  CloudIcon,
  ComputerIcon,
  Moon01Icon,
  Notification03Icon,
  PaintBoardIcon,
  Settings02Icon,
  SourceCodeSquareIcon,
  Sun01Icon,
  UserIcon,
  MessageMultiple01Icon,
} from '@hugeicons/core-free-icons'
import { useState, useEffect, Component } from 'react'
import type * as React from 'react'
import type { AccentColor, SettingsThemeMode } from '@/hooks/use-settings'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { Tabs, TabsList, TabsTab } from '@/components/ui/tabs'
import { applyTheme, useSettings } from '@/hooks/use-settings'
import { cn } from '@/lib/utils'
import {
  getChatProfileDisplayName,
  useChatSettingsStore,
} from '@/hooks/use-chat-settings'
import { useOnboardingTour } from '@/hooks/use-onboarding-tour'
import type { LoaderStyle } from '@/hooks/use-chat-settings'
import { UserAvatar } from '@/components/avatars'
import { Input } from '@/components/ui/input'
import { LogoLoader } from '@/components/logo-loader'
import { BrailleSpinner } from '@/components/ui/braille-spinner'
import type { BrailleSpinnerPreset } from '@/components/ui/braille-spinner'
import { ThreeDotsSpinner } from '@/components/ui/three-dots-spinner'
import {
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogRoot,
  DialogTitle,
} from '@/components/ui/dialog'

// ── Types ───────────────────────────────────────────────────────────────

type SectionId =
  | 'profile'
  | 'appearance'
  | 'chat'
  | 'editor'
  | 'notifications'
  | 'advanced'

const SECTIONS: Array<{ id: SectionId; label: string; icon: any }> = [
  { id: 'profile', label: 'Profile', icon: UserIcon },
  { id: 'appearance', label: 'Appearance', icon: PaintBoardIcon },
  { id: 'chat', label: 'Chat', icon: MessageMultiple01Icon },
  { id: 'editor', label: 'Editor', icon: SourceCodeSquareIcon },
  { id: 'notifications', label: 'Notifications', icon: Notification03Icon },
  { id: 'advanced', label: 'Advanced', icon: CloudIcon },
]

// ── Shared building blocks ──────────────────────────────────────────────

function SectionHeader({
  title,
  description,
}: {
  title: string
  description: string
}) {
  return (
    <div className="mb-4">
      <h3 className="text-sm font-semibold text-primary-900 dark:text-gray-100">
        {title}
      </h3>
      <p className="text-xs text-primary-500 dark:text-gray-400">
        {description}
      </p>
    </div>
  )
}

function Row({
  label,
  description,
  children,
}: {
  label: string
  description?: string
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 py-2">
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium text-primary-900 dark:text-gray-100">
          {label}
        </p>
        {description && (
          <p className="text-xs text-primary-500 dark:text-gray-400">
            {description}
          </p>
        )}
      </div>
      <div className="flex items-center gap-2">{children}</div>
    </div>
  )
}

// ── Section components ──────────────────────────────────────────────────

function ProfileContent() {
  const { settings: cs, updateSettings: updateCS } = useChatSettingsStore()
  const [profileError, setProfileError] = useState<string | null>(null)
  const [processing, setProcessing] = useState(false)
  const displayName = getChatProfileDisplayName(cs.displayName)
  const [nameError, setNameError] = useState<string | null>(null)

  function handleNameChange(value: string) {
    if (value.length > 50) {
      setNameError('Display name too long (max 50 characters)')
      return
    }
    setNameError(null)
    updateCS({ displayName: value })
  }

  async function handleAvatarUpload(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0]
    e.target.value = ''
    if (!file) return
    if (!file.type.startsWith('image/')) {
      setProfileError('Unsupported file type.')
      return
    }
    if (file.size > 10 * 1024 * 1024) {
      setProfileError('Image too large (max 10MB).')
      return
    }
    setProfileError(null)
    setProcessing(true)
    try {
      const url = URL.createObjectURL(file)
      const img = await new Promise<HTMLImageElement>((resolve, reject) => {
        const i = new Image()
        i.onload = () => resolve(i)
        i.onerror = () => reject(new Error('Failed'))
        i.src = url
      })
      const max = 128,
        scale = Math.min(1, max / Math.max(img.width, img.height))
      const w = Math.round(img.width * scale),
        h = Math.round(img.height * scale)
      const canvas = document.createElement('canvas')
      canvas.width = w
      canvas.height = h
      const ctx = canvas.getContext('2d')!
      ctx.imageSmoothingQuality = 'high'
      ctx.drawImage(img, 0, 0, w, h)
      URL.revokeObjectURL(url)
      updateCS({
        avatarDataUrl: canvas.toDataURL(
          file.type === 'image/png' ? 'image/png' : 'image/jpeg',
          0.82,
        ),
      })
    } catch {
      setProfileError('Failed to process image.')
    } finally {
      setProcessing(false)
    }
  }

  const errorId = 'profile-name-error'

  return (
    <div className="space-y-3">
      <SectionHeader
        title="Profile"
        description="Your display name and avatar for chat."
      />
      <div className="flex items-center gap-4 pb-2">
        <UserAvatar size={48} src={cs.avatarDataUrl} alt={displayName} />
        <div>
          <p className="text-sm font-medium text-primary-900 dark:text-gray-100">
            {displayName}
          </p>
          <p className="text-xs text-primary-500 dark:text-gray-400">
            Shown in sidebar and chat.
          </p>
        </div>
      </div>
      <Row label="Display name">
        <div className="flex-1 max-w-xs">
          <Input
            value={cs.displayName}
            onChange={(e) => handleNameChange(e.target.value)}
            placeholder="User"
            className="h-9 w-full"
            maxLength={50}
            aria-label="Display name"
            aria-invalid={!!nameError}
            aria-describedby={nameError ? errorId : undefined}
          />
          {nameError && (
            <p id={errorId} className="mt-1 text-xs text-red-600" role="alert">
              {nameError}
            </p>
          )}
        </div>
      </Row>
      <Row label="Profile picture">
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <label className="block">
              <input
                type="file"
                accept="image/*"
                onChange={handleAvatarUpload}
                disabled={processing}
                aria-label="Upload profile picture"
                className="block max-w-[12rem] cursor-pointer text-xs text-primary-700 dark:text-gray-300 file:mr-2 file:cursor-pointer file:rounded-md file:border file:border-primary-200 dark:file:border-gray-600 file:bg-primary-100 dark:file:bg-gray-700 file:px-2 file:py-1 file:text-xs file:font-medium file:text-primary-900 dark:file:text-gray-100 file:transition-colors hover:file:bg-primary-200 dark:hover:file:bg-gray-600 disabled:cursor-not-allowed disabled:opacity-50"
              />
            </label>
            <Button
              variant="outline"
              size="sm"
              onClick={() => updateCS({ avatarDataUrl: null })}
              disabled={!cs.avatarDataUrl || processing}
            >
              Remove
            </Button>
          </div>
          {profileError && (
            <p className="text-xs text-red-600" role="alert">
              {profileError}
            </p>
          )}
        </div>
      </Row>
    </div>
  )
}

function AppearanceContent() {
  const { settings, updateSettings } = useSettings()

  function handleThemeChange(value: string) {
    const theme = value as SettingsThemeMode
    applyTheme(theme)
    updateSettings({ theme })
  }

  function badgeClass(color: AccentColor): string {
    if (color === 'orange') return 'bg-orange-500'
    if (color === 'purple') return 'bg-purple-500'
    if (color === 'blue') return 'bg-blue-500'
    return 'bg-green-500'
  }

  return (
    <div className="space-y-3">
      <SectionHeader
        title="Appearance"
        description="Theme, accent color, and loading animation."
      />
      <Row label="Theme">
        <Tabs value={settings.theme} onValueChange={handleThemeChange}>
          <TabsList variant="default" className="gap-1">
            <TabsTab value="system">
              <HugeiconsIcon icon={ComputerIcon} size={16} strokeWidth={1.5} />
              <span>System</span>
            </TabsTab>
            <TabsTab value="light">
              <HugeiconsIcon icon={Sun01Icon} size={16} strokeWidth={1.5} />
              <span>Light</span>
            </TabsTab>
            <TabsTab value="dark">
              <HugeiconsIcon icon={Moon01Icon} size={16} strokeWidth={1.5} />
              <span>Dark</span>
            </TabsTab>
          </TabsList>
        </Tabs>
      </Row>
      <Row label="Accent color">
        <div className="flex gap-1.5">
          {(['orange', 'purple', 'blue', 'green'] as const).map((color) => (
            <Button
              key={color}
              variant="ghost"
              size="sm"
              onClick={() => updateSettings({ accentColor: color })}
              className={cn(
                'border border-primary-200 bg-primary-100/70 hover:bg-primary-200',
                settings.accentColor === color &&
                  'border-primary-500 bg-primary-200',
              )}
            >
              <span
                className={cn('size-2.5 rounded-full', badgeClass(color))}
              />
              <span className="capitalize">{color}</span>
            </Button>
          ))}
        </div>
      </Row>
      <LoaderContent />
    </div>
  )
}

function LoaderContent() {
  const { settings: cs, updateSettings: updateCS } = useChatSettingsStore()
  const styles: Array<{ value: LoaderStyle; label: string }> = [
    { value: 'dots', label: 'Dots' },
    { value: 'braille-claw', label: 'Claw' },
    { value: 'braille-orbit', label: 'Orbit' },
    { value: 'braille-breathe', label: 'Breathe' },
    { value: 'braille-pulse', label: 'Pulse' },
    { value: 'braille-wave', label: 'Wave' },
    { value: 'lobster', label: 'Lobster' },
    { value: 'logo', label: 'Logo' },
  ]
  function getPreset(s: LoaderStyle): BrailleSpinnerPreset | null {
    const m: Record<string, BrailleSpinnerPreset> = {
      'braille-claw': 'claw',
      'braille-orbit': 'orbit',
      'braille-breathe': 'breathe',
      'braille-pulse': 'pulse',
      'braille-wave': 'wave',
    }
    return m[s] ?? null
  }
  function Preview({ style }: { style: LoaderStyle }) {
    if (style === 'dots') return <ThreeDotsSpinner />
    if (style === 'lobster')
      return <span className="inline-block text-sm animate-pulse">🦞</span>
    if (style === 'logo') return <LogoLoader />
    const p = getPreset(style)
    return p ? (
      <BrailleSpinner
        preset={p}
        size={16}
        speed={120}
        className="text-primary-500"
      />
    ) : (
      <ThreeDotsSpinner />
    )
  }
  return (
    <div className="pt-2">
      <p className="mb-2 text-xs text-primary-500 dark:text-gray-400">
        Loading animation
      </p>
      <div className="grid grid-cols-4 gap-1.5">
        {styles.map((o) => (
          <button
            key={o.value}
            type="button"
            onClick={() => updateCS({ loaderStyle: o.value })}
            className={cn(
              'flex min-h-14 flex-col items-center justify-center gap-1.5 rounded-lg border px-1.5 py-1.5 transition-colors',
              cs.loaderStyle === o.value
                ? 'border-primary-500 bg-primary-200/60 text-primary-900'
                : 'border-primary-200 bg-primary-50 text-primary-700 hover:bg-primary-100',
            )}
            aria-pressed={cs.loaderStyle === o.value}
          >
            <span className="flex h-4 items-center justify-center">
              <Preview style={o.value} />
            </span>
            <span className="text-[10px] font-medium leading-3">{o.label}</span>
          </button>
        ))}
      </div>
    </div>
  )
}

function ChatContent() {
  const { settings: cs, updateSettings: updateCS } = useChatSettingsStore()
  return (
    <div className="space-y-3">
      <SectionHeader
        title="Chat Display"
        description="Control what's visible in chat messages."
      />
      <Row
        label="Show tool messages"
        description="Display tool call details when the agent uses tools."
      >
        <Switch
          checked={cs.showToolMessages}
          onCheckedChange={(c) => updateCS({ showToolMessages: c })}
          aria-label="Show tool messages"
        />
      </Row>
      <Row
        label="Show reasoning blocks"
        description="Display model thinking and reasoning process."
      >
        <Switch
          checked={cs.showReasoningBlocks}
          onCheckedChange={(c) => updateCS({ showReasoningBlocks: c })}
          aria-label="Show reasoning blocks"
        />
      </Row>
    </div>
  )
}

function EditorContent() {
  const { settings, updateSettings } = useSettings()
  return (
    <div className="space-y-3">
      <SectionHeader
        title="Editor"
        description="Configure Monaco defaults for the files workspace."
      />
      <Row label="Font size">
        <div className="flex w-full max-w-[14rem] items-center gap-2">
          <input
            type="range"
            min={12}
            max={20}
            value={settings.editorFontSize}
            onChange={(e) =>
              updateSettings({ editorFontSize: Number(e.target.value) })
            }
            className="w-full accent-primary-900 dark:accent-primary-400"
            aria-label={`Editor font size: ${settings.editorFontSize} pixels`}
            aria-valuemin={12}
            aria-valuemax={20}
            aria-valuenow={settings.editorFontSize}
          />
          <span className="w-10 text-right text-sm tabular-nums text-primary-700 dark:text-gray-300">
            {settings.editorFontSize}px
          </span>
        </div>
      </Row>
      <Row label="Word wrap">
        <Switch
          checked={settings.editorWordWrap}
          onCheckedChange={(c) => updateSettings({ editorWordWrap: c })}
          aria-label="Word wrap"
        />
      </Row>
      <Row label="Minimap">
        <Switch
          checked={settings.editorMinimap}
          onCheckedChange={(c) => updateSettings({ editorMinimap: c })}
          aria-label="Show minimap"
        />
      </Row>
    </div>
  )
}

function NotificationsContent() {
  const { settings, updateSettings } = useSettings()
  return (
    <div className="space-y-3">
      <SectionHeader
        title="Notifications"
        description="Control alert delivery and usage thresholds."
      />
      <Row label="Enable alerts">
        <Switch
          checked={settings.notificationsEnabled}
          onCheckedChange={(c) => updateSettings({ notificationsEnabled: c })}
          aria-label="Enable alerts"
        />
      </Row>
      <Row label="Usage threshold">
        <div className="flex w-full max-w-[14rem] items-center gap-2">
          <input
            type="range"
            min={50}
            max={100}
            value={settings.usageThreshold}
            onChange={(e) =>
              updateSettings({ usageThreshold: Number(e.target.value) })
            }
            className="w-full accent-primary-900 dark:accent-primary-400 disabled:opacity-50 disabled:cursor-not-allowed"
            disabled={!settings.notificationsEnabled}
            aria-label={`Usage threshold: ${settings.usageThreshold} percent`}
            aria-valuemin={50}
            aria-valuemax={100}
            aria-valuenow={settings.usageThreshold}
          />
          <span className="w-10 text-right text-sm tabular-nums text-primary-700 dark:text-gray-300">
            {settings.usageThreshold}%
          </span>
        </div>
      </Row>
    </div>
  )
}

function AdvancedContent() {
  const { settings, updateSettings } = useSettings()
  const [connectionStatus, setConnectionStatus] = useState<
    'idle' | 'testing' | 'connected' | 'failed'
  >('idle')
  const [availableModels, setAvailableModels] = useState<
    Array<{ id: string; label: string }>
  >([])
  const [modelsError, setModelsError] = useState(false)
  const [urlError, setUrlError] = useState<string | null>(null)
  const { resetTour } = useOnboardingTour()

  useEffect(() => {
    setModelsError(false)
    fetch('/api/models')
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (d?.models) {
          setAvailableModels(
            d.models.map((m: any) => ({
              id: m.id || '',
              label: m.id?.split('/').pop() || m.id || '',
            })),
          )
          setModelsError(false)
        }
      })
      .catch(() => {
        setModelsError(true)
      })
  }, [])

  function validateAndUpdateUrl(value: string) {
    if (value && value.length > 0) {
      try {
        new URL(value)
        setUrlError(null)
      } catch {
        setUrlError('Invalid URL format')
      }
    } else {
      setUrlError(null)
    }
    updateSettings({ gatewayUrl: value })
  }

  async function testConnection() {
    if (urlError) return
    setConnectionStatus('testing')
    try {
      const r = await fetch('/api/ping')
      setConnectionStatus(r.ok ? 'connected' : 'failed')
    } catch {
      setConnectionStatus('failed')
    }
  }

  const urlErrorId = 'gateway-url-error'

  return (
    <div className="space-y-4">
      <SectionHeader
        title="Gateway Connection"
        description="Set your gateway endpoint."
      />
      <Row label="Gateway URL">
        <div className="flex-1 max-w-sm">
          <input
            type="url"
            placeholder="https://api.openclaw.ai"
            value={settings.gatewayUrl}
            onChange={(e) => validateAndUpdateUrl(e.target.value)}
            className="h-9 w-full rounded-lg border border-primary-200 dark:border-gray-600 bg-primary-50 dark:bg-gray-800 px-2.5 text-sm text-primary-900 dark:text-gray-100 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary-400 dark:focus-visible:ring-primary-500"
            aria-label="Gateway URL"
            aria-invalid={!!urlError}
            aria-describedby={urlError ? urlErrorId : undefined}
          />
          {urlError && (
            <p
              id={urlErrorId}
              className="mt-1 text-xs text-red-600"
              role="alert"
            >
              {urlError}
            </p>
          )}
        </div>
      </Row>
      <Row label="Connection">
        <span
          className={cn(
            'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-medium',
            connectionStatus === 'connected' &&
              'border-green-500/35 bg-green-500/10 text-green-600',
            connectionStatus === 'failed' &&
              'border-red-500/35 bg-red-500/10 text-red-600',
            connectionStatus === 'testing' &&
              'border-accent-500/35 bg-accent-500/10 text-accent-600',
            connectionStatus === 'idle' &&
              'border-primary-300 bg-primary-100 text-primary-700',
          )}
        >
          {connectionStatus === 'idle'
            ? 'Not tested'
            : connectionStatus === 'testing'
              ? 'Testing...'
              : connectionStatus === 'connected'
                ? 'Connected'
                : 'Failed'}
        </span>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => void testConnection()}
          disabled={connectionStatus === 'testing' || !!urlError}
        >
          <HugeiconsIcon
            icon={CheckmarkCircle02Icon}
            size={16}
            strokeWidth={1.5}
          />
          Test
        </Button>
      </Row>

      <div className="border-t border-primary-200 dark:border-gray-700 pt-4">
        <SectionHeader
          title="Smart Suggestions"
          description="Proactive model suggestions."
        />
        <Row label="Enable suggestions">
          <Switch
            checked={settings.smartSuggestionsEnabled}
            onCheckedChange={(c) =>
              updateSettings({ smartSuggestionsEnabled: c })
            }
            aria-label="Enable smart suggestions"
          />
        </Row>
        <Row label="Budget model">
          <select
            value={settings.preferredBudgetModel}
            onChange={(e) =>
              updateSettings({ preferredBudgetModel: e.target.value })
            }
            className="h-9 w-full max-w-[14rem] rounded-lg border border-primary-200 dark:border-gray-600 bg-primary-50 dark:bg-gray-800 px-2 text-sm text-primary-900 dark:text-gray-100 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary-400 dark:focus-visible:ring-primary-500"
            aria-label="Preferred budget model"
          >
            <option value="">Auto-detect</option>
            {modelsError && <option disabled>Failed to load models</option>}
            {availableModels.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        </Row>
        <Row label="Premium model">
          <select
            value={settings.preferredPremiumModel}
            onChange={(e) =>
              updateSettings({ preferredPremiumModel: e.target.value })
            }
            className="h-9 w-full max-w-[14rem] rounded-lg border border-primary-200 dark:border-gray-600 bg-primary-50 dark:bg-gray-800 px-2 text-sm text-primary-900 dark:text-gray-100 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary-400 dark:focus-visible:ring-primary-500"
            aria-label="Preferred premium model"
          >
            <option value="">Auto-detect</option>
            {modelsError && <option disabled>Failed to load models</option>}
            {availableModels.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        </Row>
        <Row label="Only suggest cheaper">
          <Switch
            checked={settings.onlySuggestCheaper}
            onCheckedChange={(c) => updateSettings({ onlySuggestCheaper: c })}
            aria-label="Only suggest cheaper models"
          />
        </Row>
      </div>

      <div className="border-t border-primary-200 dark:border-gray-700 pt-4">
        <SectionHeader
          title="Onboarding"
          description="Restart the welcome tour."
        />
        <Row
          label="Restart Tour"
          description="Show the welcome walkthrough again"
        >
          <Button variant="secondary" size="sm" onClick={resetTour}>
            Restart Tour
          </Button>
        </Row>
      </div>
    </div>
  )
}

// ── Error Boundary ──────────────────────────────────────────────────────

class SettingsErrorBoundary extends Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  state = { error: null as Error | null }

  static getDerivedStateFromError(error: Error) {
    return { error }
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-full items-center justify-center p-8 text-center">
          <div>
            <p className="mb-2 text-sm font-medium text-red-500">
              Settings failed to load
            </p>
            <button
              onClick={() => this.setState({ error: null })}
              className="text-xs text-primary-600 underline hover:text-primary-900"
            >
              Try again
            </button>
          </div>
        </div>
      )
    }
    return this.props.children
  }
}

// ── Main Dialog ─────────────────────────────────────────────────────────

const CONTENT_MAP: Record<SectionId, () => React.JSX.Element> = {
  profile: ProfileContent,
  appearance: AppearanceContent,
  chat: ChatContent,
  editor: EditorContent,
  notifications: NotificationsContent,
  advanced: AdvancedContent,
}

type SettingsDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function SettingsDialog({ open, onOpenChange }: SettingsDialogProps) {
  const [active, setActive] = useState<SectionId>('profile')
  const ActiveContent = CONTENT_MAP[active]

  return (
    <DialogRoot open={open} onOpenChange={onOpenChange}>
      <DialogContent className="w-[min(680px,95vw)] h-[min(80dvh,640px)] max-h-[calc(100dvh-3rem)] overflow-hidden">
        <div className="flex h-full min-h-0 flex-col">
          {/* Header */}
          <div className="flex items-center justify-between border-b border-primary-200 px-5 py-3">
            <div>
              <DialogTitle className="text-base font-semibold">
                Settings
              </DialogTitle>
              <DialogDescription className="hidden">
                Configure ClawSuite
              </DialogDescription>
            </div>
            <DialogClose
              render={
                <Button
                  size="icon-sm"
                  variant="ghost"
                  className="text-primary-500 dark:text-gray-400 hover:bg-primary-100 dark:hover:bg-gray-800"
                  aria-label="Close"
                >
                  <HugeiconsIcon
                    icon={Cancel01Icon}
                    size={18}
                    strokeWidth={1.5}
                  />
                </Button>
              }
            />
          </div>

          <SettingsErrorBoundary>
            {/* Horizontal tabs */}
            <div className="flex gap-0.5 border-b border-primary-200 px-5 overflow-x-auto scrollbar-none">
              {SECTIONS.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  onClick={() => setActive(s.id)}
                  className={cn(
                    'flex items-center gap-1.5 whitespace-nowrap border-b-2 px-3 py-2.5 text-xs font-medium transition-colors',
                    active === s.id
                      ? 'border-accent-500 text-accent-600'
                      : 'border-transparent text-primary-500 dark:text-gray-400 hover:text-primary-700 dark:hover:text-gray-200',
                  )}
                >
                  <HugeiconsIcon icon={s.icon} size={14} strokeWidth={1.5} />
                  {s.label}
                </button>
              ))}
            </div>

            {/* Content */}
            <div className="flex-1 min-h-0 overflow-y-auto px-5 py-4">
              <ActiveContent />
            </div>
          </SettingsErrorBoundary>

          {/* Footer */}
          <div className="border-t border-primary-200 dark:border-gray-700 px-5 py-2.5 text-xs text-primary-500 dark:text-gray-400 flex items-center gap-1.5">
            <HugeiconsIcon icon={Settings02Icon} size={14} strokeWidth={1.5} />
            Changes saved automatically.
          </div>
        </div>
      </DialogContent>
    </DialogRoot>
  )
}
