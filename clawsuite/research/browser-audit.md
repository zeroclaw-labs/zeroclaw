# ClawSuite Browser View Audit Report

**Date**: 2026-02-12  
**Auditor**: Codex Sub-agent  
**Scope**: Browser monitoring architecture, gateway integration, and feasibility analysis

---

## Executive Summary

The ClawSuite browser view implements a **polling-based live browser monitoring system** that displays tabs and screenshots from an agent's browser session. It uses a **graceful degradation** pattern with automatic demo mode fallback when the OpenClaw gateway browser API is unavailable.

**Key Findings**:
- ✅ Architecture is sound with robust error handling
- ✅ Demo mode detection works at both frontend and backend levels
- ⚠️ Requires OpenClaw gateway to implement specific browser RPC methods
- ⚠️ Polling every 2 seconds may be inefficient; consider WebSocket streaming
- ✅ Gateway connection logic is well-designed with reconnection and authentication

---

## Architecture Overview

### Component Hierarchy

```
BrowserPanel.tsx (main container)
├── BrowserControls.tsx (URL bar, refresh button)
├── BrowserTabs.tsx (sidebar with tab list)
└── BrowserScreenshot.tsx (live screenshot display)
```

### Data Flow

```
Frontend (React Query)
  ↓ polls every 2s
API Routes (/api/browser/tabs, /api/browser/screenshot)
  ↓
Server Functions (browser-monitor.ts)
  ↓ RPC calls with fallback methods
Gateway Client (gateway.ts)
  ↓ WebSocket connection
OpenClaw Gateway
  ↓ browser tool
Browser Control System
```

---

## Frontend Components

### 1. BrowserPanel.tsx (Main Container)

**Responsibilities**:
- Orchestrates two polling queries (tabs and screenshots)
- Manages tab selection state
- Detects demo mode from API responses
- Displays warning banner when in demo mode

**Query Strategy**:
```typescript
// Tabs query: polls every 2 seconds
queryKey: ['browser', 'tabs']
refetchInterval: 2_000
retry: false

// Screenshot query: polls every 2 seconds per tab
queryKey: ['browser', 'screenshot', effectiveTabId]
refetchInterval: 2_000
retry: false
```

**Demo Mode Detection**:
```typescript
const demoMode =
  Boolean(tabsQuery.data?.demoMode) || Boolean(screenshotQuery.data?.demoMode)
```

**Fallback Logic**:
- When gateway fails, `createLocalFallbackTabs()` and `createLocalFallbackScreenshot()` generate synthetic responses
- Fallback responses include SVG-based placeholder screenshots
- Error messages are captured and displayed in the UI

### 2. BrowserControls.tsx

**Features**:
- URL display (read-only)
- Back/forward buttons (disabled - no navigation support yet)
- Refresh button (manually triggers refetch)
- Demo mode badge

**Status**: Minimal interaction - purely read-only monitoring

### 3. BrowserTabs.tsx

**Features**:
- Lists all open browser tabs
- Shows tab title, URL, and active state
- Click to select a tab (changes screenshot query)
- Shows loading placeholders during initial fetch
- Animated list with smooth transitions

**Status**: Functional, good UX with motion animations

### 4. BrowserScreenshot.tsx

**Features**:
- Displays base64/SVG screenshots
- Shows capture timestamp
- Fade transition between screenshot updates
- Loading spinner state

**Status**: Functional, handles both real screenshots and demo SVG fallbacks

---

## Backend Implementation

### API Routes

**`/api/browser/tabs`** (`src/routes/api/browser/tabs.ts`)
- Calls `getGatewayTabsResponse()`
- Returns JSON with tabs array and metadata
- No parameters

**`/api/browser/screenshot`** (`src/routes/api/browser/screenshot.ts`)
- Calls `getGatewayScreenshotResponse(tabId?)`
- Accepts optional `tabId` query parameter
- Returns JSON with base64 image data

### Browser Monitor (`src/server/browser-monitor.ts`)

**RPC Method Fallback Strategy**:

```typescript
// For tabs, tries in order:
BROWSER_TABS_METHODS = [
  'browser.tabs',
  'browser.list_tabs',
  'browser.get_tabs',
]

// For screenshots, tries in order:
BROWSER_SCREENSHOT_METHODS = [
  'browser.screenshot',
  'browser.capture',
  'browser.take_screenshot',
]
```

