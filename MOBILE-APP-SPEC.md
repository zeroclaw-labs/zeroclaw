# Mobile App Experience Spec

## Goal
Make ClawSuite feel like a native iOS app (think ChatGPT mobile). Currently the mobile layout mostly works (sidebar overlay, stacked dashboard) but needs polish.

## Tech Stack
- TanStack Start + React + Tailwind CSS v4 + motion (framer-motion)
- Mobile breakpoint: <768px (use `md:` prefix, mobile-first)
- Already has: `useChatMobile` hook with `isMobile` state via matchMedia

## CRITICAL BUG TO FIX
**When iOS keyboard opens, the chat header scrolls out of view.** The header must stay pinned/visible at all times, like ChatGPT. The chat layout should be:
- Header (sticky/fixed top, always visible)
- Messages (scrollable, flex-1)  
- Composer (at bottom, moves up with keyboard)

The viewport meta tag should include `interactive-widget=resizes-content` so iOS resizes the layout when keyboard appears.

Update `src/routes/__root.tsx` viewport meta to:
`width=device-width, initial-scale=1, viewport-fit=cover, maximum-scale=1, user-scalable=no, interactive-widget=resizes-content`

**Do NOT use `position: fixed` for the composer on mobile.** Use flex layout instead — the natural flex column (header → messages → composer) handles keyboard resize correctly.

## Changes Needed

### 1. Apple Liquid Glass Bottom Tab Bar (NEW FILE)
Create `src/components/mobile-tab-bar.tsx`:
- Fixed to bottom of screen, above safe area
- Frosted glass style: `bg-white/60 backdrop-blur-xl backdrop-saturate-150 border border-white/30 rounded-2xl shadow-lg`
- 5 tabs: Chat, Dashboard, Skills, Settings, More (opens sidebar)
- Active tab: filled icon + accent color + subtle white pill background
- Inactive: muted icons + labels
- Use `active:scale-95` for tap feedback
- Only visible on mobile (`md:hidden`)

### 2. Workspace Shell (`src/components/workspace-shell.tsx`)
- Add `<MobileTabBar />` at bottom (inside the root div, after everything)
- Only render on mobile
- Remove the floating hamburger button (tab bar's "More" replaces it)
- Hide `<ChatPanelToggle>` on mobile (tab bar replaces it)
- Hide `<ChatPanel>` on mobile (tab bar navigates directly)
- Add `pb-20` padding to main content on mobile NON-chat routes (so content isn't behind tab bar)
- On CHAT routes, the tab bar should still show but the composer needs clearance

### 3. Default Route (`src/routes/index.tsx`)
- Mobile: redirect to `/chat/main` instead of `/dashboard`
- Desktop: keep `/dashboard`
- Check `window.innerWidth < 768` in `beforeLoad`

### 4. Chat Screen Keyboard Fix (`src/screens/chat/chat-screen.tsx`)
- The `<main>` wrapper containing header + messages + composer must be a flex column with `h-full`
- Header: `shrink-0` (already is)
- Messages: `flex-1 min-h-0 overflow-y-auto`
- Composer: `shrink-0` (NOT fixed positioned)
- Remove the `fixedOnMobile` prop usage — just use normal flex flow
- Add bottom padding to messages area on mobile to account for tab bar (~80px)

### 5. Chat Composer (`src/screens/chat/components/chat-composer.tsx`)
- Remove the `fixedOnMobile` prop entirely
- Composer should always be `shrink-0` in the flex flow
- Keep `pb-[calc(env(safe-area-inset-bottom)+0.75rem)]` for safe area
- On mobile, add extra bottom padding to clear the tab bar: `pb-[calc(env(safe-area-inset-bottom)+5rem)]`

### 6. PWA Manifest
- File already exists at `public/manifest.json` — update `background_color` to `#fafaf9`
- In `src/routes/__root.tsx` head, ensure these meta tags exist:
  - `<meta name="apple-mobile-web-app-capable" content="yes">`
  - `<meta name="apple-mobile-web-app-status-bar-style" content="default">`  
  - `<meta name="theme-color" content="#f97316">`

### 7. Chat Sidebar Spring Animation
- In `src/screens/chat/components/chat-sidebar.tsx`, change the sidebar transition to spring:
  `transition={{ type: 'spring', stiffness: 400, damping: 30 }}`
- This makes it feel snappy like native iOS

### 8. Touch Feedback CSS (`src/styles.css`)
Add at the end:
```css
@media (hover: none) and (pointer: coarse) {
  :where(button, [data-slot='button'], a, [role='button']) {
    -webkit-tap-highlight-color: transparent;
  }
}
```

## DO NOT
- Break desktop layout
- Change gateway connection logic
- Modify API routes
- Remove any existing mobile detection that's working (sidebar overlay, dashboard stacking)

## Commit message
`feat: mobile app experience - liquid glass tab bar, keyboard fix, PWA`
