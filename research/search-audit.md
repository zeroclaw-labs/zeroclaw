# ClawSuite Search Modal Audit

**Date:** 2026-02-12  
**Auditor:** Codex Subagent  
**Files Analyzed:**
- `src/components/search/search-modal.tsx`
- `src/hooks/use-search-data.ts`

---

## Executive Summary

The search modal is **partially functional** with real data for chats, files, and activity. However, three critical issues remain:

1. **Skills search uses hardcoded data** instead of `/api/skills`
2. **Unused mock data blocks** (~114 lines) still present in search-modal.tsx
3. **No message content search** — only session titles/keys are searchable

---

## What Works ✅

### Real Data Sources
| Scope | Source | Status |
|-------|--------|--------|
| **Chats** | `/api/sessions` | ✅ Working |
| **Files** | `/api/files?action=list` | ✅ Working |
| **Activity** | `useActivityEvents` hook | ✅ Working |
| **Actions** | Client-side filter of `quickActions` | ✅ Working |

### Features
- **Keyboard navigation** (↑/↓, Enter, Esc, Tab, 1-9 hotkeys)
- **Scope filtering** (All, Chats, Files, Agents, Skills, Actions)
- **Debounced search** (200ms delay)
- **Recent searches UI** (display only, hardcoded)
- **Quick actions** (10 navigation shortcuts)

---

## What's Mocked/Hardcoded ⚠️

### 1. Skills Data (CRITICAL)
**Location:** `use-search-data.ts:32-38`

```typescript
const SKILLS_DATA: SearchSkill[] = [
  { id: 'weather', name: 'Weather', description: 'Get current weather and forecasts', installed: true },
  { id: 'browser-use', name: 'Browser Use', description: 'Automate browser interactions', installed: true },
  { id: 'codex-cli', name: 'Codex CLI', description: 'Use OpenAI Codex for coding tasks', installed: true },
  { id: 'video-frames', name: 'Video Frames', description: 'Extract frames from videos', installed: false },
  { id: 'openai-whisper', name: 'OpenAI Whisper', description: 'Transcribe audio files', installed: false },
]
```

**Problem:** Returns static array instead of fetching from `/api/skills`  
**Impact:** Skills added/removed via API won't appear in search

---

### 2. Unused Mock Data Blocks (BLOAT)
**Location:** `search-modal.tsx:43-157`

Four large mock datasets declared but never used:
- `_CHAT_RESULTS` (6 entries, 19 lines)
- `_FILE_RESULTS` (5 entries, 28 lines)
- `_AGENT_RESULTS` (4 entries, 25 lines)
- `_SKILL_RESULTS` (5 entries, 28 lines)

**Current state:**  
Line 611: `void _CHAT_RESULTS` (etc.) — suppresses unused variable warnings but keeps dead code

**Problem:** 114 lines of dead code inflate bundle size and confuse maintainers

---

### 3. Recent Searches (COSMETIC)
**Location:** `search-modal.tsx:41`

```typescript
const RECENT_SEARCHES = [
  'streaming fixes',
  'session timeout',
  'agent memory',
  'usage alerts',
]
```

**Problem:** Static demo data, not user-specific history  
**Impact:** Low (UI affordance only, not a search bug)

---

## Missing Features 🚫

### Message Content Search
**Current behavior:**  
- Searches session `friendlyId`, `key`, `title` only
- Message text is NOT indexed or searchable

**Example:**  
- User searches `"retry strategy"`
- Session titled "Gateway Retry Strategy" **will match** ✅
- Message content "investigate retry logic" **will NOT match** ❌

**Backend requirement:**  
- `/api/sessions` returns only metadata, not messages
- Need `/api/messages/search?q=...` or enhanced `/api/sessions` response

---

## Code Changes Required

### 1. Fix Skills Search (Medium Complexity)

**File:** `use-search-data.ts`

**Before:**
```typescript
// Skills (static)
const skillsResults: SearchSkill[] = SKILLS_DATA
```

