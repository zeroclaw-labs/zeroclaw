import assert from 'node:assert/strict';
import test, { beforeEach } from 'node:test';

/**
 * Pane-ownership regressions for the multi-tab chat workspace.
 *
 * The feature's safety invariant is that a conversation belongs to exactly one
 * pane: two panes on one gateway session mean two server-side Agents seeded
 * from different snapshots, diverging transcripts, and a Stop or delete in one
 * pane landing on the other's turn. These tests execute the transitions that
 * enforce that invariant, so removing either the restore-time collision repair
 * or the same-alias minting rule fails the suite instead of shipping green.
 *
 * `localStorage` is read at call time by `chatSessions`, so an in-memory
 * stand-in installed before the first import is enough — no DOM required.
 */
class MemoryStorage {
  private store = new Map<string, string>();

  get length(): number { return this.store.size; }
  clear(): void { this.store.clear(); }
  getItem(key: string): string | null { return this.store.get(key) ?? null; }
  key(index: number): string | null { return [...this.store.keys()][index] ?? null; }
  removeItem(key: string): void { this.store.delete(key); }
  setItem(key: string, value: string): void { this.store.set(key, String(value)); }
}

const storage = new MemoryStorage();
Object.assign(globalThis, { localStorage: storage });

const {
  STORAGE_KEY,
  applySessionChange,
  loadPersisted,
  makeTab,
  reservationsByKey,
  tabForOpenRequest,
  withDistinctSessions,
} = await import('./chatWorkspace.state.ts');

type Tab = ReturnType<typeof makeTab>;

function store(value: unknown): void {
  storage.setItem(STORAGE_KEY, JSON.stringify(value));
}

beforeEach(() => {
  storage.clear();
});

// ── Restore ────────────────────────────────────────────────────────────────

test('a v2 workspace restores its panes, selection, and layout verbatim', () => {
  store({
    version: 2,
    tabs: [
      { key: 'k1', alias: 'ops', sessionId: 'A' },
      { key: 'k2', alias: 'coder', sessionId: 'B' },
    ],
    activeKey: 'k2',
    layout: 'split',
    splitKeys: ['k1', 'k2'],
  });

  const restored = loadPersisted();

  assert.deepEqual(restored.tabs, [
    { key: 'k1', alias: 'ops', sessionId: 'A' },
    { key: 'k2', alias: 'coder', sessionId: 'B' },
  ]);
  assert.equal(restored.activeKey, 'k2');
  assert.equal(restored.layout, 'split');
  assert.deepEqual(restored.splitKeys, ['k1', 'k2']);
});

test('a v1 workspace upgrades each stored alias into its own pane', () => {
  storage.setItem('zeroclaw_active_session.ops', 'A');
  storage.setItem('zeroclaw_active_session.coder', 'B');
  store({ openChats: ['ops', 'coder'], activeAlias: 'coder', layout: 'split', splitAliases: ['coder', 'ops'] });

  const restored = loadPersisted();

  assert.equal(restored.tabs?.length, 2);
  assert.deepEqual(restored.tabs?.map((tb: Tab) => tb.alias), ['ops', 'coder']);
  // Each upgraded pane resumes the conversation that alias was last on.
  assert.deepEqual(restored.tabs?.map((tb: Tab) => tb.sessionId), ['A', 'B']);
  // v1 addressed panes by alias; v2 addresses them by minted key.
  const coder = restored.tabs?.find((tb: Tab) => tb.alias === 'coder');
  const ops = restored.tabs?.find((tb: Tab) => tb.alias === 'ops');
  assert.equal(restored.activeKey, coder?.key);
  assert.deepEqual(restored.splitKeys, [coder?.key, ops?.key]);
});

test('a v1 workspace deduplicates repeated aliases into one pane each', () => {
  store({ openChats: ['ops', 'ops', 'coder'] });

  const restored = loadPersisted();

  assert.deepEqual(restored.tabs?.map((tb: Tab) => tb.alias), ['ops', 'coder']);
});

test('malformed panes are dropped instead of rendering a pane with no alias', () => {
  store({
    version: 2,
    tabs: [
      { key: 'k1', alias: 'ops', sessionId: 'A' },
      { key: 'k2', sessionId: 'B' },
      null,
      { key: 'k3', alias: 'coder' },
    ],
    activeKey: 'k1',
  });

  assert.deepEqual(loadPersisted().tabs?.map((tb: Tab) => tb.key), ['k1']);
});

test('an unreadable workspace entry restores nothing rather than throwing', () => {
  storage.setItem(STORAGE_KEY, '{ not json');
  assert.deepEqual(loadPersisted(), {});
});

// ── Duplicate repair ───────────────────────────────────────────────────────

