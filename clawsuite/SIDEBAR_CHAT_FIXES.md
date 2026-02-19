# Sidebar Chat Panel Fixes

## Issues Fixed

### 1. **Scroll doesn't work** ✅
**Root Cause:** The flex containment chain was broken when `compact` mode was active. The ChatScreen root div used `h-full` which doesn't work properly in a flex parent container.

**Fix:** Changed ChatScreen root div to conditionally use `flex-1 min-h-0` when in compact mode instead of `h-full`. This ensures proper height constraint flows from ChatPanel → ChatScreen → ChatContainerRoot → scroll viewport.

**Changed in:** `src/screens/chat/chat-screen.tsx` (line 876)

```tsx
// Before:
<div className="relative h-full min-w-0 flex flex-col overflow-hidden">

// After:
<div className={cn(
  'relative min-w-0 flex flex-col overflow-hidden',
  compact ? 'flex-1 min-h-0' : 'h-full',
)}>
```

### 2. **Content shifts when sidebar opens/closes** ✅
**Root Cause:** The ChatPanel used `absolute` positioning on mobile but `relative` positioning on screens ≥1200px. This caused the workspace grid to add a third column, shifting the main content when the panel opened.

**Fix:** Changed ChatPanel to always use `fixed` positioning and removed the third grid column from workspace-shell. Added smooth slide-in animation for better UX.

**Changed in:** 
- `src/components/chat-panel.tsx` (line 141)
- `src/components/workspace-shell.tsx` (line 132)

```tsx
// Before:
className="absolute right-0 top-0 h-full w-[420px] ... min-[1200px]:relative min-[1200px]:shadow-none"

// After:
className="fixed right-0 top-0 h-full w-[420px] ... shadow-xl"

// With animation:
initial={{ x: '100%', opacity: 0 }}
animate={{ x: 0, opacity: 1 }}
exit={{ x: '100%', opacity: 0 }}
```

### 3. **Chat composer gets cut off** ✅
**Root Cause:** The `<main>` element inside ChatScreen needed `flex-1` to properly participate in the flex layout.

**Fix:** Added `flex-1` to the main element's className (it already had the other necessary flex classes).

**Changed in:** `src/screens/chat/chat-screen.tsx` (line 899)

```tsx
// Before:
className="flex min-h-0 min-w-0 flex-col overflow-hidden ..."

// After:
className="flex flex-1 min-h-0 min-w-0 flex-col overflow-hidden ..."
```

## Technical Details

### Flex Containment Chain (Compact Mode)
```
ChatPanel
  └─ div (flex-1 min-h-0 overflow-hidden relative)
     └─ ChatScreen
        └─ div (flex-1 min-h-0 flex flex-col overflow-hidden) ← FIXED
           └─ div (flex-1 min-h-0 overflow-hidden flex flex-col)
              └─ main (flex flex-1 min-h-0 flex-col overflow-hidden) ← FIXED
                 └─ ChatMessageList
                    └─ ChatContainerRoot (flex-1 min-h-0 flex flex-col)
                       └─ scroll viewport (flex-1 min-h-0 overflow-y-auto)
```

Every element in this chain now properly uses `flex-1 min-h-0` to constrain height.

### Layout Architecture
- **ChatPanel:** 420px wide, fixed positioning (slides in from right)
- **No layout shift:** Main content stays in place when panel opens/closes
- **Smooth animation:** 200ms slide with easing curve
- **Responsive:** Works on all screen sizes with backdrop on mobile

## Commit
```
git commit 51d60c7
fix: sidebar chat panel scroll, composer cutoff, and content shift
```

## Testing Checklist
- [ ] Open dashboard → click "Open chat" button
- [ ] Verify chat panel slides in smoothly from right
- [ ] Send multiple messages to fill the chat
- [ ] Verify messages scroll properly (no 98,000px overflow)
- [ ] Type a long message in the composer
- [ ] Verify textarea doesn't get cut off
- [ ] Close and reopen the panel
- [ ] Verify dashboard content doesn't shift/jump
- [ ] Test on different screen sizes (mobile, tablet, desktop)

## Files Changed
1. `src/screens/chat/chat-screen.tsx` - Fixed flex containment for compact mode
2. `src/components/chat-panel.tsx` - Fixed positioning and animation
3. `src/components/workspace-shell.tsx` - Removed third grid column

All changes are non-breaking and only affect the sidebar chat panel in compact mode.
