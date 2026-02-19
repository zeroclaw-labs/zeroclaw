# Mobile Composer Tap Target Fix

## Problem
On iOS Safari mobile, the chat composer input is hard to tap/focus. The textarea is too small — it's a thin strip and users have to tap precisely on it. Sometimes tapping doesn't focus at all and you have to tap below/around it.

## Screenshot Analysis
Looking at the screenshot, the composer area has:
- "Ask anything... (⌘↵ to send)" placeholder text in a thin input
- Below that: a row with "+", "Claude Opus 4 6 ∨", microphone icon, send button
- Below that: the bottom tab bar

The issue is the actual focusable textarea is a narrow single-line strip. The action bar below it (with model selector, mic, send) is NOT part of the tap target for focusing the input.

## Required Fix

### 1. Make the entire composer wrapper act as a tap target
In `chat-composer.tsx`, the outer composer wrapper div should have an `onClick` handler that focuses the textarea when tapped anywhere in the composer area (not just the thin textarea strip):

```tsx
// Add to the composer wrapper div
onClick={(e) => {
  // Don't steal focus from buttons/selects inside the composer
  const target = e.target as HTMLElement
  if (target.closest('button') || target.closest('select') || target.closest('[role="button"]') || target.closest('input[type="file"]')) return
  promptRef.current?.focus()
}}
```

### 2. Increase textarea minimum height on mobile
The `PromptInputTextarea` likely has a small default height. On mobile, ensure it has at least `min-h-[44px]` (iOS minimum tap target). 

In `chat-composer.tsx`, add a className to the PromptInputTextarea:
```tsx
<PromptInputTextarea
  placeholder="Ask anything... (⌘↵ to send)"
  autoFocus
  inputRef={promptRef}
  onFocus={() => setMobileKeyboardOpen(true)}
  onBlur={() => setTimeout(() => setMobileKeyboardOpen(false), 120)}
  className="min-h-[44px]"  // ← ADD THIS
/>
```

### 3. Check PromptInput padding
Look at `src/components/prompt-kit/prompt-input.tsx` — the PromptInput wrapper likely has tight padding. On mobile, ensure the wrapper has enough padding so the tap area is generous:
- The PromptInput should have at least `py-3` on mobile
- The textarea itself should fill the available space

### 4. Remove the desktop keyboard shortcut hint on mobile
The placeholder "Ask anything... (⌘↵ to send)" shows a Mac keyboard shortcut that makes no sense on mobile. On mobile, just show "Ask anything..." or "Message...".

Detect mobile and change placeholder:
```tsx
const isMobile = typeof window !== 'undefined' && window.innerWidth < 768
// ...
<PromptInputTextarea
  placeholder={isMobile ? "Message..." : "Ask anything... (⌘↵ to send)"}
  ...
/>
```

### 5. Ensure z-index stacking is correct
Current state: 
- Composer wrapper: `z-40`
- PromptInput: `relative z-50`
- Tab bar outer nav: `z-[60] pointer-events-none`
- Tab bar inner div: `pointer-events-auto`

This is correct — the PromptInput at z-50 is above the composer wrapper at z-40, and the tab bar's outer nav doesn't intercept (pointer-events-none). But verify that the PromptInput's z-50 actually creates a stacking context above everything in the chat area.

### 6. Check for `overflow: hidden` clipping
The chat-screen's `<main>` has `overflow-hidden`. The composer is a child of this main. If the composer's bottom padding extends below the main's visible area, the tap target could be clipped. Ensure the flex layout places the composer fully within the visible viewport.

## Files to modify:
1. `src/screens/chat/components/chat-composer.tsx` — onClick focus handler, mobile placeholder, textarea min-height
2. `src/components/prompt-kit/prompt-input.tsx` — possibly increase padding on mobile

## DO NOT CHANGE:
- Tab bar (mobile-tab-bar.tsx) — already correct
- Workspace shell — already correct
- Chat sidebar — already correct
- Stores — already correct

## Commit message:
`"fix: mobile composer tap target - larger hitbox, click-to-focus, mobile placeholder"`
