# Dashboard Redesign — iOS-Style Widget System

## Vision
Transform the dashboard from a dense desktop-first grid into a fluid iOS-style widget experience that feels native on mobile and elegant on desktop.

## Design Principles
1. **iOS Widget DNA** — Rounded corners (2xl), frosted glass, compact info density, tap-to-expand
2. **Glanceable** — Every widget communicates its core value in <1 second
3. **Responsive-first** — Mobile is the primary surface; desktop adds columns, not complexity
4. **Animated** — Smooth spring transitions, press states, subtle parallax on scroll
5. **Customizable** — Long-press to enter jiggle mode (edit), drag to reorder, + to add

## Widget Sizes (iOS Grid)
- **Small** (2×2): Single metric + sparkline (usage, cost, uptime)
- **Medium** (4×2): Summary + list preview (sessions, agents, tasks)
- **Large** (4×4): Full detail view (activity log, agent hub, skills)

## Component Architecture

```
src/screens/dashboard/
├── dashboard-screen.tsx          — Main layout, widget grid, edit mode
├── constants/grid-config.ts      — Breakpoints, sizing, default layouts
├── components/
│   ├── widget-shell.tsx           ✅ DONE — Unified widget container
│   ├── widget-grid.tsx            ✅ DONE — Responsive CSS grid (2-col mobile, 4-col desktop)
│   ├── metrics-widget.tsx         ✅ DONE — Small widget for single number + trend
│   ├── now-card.tsx               — TODO: Refactor with WidgetShell + pulse animation
│   ├── agent-status-widget.tsx    ✅ DONE — Migrated to WidgetShell
│   ├── recent-sessions-widget.tsx ✅ DONE — Migrated to WidgetShell
│   ├── usage-meter-widget.tsx     — TODO: Migrate to WidgetShell
│   ├── activity-log-widget.tsx    ✅ DONE — Migrated to WidgetShell
│   ├── tasks-widget.tsx           ✅ DONE — Migrated to WidgetShell
│   ├── skills-widget.tsx          ✅ DONE — Migrated to WidgetShell
│   └── notifications-widget.tsx   — TODO: Migrate to WidgetShell
```

## Remaining Tasks

### 1. Migrate NotificationsWidget → WidgetShell
- Replace `DashboardGlassCard` import with `WidgetShell`
- Add `editMode?: boolean` prop
- Change `draggable` to `draggable: _draggable` (unused)
- Wrap return with `<WidgetShell size="medium" title="Notifications" icon={Notification03Icon} ...>`

### 2. Migrate UsageMeterWidget → WidgetShell
- Replace `DashboardGlassCard` import with `WidgetShell`
- Add `editMode?: boolean` prop
- Wrap return with `<WidgetShell size="large" title="Usage Meter" icon={ChartLineData02Icon} action={<tabs...>} ...>`
- Keep all internal tab logic intact — just swap the outer wrapper

### 3. Refactor NowCard → WidgetShell + pulse animation
- File: `src/screens/dashboard/components/now-card.tsx`
- Replace outer wrapper with `<WidgetShell size="medium" ...>`
- Add pulse animation to the live activity indicator (use `animate-pulse` on the status dot)

### 4. Wire WidgetGrid into dashboard-screen.tsx
- Replace `<Responsive as ResponsiveGridLayout ...>` with `<WidgetGrid items={[...]} />`
- Build the items array mapping widgetId → size → component node
- Remove react-grid-layout imports: `import 'react-grid-layout/css/styles.css'`, `import 'react-resizable/css/styles.css'`
- Keep HeroMetricsRow → replace with 4× MetricsWidget (small) in a row
- Keep all existing data fetching hooks — only rewrap presentation

### 5. Remove react-grid-layout dependency
- After wiring is done and tsc passes: `npm uninstall react-grid-layout`
- Remove from package.json, update grid-config.ts to remove react-grid-layout types

## CSS Details
- 2-col mobile, 4-col desktop: `grid grid-cols-2 gap-3 md:grid-cols-4 md:gap-4`
- Small: `col-span-1`, Medium: `col-span-2`, Large: `col-span-2 md:col-span-4`
- Jiggle keyframe: ✅ added to styles.css as `animate-wiggle`
- Shimmer: ✅ added to styles.css as `animate-shimmer`

## Rules
- `tsc --noEmit` must pass with 0 errors before every commit
- Do NOT push — local commits only
- Do NOT touch data fetching hooks (fetchGatewayStatus, fetchSessions, fetchUsage, etc.)
- Do NOT touch the header bar
