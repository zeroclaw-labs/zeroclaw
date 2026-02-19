# Mobile V2 Spec — Tab Bar Redesign + Composer Fix + Full Sweep

## 1. Tab Bar Redesign (`src/components/mobile-tab-bar.tsx`)

### New Tab Order (exactly 5, no "More"):
```
Dashboard | Agents | **Chat** (center) | Activity | Settings
```

### Icons:
- Dashboard: `Home01Icon`
- Agents: `UserMultipleIcon` (import from `@hugeicons/core-free-icons`)
- Chat: `Chat01Icon`
- Activity: `Activity01Icon` (import from `@hugeicons/core-free-icons`)
- Settings: `Settings01Icon`

### Center Chat Treatment:
The Chat tab (index 2, center) must be visually emphasized:
- Slightly raised: use `transform: translateY(-4px)` or `-translate-y-1` on the button
- Larger icon container: `size-10` circle (vs `size-6` for others) with `bg-accent-500 text-white` always (not just when active)
- Icon size 22 (vs 18 for others)
- The label still says "Chat" below
- Active state: add a subtle ring/glow `ring-2 ring-accent-300 shadow-md`
- Spring animation on tap: `active:scale-90` with `transition-transform duration-150`

### True Centering:
Use this layout pattern to guarantee Chat is dead-center:
```tsx
<div className="grid grid-cols-5"> {/* equal columns = true center */}
  {TABS.map(...)}
</div>
```
Each tab is a flex-col centered within its grid cell.

### Remove "More" button entirely.
Remove `setSidebarCollapsed` usage from tab bar. The sidebar is accessed only via edge swipe or chat session list.

### Tab bar container:
```tsx
<nav className="fixed inset-x-0 bottom-0 z-[60] pb-[env(safe-area-inset-bottom)] md:hidden">
  <div className="mx-2 mb-1 grid grid-cols-5 rounded-2xl border border-white/30 bg-white/60 px-1 py-1 shadow-lg backdrop-blur-xl backdrop-saturate-150">
    ...tabs...
  </div>
</nav>
```

### Export a constant for consistent spacing:
```tsx
/** Total height of MobileTabBar including internal padding, used by other components for bottom insets */
export const MOBILE_TAB_BAR_OFFSET = '5rem' // ~80px: bar height + env margin
```

## 2. Chat Composer Fix (`src/screens/chat/components/chat-composer.tsx`)

The composer's bottom padding must account for the tab bar. Use the exported constant:
```tsx
// Mobile: pad below composer so it sits above tab bar
pb-[calc(env(safe-area-inset-bottom)+5rem)]  // on mobile
pb-[calc(env(safe-area-inset-bottom)+0.75rem)]  // on desktop (md:)
```

The composer must be `shrink-0` and in normal document flow (NOT `position: fixed`). It's already `shrink-0` — keep that.

### iOS Keyboard handling:
The viewport meta already has `interactive-widget=resizes-content` which makes iOS resize the layout when keyboard opens. This means:
- The flex column (header → messages → composer) naturally adjusts
- The tab bar stays fixed at bottom but gets pushed down by the keyboard (which is correct — keyboard covers it)
- The composer stays visible because it's in the flex flow

**Do NOT add any JavaScript keyboard detection or visualViewport hacks.** The CSS viewport meta handles it.

## 3. Chat Screen Layout (`src/screens/chat/chat-screen.tsx`)

The mobile layout must be a proper flex column:
```
┌─────────────────┐
│ ChatHeader      │  ← shrink-0
├─────────────────┤
│ Messages        │  ← flex-1 min-h-0 overflow-y-auto
├─────────────────┤
│ Composer        │  ← shrink-0, pb includes tab bar offset
└─────────────────┘
```

Current code (~line 1101) already uses `'flex flex-col'` for mobile. Good.

Fix `stableContentStyle`: The `mobileMessageInset` should be `'1rem'` (just a small buffer since composer is in-flow, not fixed). This is already correct from last commit.

## 4. Workspace Shell (`src/components/workspace-shell.tsx`)

Update the `pb-20` on non-chat mobile routes to use a consistent value. Change:
```tsx
isMobile && !isOnChatRoute ? 'pb-20' : '',
```
to:
```tsx
isMobile && !isOnChatRoute ? 'pb-24' : '',
```

This ensures all non-chat pages have enough clearance for the tab bar.

## 5. Full Mobile Sweep — All Screens

Every screen should already have `pb-24 md:pb-8` from the previous commit. Verify these files still have proper mobile padding:
- `src/screens/dashboard/dashboard-screen.tsx` — `pb-24`
- `src/screens/activity/activity-screen.tsx` — `pb-24`
- `src/screens/tasks/tasks-screen.tsx` — `pb-24`
- `src/screens/debug/debug-console-screen.tsx` — `pb-24`
- `src/screens/gateway/agents-screen.tsx` — `pb-24`
- `src/screens/gateway/channels-screen.tsx` — `pb-24`
- `src/routes/settings/index.tsx` — `pb-24`
- `src/screens/settings/providers-screen.tsx` — `pb-24`
- `src/routes/files.tsx` — `pb-24`
- `src/routes/terminal.tsx` — `pb-24`
- `src/routes/instances.tsx` — `pb-24`

If any are missing `pb-24` on mobile, add it.

## 6. DO NOT CHANGE
- `__root.tsx` viewport meta — keep as-is
- `chat-sidebar.tsx` — keep spring transition
- `styles.css` — keep tap highlight fix
- Desktop layout — all changes must be mobile-only (use `md:` breakpoints)
- Do not add visualViewport JavaScript hacks
- Do not use `position: fixed` for the composer
- Do not change the chat-message-list.tsx `ChatContainerRoot` component internals

## 7. Commit
After all changes, commit with: `"feat: mobile v2 - centered chat tab, composer fix, full sweep"`
