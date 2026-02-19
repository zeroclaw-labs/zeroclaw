# Mobile V6 — Gesture Refinement, Haptics, Dashboard Density

## 1. Rewrite useSwipeNavigation with Direction Locking

Replace `src/hooks/use-swipe-navigation.ts` entirely with a robust implementation:

```tsx
import { useCallback, useRef } from 'react'
import { useNavigate, useRouterState } from '@tanstack/react-router'

const TAB_ORDER = [
  '/dashboard',
  '/agent-swarm',
  '/chat/main',
  '/skills',
  '/settings',
] as const

const EDGE_ZONE = 24 // px from screen edge for chat-view edge-swipe
const LOCK_THRESHOLD = 12 // px before we lock direction
const SWIPE_MIN_X = 60
const SWIPE_MAX_Y = 25
const SWIPE_MAX_TIME = 500

type GestureState = {
  startX: number
  startY: number
  startTime: number
  locked: null | 'horizontal' | 'vertical'
  edgeSwipe: boolean // true if started within EDGE_ZONE of screen edge
}

function findCurrentTabIndex(pathname: string): number {
  if (pathname.startsWith('/dashboard')) return 0
  if (pathname.startsWith('/agent-swarm') || pathname.startsWith('/agents')) return 1
  if (pathname.startsWith('/chat') || pathname === '/new' || pathname === '/') return 2
  if (pathname.startsWith('/skills')) return 3
  if (pathname.startsWith('/settings')) return 4
  return -1
}

function isOnChatRoute(pathname: string): boolean {
  return pathname.startsWith('/chat') || pathname === '/new' || pathname === '/'
}

function shouldIgnoreTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false
  return Boolean(
    target.closest(
      'input, textarea, button, select, a, pre, code, [role="button"], [role="slider"], [contenteditable], .no-swipe'
    ),
  )
}

function triggerHaptic() {
  try {
    if (typeof navigator !== 'undefined' && 'vibrate' in navigator) {
      navigator.vibrate(10)
    }
  } catch {}
}

export function useSwipeNavigation() {
  const navigate = useNavigate()
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const gestureRef = useRef<GestureState | null>(null)

  const onTouchStart = useCallback(
    (e: React.TouchEvent<HTMLElement>) => {
      const touch = e.touches[0]
      if (!touch || shouldIgnoreTarget(e.target)) {
        gestureRef.current = null
        return
      }

      const screenWidth = window.innerWidth
      const isEdge = touch.clientX <= EDGE_ZONE || touch.clientX >= screenWidth - EDGE_ZONE

      gestureRef.current = {
        startX: touch.clientX,
        startY: touch.clientY,
        startTime: Date.now(),
        locked: null,
        edgeSwipe: isEdge,
      }
    },
    [],
  )

  const onTouchMove = useCallback(
    (e: React.TouchEvent<HTMLElement>) => {
      const gesture = gestureRef.current
      if (!gesture) return

      const touch = e.touches[0]
      if (!touch) return

      // Lock direction after threshold
      if (!gesture.locked) {
        const dx = Math.abs(touch.clientX - gesture.startX)
        const dy = Math.abs(touch.clientY - gesture.startY)
        if (dx >= LOCK_THRESHOLD || dy >= LOCK_THRESHOLD) {
          gesture.locked = dx > dy ? 'horizontal' : 'vertical'
        }
      }

      // If horizontal locked, prevent vertical scroll
      if (gesture.locked === 'horizontal') {
        e.preventDefault()
      }
    },
    [],
  )

  const onTouchEnd = useCallback(
    (e: React.TouchEvent<HTMLElement>) => {
      const gesture = gestureRef.current
      gestureRef.current = null
      if (!gesture) return

      const touch = e.changedTouches[0]
      if (!touch) return

      const dx = touch.clientX - gesture.startX
      const dy = touch.clientY - gesture.startY
      const dt = Date.now() - gesture.startTime

      // Must be horizontal-locked (or unlocked but meeting thresholds)
      if (gesture.locked === 'vertical') return
      if (Math.abs(dx) < SWIPE_MIN_X || Math.abs(dy) > SWIPE_MAX_Y || dt > SWIPE_MAX_TIME) return

      // On chat routes, only allow edge swipes to prevent accidental nav while reading
      if (isOnChatRoute(pathname) && !gesture.edgeSwipe) return

      const currentIndex = findCurrentTabIndex(pathname)
      if (currentIndex === -1) return

      const nextIndex = dx < 0
        ? Math.min(currentIndex + 1, TAB_ORDER.length - 1)
        : Math.max(currentIndex - 1, 0)

      if (nextIndex === currentIndex) return

      triggerHaptic()
      void navigate({ to: TAB_ORDER[nextIndex] })
    },
    [navigate, pathname],
  )

  return { onTouchStart, onTouchMove, onTouchEnd }
}
```

