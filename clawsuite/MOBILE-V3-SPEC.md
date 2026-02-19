# Mobile V3 — Fix Input Focus Bug + Immersive Chat + Tab Bar Contrast + Drawer Cleanup

## CRITICAL BUG: Chat input not focusable on mobile Safari

### Root Cause Analysis (do this FIRST before any UI changes)

The most likely cause is the mobile sidebar backdrop in `workspace-shell.tsx`:

```tsx
{isMobile && !sidebarCollapsed && (
  <div
    className="fixed inset-0 z-40 bg-black/50 md:hidden"
    onClick={() => setSidebarCollapsed(true)}
  />
)}
```

This backdrop is rendered inside the grid container. When the sidebar IS collapsed, this element is not rendered (good). BUT — check if the `ChatSidebar` component itself (which uses `motion.aside` with `width: 0` when collapsed on mobile) still has `pointer-events` or an invisible overlay.

Also check: The `motion.aside` in `chat-sidebar.tsx` animates to `width: 0` on mobile when collapsed. Even at width 0, if it has `overflow: visible` or a child with absolute/fixed positioning, it could intercept taps.

### Fix for sidebar (chat-sidebar.tsx)

When collapsed on mobile, the sidebar aside must have `pointer-events: none` AND `overflow: hidden`:

In `chat-sidebar.tsx`, around line 882-898 where the `motion.aside` is rendered:
- Add to className: `isMobile && isCollapsed ? 'pointer-events-none overflow-hidden' : ''`
- OR set `aria-hidden={true}` AND `inert` attribute when collapsed on mobile

### Fix for workspace-shell backdrop

The current backdrop code is correct (only renders when `!sidebarCollapsed`). But add explicit safety:
- When sidebar IS collapsed, ensure no residual overlay exists
- Add `pointer-events-none` to the sidebar container when collapsed on mobile

### Fix for tab bar intercepting taps

The tab bar is `fixed inset-x-0 bottom-0 z-[60]`. The composer is `z-30`. This means the tab bar is ABOVE the composer in stacking context, which could block taps on the lower part of the composer.

**Fix:** When on a chat route AND `mobileKeyboardOpen` is true, the tab bar already returns `null` (good). But the `z-[60]` on the tab bar vs `z-30` on the composer means the tab bar overlaps the bottom of the composer area even when not focused.

Change composer z-index to `z-40` (above the tab bar won't matter since tab bar hides on focus, but this prevents any edge cases).

Actually, the real fix: the composer's bottom padding already accounts for the tab bar height. So the actual input textarea should be above the tab bar visually. The issue is that the tab bar's `fixed` positioning + `z-[60]` creates a layer that captures taps in the area where the composer's padding is (below the actual input but inside the composer div).

**Solution:** Add `pointer-events-none` to the tab bar's outer padding area, and `pointer-events-auto` only to the inner bar div. This way taps in the safe-area padding pass through to content below.

In `mobile-tab-bar.tsx`:
```tsx
<nav className="fixed inset-x-0 bottom-0 z-[60] pointer-events-none pb-[env(safe-area-inset-bottom)] md:hidden">
  <div className="pointer-events-auto mx-2 mb-1 grid grid-cols-5 ...">
    ...tabs...
  </div>
</nav>
```

## Tab Bar Contrast

Current: `bg-white/70 backdrop-blur-2xl backdrop-saturate-[1.8]`

Change to: `bg-gray-900/80 backdrop-blur-2xl backdrop-saturate-150` with light text.

This gives a dark glass effect that clearly separates from the white content:
- Tab labels: `text-gray-400` (inactive), `text-white` (active)
- Icon circles: inactive = `text-gray-400`, active (non-chat) = `bg-white/20 text-white`
- Chat center pill: `bg-accent-500 text-white` (stays the same)
- Border: `border-white/10`

## Immersive Chat Mode (already partially implemented)

The current implementation uses `mobileKeyboardOpen` state set by composer `onFocus`/`onBlur`. This is correct. Verify:

1. `onFocus` on PromptInputTextarea sets `mobileKeyboardOpen = true` → tab bar hides
2. `onBlur` sets `mobileKeyboardOpen = false` → tab bar returns
3. Composer padding switches from `MOBILE_TAB_BAR_OFFSET` (5rem) to `0.5rem` when keyboard open

This should work. If there's a timing issue (blur fires before keyboard fully closes), add a small delay:
```tsx
onBlur={() => setTimeout(() => setMobileKeyboardOpen(false), 100)}
```

## Sidebar Drawer Cleanup for Mobile

In `chat-sidebar.tsx`, the sidebar currently shows all items (Terminal, Cron, Debug, Logs, etc.) on mobile. For mobile, filter the navigation items:

### Mobile sidebar should show:
- Search
- New Session button
- Sessions list
- Dashboard (link)
- Agent Hub (link)
- Settings (link)

### Mobile sidebar should HIDE:
- Terminal
- Browser
- Tasks (accessible via Activity tab)
- Skills
- Cron Jobs
- Logs (accessible via Activity tab)
- Debug
- Files
- Memory
- ALL gateway items (Channels, Instances, Sessions, Usage, Agents, Nodes)

These items are accessible through the tab bar's Activity and Settings sections.

To implement: wrap the `suiteItems` and `gatewayItems` NavGroup sections with a mobile check. On mobile, only show a simplified nav:

```tsx
// Filter items for mobile
const mobileSuiteItems = isMobile
  ? suiteItems.filter(i => ['Dashboard', 'Agent Hub'].includes(i.label))
  : suiteItems

const mobileGatewayItems = isMobile ? [] : gatewayItems
```

Then use `mobileSuiteItems` and `mobileGatewayItems` in the render.

## Hamburger Button Consistency

Currently the hamburger (sidebar toggle) only shows on the Chat screen via `ChatHeader` with `showSidebarButton={isMobile}`.

For consistency, add a hamburger to the `DashboardScreen` header too. In `dashboard-screen.tsx`, add a sidebar toggle button in the header's left section (before the logo), visible only on mobile.

Import needed:
```tsx
import { useWorkspaceStore } from '@/stores/workspace-store'
import { HugeiconsIcon } from '@hugeicons/react'
import { Menu01Icon } from '@hugeicons/core-free-icons'
```

Add before the OpenClawStudioIcon:
```tsx
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

For other tab screens (Activity, Settings, Agents), they should also have a way to open the sidebar. BUT since those screens have their own headers already, and the sidebar is primarily for session switching, it's fine to only have the hamburger on Chat + Dashboard.

## Files to modify:
1. `src/components/mobile-tab-bar.tsx` — pointer-events fix, dark glass theme
2. `src/components/workspace-shell.tsx` — sidebar pointer-events safety
3. `src/screens/chat/components/chat-sidebar.tsx` — pointer-events when collapsed, filter nav items for mobile
4. `src/screens/chat/components/chat-composer.tsx` — z-index bump, blur delay
5. `src/screens/dashboard/dashboard-screen.tsx` — hamburger button on mobile

## DO NOT CHANGE:
- `__root.tsx` — viewport meta is correct
- `styles.css` — tap highlight fix is correct
- `chat-message-list.tsx` — scroll behavior is correct
- `stores/workspace-store.ts` — mobileKeyboardOpen state is correct
- Tab order (Dashboard | Agents | Chat | Activity | Settings) — keep this
- Chat center pill treatment — keep the raised design

## Commit message:
`"fix: mobile input focus bug, dark glass tab bar, immersive chat polish, drawer cleanup"`
