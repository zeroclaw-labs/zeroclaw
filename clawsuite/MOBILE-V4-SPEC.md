# Mobile V4 — Final Polish + Bugfix Sweep

Read this entire spec before making changes. Implement ALL items.

## 0) FIX: Chat input hitbox/focus bug (CRITICAL — do this FIRST)

### Problem
On Chat tab in iOS Safari, tapping the composer input often doesn't focus it. Tapping slightly below/around it near the tab bar area sometimes works. This means an invisible element is intercepting taps above the input.

### Current state of the code
- Tab bar (`mobile-tab-bar.tsx`): outer `<nav>` has `pointer-events-none`, inner `<div>` has `pointer-events-auto` — this is CORRECT.
- Sidebar (`chat-sidebar.tsx`): collapsed mobile sidebar has `pointer-events-none overflow-hidden` + `inert` — this is CORRECT.
- Composer (`chat-composer.tsx`): z-index is `z-40` — this should be fine.

### Likely remaining culprit
The composer wrapper div itself has `pb-[calc(env(safe-area-inset-bottom)+var(--mobile-tab-bar-offset))]` which adds ~5rem of padding below the actual input. This padding area sits under the tab bar. But the TAB BAR at z-[60] could still intercept taps at the very bottom of the composer padding even though the outer nav is pointer-events-none.

