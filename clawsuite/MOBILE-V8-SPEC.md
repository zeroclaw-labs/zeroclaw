# Mobile V8 — Polish + Stability Pass

## 0A. Tab Bar Disappearing Fix

**Root cause analysis:** The MobileTabBar uses `fixed` positioning inside a container that may have `transform` (from `page-transition` animation class on route changes). CSS spec: `transform` on a parent creates a new containing block, which breaks `position: fixed` — the element becomes positioned relative to that transformed parent instead of the viewport.

**Fix in `src/components/workspace-shell.tsx`:**
- Move `<MobileTabBar />` OUTSIDE the grid container div. Currently it's already outside — good.
- BUT: the page-transition div wrapping `<Outlet />` has `key={pathname}` which re-mounts and animates. Check if any parent has `transform`.
- The `page-transition` CSS class uses `animation: page-fade-in` with opacity only (no transform) — that's fine.
- The real issue: `backdrop-blur-2xl backdrop-saturate-150` on the tab bar itself can sometimes cause compositing issues on iOS Safari.

**Fix in `src/components/mobile-tab-bar.tsx`:**
- Remove `backdrop-blur-2xl backdrop-saturate-150` — replace with solid-ish background: `bg-white/95 dark:bg-gray-900/95` (still slightly translucent but no blur that causes compositing bugs)
- Add `will-change-transform` to force GPU layer and prevent disappearing
- Change from `fixed` to use a different strategy: keep `fixed` but add `-webkit-transform: translateZ(0)` via `transform-gpu` class to create a stable compositing layer
- Make sure the nav is NOT inside any element with `overflow: hidden`

**Verify in workspace-shell.tsx:**
- The outer div has `h-dvh` — good
- The grid has `overflow-hidden` — MobileTabBar is OUTSIDE the grid, rendered after it — good
- Confirm: `{isMobile ? <MobileTabBar /> : null}` is a sibling of the grid div, not inside it — YES, confirmed

**Additional fix:** Remove `pointer-events-none` from the outer nav and `pointer-events-auto` from inner div. Instead, just use `pointer-events-auto` on the nav itself. The `pointer-events-none` wrapper pattern can cause iOS Safari rendering bugs where the element gets optimized away.

Revised MobileTabBar structure:
```tsx
<nav className="fixed inset-x-0 bottom-0 z-[60] md:hidden" style={{ paddingBottom: 'env(safe-area-inset-bottom)' }}>
  <div className="mx-2 mb-1 grid grid-cols-5 gap-1 rounded-2xl border border-primary-200/60 bg-white/95 dark:bg-gray-900/95 px-1 py-1.5 shadow-[0_2px_20px_rgba(0,0,0,0.08)] transform-gpu">
    ...tabs...
  </div>
</nav>
```

## 0B. Chat Composer / Tab Bar Overlap Fix

**Problem:** Tab bar overlaps composer hitbox. The `mobileKeyboardOpen` state hides the tab bar when keyboard is open, but there may be a gap — the tab bar is `z-[60]` and covers the bottom ~80px.

**Fix strategy:** When on chat route, the chat screen already manages its own bottom padding. But the composer needs to sit ABOVE the tab bar when tab bar is visible.

**Fix in chat-screen.tsx or chat-composer area:**
- The chat screen has `overflow-hidden` on mobile (from workspace-shell) — content fills the full height
- The composer sits at the bottom of the chat flex column
- When tab bar is visible (keyboard closed), composer needs `pb-24 md:pb-0` (or `mb-[5rem] md:mb-0`) to push above the tab bar
- When tab bar is hidden (keyboard open), no extra padding needed

**Fix approach:** In workspace-shell.tsx, the chat route already has NO `pb-24` (only non-chat routes get it). The chat screen manages its own layout. So the fix goes into the chat screen itself.

In `src/screens/chat/chat-screen.tsx`:
- Find the main flex container that holds header + messages + composer
- Add conditional bottom padding: when `!mobileKeyboardOpen && isMobile`, add `pb-20` (matches tab bar height)
- This ensures the composer is always above the tab bar

**Alternative simpler fix:** Add padding to the composer wrapper specifically:
- In the chat screen's main container, when on mobile and keyboard NOT open, apply `pb-20`

**Also fix in mobile-tab-bar.tsx:**
- The nav should have `pointer-events-auto` only on the actual bar, not on any invisible area
- Currently the outer nav is `pointer-events-none` with inner `pointer-events-auto` — this is actually correct for preventing tap interception on the safe-area padding zone
- But let's simplify: make the outer nav have `pointer-events-none` and each button inside have pointer-events-auto implicitly (they do, since pointer-events-auto on the grid div)

Wait — the REAL issue might be: the chat area has `overflow-hidden` and fills the grid cell. The tab bar is fixed at z-60 and overlaps the chat. The composer at the bottom of the chat is under the tab bar.

**Definitive fix:**
1. In `workspace-shell.tsx`: For chat routes on mobile, the `<main>` element should have `pb-20` when keyboard is NOT open (tab bar visible)
2. Actually, easier: just always give the main element `pb-20` on mobile for ALL routes (chat and non-chat), and remove the conditional

Current: `isMobile && !isOnChatRoute ? 'pb-24' : ''`
Change to: `isMobile ? 'pb-20' : ''`

But chat route has overflow-hidden... The pb won't help on the main element if it's overflow-hidden. The padding needs to be INSIDE the chat screen's scrollable area.