**Response Normalization**:
- Coerces various field names (`id`, `tabId`, `key`) into standardized format
- Handles multiple image data formats:
  - `imageDataUrl` (preferred)
  - `dataUrl`, `screenshot`, `image` (base64 strings)
  - `base64` with `mimeType`
- Synthesizes missing fields (title, URL, active state)

**Demo Fallbacks**:
- `buildDemoTabsResponse()`: Returns 3 synthetic tabs
- `buildDemoScreenshotResponse()`: Generates SVG placeholder with:
  - Browser chrome mockup
  - Current URL display
  - Error message
  - Timestamp

**Error Handling**:
- Catches all gateway RPC errors
- Always returns valid response (never throws)
- Sets `demoMode: true` and includes error message
- Frontend never sees failed requests - only demo responses

---

## Gateway Connection

### Gateway Client (`src/server/gateway.ts`)

**Connection Details**:

```typescript
// Configuration from environment
CLAWDBOT_GATEWAY_URL    // default: ws://127.0.0.1:18789
CLAWDBOT_GATEWAY_TOKEN  // recommended auth
CLAWDBOT_GATEWAY_PASSWORD  // alternative auth
```

**Protocol**:
- WebSocket-based JSON-RPC (protocol version 3)
- Request/response pattern with UUID correlation
- Event streaming support (not used for browser monitoring)

**Authentication Flow**:
1. Open WebSocket connection
2. Send `connect` request with auth credentials
3. Wait for authenticated response
4. Send browser RPC requests

**Resilience Features**:
- **Automatic reconnection** with exponential backoff (1s → 2s → 4s → 30s max)
- **Request queuing** during disconnection/reconnection
- **Heartbeat/ping-pong** keepalive (30s interval, 10s timeout)
- **Clean shutdown** with pending request rejection

**Request Lifecycle**:
```
Client.request(method, params)
  → Queue request
  → Ensure connection (may trigger reconnect)
  → Flush queue when authenticated
  → Send JSON frame
  → Wait for response frame (matched by ID)
  → Resolve promise
```

**Client API**:
```typescript
gatewayRpc<T>(method: string, params?: unknown): Promise<T>
gatewayConnectCheck(): Promise<void>
cleanupGatewayConnection(): Promise<void>
```

---

## Demo Mode Detection

### Trigger Conditions

Demo mode activates when:

1. **Gateway connection fails**:
   - WebSocket connection refused
   - Authentication error
   - Network timeout

2. **RPC method not found**:
   - Gateway doesn't implement `browser.tabs` (or fallback methods)
   - Gateway doesn't implement `browser.screenshot` (or fallback methods)

3. **Invalid response format**:
   - Gateway returns non-object payload
   - Screenshot response missing image data
   - Tabs response is empty array

### Detection Flow

```
Frontend Poll
  ↓
API Route
  ↓
browser-monitor.ts
  ↓ try RPC call
  ↓ catch error OR detect invalid response
  ↓ return { ok: true, demoMode: true, error: "reason", ...fallbackData }
  ↓
Frontend merges demoMode flags
  ↓ shows banner + renders fallback content
```

### User Experience in Demo Mode

**Visual Indicators**:
- Amber warning banner at top of page
- "Demo Mode" badge in controls
- SVG placeholder screenshots with explanation
- Synthetic tab list (3 example tabs)

**Functionality**:
- All UI remains interactive (no crashes)
- Tab selection still works (changes demo screenshot)
- Refresh button still functional
- Error messages shown in banner

---

## Gateway RPC Requirements

For ClawSuite browser view to work with OpenClaw gateway, the gateway must implement:

### Method: `browser.tabs` (or `browser.list_tabs` or `browser.get_tabs`)

**Parameters**: None (or empty object)

**Expected Response**:
```json
{
  "tabs": [
    {
      "id": "tab-123",
      "title": "Example Page",
      "url": "https://example.com",
      "active": true
    }
  ],
  "activeTabId": "tab-123"
}
```

**Flexible Field Names**:
- Top-level: `tabs` or `items`
- Tab ID: `id`, `tabId`, or `key`
- Active flag: `active` or `isActive`
- URL: `url` or `href`

### Method: `browser.screenshot` (or `browser.capture` or `browser.take_screenshot`)

