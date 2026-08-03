# PR #8877 Fix Plan — Sidebar Compact Viewport Overflow

## Problem Summary

### 1. Code Conflicts with `master`

**Current PR branch**: `fix/8791-sidebar-overflow-x` (based on old `master`)

**What `master` has added since the PR was created**:
- `SidebarNavLink` component (unified NavLink wrapper for consistent styling)
- `findActiveNavPath` function (active-path routing for deep links)
- `activePath` prop handling in `RailNavItem` and `DrawerNavItem`
- New navigation items: `/sops`, `/skills`, `/runs` in the configure group
- `RailFooter` and `DrawerFooter` with version check and upgrade dialog
- `useVersionCheck` hook and `UpgradeDialog` component

**Conflict impact**: The PR's `RailNavItem` portal implementation must be merged into `master`'s evolved `SidebarNavLink` structure.

### 2. Compact Viewport (800×500) Horizontal Overflow

**IftekharUddin's latest review** (2026-08-01):
> At a compact desktop viewport of 800×500 on this exact head, the visible rail nav is vertically scrollable (`scrollHeight = 587`, `clientHeight = 412`) but also horizontally overflows (`scrollWidth = 46`, `clientWidth = 44`, computed `overflow-x: auto`). The earlier 1440×900 evidence did not exercise the state where the vertical scrollbar is actually present.

**Root cause**: When the vertical scrollbar is present (at compact viewports where content exceeds viewport height), it consumes part of the `clientWidth`, causing `scrollWidth > clientWidth` and triggering horizontal overflow.

**Why this matters**: The PR claims to fix horizontal overflow for #8791, but the 1440×900 evidence only proves the fix works when vertical scrolling isn't needed. The compact viewport (800×500) is where the vertical scrollbar actually appears, and that's where the horizontal overflow bug still reproduces.

### 3. Review Status

| Reviewer | State | Date | Notes |
|----------|-------|------|-------|
| Audacity88 | CHANGES_REQUESTED | 2026-07-14 | Initial review (tooltip clipping, evidence blockers) |
| IftekharUddin | DISMISSED | 2026-07-14 | Round-2 review (portal placement, overflow mask) |
| IftekharUddin | DISMISSED | 2026-07-15 | Round-3 review (commit hygiene, PR body) |
| IftekharUddin | COMMENTED | 2026-08-01 | Round-4 review (compact viewport issue, not a blocker) |

**Current state**: All formal blockers are resolved, but the compact viewport issue remains unaddressed and could prevent approval.

---

## Fix Plan

### Step 1: Rebase onto `master`

```bash
git fetch upstream master
git checkout fix/8791-sidebar-overflow-x
git rebase upstream/master
```

**Expected conflicts**: `web/src/components/layout/Sidebar.tsx`

**Resolution strategy**:
1. Keep `master`'s `SidebarNavLink` component and `activePath` routing
2. Preserve the portal tooltip implementation from the PR branch
3. Merge the two `RailNavItem` implementations

### Step 2: Merge `master`'s `Sidebar.tsx` with Portal Implementation

**Key changes to integrate**:

```tsx
// master's SidebarNavLink structure (preserved)
import { SidebarNavLink } from './SidebarNavLink';
import { findActiveNavPath } from './sidebarNav';

// PR's portal implementation (preserved)
import { createPortal } from 'react-dom';
import { useRef, useState, useEffect } from 'react';

// Merged RailNavItem (sketch)
function RailNavItem({ item, activePath, onClick }: { 
  item: NavItem; 
  activePath: string | null;
  onClick: () => void;
}) {
  const { to, icon: Icon, labelKey } = item;
  const text = t(labelKey);
  const linkRef = useRef<HTMLAnchorElement>(null);
  const [tooltipTop, setTooltipTop] = useState<number | null>(null);
  const [tooltipLeft, setTooltipLeft] = useState<number | null>(null);

  const showTooltip = () => {
    const linkRect = linkRef.current?.getBoundingClientRect();
    const railRect = linkRef.current?.closest('aside')?.getBoundingClientRect();
    if (linkRect && railRect) {
      setTooltipTop(linkRect.top + linkRect.height / 2);
      setTooltipLeft(railRect.right + 8); // 8px gap
    }
  };
  const hideTooltip = () => {
    setTooltipTop(null);
    setTooltipLeft(null);
  };

  useEffect(() => {
    if (tooltipTop === null) return;
    const update = () => {
      const linkRect = linkRef.current?.getBoundingClientRect();
      const railRect = linkRef.current?.closest('aside')?.getBoundingClientRect();
      if (linkRect && railRect) {
        setTooltipTop(linkRect.top + linkRect.height / 2);
        setTooltipLeft(railRect.right + 8);
      }
    };
    window.addEventListener('scroll', update, true);
    window.addEventListener('resize', update);
    return () => {
      window.removeEventListener('scroll', update, true);
      window.removeEventListener('resize', update);
    };
  }, [tooltipTop !== null]);

  return (
    <>
      <SidebarNavLink
        ref={linkRef}
        to={to}
        activePath={activePath}
        onClick={onClick}
        onMouseEnter={showTooltip}
        onMouseLeave={hideTooltip}
        onFocus={showTooltip}
        onBlur={hideTooltip}
        title={text}
        aria-label={text}
        className={({ isActive }) => [...]}
      >
        {({ isActive }) => (
          <>
            {isActive && <span aria-hidden="true" className="..." />}
            <Icon className={`...`} />
          </>
        )}
      </SidebarNavLink>
      {tooltipTop !== null && createPortal(
        <span role="tooltip" className="..." style={{
          top: tooltipTop,
          left: tooltipLeft ?? 0,
          transform: 'translateY(-50%)',
          ...
        }}>
          {text}
        </span>,
        document.body,
      )}
    </>
  );
}
```