test('restoring two panes that claim one conversation moves the later pane off it', () => {
  store({
    version: 2,
    tabs: [
      { key: 'k1', alias: 'ops', sessionId: 'SHARED' },
      { key: 'k2', alias: 'ops', sessionId: 'SHARED' },
    ],
    activeKey: 'k1',
  });

  const tabs = loadPersisted().tabs ?? [];

  assert.equal(tabs.length, 2);
  // The first claimant keeps the conversation; the second is moved to a fresh
  // one. Without the repair both panes would open one gateway session.
  assert.equal(tabs[0]?.sessionId, 'SHARED');
  assert.notEqual(tabs[1]?.sessionId, 'SHARED');
  assert.equal(new Set(tabs.map((tb) => tb.sessionId)).size, 2);
});

test('collision repair leaves every pane on a distinct conversation', () => {
  const repaired = withDistinctSessions([
    { key: 'k1', alias: 'ops', sessionId: 'A' },
    { key: 'k2', alias: 'ops', sessionId: 'A' },
    { key: 'k3', alias: 'coder', sessionId: 'A' },
    { key: 'k4', alias: 'coder', sessionId: 'B' },
  ]);

  assert.equal(repaired.length, 4);
  assert.equal(new Set(repaired.map((tb) => tb.sessionId)).size, 4);
  // Panes keep their identity and agent; only the contested conversation moves.
  assert.deepEqual(repaired.map((tb) => tb.key), ['k1', 'k2', 'k3', 'k4']);
  assert.deepEqual(repaired.map((tb) => tb.alias), ['ops', 'ops', 'coder', 'coder']);
  assert.equal(repaired[0]?.sessionId, 'A');
  assert.equal(repaired[3]?.sessionId, 'B');
});

// ── Opening panes ──────────────────────────────────────────────────────────

test('opening an agent that is already open mints a distinct conversation', () => {
  storage.setItem('zeroclaw_active_session.ops', 'A');
  const first = tabForOpenRequest([], 'ops');
  const second = tabForOpenRequest([first], 'ops');

  // The first pane resumes where the alias left off...
  assert.equal(first.sessionId, 'A');
  // ...and the second starts somewhere new, so the two never share a transcript.
  assert.notEqual(second.sessionId, first.sessionId);
  assert.notEqual(second.key, first.key);
  assert.equal(second.alias, 'ops');
});

test('opening a different agent resumes that alias, it does not fork a conversation', () => {
  storage.setItem('zeroclaw_active_session.ops', 'A');
  storage.setItem('zeroclaw_active_session.coder', 'B');
  const ops = tabForOpenRequest([], 'ops');
  const coder = tabForOpenRequest([ops], 'coder');

  assert.equal(coder.sessionId, 'B');
});

// ── Reservations ───────────────────────────────────────────────────────────

test('each pane reserves its siblings conversations and never its own', () => {
  const tabs: Tab[] = [
    { key: 'k1', alias: 'ops', sessionId: 'A' },
    { key: 'k2', alias: 'ops', sessionId: 'B' },
    { key: 'k3', alias: 'coder', sessionId: 'C' },
  ];

  const reserved = reservationsByKey(tabs);

  assert.deepEqual(reserved.k1, ['B', 'C']);
  assert.deepEqual(reserved.k2, ['A', 'C']);
  assert.deepEqual(reserved.k3, ['A', 'B']);
});

test('a lone pane reserves nothing', () => {
  assert.deepEqual(reservationsByKey([{ key: 'k1', alias: 'ops', sessionId: 'A' }]), { k1: [] });
});

test('moving a pane to another conversation updates its siblings reservations', () => {
  const tabs: Tab[] = [
    { key: 'k1', alias: 'ops', sessionId: 'A' },
    { key: 'k2', alias: 'ops', sessionId: 'B' },
  ];

  const moved = applySessionChange(tabs, 'k2', 'C');

  assert.deepEqual(moved.map((tb) => tb.sessionId), ['A', 'C']);
  const reserved = reservationsByKey(moved);
  // The pane it left is released, and the one it took is now off-limits to k1.
  assert.deepEqual(reserved.k1, ['C']);
  assert.deepEqual(reserved.k2, ['A']);
});

test('a pane move rewrites only that pane and keeps identity stable', () => {
  const tabs: Tab[] = [
    { key: 'k1', alias: 'ops', sessionId: 'A' },
    { key: 'k2', alias: 'coder', sessionId: 'B' },
  ];

  const moved = applySessionChange(tabs, 'k1', 'Z');

  assert.deepEqual(moved[0], { key: 'k1', alias: 'ops', sessionId: 'Z' });
  // Untouched panes keep referential identity so their providers do not rerender.
  assert.equal(moved[1], tabs[1]);
});

test('reporting the conversation a pane is already on is a no-op', () => {
  const tabs: Tab[] = [{ key: 'k1', alias: 'ops', sessionId: 'A' }];
  assert.equal(applySessionChange(tabs, 'k1', 'A')[0], tabs[0]);
});