**After:**
```typescript
// Skills (from API)
const skillsQuery = useQuery({
  queryKey: ['search', 'skills'],
  queryFn: async () => {
    const res = await fetch('/api/skills')
    if (!res.ok) return []
    const data = await res.json()
    return Array.isArray(data.skills) ? data.skills.map((s: any) => ({
      id: String(s.id || s.name),
      name: String(s.name || ''),
      description: String(s.description || ''),
      installed: Boolean(s.installed),
    })) : []
  },
  enabled: scope === 'all' || scope === 'skills',
  staleTime: 60_000,
})

return {
  sessions: sessionsQuery.data || [],
  files: filesQuery.data || [],
  skills: skillsQuery.data || [],  // Changed
  activity: activityResults,
  isLoading: sessionsQuery.isLoading || filesQuery.isLoading || skillsQuery.isLoading,  // Added
}
```

**Remove:**
```typescript
const SKILLS_DATA: SearchSkill[] = [...]  // Delete lines 32-38
```

**Complexity:** 🟡 Medium  
- **Lines changed:** ~20
- **Risk:** Low (isolated change)
- **Dependencies:** Requires `/api/skills` endpoint exists and returns `{ skills: [...] }`
- **Testing:** Verify skills list loads, search filters work

---

### 2. Remove Mock Data Blocks (Low Complexity)

**File:** `search-modal.tsx`

**Delete lines 43-157:**
```typescript
// DELETE EVERYTHING FROM:
type ChatMock = { ... }
// ...through...
const _SKILL_RESULTS: Array<SkillMock> = [...]
```

**Delete lines 608-611:**
```typescript
// Preserve mock data for future use
void _CHAT_RESULTS
void _FILE_RESULTS
void _AGENT_RESULTS
void _SKILL_RESULTS
```

**Complexity:** 🟢 Low  
- **Lines removed:** ~120
- **Risk:** None (dead code)
- **Testing:** None required (TypeScript will catch any accidental usage)

---

### 3. Add Message Content Search (High Complexity)

**Backend required:**

**Option A:** New endpoint
```typescript
// GET /api/messages/search?q=retry&limit=20
{
  "results": [
    {
      "messageId": "msg-123",
      "sessionKey": "main",
      "sessionTitle": "Gateway Retry Strategy",
      "content": "Investigate why streaming chunks stop after provider reconnect.",
      "role": "user",
      "timestamp": 1739340060000
    }
  ]
}
```

**Option B:** Enhance `/api/sessions` with optional `?includeMessages=true&q=...`

**Frontend changes:**

**File:** `use-search-data.ts`

Add new query:
```typescript
export type SearchMessage = {
  id: string
  sessionKey: string
  sessionTitle: string
  content: string
  role: string
  timestamp: number
}

async function fetchMessages(query: string): Promise<SearchMessage[]> {
  if (!query.trim()) return []
  const res = await fetch(`/api/messages/search?q=${encodeURIComponent(query)}&limit=50`)
  if (!res.ok) return []
  const data = await res.json()
  return Array.isArray(data.results) ? data.results.map((m: any) => ({
    id: String(m.messageId || m.id),
    sessionKey: String(m.sessionKey),
    sessionTitle: String(m.sessionTitle || m.sessionKey),
    content: String(m.content || ''),
    role: String(m.role || 'user'),
    timestamp: Number(m.timestamp || Date.now()),
  })) : []
}

export function useSearchData(scope: ..., query: string) {  // Add query param
  const messagesQuery = useQuery({
    queryKey: ['search', 'messages', query],
    queryFn: () => fetchMessages(query),
    enabled: (scope === 'all' || scope === 'chats') && query.trim().length > 2,
    staleTime: 10_000,
  })

  return {
    sessions: sessionsQuery.data || [],
    files: filesQuery.data || [],
    skills: skillsQuery.data || [],
    activity: activityResults,
    messages: messagesQuery.data || [],  // NEW
    isLoading: ...,
  }
}
```

**File:** `search-modal.tsx`

