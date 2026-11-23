import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import type { PageReloadDeps } from "./chunkReload.ts";

// A page's code that fails to load (a gateway reload refused the request)
// reloads the page once; a second failure soon after shows the error instead
// of reloading again.

const { loadPageOrReload, PAGE_RELOAD_GUARD_MS } = await import("./chunkReload.ts");

class MemoryStorage {
  readonly values = new Map<string, string>();
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  setItem(key: string, value: string): void { this.values.set(key, value); }
}

function harness(overrides: Partial<PageReloadDeps> = {}) {
  const storage = new MemoryStorage();
  const calls = { reloads: 0, delays: [] as number[] };
  let clock = 1_000_000;
  const deps: PageReloadDeps = {
    storage,
    reload: () => {
      calls.reloads += 1;
    },
    now: () => clock,
    schedule: (fn, ms) => {
      calls.delays.push(ms);
      fn();
    },
    ...overrides,
  };
  return { deps, calls, storage, advance: (ms: number) => (clock += ms) };
}

const failed = () => Promise.reject(new TypeError("Failed to fetch dynamically imported module"));

/** Whether the promise settled within a few turns of the event loop. */
async function settled(promise: Promise<unknown>): Promise<boolean> {
  let done = false;
  promise.then(() => (done = true), () => (done = true));
  await new Promise<void>((resolve) => setTimeout(resolve, 10));
  return done;
}

test("a page that loads is returned, with no reload and nothing stored", async () => {
  const h = harness();
  const page = { default: "Dashboard" };
  assert.equal(await loadPageOrReload(async () => page, h.deps), page);
  assert.equal(h.calls.reloads, 0);
  assert.equal(h.storage.values.size, 0);
});

test("a page that fails to load reloads once after a second and stays loading", async () => {
  const h = harness();
  const pending = loadPageOrReload(failed, h.deps);
  assert.equal(await settled(pending), false);
  assert.equal(h.calls.reloads, 1);
  assert.deepEqual(h.calls.delays, [1000]);
});

test("a second failure soon after the reload shows the error instead of looping", async () => {
  const h = harness();
  assert.equal(await settled(loadPageOrReload(failed, h.deps)), false);
  h.advance(5_000);
  const second = loadPageOrReload(failed, h.deps);
  // It fails at once instead of waiting for another reload.
  assert.equal(await settled(second), true);
  await assert.rejects(second, /dynamically imported module/);
  assert.equal(h.calls.reloads, 1);
});

test("a failure long after the last reload may reload again", async () => {
  const h = harness();
  assert.equal(await settled(loadPageOrReload(failed, h.deps)), false);
  h.advance(PAGE_RELOAD_GUARD_MS + 1);
  const pending = loadPageOrReload(failed, h.deps);
  assert.equal(await settled(pending), false);
  assert.equal(h.calls.reloads, 2);
});

test("without storage to guard it, a failure shows the error and never reloads", async () => {
  for (const storage of [
    null,
    {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {},
    },
  ]) {
    const h = harness({ storage });
    const loading = loadPageOrReload(failed, h.deps);
    assert.equal(await settled(loading), true);
    await assert.rejects(loading, /dynamically imported module/);
    assert.equal(h.calls.reloads, 0);
  }
});

test("every lazily loaded page goes through the reload guard", () => {
  const source = readFileSync(new URL("../router/lazyPages.tsx", import.meta.url), "utf8");
  const imports = source.match(/import\('\.\.\/pages\//g) ?? [];
  const guarded = source.match(/lazyPage\(\(\) =>\s*import\('\.\.\/pages\//g) ?? [];
  assert.ok(imports.length > 10);
  assert.equal(guarded.length, imports.length);
  assert.equal(/\blazy\(\(\) =>\s*import\(/.test(source), false);
});
