# Mobile V5 — Comprehensive Mobile Polish

## 1. Header Consistency — Hamburger on Every Screen

Currently only Dashboard has the hamburger menu. Every top-level screen needs it on mobile.

### Files to modify:
- `src/routes/agent-swarm.tsx` — Add hamburger button to header (left side)
- `src/routes/skills.tsx` (or wherever Skills screen lives) — Add hamburger
- `src/screens/settings/index.tsx` (or settings route) — Add hamburger
- `src/screens/activity/activity-screen.tsx` — Add hamburger if it has a header

### Pattern (copy from dashboard-screen.tsx):
```tsx
import { useWorkspaceStore } from '@/stores/workspace-store'
const setSidebarCollapsed = useWorkspaceStore((s) => s.setSidebarCollapsed)
// In header, left side:
{isMobile && (
  <button
    type="button"
    onClick={() => setSidebarCollapsed(false)}
    className="flex size-9 items-center justify-center rounded-lg text-primary-600 active:scale-95"
    aria-label="Open menu"
  >
    <HugeiconsIcon icon={Menu01Icon} size={20} strokeWidth={1.5} />
  </button>
)}
```

For screens that don't have isMobile detection yet, add:
```tsx
const [isMobile, setIsMobile] = useState(false)
useEffect(() => {
  const media = window.matchMedia('(max-width: 767px)')
  const update = () => setIsMobile(media.matches)
  update()
  media.addEventListener('change', update)
  return () => media.removeEventListener('change', update)
}, [])
```

## 2. Dashboard Skills Widget — Clamp to 3 on Mobile

In `src/screens/dashboard/components/skills-widget.tsx`:

Change the skills slice to be responsive:
```tsx
const isMobile = typeof window !== 'undefined' && window.innerWidth < 768
const skills = useMemo(() => {
  const source = Array.isArray(skillsQuery.data) ? skillsQuery.data : []
  return source.slice(0, isMobile ? 3 : 6)
}, [skillsQuery.data])
```

Actually, better approach — use a state + matchMedia like other components for reactivity.

## 3. Agent Hub — Tighter Mobile Layout

In `src/routes/agent-swarm.tsx`:
- The office canvas is already at h-[350px] on mobile — reduce to h-[250px]
- Stats (Active/Done/Tokens/Cost) from ActivityPanel should appear ABOVE the office on mobile
- Tighten header spacing

## 4. Settings — Mobile Horizontal Scroll Tabs

Find the settings screen and convert the tab navigation to a horizontal scrollable row on mobile instead of wrapping tabs.

Look for the settings route/screen files. The tabs should use:
```tsx
<div className="flex overflow-x-auto gap-1 pb-1 md:flex-wrap scrollbar-none">
```

## 5. Swipe Gesture Navigation Between Tabs (NEW FEATURE)

Create `src/hooks/use-swipe-navigation.ts`:

```tsx
import { useRef, useCallback } from 'react'
import { useNavigate, useLocation } from '@tanstack/react-router'

const TAB_ORDER = ['/dashboard', '/agent-swarm', '/chat/main', '/skills', '/settings']

function findCurrentTabIndex(pathname: string): number {
  if (pathname.startsWith('/dashboard')) return 0
  if (pathname.startsWith('/agent-swarm') || pathname.startsWith('/agents')) return 1
  if (pathname.startsWith('/chat') || pathname === '/new' || pathname === '/') return 2
  if (pathname.startsWith('/skills')) return 3
  if (pathname.startsWith('/settings')) return 4
  return -1
}

export function useSwipeNavigation() {
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const touchRef = useRef<{ startX: number; startY: number; startTime: number } | null>(null)

  const onTouchStart = useCallback((e: React.TouchEvent) => {
    const touch = e.touches[0]
    if (!touch) return
    // Skip if touch starts on interactive elements
    const target = e.target as HTMLElement
    if (target.closest('input, textarea, select, button, [role="button"], [role="slider"], pre, code, .no-swipe')) return
    touchRef.current = { startX: touch.clientX, startY: touch.clientY, startTime: Date.now() }
  }, [])

  const onTouchEnd = useCallback((e: React.TouchEvent) => {
    if (!touchRef.current) return
    const touch = e.changedTouches[0]
    if (!touch) return

    const dx = touch.clientX - touchRef.current.startX
    const dy = touch.clientY - touchRef.current.startY
    const dt = Date.now() - touchRef.current.startTime
    touchRef.current = null

    // Thresholds: horizontal > 60px, vertical < 30px, time < 500ms
    if (Math.abs(dx) < 60 || Math.abs(dy) > 30 || dt > 500) return

    const currentIndex = findCurrentTabIndex(pathname)
    if (currentIndex === -1) return

    const nextIndex = dx < 0
      ? Math.min(currentIndex + 1, TAB_ORDER.length - 1)  // swipe left → next tab
      : Math.max(currentIndex - 1, 0)                       // swipe right → prev tab

    if (nextIndex !== currentIndex) {
      void navigate({ to: TAB_ORDER[nextIndex] })
    }
  }, [navigate, pathname])

  return { onTouchStart, onTouchEnd }
}
```

### Wire it up in workspace-shell.tsx:
In the main content area wrapper, add the touch handlers:
```tsx
const { onTouchStart, onTouchEnd } = useSwipeNavigation()
// On the main content div:
<main onTouchStart={onTouchStart} onTouchEnd={onTouchEnd} className="...">
```

Only apply on mobile (check isMobile before attaching handlers).

## 6. Bottom Padding Consistency

Every screen's scroll container needs `pb-24 md:pb-8` for tab bar clearance.

Check these files have it:
- dashboard-screen.tsx ✅ (already has pb-24)
- agent-swarm.tsx ✅ (already has pb-24)
- skills route
- settings route
- activity screen
- any other top-level screen

## 7. Chat Input Debug — elementFromPoint

In `chat-composer.tsx`, add a dev-only debug tap handler on mobile that logs what element is at the tap point. This helps verify no overlay is intercepting.

Actually, skip the debug code — instead just verify the existing fixes are solid:
- PromptInput has `relative z-50` ✓
- Composer wrapper has `onClick` → focus handler ✓
- Textarea has `min-h-[44px]` ✓
- Tab bar outer nav is `pointer-events-none` ✓
- Sidebar is `pointer-events-none + inert` when collapsed ✓

The main remaining risk is the sidebar backdrop. Verify in `workspace-shell.tsx` or `chat-sidebar.tsx` that when sidebar closes on mobile, backdrop is fully unmounted or has `pointer-events-none`.

## DO NOT CHANGE:
- Tab bar tab order (Dashboard | Agent Hub | Chat | Skills | Settings) — already correct
- Tab bar color scheme (light glass) — already correct  
- Chat message list layout
- Desktop layouts (only touch mobile breakpoints)

## Commit message:
```
feat: mobile v5 - swipe nav, header consistency, density optimization, settings mobile tabs

- Swipe left/right between tabs (60px threshold, ignores interactive elements)
- Hamburger menu on all screens (not just dashboard)
- Dashboard skills widget clamped to 3 on mobile
- Agent hub: tighter office canvas (250px), stats visible sooner
- Settings: horizontal scroll tabs on mobile
- Consistent pb-24 bottom padding across all screens
- Full mobile QA sweep
```