```typescript
const { sessions, files, skills, activity, messages } = useSearchData(scope, debouncedQuery)

// Inside resultItems useMemo:
const messageResults = messages.map<SearchResultItemData>((entry) => ({
  id: entry.id,
  scope: 'chats',
  icon: <HugeiconsIcon icon={Chat01Icon} size={20} strokeWidth={1.5} />,
  title: entry.sessionTitle,
  snippet: entry.content.slice(0, 120) + (entry.content.length > 120 ? '...' : ''),
  meta: new Date(entry.timestamp).toLocaleTimeString(),
  badge: entry.role === 'assistant' ? '🤖' : '👤',
  onSelect: () => {
    closeModal()
    navigate({
      to: '/chat/$sessionKey',
      params: { sessionKey: entry.sessionKey },
      search: { highlight: entry.id },  // Optional: scroll to message
    })
  },
}))

// Merge into results:
if (scope === 'chats') return [...chats, ...messageResults]
if (scope === 'all') return [...chats, ...messageResults, ...fileResults, ...]
```

**Complexity:** 🔴 High  
- **Lines changed:** ~80-100
- **Risk:** Medium (requires backend, changes multiple files)
- **Dependencies:**
  - Backend `/api/messages/search` endpoint
  - Database indexing (FTS or vector search for performance)
  - Optional: Message highlighting in chat view
- **Testing:**
  - Search returns relevant messages
  - Navigation jumps to correct session
  - Performance with large message history
  - Empty state when no matches

---

## Complexity Estimates

| Task | Complexity | Effort | Risk | Priority |
|------|-----------|--------|------|----------|
| Fix skills search | 🟡 Medium | 30 min | Low | High |
| Remove mock data | 🟢 Low | 5 min | None | Medium |
| Add message search | 🔴 High | 4-8 hrs | Medium | High |

### Breakdown: Message Search (8 hours)
- Backend endpoint: 2 hrs
- Database indexing: 2 hrs
- Frontend integration: 2 hrs
- Testing + polish: 2 hrs

---

## Recommendations

### Immediate (Next Session)
1. ✅ **Fix skills search** — 30 min, low risk, immediate user value
2. ✅ **Remove mock data** — 5 min, reduces bundle size by ~4KB

### Short-Term (This Week)
3. 🔍 **Design message search UX**
   - Should message results be inline with session results or separate?
   - What's the minimum query length to trigger search?
   - Should we show message role (user/assistant/system)?

### Medium-Term (Next Sprint)
4. 🏗️ **Implement message search backend**
   - SQLite FTS5 for full-text search
   - Or pg_trgm if using PostgreSQL
   - Index `messages.content`, `messages.role`, join to `sessions.key`

5. 🎨 **Implement message search frontend**
   - Pass query to `useSearchData`
   - Merge message results into result list
   - Add highlight/scroll-to-message navigation

### Nice-to-Have
- **Persist recent searches** (localStorage or backend)
- **Search filters** (by date, role, session)
- **Fuzzy matching** (typo tolerance)
- **Search analytics** (track popular queries)

---

## File Structure After Changes

```
src/
├── components/search/
│   ├── search-modal.tsx          [-114 lines, +15 lines for message results]
│   ├── search-input.tsx          [unchanged]
│   ├── search-results.tsx        [unchanged]
│   └── quick-actions.tsx         [unchanged]
├── hooks/
│   └── use-search-data.ts        [+35 lines for skills API + messages]
└── routes/api/
    └── messages/
        └── search.ts             [NEW: ~80 lines]
```

---

## Testing Checklist

### Before Deployment
- [ ] Skills search loads from `/api/skills`
- [ ] Skills filter correctly by name/description
- [ ] Mock data removed, no TypeScript errors
- [ ] Bundle size reduced (verify with `npm run build`)

### After Message Search
- [ ] Message search triggers after 3+ characters
- [ ] Results show relevant messages with context
- [ ] Clicking message navigates to correct session
- [ ] Performance acceptable with 1000+ messages
- [ ] Empty state displays when no results

---

## Notes

- Current search is **client-side filtered** (good for <1000 items)
- Consider **server-side pagination** if data grows beyond 5000 items
- Skills API should return `installed` field (true/false) for badge display
- Message search may need **debounce increase** (500ms) to avoid excessive API calls

---

**End of Audit**
