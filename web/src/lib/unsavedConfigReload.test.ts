import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import React, { createElement } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { PageReloadDeps } from "./chunkReload.ts";

// Unsaved config edits live only in the config draft store, which outlives
// the config form: after editing a field the user can open another page and
// still see the unsaved-changes banner. A reload from there, such as the one
// after a page's code fails to load, must ask before it drops those edits.

const fakeWindow = new EventTarget();
Object.assign(globalThis, { window: fakeWindow, React });
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const { ConfigDraftProvider, useConfigDraft } = await import("./draftStore.tsx");
const { loadPageOrReload } = await import("./chunkReload.ts");

type Drafts = ReturnType<typeof useConfigDraft>;

/** What the browser does before a reload: fire `beforeunload`, and ask the user if a handler cancels it. */
function reloadAsks(): boolean {
  const event = new Event("beforeunload", { cancelable: true });
  Object.defineProperty(event, "returnValue", { value: true, writable: true });
  fakeWindow.dispatchEvent(event);
  return event.defaultPrevented;
}

const failed = () => Promise.reject(new TypeError("Failed to fetch dynamically imported module"));

/** A failed page load whose reload records whether the browser would ask first. */
async function failPageLoad(): Promise<boolean[]> {
  const asked: boolean[] = [];
  const values = new Map<string, string>();
  const deps: PageReloadDeps = {
    storage: {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => void values.set(key, value),
    },
    reload: () => asked.push(reloadAsks()),
    now: () => 1_000_000,
    schedule: (fn) => fn(),
  };
  void loadPageOrReload(failed, deps);
  await new Promise<void>((resolve) => setTimeout(resolve, 10));
  return asked;
}

let renderer: ReactTestRenderer | null = null;
let drafts: Drafts | null = null;

/** The store with no config form mounted, as on any other page. */
async function mountStore(): Promise<Drafts> {
  function Probe() {
    drafts = useConfigDraft();
    return null;
  }
  await act(async () => {
    renderer = create(createElement(ConfigDraftProvider, null, createElement(Probe)));
  });
  assert.ok(drafts);
  return drafts;
}

async function edit(change: (store: Drafts) => void): Promise<void> {
  assert.ok(drafts);
  const store = drafts;
  await act(async () => {
    change(store);
  });
}

afterEach(async () => {
  await act(async () => {
    renderer?.unmount();
  });
  renderer = null;
  drafts = null;
});

test("a failed page load with an unsaved config edit asks before it reloads", async () => {
  await mountStore();
  await edit((store) => store.setDraft("channels.telegram.main.bot_token", "123:abc", "123:abc"));
  assert.deepEqual(await failPageLoad(), [true]);
});

test("a staged unset is unsaved too", async () => {
  await mountStore();
  await edit((store) => store.stageTombstone("channels.telegram.main.proxy_url"));
  assert.deepEqual(await failPageLoad(), [true]);
});

test("with nothing unsaved, the reload doesn't ask", async () => {
  await mountStore();
  assert.deepEqual(await failPageLoad(), [false]);
  await edit((store) => store.setDraft("channels.telegram.main.bot_token", "123:abc", "123:abc"));
  await edit((store) => store.discardAll());
  assert.deepEqual(await failPageLoad(), [false]);
});