**Best fix:**
- Keep workspace-shell main as `pb-0` for chat routes (overflow-hidden)
- In chat-screen.tsx: add bottom padding to the outermost container that accounts for tab bar when visible
- Specifically: the chat screen renders `<main ref={mainRef}>` as a flex column. At the bottom is the composer. Below that, nothing — but the tab bar overlaps.
- Add a spacer div after the composer when `isMobile && !mobileKeyboardOpen`:
  ```tsx
  {isMobile && !mobileKeyboardOpen && <div className="shrink-0 h-20" />}
  ```
  Or better: wrap the whole chat layout and add padding-bottom.

Actually the simplest most reliable fix: in workspace-shell.tsx, change the main padding logic:
- ALL routes on mobile get `pb-20` 
- For chat routes: change from `overflow-hidden` to `overflow-hidden` but with the pb applied to the inner content

Let me look at this differently. The chat screen itself should handle its own bottom offset. The workspace shell shouldn't try to guess.

**Final approach for chat-screen.tsx:**
- Import `useWorkspaceStore` (already imported)
- Read `mobileKeyboardOpen` 
- The chat screen's outermost div already uses `h-full` — which fills the main area
- The main flex column inside has: header, messages (flex-1), composer
- Add `style={{ paddingBottom: isMobile && !mobileKeyboardOpen ? '5rem' : undefined }}` to the outermost container

## 1. Settings: Remove Hamburger

**`src/routes/settings/index.tsx`:**
- Remove `Menu01Icon` from imports
- Remove `useWorkspaceStore` import and `setSidebarCollapsed` usage
- Remove the mobile header hamburger button block (lines ~253-262)
- Keep the "Settings" h1 title

## 2. Skills Page Mobile Optimization

**`src/screens/skills/skills-screen.tsx`:**

In the SkillsGrid component, modify the card layout for mobile:
- Reduce `min-h-[220px]` to `min-h-0` on mobile (let content determine height)
- Make skill icon + name inline on mobile (horizontal layout)
- Clamp description to 1 line on mobile, 3 lines on desktop
- Make the "Installed" badge smaller: `text-[10px] px-1.5 py-0`
- Move Uninstall button into a `⋯` overflow or just keep Details
- Reduce padding from `p-4` to `p-3` on mobile

Card redesign for mobile:
```tsx
<motion.article className="flex flex-col rounded-2xl border border-primary-200 bg-primary-50/85 p-3 shadow-sm backdrop-blur-sm md:min-h-[220px] md:p-4">
  <div className="flex items-start gap-3">
    {/* Icon prominent */}
    <span className="text-2xl leading-none md:text-xl">{skill.icon}</span>
    <div className="min-w-0 flex-1">
      <div className="flex items-center justify-between gap-2">
        <h3 className="line-clamp-1 text-sm font-medium text-ink md:text-base">{skill.name}</h3>
        <span className="shrink-0 rounded-md border px-1.5 py-0 text-[10px] md:px-2 md:py-0.5 md:text-xs ...">{skill.installed ? 'Installed' : 'Available'}</span>
      </div>
      <p className="text-[11px] text-primary-500 md:text-xs">by {skill.author}</p>
    </div>
  </div>
  <p className="mt-1.5 line-clamp-1 text-xs text-primary-500 md:mt-2 md:line-clamp-3 md:min-h-[58px] md:text-sm">{skill.description}</p>
  {/* Tags row - hide on mobile to save space, or show 1-2 */}
  <div className="mt-1.5 hidden flex-wrap items-center gap-1.5 md:mt-2 md:flex">
    ...tags...
  </div>
  {/* Actions row - compact on mobile */}
  <div className="mt-2 flex items-center justify-between gap-2 md:mt-auto md:pt-3">
    <Button variant="outline" size="sm" className="h-8 text-xs md:h-9" onClick={() => onOpenDetails(skill)}>Details</Button>
    {/* Toggle + uninstall inline */}
    ...
  </div>
</motion.article>
```

## 3. Tab Bar Polish
Already addressed in 0A above.

## 4. Overlay Cleanup
**`src/components/dashboard-overflow-panel.tsx`** and **`src/components/mobile-sessions-panel.tsx`:**
- Already unmount when `!open` (return null) — confirmed good
- Verify backdrop onClick calls onClose — confirmed good
- No changes needed

## 5. Swipe Navigation
**`src/hooks/use-swipe-navigation.ts`:**
- Already has `mobileKeyboardOpen` check? No — need to add it
- When keyboard is open, disable swipe entirely
- Add: read `mobileKeyboardOpen` from workspace store. If true, don't process touch events.

Actually, the hook doesn't use the store. It uses refs. Let's keep it simple:
- In `onTouchStart`, check `useWorkspaceStore.getState().mobileKeyboardOpen` — if true, return early

## Summary of files to change:
1. `src/components/mobile-tab-bar.tsx` — fix disappearing (remove blur, add transform-gpu, simplify pointer-events)
2. `src/components/workspace-shell.tsx` — ensure mobile padding consistent
3. `src/screens/chat/chat-screen.tsx` — add bottom spacer for tab bar clearance
4. `src/routes/settings/index.tsx` — remove hamburger
5. `src/screens/skills/skills-screen.tsx` — compact mobile card layout
6. `src/hooks/use-swipe-navigation.ts` — disable when keyboard open
