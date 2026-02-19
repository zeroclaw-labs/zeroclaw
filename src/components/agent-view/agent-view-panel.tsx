import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  ArrowDown01Icon,
  ArrowExpand01Icon,
  ArrowRight01Icon,
  BotIcon,
  Cancel01Icon,
  Link01Icon,
} from '@hugeicons/core-free-icons'
import { useNavigate } from '@tanstack/react-router'
import {
  AnimatePresence,
  LayoutGroup,
  motion,
  useReducedMotion,
} from 'motion/react'
import { AgentCard } from './agent-card'
import { useAgentSpawn } from './hooks/use-agent-spawn'
import type {
  AgentNode,
  AgentNodeStatus,
  AgentStatusBubble,
} from './agent-card'
import type { ActiveAgent } from '@/hooks/use-agent-view'
import { AgentChatModal } from '@/components/agent-chat/AgentChatModal'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsiblePanel,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  ScrollAreaCorner,
  ScrollAreaRoot,
  ScrollAreaScrollbar,
  ScrollAreaThumb,
  ScrollAreaViewport,
} from '@/components/ui/scroll-area'
import { formatCost, useAgentView } from '@/hooks/use-agent-view'
import { useCliAgents } from '@/hooks/use-cli-agents'
import { useSounds } from '@/hooks/use-sounds'
import { OrchestratorAvatar } from '@/components/orchestrator-avatar'
import { useOrchestratorState } from '@/hooks/use-orchestrator-state'
import { useChatActivityStore } from '@/stores/chat-activity-store'
import { BrowserSidebarPreview } from '@/components/browser-view/browser-sidebar-preview'
import { cn } from '@/lib/utils'

function getLastUserMessageBubbleElement(): HTMLElement | null {
  const nodes = document.querySelectorAll<HTMLElement>(
    '[data-chat-message-role="user"] [data-chat-message-bubble="true"]',
  )
  return nodes.item(nodes.length - 1)
}

function formatRelativeMs(msAgo: number): string {
  const seconds = Math.max(0, Math.floor(msAgo / 1000))
  if (seconds < 60) return `${seconds}s ago`
  const minutes = Math.floor(seconds / 60)
  return `${minutes}m ago`
}

function summarizeTask(raw: string): string {
  if (!raw) return ''
  // Strip "exec " prefix and clean up codex command noise
  let t = raw
    .replace(/^exec\s+/i, '')
    .replace(/^codex\s+exec\s+--full-auto\s+/i, '')
  // Remove quotes wrapping the whole thing
  t = t.replace(/^['"]|['"]$/g, '')
  // Take first sentence or first 60 chars
  const firstLine = t.split(/[.\n]/)[0] || t
  return firstLine.slice(0, 60).trim() + (firstLine.length > 60 ? '…' : '')
}

function formatRuntimeLabel(runtimeSeconds: number): string {
  const clampedSeconds = Math.max(0, Math.floor(runtimeSeconds))
  const hours = Math.floor(clampedSeconds / 3600)
  const minutes = Math.floor((clampedSeconds % 3600) / 60)
  const seconds = clampedSeconds % 60

  return [
    String(hours).padStart(2, '0'),
    String(minutes).padStart(2, '0'),
    String(seconds).padStart(2, '0'),
  ].join(':')
}

const AGENT_NAME_KEY = 'clawsuite-agent-name'

function getStoredAgentName(): string {
  try {
    const v = localStorage.getItem(AGENT_NAME_KEY)
    if (v && v.trim()) return v.trim()
  } catch {
    /* noop */
  }
  return ''
}

const STATE_GLOW: Record<string, string> = {
  idle: 'border-primary-300/70',
  reading: 'border-blue-400/50 shadow-[0_0_8px_rgba(59,130,246,0.15)]',
  thinking: 'border-yellow-400/50 shadow-[0_0_8px_rgba(234,179,8,0.15)]',
  responding: 'border-emerald-400/50 shadow-[0_0_8px_rgba(34,197,94,0.2)]',
  'tool-use': 'border-violet-400/50 shadow-[0_0_8px_rgba(139,92,246,0.15)]',
  orchestrating: 'border-accent-400/50 shadow-[0_0_8px_rgba(249,115,22,0.2)]',
}

function OrchestratorCard({
  compact = false,
  cardRef,
}: {
  compact?: boolean
  cardRef?: (element: HTMLElement | null) => void
}) {
  const { state, label } = useOrchestratorState()
  const glowClass = STATE_GLOW[state] ?? STATE_GLOW.idle

  const [agentName, setAgentName] = useState(getStoredAgentName)
  const [isEditing, setIsEditing] = useState(false)
  const [editValue, setEditValue] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)

  // Fetch model from gateway
  const [model, setModel] = useState('')
  useEffect(() => {
    let cancelled = false
    async function fetchModel() {
      try {
        const res = await fetch('/api/session-status')
        if (!res.ok) return
        const data = await res.json()
        const payload = data.payload ?? data
        const m = payload.model ?? payload.currentModel ?? ''
        if (!cancelled && m) setModel(String(m))
      } catch {
        /* noop */
      }
    }
    void fetchModel()
    const timer = setInterval(fetchModel, 30_000)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [])

  const displayName = agentName || 'Agent'

  function startEdit() {
    setEditValue(agentName)
    setIsEditing(true)
    setTimeout(() => inputRef.current?.focus(), 50)
  }

  function commitEdit() {
    const trimmed = editValue.trim()
    setAgentName(trimmed)
    setIsEditing(false)
    try {
      localStorage.setItem(AGENT_NAME_KEY, trimmed)
    } catch {
      /* noop */
    }
  }

  return (
    <div
      ref={cardRef}
      className={cn(
        'relative rounded-2xl border bg-gradient-to-br from-primary-100/80 via-primary-100/60 to-primary-200/40 transition-all duration-500',
        compact ? 'p-2' : 'p-3',
        glowClass,
      )}
    >
      {state !== 'idle' && (
        <div className="pointer-events-none absolute inset-0 animate-pulse rounded-2xl bg-gradient-to-br from-accent-500/[0.03] to-transparent" />
      )}

      <div
        className={cn(
          'relative flex items-center',
          compact ? 'gap-2' : 'flex-col text-center gap-2',
        )}
      >
        <OrchestratorAvatar size={compact ? 32 : 52} />

        <div className="min-w-0 flex-1">
          <div
            className={cn(
              'flex items-center gap-1.5',
              !compact && 'justify-center',
            )}
          >
            {isEditing ? (
              <input
                ref={inputRef}
                value={editValue}
                onChange={(e) => setEditValue(e.target.value)}
                onBlur={commitEdit}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') commitEdit()
                  if (e.key === 'Escape') setIsEditing(false)
                }}
                placeholder="Agent name..."
                className="w-24 rounded border border-primary-300/70 bg-primary-50 px-1.5 py-0.5 text-xs font-semibold text-primary-900 outline-none focus:border-accent-400"
                maxLength={20}
              />
            ) : (
              <button
                type="button"
                onClick={startEdit}
                className={cn(
                  'font-semibold text-primary-900 hover:text-accent-600 transition-colors',
                  compact ? 'text-[11px]' : 'text-xs',
                )}
                title="Click to rename"
              >
                {displayName}
              </button>
            )}
            {!compact && (
              <span className="rounded-full bg-accent-500/15 px-1.5 py-0.5 text-[9px] font-medium text-accent-600">
                Main Agent
              </span>
            )}
          </div>
          <p
            className={cn(
              'text-primary-600',
              compact ? 'text-[9px]' : 'mt-0.5 text-[10px]',
            )}
          >
            {label}
          </p>
          {!compact && model && (
            <p className="mt-0.5 truncate text-[9px] font-mono text-primary-500">
              {model}
            </p>
          )}
        </div>
      </div>
    </div>
  )
}

