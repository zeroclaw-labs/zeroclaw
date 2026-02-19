# Mobile Optimization Task

## Goal
Make every ClawSuite screen work well on mobile (iPhone Safari, 390px width).
Think ChatGPT mobile app — clean, usable, touch-friendly.

## Current State
- Sidebar: collapses to 48px icon strip on mobile (should be 0px, hidden completely)
- Grid layout: `grid-cols-[auto_1fr]` — on mobile should be single column with sidebar as overlay
- Dashboard, chat, skills, etc. are all desktop-optimized with no responsive breakpoints
- Tailscale URL works: `https://erics-macbook-pro.tailcfa706.ts.net:8443`

## Architecture
- TanStack Start + React + Tailwind CSS v4
- Motion (framer-motion) for sidebar animations
- All styles use Tailwind utility classes
- Mobile breakpoint: `md:` prefix (768px)

## What Needs to Change

### 1. Layout Shell (`src/components/workspace-shell.tsx`)
- On mobile (`<768px`): single column grid, sidebar is a fixed overlay (z-50)
- Sidebar opens via hamburger button, closes on backdrop tap or nav selection
- Backdrop overlay (`bg-black/50`) when sidebar is open
- Already has: auto-collapse on mobile load, backdrop div (may need fixing)

### 2. Sidebar (`src/screens/chat/components/chat-sidebar.tsx`)
- On mobile: `width: 0` when collapsed (not 48px icon strip)
- On mobile: `position: fixed`, full height overlay with shadow when open
- On mobile: `width: 300px` (or 85vw) when open
- Close sidebar on any nav item click (already partially implemented via `handleSelectSession`)
- Add swipe-to-close gesture (nice to have, not required)

### 3. Dashboard (`src/screens/dashboard/`)
- Widget grid should stack vertically on mobile (single column)
- Cards should be full-width
- Remove "Open terminal with Cmd+`" tip on mobile
- Stats cards (sessions, agents, uptime, cost) should be 2x2 grid on mobile

### 4. Chat Screen (`src/screens/chat/chat-screen.tsx`)
- Already has `useChatMobile` hook and `isMobile` detection
- Ensure message bubbles have proper padding (not edge-to-edge)
- Composer should be fixed at bottom, not overflow
- System messages should be collapsible or hidden by default on mobile
- Context bar / header should be compact on mobile
- Hide file explorer toggle on mobile (already done)
- Hide terminal panel on mobile (already done)

### 5. Skills Screen (`src/screens/skills/` or route)
- Card grid → single column on mobile
- Touch-friendly card actions

### 6. Agent Hub (`src/routes/agents.tsx`)
- Responsive layout for agent cards/list

### 7. Settings/Providers (`src/screens/settings/`)
- Form inputs should be full-width on mobile
- Provider cards stack vertically

### 8. All Other Screens
- Files, Logs, Debug, Terminal, Tasks, Activity, Channels, Instances, Sessions
- Each should gracefully handle narrow viewport
- Tables → card/list layout on mobile, or horizontal scroll
- No horizontal overflow (critical!)

## Rules
- Use Tailwind responsive prefixes (`md:`, `lg:`) — mobile-first approach
- Don't break desktop layout — all changes should be additive mobile styles
- Test at 390px width (iPhone 15 Pro)
- Use `dvh` for viewport height (already using `h-dvh`)
- Touch targets minimum 44px
- No hover-only interactions on mobile
- Prefer `@media (max-width: 767px)` patterns via Tailwind

## Do NOT
- Change the gateway connection logic
- Modify API routes
- Change any business logic
- Alter the color scheme or design language
- Remove any desktop features — only adapt for mobile

## Branch
Work on a new branch: `feat/mobile-optimization`
Base off current HEAD of main.