**Parameters**:
```json
{
  "tabId": "tab-123"  // optional; if omitted, use active tab
}
```

**Expected Response**:
```json
{
  "imageDataUrl": "data:image/png;base64,iVBORw0KGgo...",
  "currentUrl": "https://example.com",
  "activeTabId": "tab-123"
}
```

**Flexible Field Names**:
- Image data: `imageDataUrl`, `dataUrl`, `screenshot`, `image`, or `base64` (with `mimeType`)
- Current URL: `currentUrl`, `url`, or `href`
- Tab ID: `activeTabId`, `tabId`, or `id`

**Image Formats**:
- PNG or JPEG recommended
- SVG acceptable for debugging/placeholders
- Must be data URL or base64 string

---

## Feasibility Assessment

### ✅ Can ClawSuite Connect to OpenClaw Gateway?

**YES**, if:

1. **Environment configured**:
   ```bash
   CLAWDBOT_GATEWAY_URL=ws://127.0.0.1:18789
   CLAWDBOT_GATEWAY_TOKEN=your-token-here
   ```

2. **Gateway is running** and accessible on the specified WebSocket endpoint

3. **Authentication succeeds** (token or password match gateway config)

### ⚠️ Can It Control the Browser?

**PARTIALLY**, depends on OpenClaw gateway implementation:

- **If gateway implements `browser.tabs` and `browser.screenshot` methods** → ✅ Works perfectly
- **If gateway has browser control but different method names** → ⚠️ May work with fallback methods
- **If gateway has no browser control** → ❌ Will stay in demo mode

### Current Limitations

**Read-Only Monitoring**:
- No navigation control (back/forward/URL entry)
- No tab opening/closing
- No interaction with page content
- No snapshot/action triggering

**Polling Inefficiency**:
- 2-second polling interval for both tabs and screenshots
- Could miss rapid tab changes
- Unnecessary network traffic when nothing changes
- Latency: up to 2 seconds before updates appear

---

## Recommended Changes

### Priority 1: Gateway Integration Verification

**Action**: Check if OpenClaw gateway implements required browser methods

```bash
# Test from Node.js/CLI
node -e "
  const gateway = require('./src/server/gateway.ts');
  gateway.gatewayRpc('browser.tabs').then(console.log).catch(console.error);
"
```

**If methods don't exist**:
- Add gateway plugin/handler for browser control
- Or map ClawSuite method names to existing OpenClaw browser tool

### Priority 2: Add WebSocket Streaming for Screenshots

**Problem**: Polling every 2 seconds is wasteful

**Solution**: Use gateway event stream for screenshot updates

```typescript
// In browser-monitor.ts
import { onGatewayEvent } from './gateway'

onGatewayEvent((frame) => {
  if (frame.type === 'event' && frame.event === 'browser.screenshot_updated') {
    // Push update to connected clients via Server-Sent Events
    broadcastScreenshotUpdate(frame.payload)
  }
})
```

**Benefits**:
- Real-time updates (no 2-second delay)
- Lower bandwidth (only send when changed)
- Battery friendly (no constant polling)

### Priority 3: Add Browser Control Actions

**Current Gap**: Controls exist but are disabled

**Add Methods**:
- `browser.navigate(url)` - Address bar URL entry
- `browser.back()` / `browser.forward()` - Navigation buttons
- `browser.refresh()` - Reload current tab
- `browser.open_tab(url?)` - New tab button
- `browser.close_tab(tabId)` - Close tab button
- `browser.focus_tab(tabId)` - Switch active tab

**UI Changes**:
- Enable back/forward buttons when history available
- Make URL bar editable with submit handler
- Add "+" button for new tabs
- Add "×" button on each tab card

### Priority 4: Add Snapshot/Actions Integration

**Goal**: Let users trigger browser actions from ClawSuite

**New Features**:
- "Take Snapshot" button → calls `browser.snapshot()` with refs
- "Click Element" mode → overlay clickable elements on screenshot
- "Type Text" input → sends keyboard input to page
- Action history panel → shows recent browser tool calls

**Use Case**: Debugging agent browser automation

### Priority 5: Improve Error Messaging

**Current**: Generic error text in demo banner