Actually, the more likely issue: look at `chat-screen.tsx` for any `relative` or `overflow-hidden` containers that create stacking contexts. Also check if any `transform` on parent elements (like the sidebar's `motion.aside` which uses transform for animation) creates a new stacking context that traps the composer's z-index.

### Required debugging approach
Before making UI changes, add a TEMPORARY debug helper to identify what's blocking taps. In `workspace-shell.tsx`, add this useEffect (REMOVE after debugging):

```tsx
useEffect(() => {
  if (!isMobile) return
  function handler(e: PointerEvent) {
    const el = document.elementFromPoint(e.clientX, e.clientY)
    if (el) {
      const cs = getComputedStyle(el)
      console.log('[tap-debug]', {
        tag: el.tagName,
        id: el.id,
        className: (el.className || '').toString().slice(0, 100),
        zIndex: cs.zIndex,
        position: cs.position,
        pointerEvents: cs.pointerEvents,
        x: e.clientX,
        y: e.clientY,
      })
    }
  }
  document.addEventListener('pointerdown', handler, { capture: true })
  return () => document.removeEventListener('pointerdown', handler, { capture: true })
}, [isMobile])
```

Then THINK about what the output would be. Since we can't physically tap on the device, reason through the DOM structure:

The chat screen structure on mobile is:
```
<div class="h-dvh"> (workspace shell)
  <motion.aside width=0 pointer-events-none overflow-hidden inert> (sidebar)
  <div class="fixed inset-0 z-40"> (backdrop — ONLY when sidebar open)
  <main class="h-full overflow-hidden"> (content area)
    <div class="h-full flex flex-col"> (chat-screen outer)
      <div class="flex-1 flex flex-col"> (chat-screen inner)
        <main class="flex h-full flex-col overflow-hidden"> (chat main)
          <ChatHeader /> (shrink-0)
          <ChatMessageList /> (flex-1 overflow-y-auto)
          <ChatComposer z-40 /> (shrink-0)
        </main>
      </div>
    </div>
  </main>
  <nav class="fixed bottom-0 z-[60] pointer-events-none"> (tab bar)
    <div class="pointer-events-auto"> (tab bar inner)
  </nav>
</div>
```

The issue: The `<main>` in workspace-shell has `overflow-hidden` on chat routes. The composer is inside this `<main>`. The tab bar is OUTSIDE and ABOVE (z-[60] > z-40). Even though the nav has pointer-events-none, the inner div has pointer-events-auto. The tab bar's inner div's grid cells extend across the full width. If the composer's bottom padding overlaps with where the tab bar's pointer-events-auto div is, taps go to the tab bar buttons instead of the composer.

### THE FIX
The actual fix is to ensure the composer's bottom padding (the area between the actual input and the tab bar) does NOT have interactive content. The tab bar is already positioned correctly. The issue is the OVERLAP ZONE.

Two options:
1. **Reduce composer bottom padding** so the actual textarea/buttons sit entirely ABOVE the tab bar. Then the tab bar area is purely tab bar territory.
2. **Give the composer a higher z-index than the tab bar** AND ensure pointer-events work.

Go with option 1: The composer's `--mobile-tab-bar-offset` should position the CONTENT (textarea + buttons) above the tab bar, with the padding below just being dead space. This is already the intent. If taps still fail, the issue is likely the textarea itself being too small or the tap target area being insufficient.

**Specific fix**: In `chat-composer.tsx`, ensure the `PromptInput` component (which wraps the textarea) has `relative z-50` so it's above everything. Add to the PromptInput's className: `'relative z-50'`.

Also, look at whether the PromptInput component renders any elements that could intercept taps. Check `prompt-input.tsx` for pointer-events issues.

## 1) Tab bar styling: soft glass, NOT heavy dark

REVERT the dark theme. Go back to a LIGHT glass style, but slightly more visible than pure white:

```tsx
// Replace current:
// bg-gray-900/80 border-white/10 ... text-white ... text-gray-400

// With:
<div className="pointer-events-auto mx-2 mb-1 grid grid-cols-5 rounded-2xl border border-primary-200/60 bg-white/80 px-1 py-1.5 shadow-[0_2px_20px_rgba(0,0,0,0.08)] backdrop-blur-2xl backdrop-saturate-150">
```

Tab colors (light mode):
- Inactive icon: `text-primary-400`
- Inactive label: `text-primary-400`  
- Active icon (non-chat): `size-7 bg-accent-500/15 text-accent-600`
- Active label: `text-accent-600`
- Chat center pill: `size-9 bg-accent-500 text-white` (keep as-is)
- Chat center pill active: add `ring-2 ring-accent-300/60 shadow-md`

This gives a clean, iOS-native feel. NOT heavy dark. Subtle glass with warm accent highlights.

## 2) Dashboard header: declutter on mobile

On mobile only, simplify the top-right area of the dashboard header.

Current right side on mobile: ThemeToggle + NotificationsPopover + Settings button (all visible).

Change to on mobile:
- Only show: one settings gear button that opens the existing `SettingsDialog`
- Hide: ThemeToggle, NotificationsPopover (these go into the settings dialog or drawer)
- On desktop: keep everything as-is

In `dashboard-screen.tsx`, wrap the right controls:
```tsx
{/* Right controls */}
<div className="ml-auto flex items-center gap-2">
  {!isMobile && <HeaderAmbientStatus />}
  {!isMobile && <ThemeToggle />}
  {!isMobile && (
    <div className="flex items-center gap-1 rounded-full border border-primary-200 bg-primary-100/65 p-1">
      <NotificationsPopover />
      <SettingsButton />
    </div>
  )}
  {isMobile && (
    <button onClick={() => setDashSettingsOpen(true)} className="...settings gear styles...">
      <HugeiconsIcon icon={Settings01Icon} size={18} />
    </button>
  )}
</div>
```

## 3) Sidebar/drawer: restructured mobile IA

Undo the over-aggressive filter we applied. Instead, keep ALL items but organize them into collapsible groups on mobile:

Replace the current filter (`isMobile ? suiteItems.filter(...)`) with a grouping approach:

On mobile, the sidebar should show items in this order:
1. **Sessions section** (search + new session + session list) — already exists, keep as-is
2. **Suite section** with ALL items, but grouped:
   - Primary (always visible): Dashboard, Agent Hub, Skills
   - System (collapsed by default on mobile): Files, Memory, Tasks, Terminal, Browser, Cron Jobs, Logs, Debug
3. **Gateway section**: Keep showing on mobile BUT collapsed by default

Implementation: Instead of filtering items out, split `suiteItems` into two groups on mobile:
```tsx
const primarySuiteLabels = ['Dashboard', 'Agent Hub', 'Skills']
const mobilePrimarySuite = suiteItems.filter(i => primarySuiteLabels.includes(i.label))
const mobileSecondarySuite = suiteItems.filter(i => !primarySuiteLabels.includes(i.label))
```

Then render:
```tsx
{/* Primary suite items — always visible */}
<CollapsibleSection items={isMobile ? mobilePrimarySuite : suiteItems} ... />

{/* Secondary suite items — collapsed on mobile */}
{isMobile && mobileSecondarySuite.length > 0 && (
  <>
    <SectionLabel label="System" ... collapsed by default ... />
    <CollapsibleSection items={mobileSecondarySuite} ... />
  </>
)}

{/* Gateway — show on mobile too, but collapsed */}
<SectionLabel label="Gateway" ... />
<CollapsibleSection items={gatewayItems} ... />
```

Remove the `{!isMobile && (...)}` wrapper around Gateway section that we added in V3.

## 4) Layout correctness

Verify these are still correct (they should be from prior commits):
- `workspace-shell.tsx`: `pb-24` on non-chat mobile routes
- All screen components: `pb-24 md:pb-8` pattern
- Chat screen: flex column layout with `overflow-hidden` on main
- `h-dvh` on root container (NOT `h-screen` or `100vh`)

## 5) Remove debug helper

After implementing all changes, make sure to NOT include the pointerdown debug logger in the final code. It was only for reasoning.

## Files to modify:
1. `src/components/mobile-tab-bar.tsx` — light glass theme, pointer-events kept
2. `src/screens/chat/components/chat-composer.tsx` — add relative z-50 to PromptInput
3. `src/screens/dashboard/dashboard-screen.tsx` — declutter mobile header
4. `src/screens/chat/components/chat-sidebar.tsx` — restructure mobile nav groups

## DO NOT CHANGE:
- `__root.tsx` — viewport meta correct
- `styles.css` — tap highlight fix correct  
- `stores/workspace-store.ts` — mobileKeyboardOpen state correct
- `workspace-shell.tsx` — pointer-events + layout correct
- Desktop layout — all changes mobile-only via responsive conditions

## Commit message:
`"fix: mobile v4 - input focus fix, soft glass tab bar, dashboard declutter, drawer restructure"`
