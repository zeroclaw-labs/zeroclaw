import assert from "node:assert/strict";
import test, { beforeEach } from "node:test";

/**
 * `chatSessions` reads `localStorage` at call time, so a minimal in-memory
 * stand-in is enough — no DOM needed. Installed before the module is imported
 * because the module is evaluated on first import.
 */
class MemoryStorage {
  private store = new Map<string, string>();
  /** When set, every write throws — models private mode / exhausted quota. */
  failWrites = false;

  getItem(key: string): string | null {
    return this.store.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    if (this.failWrites) throw new Error("quota exceeded");
    this.store.set(key, value);
  }

  removeItem(key: string): void {
    this.store.delete(key);
  }

  clear(): void {
    this.store.clear();
    this.failWrites = false;
  }
}

const storage = new MemoryStorage();
(globalThis as unknown as { localStorage: MemoryStorage }).localStorage = storage;

const { getActiveSessionId, newSessionId, setActiveSessionId } =
  await import("./chatSessions.ts");

const ACTIVE = "zeroclaw_active_session.ops";
const LEGACY = "zeroclaw_session_id.ops";

beforeEach(() => {
  storage.clear();
});

test("getActiveSessionId mints and persists an id for a new agent", () => {
  const id = getActiveSessionId("ops");
  assert.ok(id.length > 0);
  assert.equal(storage.getItem(ACTIVE), id);
});

test("getActiveSessionId is stable across calls", () => {
  const first = getActiveSessionId("ops");
  assert.equal(getActiveSessionId("ops"), first);
});

test("getActiveSessionId adopts the pre-multi-session key and retires it", () => {
  storage.setItem(LEGACY, "legacy-conversation");

  // The upgrade must land the operator back in the conversation they had, not
  // a blank one.
  assert.equal(getActiveSessionId("ops"), "legacy-conversation");
  assert.equal(storage.getItem(ACTIVE), "legacy-conversation");
  // Adopted, not mirrored: the old key is gone so the two cannot drift.
  assert.equal(storage.getItem(LEGACY), null);
});

test("an existing pointer wins over a stale legacy key", () => {
  storage.setItem(ACTIVE, "current-conversation");
  storage.setItem(LEGACY, "legacy-conversation");

  assert.equal(getActiveSessionId("ops"), "current-conversation");
});

test("agents keep separate pointers", () => {
  const ops = getActiveSessionId("ops");
  const research = getActiveSessionId("research");

  assert.notEqual(ops, research);
  assert.equal(getActiveSessionId("ops"), ops);
});

test("setActiveSessionId moves the agent to another conversation", () => {
  getActiveSessionId("ops");
  setActiveSessionId("ops", "second-conversation");

  assert.equal(getActiveSessionId("ops"), "second-conversation");
});

test("unwritable storage still yields a usable id", () => {
  storage.failWrites = true;

  // Private mode must degrade to a volatile conversation, never throw into the
  // render path.
  assert.ok(getActiveSessionId("ops").length > 0);
  assert.doesNotThrow(() => setActiveSessionId("ops", "whatever"));
});

test("newSessionId returns distinct ids", () => {
  assert.notEqual(newSessionId(), newSessionId());
});