function getHistoryPillClassName(status: 'success' | 'failed'): string {
  if (status === 'failed') {
    return 'border-red-500/50 bg-red-500/10 text-red-300'
  }
  return 'border-emerald-500/40 bg-emerald-500/10 text-emerald-300'
}

function getStatusLabel(status: AgentNodeStatus): string {
  if (status === 'failed') return 'failed'
  if (status === 'thinking') return 'thinking'
  if (status === 'complete') return 'complete'
  if (status === 'queued') return 'queued'
  return 'running'
}

function getAgentStatus(agent: ActiveAgent): AgentNodeStatus {
  const status = agent.status.toLowerCase()
  if (status === 'thinking') return 'thinking'
  if (['failed', 'error', 'cancelled', 'canceled', 'killed'].includes(status)) {
    return 'failed'
  }
  if (
    ['complete', 'completed', 'success', 'succeeded', 'done'].includes(
      status,
    ) ||
    agent.progress >= 99
  ) {
    return 'complete'
  }
  return 'running'
}

function getStatusBubble(
  status: AgentNodeStatus,
  progress: number,
): AgentStatusBubble {
  if (status === 'thinking') {
    return { type: 'thinking', text: 'Reasoning through next step' }
  }
  if (status === 'failed') {
    return { type: 'error', text: 'Execution failed, awaiting retry' }
  }
  if (status === 'complete') {
    return { type: 'checkpoint', text: 'Checkpoint complete' }
  }
  if (status === 'queued') {
    return { type: 'question', text: 'Queued for dispatch' }
  }
  const clampedProgress = Math.max(0, Math.min(100, Math.round(progress)))
  return { type: 'checkpoint', text: `${clampedProgress}% complete` }
}