**Better**:
```typescript
// Categorize errors
if (error.message.includes('ECONNREFUSED')) {
  return {
    error: 'Gateway not running. Start with: openclaw gateway start',
    demoMode: true,
  }
}

if (error.message.includes('authentication')) {
  return {
    error: 'Gateway auth failed. Check CLAWDBOT_GATEWAY_TOKEN',
    demoMode: true,
  }
}

if (error.message.includes('method not found')) {
  return {
    error: 'Gateway browser API not available. Update OpenClaw or enable browser plugin.',
    demoMode: true,
  }
}
```

### Priority 6: Add Profile/Target Selection

**Current**: Monitors single default browser session

**Enhancement**: Let users choose browser profile

```typescript
// Add dropdown in BrowserControls
<Select value={profile} onChange={setProfile}>
  <option value="chrome">Chrome (Extension)</option>
  <option value="openclaw">OpenClaw Browser</option>
  <option value="host">Host Browser</option>
</Select>

// Pass to API
fetchBrowserTabs({ profile: 'chrome' })

// Gateway call
gatewayRpc('browser.tabs', { profile: 'chrome' })
```

### Priority 7: Add Screenshot Quality Settings

**Current**: Full-size screenshots every 2 seconds (bandwidth intensive)

**Options**:
```typescript
// Settings panel
<Settings>
  <RangeInput label="Quality" value={quality} min={30} max={100} />
  <Select label="Update Rate" value={interval}>
    <option value={1000}>1 second (fast)</option>
    <option value={2000}>2 seconds (default)</option>
    <option value={5000}>5 seconds (battery saver)</option>
  </Select>
  <Checkbox label="Auto-pause when tab inactive" />
</Settings>

// Pass to screenshot API
{ tabId: 'abc', quality: 75, maxWidth: 1280 }
```

---

## Security Considerations

### Current Security Posture

