# Mobile V7 — Remove Hamburger, Add Dashboard Overflow + Chat Sessions Panel

## Overview
Remove the global hamburger/drawer on mobile entirely. Bottom tab bar is the only primary nav.
Dashboard gets a contextual overflow panel for system/advanced tools.
Chat gets a lightweight sessions panel (replaces sidebar toggle).

## 1. Remove Hamburger on Mobile

### Files to modify:

**`src/screens/dashboard/dashboard-screen.tsx`**
- Remove the `Menu01Icon` import (if no longer used)
- Remove the `setSidebarCollapsed` usage
- Remove the hamburger button block (the `isMobile &&` button with `Menu01Icon`)
- Replace with overflow icon (see section 3)

**`src/routes/agent-swarm.tsx`**
- Remove `Menu01Icon` import
- Remove `setSidebarCollapsed` from store
- Remove the hamburger button (the `isMobile &&` block with `Menu01Icon`)
- Just show the title directly without hamburger

**`src/screens/skills/skills-screen.tsx`**
- Remove `Menu01Icon` import
- Remove `setSidebarCollapsed` from store
- Remove the hamburger button block
- Show title directly

**`src/screens/chat/components/chat-header.tsx`**
- Remove `Menu01Icon` import
- Remove the `showSidebarButton` / `onOpenSidebar` props entirely
- Replace with: Left side shows compact "ClawSuite" text, Right side shows a sessions icon button
- Add `onOpenSessions?: () => void` prop
- Import `Chat01Icon` or `ListViewIcon` for sessions button

**`src/screens/chat/chat-screen.tsx`**
- Remove `handleOpenSidebar` callback
- Remove `showSidebarButton={isMobile}` and `onOpenSidebar={handleOpenSidebar}` props from ChatHeader
- Add state `const [sessionsOpen, setSessionsOpen] = useState(false)`
- Pass `onOpenSessions={() => setSessionsOpen(true)}` to ChatHeader
- Render `<MobileSessionsPanel>` when sessionsOpen is true (see section 4)

**`src/components/workspace-shell.tsx`**
- Remove the mobile sidebar edge-swipe gesture (the useEffect with EDGE_ZONE_PX)
- Remove the mobile backdrop overlay (`isMobile && !sidebarCollapsed` div)
- On mobile, never render `<ChatSidebar>` at all — wrap it in `{!isMobile && <ChatSidebar ... />}`
- Keep desktop sidebar behavior completely unchanged

## 2. Tab Bar Refinements

**`src/components/mobile-tab-bar.tsx`**
- Add `gap-1` to the grid for slightly more spacing between icons
- Chat pill: change `-translate-y-0.5` to `-translate-y-1` (4px more bleed)
- Reduce glow: change `ring-2 ring-accent-300/60 shadow-md` to `ring-1 ring-accent-200/40 shadow-sm`
- Background stays light (current `bg-white/80` is good)

## 3. Dashboard Overflow Panel

**New file: `src/components/dashboard-overflow-panel.tsx`**

Create a compact bottom sheet / floating panel component:

```tsx
import { useState, useEffect, useRef } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  File01Icon,
  BrainIcon,
  Task01Icon,
  ComputerTerminal01Icon,
  GlobeIcon,
  Clock01Icon,
  ListViewIcon,
  ServerStack01Icon,
  ApiIcon,
} from '@hugeicons/core-free-icons'
import { cn } from '@/lib/utils'

type OverflowItem = {
  icon: typeof File01Icon
  label: string
  to: string
}

const SYSTEM_ITEMS: OverflowItem[] = [
  { icon: File01Icon, label: 'Files', to: '/files' },
  { icon: BrainIcon, label: 'Memory', to: '/memory' },
  { icon: Task01Icon, label: 'Tasks', to: '/tasks' },
  { icon: ComputerTerminal01Icon, label: 'Terminal', to: '/terminal' },
  { icon: GlobeIcon, label: 'Browser', to: '/browser' },
  { icon: Clock01Icon, label: 'Cron Jobs', to: '/cron' },
  { icon: ListViewIcon, label: 'Logs', to: '/logs' },
  { icon: ApiIcon, label: 'Debug', to: '/debug' },
]

const GATEWAY_ITEMS: OverflowItem[] = [
  { icon: ServerStack01Icon, label: 'Channels', to: '/channels' },
]

type Props = {
  open: boolean
  onClose: () => void
}

export function DashboardOverflowPanel({ open, onClose }: Props) {
  // Renders a compact bottom sheet with system/gateway tools
  // Tap outside closes (onClose)
  // Fully unmounts when closed (don't render if !open)
  // Uses portal? No — render inline, position fixed
  // Animate: slide up from bottom, 200ms
  // Items: 2-column grid of icon+label buttons
  // Each item navigates to its route and calls onClose
}
```

**`src/screens/dashboard/dashboard-screen.tsx`** changes:
- Import a "more" icon: `MoreHorizontalIcon` or use `Settings01Icon` (already imported)
- Actually use `Menu11Icon` from hugeicons or a simple `⋯` text
- Replace the hamburger button with overflow trigger:
```tsx
{isMobile && (
  <button onClick={() => setOverflowOpen(true)} ...>
    <HugeiconsIcon icon={MoreHorizontalCircle01Icon} size={18} />
  </button>
)}
```
- Add state: `const [overflowOpen, setOverflowOpen] = useState(false)`
- Render `<DashboardOverflowPanel open={overflowOpen} onClose={() => setOverflowOpen(false)} />`

## 4. Chat Mobile Sessions Panel

**New file: `src/components/mobile-sessions-panel.tsx`**

A lightweight slide-in panel for session switching:
- Slides in from the right side
- Width: ~80% of screen
- Shows session list (reuse `SidebarSessions` or simplified version)
- "New Chat" button at top
- Tap outside closes
- Does NOT interfere with swipe gestures (add `.no-swipe` class)
- Unmounts when closed

```tsx
type Props = {
  open: boolean
  onClose: () => void
  sessions: SessionMeta[]
  activeFriendlyId: string
  onSelectSession: (key: string) => void
  onNewChat: () => void
}
```

## 5. Gesture + Swipe Preservation
- No changes to `use-swipe-navigation.ts` — V6 gestures are solid
- The new panels use `.no-swipe` class to prevent gesture conflicts
- Sessions panel and overflow panel both have backdrop that closes on tap

## 6. Summary of all files to modify:
1. `src/components/workspace-shell.tsx` — remove mobile sidebar/backdrop/edge-swipe
2. `src/components/mobile-tab-bar.tsx` — spacing + pill refinements
3. `src/screens/dashboard/dashboard-screen.tsx` — hamburger → overflow trigger
4. `src/routes/agent-swarm.tsx` — remove hamburger
5. `src/screens/skills/skills-screen.tsx` — remove hamburger
6. `src/screens/chat/components/chat-header.tsx` — hamburger → sessions button
7. `src/screens/chat/chat-screen.tsx` — wire sessions panel
8. NEW `src/components/dashboard-overflow-panel.tsx` — system tools panel
9. NEW `src/components/mobile-sessions-panel.tsx` — chat session switcher