export function AgentViewPanel() {
  // Sound notifications for agent events
  useSounds({ autoPlay: true })

  // Start gateway polling for orchestrator state (detects activity from Telegram/other channels)
  const startGatewayPoll = useChatActivityStore((s) => s.startGatewayPoll)
  const stopGatewayPoll = useChatActivityStore((s) => s.stopGatewayPoll)
  useEffect(() => {
    startGatewayPoll()
    return () => stopGatewayPoll()
  }, [startGatewayPoll, stopGatewayPoll])

  const {
    isOpen,
    isDesktop,
    panelVisible,
    showFloatingToggle,
    panelWidth,
    nowMs,
    lastRefreshedMs,
    activeAgents,
    queuedAgents,
    historyAgents,
    historyOpen,
    isLoading,
    isLiveConnected,
    errorMessage,
    setOpen,
    setHistoryOpen,
    killAgent,
    cancelQueueTask,
    activeCount,
  } = useAgentView()

  const navigate = useNavigate()

  // Transcript modal removed — View button now navigates to /agent-swarm
  const [selectedAgentChat, setSelectedAgentChat] = useState<{
    sessionKey: string
    agentName: string
    statusLabel: string
  } | null>(null)
  const [cliAgentsExpanded, setCliAgentsExpanded] = useState(true)
  const [browserPreviewExpanded, setBrowserPreviewExpanded] = useState(true)
  const cliAgentsQuery = useCliAgents()
  const cliAgents = cliAgentsQuery.data ?? []
  // Auto: expanded avatar when idle, compact when agents are working
  const viewMode =
    activeCount > 0 || cliAgents.length > 0 ? 'compact' : 'expanded'

  // Auto-expand history only when first entry arrives
  const prevHistoryCount = useRef(0)
  useEffect(() => {
    if (historyAgents.length > 0 && prevHistoryCount.current === 0) {
      setHistoryOpen(true)
    }
    prevHistoryCount.current = historyAgents.length
  }, [historyAgents.length, setHistoryOpen])

  const totalCost = useMemo(
    function getTotalCost() {
      return activeAgents.reduce(function sumCost(total, agent) {
        return total + agent.estimatedCost
      }, 0)
    },
    [activeAgents],
  )

  const activeNodes = useMemo(
    function buildActiveNodes() {
      return activeAgents
        .map(function mapAgentToNode(agent) {
          const runtimeSeconds = Math.max(
            1,
            Math.floor((nowMs - agent.startedAtMs) / 1000),
          )
          const status = getAgentStatus(agent)

          return {
            id: agent.id,
            name: agent.name,
            task: agent.task,
            model: agent.model,
            progress: agent.progress,
            runtimeSeconds,
            tokenCount: agent.tokenCount,
            cost: agent.estimatedCost,
            status,
            isLive: agent.isLive,
            statusBubble: getStatusBubble(status, agent.progress),
            sessionKey: agent.id, // Use agent id as session key
          } satisfies AgentNode
        })
        .sort(function sortByProgressDesc(left, right) {
          if (right.progress !== left.progress) {
            return right.progress - left.progress
          }
          return left.name.localeCompare(right.name)
        })
    },
    [activeAgents, nowMs],
  )

  const queuedNodes = useMemo(
    function buildQueuedNodes() {
      return queuedAgents.map(function mapQueuedAgent(task, index) {
        return {
          id: task.id,
          name: task.name,
          task: task.description,
          model: 'queued',
          progress: 5 + index * 7,
          runtimeSeconds: 0,
          tokenCount: 0,
          cost: 0,
          status: 'queued',
          statusBubble: getStatusBubble('queued', 0),
        } satisfies AgentNode
      })
    },
    [queuedAgents],
  )

  // Swarm node stats removed — OrchestratorCard now serves as the main agent representation

  const activeNodeIds = useMemo(
    () => activeNodes.map((node) => node.id),
    // Stabilize: only recompute when the sorted id string changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [activeNodes.map((n) => n.id).join(',')],
  )
  const agentSpawn = useAgentSpawn(activeNodeIds)
  const shouldReduceMotion = useReducedMotion()
  const networkLayerRef = useRef<HTMLDivElement | null>(null)
  const [sourceBubbleRect, setSourceBubbleRect] = useState<DOMRect | null>(null)

  const visibleActiveNodes = useMemo(
    function getVisibleActiveNodes() {
      return activeNodes.filter(function keepRenderedNode(node) {
        return agentSpawn.shouldRenderCard(node.id)
      })
    },
    [activeNodes, agentSpawn],
  )

  const spawningNodes = useMemo(
    function getSpawningNodes() {
      return activeNodes.filter(function keepSpawningNode(node) {
        return agentSpawn.isSpawning(node.id)
      })
    },
    [activeNodes, agentSpawn],
  )

  const updateSourceBubbleRect = useCallback(function updateSourceBubbleRect() {
    if (typeof document === 'undefined') return
    const element = getLastUserMessageBubbleElement()
    if (!element) {
      setSourceBubbleRect(null)
      return
    }
    setSourceBubbleRect(element.getBoundingClientRect())
  }, [])

  useEffect(
    function syncSourceBubbleRect() {
      if (!panelVisible) return
      updateSourceBubbleRect()
      window.addEventListener('resize', updateSourceBubbleRect)
      window.addEventListener('scroll', updateSourceBubbleRect, true)
      return function cleanupSourceBubbleTracking() {
        window.removeEventListener('resize', updateSourceBubbleRect)
        window.removeEventListener('scroll', updateSourceBubbleRect, true)
      }
    },
    [panelVisible, updateSourceBubbleRect],
  )

  const statusCounts = useMemo(
    function getStatusCounts() {
      return visibleActiveNodes.reduce(
        function summarizeCounts(counts, item) {
          if (item.status === 'thinking') {
            return { ...counts, thinking: counts.thinking + 1 }
          }
          if (item.status === 'failed') {
            return { ...counts, failed: counts.failed + 1 }
          }
          if (item.status === 'complete') {
            return { ...counts, complete: counts.complete + 1 }
          }
          return { ...counts, running: counts.running + 1 }
        },
        { running: 0, thinking: 0, failed: 0, complete: 0 },
      )
    },
    [visibleActiveNodes],
  )

  // View functionality is now handled inline within AgentCard via useInlineDetail

  function handleChatByNodeId(nodeId: string) {
    const activeNode = activeNodes.find(function matchActiveNode(node) {
      return node.id === nodeId
    })
    if (activeNode) {
      setSelectedAgentChat({
        sessionKey: activeNode.id,
        agentName: activeNode.name,
        statusLabel: getStatusLabel(activeNode.status),
      })
      return
    }

    const queuedNode = queuedNodes.find(function matchQueuedNode(node) {
      return node.id === nodeId
    })
    if (!queuedNode) return

    setSelectedAgentChat({
      sessionKey: queuedNode.id,
      agentName: queuedNode.name,
      statusLabel: getStatusLabel(queuedNode.status),
    })
  }

  return (
    <>
      {isDesktop ? (
        <motion.aside
          initial={false}
          animate={{ x: panelVisible ? 0 : panelWidth }}
          transition={{ duration: 0.22, ease: 'easeInOut' }}
          className={cn(
            'fixed inset-y-0 right-0 z-40 w-80 border-l border-primary-300/70 bg-primary-100/92 backdrop-blur-xl',
            panelVisible ? 'pointer-events-auto' : 'pointer-events-none',
          )}
        >
          <div className="border-b border-primary-300/70 px-3 py-2">
            {/* Row 1: Count left | Title center | Actions right */}
            <div className="flex items-center justify-between">
              {/* Left — active agent count + live indicator */}
              <div className="flex items-center gap-1.5">
                <span
                  className={cn(
                    'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium tabular-nums cursor-default',
                    activeCount > 0
                      ? 'border-emerald-400/40 bg-emerald-500/10 text-emerald-700'
                      : 'border-primary-300/70 bg-primary-200/50 text-primary-700',
                  )}
                  title={`${activeCount} agent${activeCount !== 1 ? 's' : ''} running · ${historyAgents.length} in history · ${queuedAgents.length} queued`}
                >
                  {isLiveConnected ? (
                    <motion.span
                      animate={
                        activeCount > 0
                          ? { opacity: [0.4, 1, 0.4], scale: [1, 1.2, 1] }
                          : { opacity: [0.4, 1, 0.4] }
                      }
                      transition={{
                        duration: 1.4,
                        repeat: Infinity,
                        ease: 'easeInOut',
                      }}
                      className={cn(
                        'size-1.5 rounded-full',
                        activeCount > 0 ? 'bg-emerald-400' : 'bg-emerald-400',
                      )}
                    />
                  ) : (
                    <span className="size-1.5 rounded-full bg-primary-400/50" />
                  )}
                  {activeCount}
                </span>
              </div>

              {/* Center — title */}
              <h2 className="text-sm font-semibold text-primary-900">
                Agent Hub
              </h2>

              {/* Right — expand + close */}
              <div className="flex items-center gap-1">
                <Button
                  size="icon-sm"
                  variant="ghost"
                  onClick={function handleExpandHub() {
                    setOpen(false)
                    navigate({ to: '/agent-swarm' })
                  }}
                  aria-label="Open Agent Hub"
                  title="Open Agent Hub"
                >
                  <HugeiconsIcon
                    icon={ArrowExpand01Icon}
                    size={16}
                    strokeWidth={1.5}
                  />
                </Button>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  onClick={function handleClosePanel() {
                    setOpen(false)
                  }}
                  aria-label="Hide Agent View"
                >
                  <HugeiconsIcon
                    icon={Cancel01Icon}
                    size={18}
                    strokeWidth={1.5}
                  />
                </Button>
              </div>
            </div>
            {/* Row 2: Stats */}
            {activeCount > 0 || queuedAgents.length > 0 ? (
              <p className="mt-1 text-[10px] text-primary-600 tabular-nums">
                {activeCount} active · {queuedAgents.length} queued ·{' '}
                {formatCost(totalCost)}
              </p>
            ) : null}
          </div>

          <ScrollAreaRoot className="h-[calc(100vh-3.25rem)]">
            <ScrollAreaViewport>
              <div className="space-y-3 p-3">
                {/* Main Agent Card */}
                <OrchestratorCard compact={viewMode === 'compact'} />

                {/* Swarm — agent cards */}
                <section className="rounded-2xl border border-primary-300/70 bg-primary-200/35 p-1">
                  {/* Centered Swarm pill */}
                  <div className="mb-1 flex justify-center">
                    <span className="rounded-full border border-primary-300/70 bg-primary-100/80 px-3 py-0.5 text-[10px] font-medium text-primary-600 shadow-sm">
                      Swarm
                    </span>
                  </div>

                  <div className="mb-1 flex items-center justify-between">
                    <div>
                      <p className="text-[10px] text-primary-600 tabular-nums">
                        {isLoading
                          ? 'syncing...'
                          : statusCounts.running === 0 &&
                              statusCounts.thinking === 0 &&
                              statusCounts.failed === 0 &&
                              statusCounts.complete === 0
                            ? 'No subagents'
                            : [
                                statusCounts.running > 0 &&
                                  `${statusCounts.running} running`,
                                statusCounts.thinking > 0 &&
                                  `${statusCounts.thinking} thinking`,
                                statusCounts.failed > 0 &&
                                  `${statusCounts.failed} failed`,
                                statusCounts.complete > 0 &&
                                  `${statusCounts.complete} complete`,
                              ]
                                .filter(Boolean)
                                .join(' · ')}
                      </p>
                      {errorMessage ? (
                        <p className="line-clamp-1 text-[10px] text-red-300 tabular-nums">
                          {errorMessage}
                        </p>
                      ) : null}
                    </div>
                    <div className="text-right text-[10px] text-primary-500 tabular-nums">
                      <p>
                        {isLoading
                          ? ''
                          : `synced ${formatRelativeMs(nowMs - lastRefreshedMs)}`}
                      </p>
                    </div>
                  </div>

                  <LayoutGroup id="agent-swarm-grid">
                    {activeNodes.length > 0 ||
                    spawningNodes.length > 0 ||
                    queuedNodes.length > 0 ? (
                      <motion.div
                        ref={networkLayerRef}
                        layout
                        transition={{
                          layout: {
                            type: 'spring',
                            stiffness: 320,
                            damping: 30,
                          },
                        }}
                        className="relative rounded-xl border border-primary-300/70 bg-linear-to-b from-primary-100 via-primary-100 to-primary-200/40 p-1"
                      >
                        <AnimatePresence initial={false}>
                          {spawningNodes.map(
                            function renderSpawningGhost(node, index) {
                              const fallbackLeft = 24 + index * 14
                              const fallbackTop = 128 + index * 10
                              const width = sourceBubbleRect
                                ? Math.min(sourceBubbleRect.width, 152)
                                : 124
                              const height = sourceBubbleRect
                                ? Math.min(sourceBubbleRect.height, 44)
                                : 32
                              const top = sourceBubbleRect
                                ? sourceBubbleRect.top
                                : fallbackTop
                              const left = sourceBubbleRect
                                ? sourceBubbleRect.left +
                                  sourceBubbleRect.width -
                                  width
                                : fallbackLeft

                              return (
                                <motion.div
                                  key={`spawn-ghost-${node.id}`}
                                  layoutId={agentSpawn.getSharedLayoutId(
                                    node.id,
                                  )}
                                  initial={
                                    shouldReduceMotion
                                      ? { opacity: 0, scale: 0.96 }
                                      : { opacity: 0, scale: 0.9 }
                                  }
                                  animate={
                                    shouldReduceMotion
                                      ? { opacity: 0.65, scale: 1 }
                                      : {
                                          opacity: [0.5, 0.85, 0.5],
                                          scale: [0.94, 1, 0.94],
                                        }
                                  }
                                  exit={{ opacity: 0, scale: 0.94 }}
                                  transition={
                                    shouldReduceMotion
                                      ? { duration: 0.12, ease: 'easeOut' }
                                      : { duration: 0.42, ease: 'easeInOut' }
                                  }
                                  className="pointer-events-none fixed z-30 rounded-full border border-accent-500/40 bg-accent-500/20 shadow-sm backdrop-blur-sm"
                                  style={{ top, left, width, height }}
                                />
                              )
                            },
                          )}
                        </AnimatePresence>

                        {activeNodes.length > 0 || spawningNodes.length > 0 ? (
                          <motion.div
                            layout
                            transition={{
                              layout: {
                                type: 'spring',
                                stiffness: 360,
                                damping: 34,
                              },
                            }}
                            className={cn(
                              'grid gap-1.5 items-start',
                              viewMode === 'compact'
                                ? 'grid-cols-2'
                                : 'grid-cols-1',
                            )}
                          >
                            <AnimatePresence mode="popLayout" initial={false}>
                              {visibleActiveNodes.map(
                                function renderActiveNode(node) {
                                  return (
                                    <motion.div
                                      key={node.id}
                                      layout="position"
                                      initial={{
                                        y: -18,
                                        opacity: 0,
                                        scale: 0.96,
                                      }}
                                      animate={{ y: 0, opacity: 1, scale: 1 }}
                                      exit={{ y: 10, opacity: 0, scale: 0.88 }}
                                      transition={{
                                        type: 'spring',
                                        stiffness: 300,
                                        damping: 25,
                                      }}
                                      className="w-full"
                                    >
                                      <AgentCard
                                        node={node}
                                        layoutId={agentSpawn.getSharedLayoutId(
                                          node.id,
                                        )}
                                        viewMode={viewMode}
                                        onChat={handleChatByNodeId}
                                        onKill={killAgent}
                                        useInlineDetail
                                        className={cn(
                                          agentSpawn.isSpawning(node.id)
                                            ? 'ring-2 ring-accent-500/35'
                                            : '',
                                        )}
                                      />
                                    </motion.div>
                                  )
                                },
                              )}
                            </AnimatePresence>
                          </motion.div>
                        ) : null}

                        {queuedNodes.length > 0 ? (
                          <motion.div layout className="mt-1.5 space-y-1">
                            <p className="text-[10px] text-primary-600 tabular-nums">
                              Queue
                            </p>
                            <motion.div
                              layout
                              className={cn(
                                'grid gap-1.5 items-start',
                                viewMode === 'compact'
                                  ? 'grid-cols-2'
                                  : 'grid-cols-1',
                              )}
                            >
                              {queuedNodes.map(function renderQueuedNode(node) {
                                return (
                                  <div key={node.id} className="w-full">
                                    <AgentCard
                                      node={node}
                                      layoutId={agentSpawn.getCardLayoutId(
                                        node.id,
                                      )}
                                      viewMode={viewMode}
                                      onChat={handleChatByNodeId}
                                      onCancel={cancelQueueTask}
                                      useInlineDetail
                                    />
                                  </div>
                                )
                              })}
                            </motion.div>
                          </motion.div>
                        ) : null}
                      </motion.div>
                    ) : (
                      <p
                        ref={
                          networkLayerRef as React.RefObject<HTMLParagraphElement>
                        }
                        className="text-[11px] text-pretty text-primary-600 py-1"
                      >
                        No active subagents. Spawn agents from chat to see them
                        here.
                      </p>
                    )}
                  </LayoutGroup>
                </section>

                {/* History — only show when there are entries */}
                {historyAgents.length > 0 ? (
                  <section className="rounded-2xl border border-primary-300/70 bg-primary-200/35 p-2">
                    <Collapsible
                      open={historyOpen}
                      onOpenChange={setHistoryOpen}
                    >
                      <div className="flex items-center justify-between">
                        <CollapsibleTrigger className="h-7 px-0 text-xs font-medium hover:bg-transparent">
                          <HugeiconsIcon
                            icon={
                              historyOpen ? ArrowDown01Icon : ArrowRight01Icon
                            }
                            size={20}
                            strokeWidth={1.5}
                          />
                          History
                        </CollapsibleTrigger>
                        <span className="rounded-full bg-primary-300/70 px-2 py-0.5 text-[11px] text-primary-800 tabular-nums">
                          {historyAgents.length}
                        </span>
                      </div>
                      <CollapsiblePanel contentClassName="pt-1">
                        <div className="flex flex-wrap gap-1.5">
                          {historyAgents
                            .slice(0, 10)
                            .map(function renderHistoryPill(item) {
                              return (
                                <button
                                  key={item.id}
                                  type="button"
                                  className={cn(
                                    'inline-flex max-w-full items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11px] tabular-nums',
                                    getHistoryPillClassName(item.status),
                                  )}
                                  onClick={function handleHistoryView() {
                                    setOpen(false)
                                    navigate({ to: '/agent-swarm' })
                                  }}
                                >
                                  <HugeiconsIcon
                                    icon={Link01Icon}
                                    size={20}
                                    strokeWidth={1.5}
                                  />
                                  <span className="truncate">{item.name}</span>
                                  <span className="opacity-80">
                                    {formatCost(item.cost)}
                                  </span>
                                </button>
                              )
                            })}
                        </div>
                      </CollapsiblePanel>
                    </Collapsible>
                  </section>
                ) : null}

                <section className="rounded-2xl border border-primary-300/70 bg-primary-200/35 p-2">
                  <Collapsible
                    open={cliAgentsExpanded}
                    onOpenChange={setCliAgentsExpanded}
                  >
                    <div className="flex items-center justify-between">
                      <CollapsibleTrigger className="h-7 px-0 text-xs font-medium hover:bg-transparent">
                        <HugeiconsIcon
                          icon={
                            cliAgentsExpanded
                              ? ArrowDown01Icon
                              : ArrowRight01Icon
                          }
                          size={20}
                          strokeWidth={1.5}
                        />
                        ⚡ CLI Agents
                      </CollapsibleTrigger>
                      <span className="rounded-full bg-primary-300/70 px-2 py-0.5 text-[11px] text-primary-800 tabular-nums">
                        {cliAgents.length}
                      </span>
                    </div>
                    <CollapsiblePanel contentClassName="pt-1">
                      <div className="space-y-0.5">
                        {cliAgentsQuery.isLoading ? (
                          <p className="px-2 py-1 text-[11px] text-primary-500 tabular-nums">
                            Scanning...
                          </p>
                        ) : null}
                        {!cliAgentsQuery.isLoading && !cliAgents.length ? (
                          <p className="px-2 py-1 text-[11px] text-primary-500">
                            No agents running
                          </p>
                        ) : null}
                        {cliAgents.map(function renderCliAgent(agent) {
                          const progressPct =
                            agent.status === 'finished'
                              ? 100
                              : Math.min(
                                  95,
                                  Math.round(
                                    (agent.runtimeSeconds / 600) * 100,
                                  ),
                                )
                          return (
                            <div
                              key={agent.pid}
                              className="rounded-lg px-2 py-1.5 hover:bg-primary-200/50"
                            >
                              <div className="flex items-center gap-1.5">
                                <span
                                  className={cn(
                                    'size-1.5 shrink-0 rounded-full',
                                    agent.status === 'running'
                                      ? 'bg-emerald-500'
                                      : 'bg-gray-400',
                                  )}
                                />
                                <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-primary-800">
                                  {agent.name}
                                </span>
                                <span className="shrink-0 text-[10px] text-primary-500 tabular-nums">
                                  {formatRuntimeLabel(agent.runtimeSeconds)}
                                </span>
                              </div>
                              {agent.task ? (
                                <p className="mt-0.5 truncate pl-3 text-[10px] text-primary-500">
                                  {summarizeTask(agent.task)}
                                </p>
                              ) : null}
                              <div className="mt-1 ml-3 h-1 overflow-hidden rounded-full bg-primary-200">
                                <div
                                  className={cn(
                                    'h-full rounded-full transition-all duration-500',
                                    agent.status === 'finished'
                                      ? 'bg-primary-400'
                                      : 'bg-emerald-400',
                                  )}
                                  style={{ width: `${progressPct}%` }}
                                />
                              </div>
                            </div>
                          )
                        })}
                      </div>
                    </CollapsiblePanel>
                  </Collapsible>
                </section>

                <section className="rounded-2xl border border-primary-300/70 bg-primary-200/35 p-2">
                  <Collapsible
                    open={browserPreviewExpanded}
                    onOpenChange={setBrowserPreviewExpanded}
                  >
                    <div className="flex items-center justify-between">
                      <CollapsibleTrigger className="h-7 px-0 text-xs font-medium hover:bg-transparent">
                        <HugeiconsIcon
                          icon={
                            browserPreviewExpanded
                              ? ArrowDown01Icon
                              : ArrowRight01Icon
                          }
                          size={20}
                          strokeWidth={1.5}
                        />
                        🌐 Browser
                      </CollapsibleTrigger>
                    </div>
                    <CollapsiblePanel contentClassName="pt-1">
                      <BrowserSidebarPreview />
                    </CollapsiblePanel>
                  </Collapsible>
                </section>
              </div>
            </ScrollAreaViewport>
            <ScrollAreaScrollbar>
              <ScrollAreaThumb />
            </ScrollAreaScrollbar>
            <ScrollAreaCorner />
          </ScrollAreaRoot>
        </motion.aside>
      ) : (
        /* Mobile: slide-up sheet */
        <AnimatePresence>
          {isOpen ? (
            <>
              <motion.div
                key="agent-sheet-backdrop"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.2 }}
                className="fixed inset-0 z-[80] bg-black/40 backdrop-blur-sm"
                onClick={() => setOpen(false)}
              />
              <motion.div
                key="agent-sheet"
                initial={{ y: '100%' }}
                animate={{ y: 0 }}
                exit={{ y: '100%' }}
                transition={{ type: 'spring', damping: 28, stiffness: 300 }}
                className="fixed inset-x-0 bottom-0 z-[81] max-h-[85vh] overflow-y-auto rounded-t-2xl border-t border-primary-300/70 bg-primary-100/95 backdrop-blur-xl"
              >
                {/* Drag handle */}
                <div className="sticky top-0 z-10 flex justify-center bg-primary-100/95 pt-2 pb-1 backdrop-blur-xl">
                  <div className="h-1 w-10 rounded-full bg-primary-400/50" />
                </div>
                {/* Header */}
                <div className="flex items-center justify-between border-b border-primary-300/70 px-4 pb-2">
                  <div className="flex items-center gap-1.5">
                    <span
                      className={cn(
                        'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium tabular-nums',
                        activeCount > 0
                          ? 'border-emerald-400/40 bg-emerald-500/10 text-emerald-700'
                          : 'border-primary-300/70 bg-primary-200/50 text-primary-700',
                      )}
                    >
                      <span
                        className={cn(
                          'size-1.5 rounded-full',
                          activeCount > 0 ? 'bg-emerald-400 animate-pulse' : 'bg-primary-400/50',
                        )}
                      />
                      {activeCount}
                    </span>
                  </div>
                  <h2 className="text-sm font-semibold text-primary-900">Agent Hub</h2>
                  <button
                    type="button"
                    onClick={() => setOpen(false)}
                    className="rounded-lg p-1.5 text-primary-500 hover:bg-primary-200"
                    aria-label="Close"
                  >
                    <svg width="18" height="18" viewBox="0 0 16 16" fill="none">
                      <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
                    </svg>
                  </button>
                </div>
                {/* Content — same as desktop sidebar */}
                <div className="space-y-3 p-3">
                  <OrchestratorCard compact={viewMode === 'compact'} />
                  <section className="rounded-2xl border border-primary-300/70 bg-primary-200/35 p-1">
                    <div className="mb-1 flex justify-center">
                      <span className="rounded-full border border-primary-300/70 bg-primary-100/80 px-3 py-0.5 text-[10px] font-medium text-primary-600 shadow-sm">
                        Swarm
                      </span>
                    </div>
                    <div className="mb-1 flex items-center justify-between px-1">
                      <p className="text-[10px] text-primary-600 tabular-nums">
                        {isLoading
                          ? 'syncing...'
                          : activeNodes.length === 0 && queuedNodes.length === 0
                            ? 'No subagents'
                            : `${activeNodes.length} active · ${queuedNodes.length} queued`}
                      </p>
                    </div>
                    {activeNodes.length > 0 ? (
                      <div className="space-y-1.5 p-1">
                        {activeNodes.map((node) => (
                          <div key={node.id} className="rounded-xl border border-primary-300/70 bg-primary-100 p-2">
                            <div className="flex items-center justify-between">
                              <span className="text-xs font-medium text-primary-900 truncate">{node.name}</span>
                              <span className="text-[10px] text-primary-500 tabular-nums">{node.statusBubble.text}</span>
                            </div>
                            <p className="mt-0.5 text-[10px] text-primary-600 line-clamp-2">{node.task}</p>
                            <div className="mt-1 flex items-center justify-between">
                              <span className="text-[10px] text-primary-500 tabular-nums">{formatRuntimeLabel(node.runtimeSeconds)}</span>
                              <button
                                type="button"
                                onClick={() => killAgent(node.id)}
                                className="text-[10px] text-red-500 hover:text-red-700 font-medium"
                              >
                                Kill
                              </button>
                            </div>
                          </div>
                        ))}
                      </div>
                    ) : null}
                  </section>
                  {historyAgents.length > 0 ? (
                    <section className="rounded-2xl border border-primary-300/70 bg-primary-200/35 p-2">
                      <button
                        type="button"
                        onClick={() => setHistoryOpen(!historyOpen)}
                        className="flex w-full items-center justify-between text-[11px] font-medium text-primary-700"
                      >
                        <span>History ({historyAgents.length})</span>
                        <span>{historyOpen ? '▾' : '▸'}</span>
                      </button>
                      {historyOpen ? (
                        <div className="mt-1.5 space-y-1">
                          {historyAgents.map((agent) => (
                            <div key={agent.id} className="flex items-center justify-between rounded-lg bg-primary-100/60 px-2 py-1.5">
                              <div className="min-w-0">
                                <span className="text-[11px] font-medium text-primary-800 truncate block">{agent.name}</span>
                                <span className="text-[10px] text-primary-500">{agent.status}</span>
                              </div>
                              <button
                                type="button"
                                onClick={() => setSelectedAgentChat({ sessionKey: agent.id, agentName: agent.name, statusLabel: agent.status })}
                                className="text-[10px] text-accent-600 hover:text-accent-800 font-medium"
                              >
                                View
                              </button>
                            </div>
                          ))}
                        </div>
                      ) : null}
                    </section>
                  ) : null}
                </div>
              </motion.div>
            </>
          ) : null}
        </AnimatePresence>
      )}

      <AnimatePresence>
        {showFloatingToggle ? (
          <motion.button
            type="button"
            initial={{ opacity: 0, y: 16 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: 10 }}
            transition={{ duration: 0.2, ease: 'easeOut' }}
            onClick={function handleOpenPanel() {
              setOpen(true)
            }}
            className="fixed right-4 bottom-4 z-30 inline-flex size-12 items-center justify-center rounded-full bg-linear-to-br from-accent-500 to-accent-600 text-primary-50 shadow-lg"
            aria-label="Open Agent View"
          >
            <motion.span
              animate={
                activeCount > 0
                  ? {
                      scale: [1, 1.05, 1],
                      opacity: [0.95, 1, 0.95],
                    }
                  : { scale: 1, opacity: 1 }
              }
              transition={{
                duration: 1.5,
                repeat: Infinity,
                ease: 'easeInOut',
              }}
              className="inline-flex"
            >
              <HugeiconsIcon icon={BotIcon} size={20} strokeWidth={1.5} />
            </motion.span>
            <span className="absolute -top-1 -right-1 inline-flex size-5 items-center justify-center rounded-full bg-primary-950 text-[11px] font-medium text-primary-50 tabular-nums">
              {activeCount}
            </span>
          </motion.button>
        ) : null}
      </AnimatePresence>

      <AgentChatModal
        open={selectedAgentChat !== null}
        sessionKey={selectedAgentChat?.sessionKey ?? ''}
        agentName={selectedAgentChat?.agentName ?? 'Agent'}
        statusLabel={selectedAgentChat?.statusLabel ?? 'running'}
        onOpenChange={function handleAgentChatOpenChange(nextOpen) {
          if (!nextOpen) {
            setSelectedAgentChat(null)
          }
        }}
      />
    </>
  )
}