**✅ Good**:
- Gateway requires authentication (token or password)
- No browser control exposed to unauthenticated users
- Read-only by default (can't navigate or interact)
- Runs server-side (no direct browser access from client)

**⚠️ Moderate Risk**:
- Screenshots may contain sensitive information
- Tab URLs reveal browsing history
- Polling creates predictable traffic pattern

**Recommendations**:
1. **Add screenshot redaction**: Blur sensitive text regions
2. **Limit to operator role**: Require `operator.admin` scope
3. **Add audit logging**: Log all browser API access
4. **Rate limiting**: Prevent polling abuse
5. **Session isolation**: Ensure users only see their own browser sessions

---

## Performance Analysis

### Current Resource Usage

**Network**:
- Tabs API: ~1-2 KB per poll (every 2s) = **~1 KB/s**
- Screenshot API: ~50-200 KB per poll (every 2s) = **25-100 KB/s**
- Total bandwidth: **~100 KB/s per viewer** (continuous)

**Server CPU**:
- Minimal (just JSON serialization and WebSocket forwarding)
- Gateway does the heavy lifting (browser control)

**Browser Rendering**:
- Re-renders on every screenshot update (every 2s)
- Motion animations on tab list
- React Query caching reduces unnecessary re-renders

### Optimization Opportunities

1. **Delta compression**: Only send changed pixels
2. **Adaptive polling**: Slow down when page is idle
3. **Viewport clipping**: Only capture visible area
4. **Format optimization**: Use WebP or AVIF instead of PNG
5. **Debounced updates**: Batch rapid changes

---

## Testing Recommendations

### Unit Tests

```typescript
// Test demo fallback logic
describe('browser-monitor', () => {
  it('returns demo tabs when gateway throws', async () => {
    mockGatewayRpc.mockRejectedValue(new Error('Connection refused'))
    const result = await getGatewayTabsResponse()
    expect(result.demoMode).toBe(true)
    expect(result.tabs.length).toBeGreaterThan(0)
  })

  it('normalizes various tab field names', () => {
    const raw = { tabId: 'abc', href: 'https://example.com', active: true }
    const normalized = normalizeTab(raw, 0)
    expect(normalized.id).toBe('abc')
    expect(normalized.url).toBe('https://example.com')
    expect(normalized.isActive).toBe(true)
  })
})
```

### Integration Tests

```typescript
// Test full flow with mock gateway
describe('Browser API', () => {
  beforeEach(() => {
    startMockGateway({
      'browser.tabs': { tabs: [{ id: 'test', title: 'Test', url: 'https://test.com', active: true }] },
      'browser.screenshot': { imageDataUrl: 'data:image/png;base64,test', currentUrl: 'https://test.com' }
    })
  })

  it('fetches tabs from gateway', async () => {
    const response = await fetch('/api/browser/tabs')
    const data = await response.json()
    expect(data.demoMode).toBe(false)
    expect(data.tabs[0].title).toBe('Test')
  })
})
```

### Manual Testing Checklist

- [ ] Start ClawSuite without gateway running → demo mode activates
- [ ] Start gateway → demo mode clears within 2 seconds
- [ ] Open multiple browser tabs → all appear in sidebar
- [ ] Click tab in sidebar → screenshot switches
- [ ] Close tab in browser → disappears from sidebar within 2 seconds
- [ ] Navigate in browser → URL updates in controls
- [ ] Network disconnect → graceful reconnection
- [ ] Invalid auth credentials → helpful error message

---

## Conclusion

**Overall Assessment**: **B+ (Good, needs minor improvements)**

**Strengths**:
- ✅ Robust error handling with graceful degradation
- ✅ Clean architecture with separation of concerns
- ✅ Well-structured gateway client with reconnection logic
- ✅ Good UX with loading states and animations
- ✅ Flexible response normalization handles various gateway formats

**Weaknesses**:
- ⚠️ Polling-based updates are inefficient (should use streaming)
- ⚠️ Read-only (no browser control actions)
- ⚠️ Depends on gateway implementing specific RPC methods (not verified)
- ⚠️ No profile/target selection (assumes single browser session)
- ⚠️ Screenshots may be bandwidth-heavy without compression

**Feasibility**: **HIGH** - Will work immediately if OpenClaw gateway implements the `browser.tabs` and `browser.screenshot` RPC methods. If methods don't exist, it degrades gracefully to demo mode.

**Next Steps**:
1. **Verify gateway methods exist**: Test `browser.tabs` and `browser.screenshot` RPC calls
2. **If missing**: Add gateway handlers or map to existing OpenClaw browser tool
3. **Implement streaming**: Replace polling with event-based updates
4. **Add controls**: Enable navigation, tab management, and actions
5. **Optimize**: Add quality settings and adaptive polling

**Estimated Work**:
- Gateway method verification: 30 minutes
- Streaming implementation: 4-6 hours
- Browser controls: 8-10 hours
- Full feature parity: 20-30 hours

---

## Appendix: Code References

### Key Files

| Path | Purpose | LOC |
|------|---------|-----|
| `src/components/browser-view/BrowserPanel.tsx` | Main container component | ~180 |
| `src/components/browser-view/BrowserControls.tsx` | URL bar and refresh button | ~60 |
| `src/components/browser-view/BrowserScreenshot.tsx` | Screenshot display | ~70 |
| `src/components/browser-view/BrowserTabs.tsx` | Tab sidebar | ~110 |
| `src/server/browser-monitor.ts` | Gateway RPC and demo fallbacks | ~320 |
| `src/server/gateway.ts` | WebSocket client with reconnection | ~480 |
| `src/routes/api/browser/tabs.ts` | API route for tabs | ~15 |
| `src/routes/api/browser/screenshot.ts` | API route for screenshots | ~15 |

### Environment Variables

```bash
# Required for gateway connection
CLAWDBOT_GATEWAY_URL=ws://127.0.0.1:18789
CLAWDBOT_GATEWAY_TOKEN=your-gateway-token

# Alternative auth (not recommended)
CLAWDBOT_GATEWAY_PASSWORD=your-gateway-password
```

### Gateway RPC Call Examples

```typescript
// Fetch tabs
const tabs = await gatewayRpc('browser.tabs')
// Returns: { tabs: [...], activeTabId: '...' }

// Fetch screenshot
const screenshot = await gatewayRpc('browser.screenshot', { tabId: 'abc' })
// Returns: { imageDataUrl: 'data:image/png;base64,...', currentUrl: '...', activeTabId: '...' }

// Hypothetical controls (not yet implemented)
await gatewayRpc('browser.navigate', { url: 'https://example.com' })
await gatewayRpc('browser.close_tab', { tabId: 'abc' })
```

---

**Report Generated**: 2026-02-12 00:30 EST  
**Agent**: Codex Sub-agent (browser-audit-v2)  
**Session**: agent:codex:subagent:8393b793-31ca-4707-bc8f-a04868a3aec1