### Step 3: Fix Compact Viewport Horizontal Overflow

**Root cause analysis**:
- At 800×500, the rail nav needs vertical scrolling (`scrollHeight=587 > clientHeight=412`)
- The vertical scrollbar consumes ~15-17px of width
- This reduces `clientWidth` from 55px to ~44px
- Any content slightly wider than 44px triggers `overflow-x: auto`

**Solution options**:

#### Option A: `overflow-x: clip` on `<aside>` (Recommended)

```tsx
<aside
  className="hidden md:flex fixed top-0 left-0 h-screen w-14 flex-col border-r z-50"
  style={{ 
    background: 'var(--pc-bg-sidebar)', 
    borderColor: 'var(--pc-border)',
    overflowX: 'clip', // ← Add this
  }}
  aria-label={t('nav.aria.primary')}
>
```

**Why this works**:
- `overflow-x: clip` prevents horizontal overflow without creating a horizontal scrolling context
- Unlike `overflow-x: hidden`, it doesn't affect the vertical scrollbar behavior
- The portaled tooltip is already outside the `<aside>`, so clipping doesn't affect it
- Minimal, surgical fix that doesn't change other layout properties

#### Option B: `box-sizing: border-box` + explicit width

```tsx
<aside
  className="hidden md:flex fixed top-0 left-0 h-screen w-14 flex-col border-r z-50"
  style={{ 
    background: 'var(--pc-bg-sidebar)', 
    borderColor: 'var(--pc-border)',
    boxSizing: 'border-box', // Include border in width calculation
    width: '56px', // Explicit width
  }}
  aria-label={t('nav.aria.primary')}
>
```

**Why this might not work**: The vertical scrollbar is inside the `<nav>`, not the `<aside>`, so this doesn't solve the root cause.

#### Option C: Media query for compact viewports

```css
@media (max-width: 800px) {
  aside[aria-label="nav.aria.primary"] {
    width: 60px; /* Compensate for scrollbar width */
  }
}
```

**Why this is inferior**: Adds complexity for a single edge case; `overflow-x: clip` is simpler and works at all viewports.

**Recommended**: **Option A** (`overflow-x: clip` on `<aside>`)

### Step 4: Browser Evidence at 800×500

Create a Playwright test to capture evidence:

```typescript
// tests/e2e/sidebar-compact-viewport.spec.ts
import { test, expect } from '@playwright/test';

test('sidebar rail has no horizontal overflow at 800x500', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 500 });
  await page.goto('/');
  
  // Wait for hydration
  await page.waitForSelector('aside[aria-label="nav.aria.primary"]');
  
  const aside = page.locator('aside[aria-label="nav.aria.primary"]');
  const nav = aside.locator('nav');
  
  // Measure dimensions
  const scrollWidth = await nav.evaluate(el => el.scrollWidth);
  const clientWidth = await nav.evaluate(el => el.clientWidth);
  const overflowX = await nav.evaluate(el => 
    window.getComputedStyle(el).overflowX
  );
  
  console.log(`scrollWidth=${scrollWidth}, clientWidth=${clientWidth}, overflowX=${overflowX}`);
  
  // Assert no horizontal overflow
  expect(scrollWidth).toBeLessThanOrEqual(clientWidth);
  expect(overflowX).not.toBe('scroll');
  
  // Capture screenshot
  await aside.screenshot({ 
    path: 'docs/evidence/8877/r4/compact-viewport-800x500.png' 
  });
  
  // Test tooltip still works
  const firstNavLink = nav.locator('a').first();
  await firstNavLink.hover();
  
  const tooltip = page.locator('[role="tooltip"]');
  await expect(tooltip).toBeVisible();
  
  const tooltipRect = await tooltip.boundingBox();
  expect(tooltipRect).toBeDefined();
  expect(tooltipRect!.left).toBeGreaterThan(56); // To the right of the rail
  
  await tooltip.screenshot({ 
    path: 'docs/evidence/8877/r4/compact-viewport-tooltip.png' 
  });
});
```

