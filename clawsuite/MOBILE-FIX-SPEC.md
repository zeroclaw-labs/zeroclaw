# Mobile Fix Spec — Chat Scrolling + Full Mobile UI Audit

## Problem
Chat screen on mobile is not scrollable and header disappears. Other screens (dashboard, settings, etc.) aren't optimized for mobile either.

## CRITICAL: Chat Screen Fix

The chat screen (`src/screens/chat/chat-screen.tsx`) has a broken layout on mobile. The issue:

1. The outer container uses `relative` for mobile layout (line ~1101: `isMobile ? 'relative' : 'grid ...'`) — this breaks the flex column flow needed for header + scrollable messages + composer.
2. The message list needs to scroll independently while header stays pinned at top and composer at bottom.

### Fix chat-screen.tsx

**Outer wrapper** (~line 1097): Change the mobile branch from `'relative'` to `'flex flex-col'`:
```tsx
compact
  ? 'flex flex-col w-full'
  : isMobile
    ? 'flex flex-col'  // <-- was 'relative'
    : 'grid grid-cols-[auto_1fr] grid-rows-[minmax(0,1fr)]',
```

**Main area** (~line 1119): The `<main>` already has `flex h-full flex-col overflow-hidden`. This is correct — it creates the flex column with header, message list, composer.

**ChatMessageList wrapper**: Make sure the message list container has `flex-1 min-h-0 overflow-y-auto` so it scrolls within the flex column. Check `chat-message-list.tsx` — its outer div should have `overflow-y-auto` and `flex-1 min-h-0`.

### Fix chat-composer.tsx

The composer bottom padding `pb-[calc(env(safe-area-inset-bottom)+5rem)]` on mobile is too much. The tab bar is ~4rem + safe area. Change to:
```
pb-[calc(env(safe-area-inset-bottom)+4.5rem)] md:pb-[calc(env(safe-area-inset-bottom)+0.75rem)]
```

### Fix stableContentStyle in chat-screen.tsx

The `mobileMessageInset` of `calc(env(safe-area-inset-bottom) + 5rem)` as paddingBottom on the message list may be fighting with the flex layout. On mobile, the composer is already in-flow (not fixed), so the message list doesn't need extra bottom padding for the composer. Change:
```tsx
const mobileMessageInset = isMobile
  ? '1rem'  // just a small buffer, composer is in-flow
  : null
```

## Dashboard Mobile Optimization

File: `src/screens/dashboard/dashboard-screen.tsx`

1. The dashboard header has too many items crammed on mobile. On mobile (< 768px):
   - Hide the `HeaderAmbientStatus` (clock) component
   - Stack the right-side controls more tightly
   - Reduce header padding: `px-3 py-2` on mobile

2. The `react-grid-layout` grid: On mobile, all widgets should stack vertically (single column). Check that `GRID_BREAKPOINTS` and `GRID_COLS` have a mobile breakpoint with 1 column.

3. Add `pb-24` to the dashboard main container on mobile for tab bar clearance (currently the workspace-shell adds `pb-20` but if dashboard has its own scroll container it may not inherit it).

## Settings Screen Mobile Optimization

File: `src/screens/settings/` — Make sure:
1. Settings pages use single-column layout on mobile
2. Form inputs are full-width on mobile
3. Provider cards stack vertically
4. Add bottom padding for tab bar

## All Other Screens

For each route screen, ensure:
1. Content has `pb-24 md:pb-0` for mobile tab bar clearance
2. Headers are compact on mobile (smaller text, tighter padding)
3. Tables convert to card/list layout on mobile or have horizontal scroll
4. No horizontal overflow

Screens to check:
- `src/screens/activity/activity-screen.tsx`
- `src/screens/tasks/tasks-screen.tsx`
- All route pages under `src/routes/` that render their own layouts (agents, channels, debug, files, logs, terminal, instances)

## DO NOT CHANGE
- The `mobile-tab-bar.tsx` — it's working fine
- The `workspace-shell.tsx` — the outer shell is correct
- The `__root.tsx` viewport meta — keep as-is
- The `chat-sidebar.tsx` spring transition — working
- The `styles.css` tap highlight fix — keep

## Test Criteria
After changes, on a 390px wide viewport:
1. Chat: header visible at top, messages scroll, composer at bottom above tab bar, keyboard opens without breaking layout
2. Dashboard: single column, all widgets visible, scrollable, tab bar not covering content
3. Settings: forms usable, nothing overflows
4. All screens: no horizontal scroll, tab bar clearance at bottom