### Wire onTouchMove in workspace-shell.tsx:
Add `onTouchMove` alongside `onTouchStart` and `onTouchEnd` on the main content area.

## 2. Activity Ticker — Dismissible on Mobile

In `src/components/activity-ticker.tsx`:
- Add a dismiss button (small × on the right)
- Use localStorage to persist dismissal: `clawsuite-ticker-dismissed`
- On mobile, make it more compact: `h-8` instead of `h-9`, `text-[11px]` instead of `text-xs`
- If dismissed, return null

Add state:
```tsx
const [dismissed, setDismissed] = useState(() => {
  if (typeof window === 'undefined') return false
  return localStorage.getItem('clawsuite-ticker-dismissed') === 'true'
})
const handleDismiss = (e: React.MouseEvent) => {
  e.stopPropagation()
  setDismissed(true)
  localStorage.setItem('clawsuite-ticker-dismissed', 'true')
}
if (dismissed) return null
```

Add dismiss button:
```tsx
<button onClick={handleDismiss} className="ml-auto text-primary-400 hover:text-primary-600 p-1" aria-label="Dismiss">
  <span className="text-xs">✕</span>
</button>
```

## 3. Sidebar Drawer — Don't Duplicate Tab Destinations

In `src/screens/chat/components/chat-sidebar.tsx`:
- The mobile primary suite items currently show Dashboard, Agent Hub, Skills
- These are ALL in the bottom tab bar already — showing them in the drawer is redundant
- On mobile, REMOVE the primary suite section entirely
- Keep only: Sessions list (top), then collapsible "System" group, then collapsible "Gateway" group
- This makes the drawer focused: sessions + advanced tools only

Change the mobile rendering logic:
- Remove `mobilePrimarySuite` from rendering on mobile
- Remove the "Suite" section label on mobile
- Sessions should be first (already ordered with flex order-1)
- System group second
- Gateway group third

## 4. CSS touch-action

In `src/styles.css`, add:
```css
/* Preserve vertical scroll while we handle horizontal gestures */
@media (max-width: 767px) {
  main {
    touch-action: pan-y;
  }
}
```

## 5. Tab Switch Slide Animation

In `src/components/workspace-shell.tsx`, wrap the `<Outlet />` or main content with a subtle transition.

Actually, the simplest approach: use CSS transitions on the main content area.
Since TanStack Router doesn't have built-in page transitions, add a quick opacity fade:

In workspace-shell.tsx, around the Outlet, add:
```tsx
<div key={pathname} className="animate-in fade-in duration-150 h-full">
  <Outlet />
</div>
```

Or if animate-in isn't available, use inline:
```tsx
<motion.div
  key={pathname}
  initial={{ opacity: 0.7, x: direction * 20 }}
  animate={{ opacity: 1, x: 0 }}
  transition={{ duration: 0.15 }}
  className="h-full"
>
  <Outlet />
</motion.div>
```

Actually, skip motion for now — just add a simple CSS fade to avoid complexity. Use:
```css
@keyframes page-fade-in {
  from { opacity: 0.85; }
  to { opacity: 1; }
}
.page-transition {
  animation: page-fade-in 0.15s ease-out;
}
```

## DO NOT CHANGE:
- mobile-tab-bar.tsx (tab order is correct)
- chat-composer.tsx (input fixes are done)
- dashboard-screen.tsx layout (already optimized)
- hero-metrics-row.tsx (already compact)

## Commit message:
```
feat: mobile v6 - robust swipe gestures, haptic feedback, dashboard density, drawer cleanup

- Swipe: direction locking after 12px, edge-only on chat, CSS touch-action: pan-y
- Haptic: navigator.vibrate(10) on successful tab switch
- Activity ticker: dismissible with × button, persists to localStorage
- Sidebar drawer: removed redundant tab destinations on mobile, sessions-first
- Page transitions: subtle fade-in on tab switch
- QA: vertical scroll preserved, chat input unaffected, safe-area correct
```