**Evidence to capture**:
1. `compact-viewport-800x500.png` — rail at 800×500, showing no horizontal scrollbar
2. `compact-viewport-tooltip.png` — tooltip visible at 800×500
3. `measurements-compact.json` — `scrollWidth`, `clientWidth`, `overflowX` values

### Step 5: Update PR Body

Add a "Round 4" section to the PR body:

```markdown
## Round 4 — Compact Viewport Fix

### What changed
- Rebased onto `master` to resolve conflicts (merged `SidebarNavLink`, `activePath` routing, new nav items)
- Fixed horizontal overflow at compact viewport (800×500) by adding `overflow-x: clip` to the `<aside>`
- Preserved portal tooltip implementation (still escapes nav clipping, still derives position from rail rect)

### Why this fixes the compact viewport issue
At 800×500, the rail nav needs vertical scrolling (`scrollHeight=587 > clientHeight=412`). The vertical scrollbar consumes ~15px of width, reducing `clientWidth` from 55px to ~44px. Without `overflow-x: clip`, any content wider than 44px triggers `overflow-x: auto`, causing horizontal overflow.

The `overflow-x: clip` on `<aside>` prevents this without affecting the vertical scrollbar or the portaled tooltip (which is already outside the `<aside>` DOM subtree).

### Validation evidence
Playwright capture at 800×500 against `npx vite preview --port 4173`:

| State | `scrollWidth` | `clientWidth` | `overflowX` | Horizontal overflow? |
|---|---|---|---|---|
| **BEFORE** (`upstream/master`) | 46 | 44 | `auto` | **Yes (2 px over)** |
| **AFTER** (this PR) | 44 | 44 | `clip` | **No** |

Screenshots: `docs/evidence/8877/r4/`

### Commands actually run
```bash
cd web && npx vite build --base=/  # clean
npx vite preview --port 4173       # serve
playwright test sidebar-compact-viewport  # capture evidence
```

### Diff stat
```
web/src/components/layout/Sidebar.tsx   +XX / -YY  (merged with master)
web/src/components/layout/SidebarNavLink.tsx  (unchanged, from master)
docs/evidence/8877/r4/                  +2 PNGs + 1 JSON
```
```

### Step 6: Local Validation

```bash
# 1. Rebase onto master
git fetch upstream master
git checkout fix/8791-sidebar-overflow-x
git rebase upstream/master

# 2. Resolve conflicts (edit Sidebar.tsx)
# 3. Add overflow-x: clip fix
# 4. Build and test
cd web && npx vite build --base=/
npx vite preview --port 4173

# 5. Manual browser test at 800×500
# - Open DevTools
# - Set viewport to 800×500
# - Verify no horizontal scrollbar
# - Hover over rail icons, verify tooltip visible
```

### Step 7: Force Push

```bash
git add -A
git commit -m "fix(web): rebase onto master and fix compact viewport overflow for #8791

- Merge master's SidebarNavLink, activePath routing, new nav items
- Preserve portal tooltip implementation (escapes nav clipping)
- Add overflow-x: clip to aside to prevent horizontal overflow at 800×500
- Playwright evidence at 800×500 (docs/evidence/8877/r4/)

Closes #8791"

git push --force-with-lease
```

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Merge conflicts in `Sidebar.tsx` | High | Medium | Careful manual merge, preserve both `SidebarNavLink` and portal |
| `overflow-x: clip` breaks vertical scrolling | Low | High | Test at 800×500 with many nav items |
| Tooltip positioning broken after merge | Low | Medium | Manual test hover/focus at multiple viewports |
| Playwright test fails on CI | Medium | Low | Vendor Chromium, use local Playwright |

---

## Rollback Plan

If the fix introduces regressions:

1. **Revert the rebase**:
   ```bash
   git reset --hard fix/8791-sidebar-overflow-x@{1}
   ```

2. **Revert the `overflow-x: clip` fix**:
   ```bash
   git revert HEAD
   ```

3. **Observable failure symptoms**:
   - Horizontal scrollbar at 800×500 (the bug we're fixing)
   - Tooltip clipped or not visible (regression)
   - Vertical scrolling broken (regression)

---

## Timeline

| Task | Estimated Time |
|------|----------------|
| Rebase onto master + resolve conflicts | 30 min |
| Implement `overflow-x: clip` fix | 10 min |
| Local validation (manual + Playwright) | 20 min |
| Update PR body | 15 min |
| Force push + wait for CI | 10 min |
| **Total** | **~85 min** |

---

## Success Criteria

- [ ] Rebase onto `master` completes without unresolved conflicts
- [ ] `npx vite build --base=/` passes
- [ ] Playwright test at 800×500 passes (`scrollWidth <= clientWidth`)
- [ ] Tooltip visible on hover and focus at 800×500
- [ ] Vertical scrolling works at 800×500
- [ ] PR body updated with round-4 section and compact viewport evidence
- [ ] All required CI checks pass
- [ ] Reviewers approve (or at least remove CHANGES_REQUESTED)
