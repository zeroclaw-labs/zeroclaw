import assert from "node:assert/strict";
import test from "node:test";
import {
  Window,
  type Document as HappyDocument,
  type HTMLButtonElement as HappyHTMLButtonElement,
} from "happy-dom";
import { createJiti } from "jiti";
import * as React from "react";
import { act } from "react";

function findButton(
  document: HappyDocument,
  label: string,
): HappyHTMLButtonElement {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  assert.ok(button, `expected button ${label}`);
  return button as HappyHTMLButtonElement;
}

async function flushEffects(): Promise<void> {
  for (let i = 0; i < 4; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

test("Save all refreshes the saved baseline and preserves in-flight edits", async () => {
  const domWindow = new Window();
  const container = domWindow.document.createElement("div");
  domWindow.document.body.appendChild(container);
  const globalKeys = [
    "window",
    "document",
    "navigator",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLSelectElement",
    "HTMLButtonElement",
    "Node",
    "Element",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "Text",
    "localStorage",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "IS_REACT_ACT_ENVIRONMENT",
  ] as const;
  const previous = new Map<string, PropertyDescriptor | undefined>();
  for (const key of globalKeys) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
  }
  const previousFetch = globalThis.fetch;
  let savedValue = false;
  let listCalls = 0;
  const patches: unknown[][] = [];
  let delayNextList = false;
  let releasePendingList: (() => void) | undefined;
  let delayNextPatch = false;
  let failNextPatch = false;
  let releasePendingPatch: (() => void) | undefined;

  const globals: Record<string, unknown> = {
    window: domWindow,
    document: domWindow.document,
    navigator: domWindow.navigator,
    HTMLElement: domWindow.HTMLElement,
    HTMLInputElement: domWindow.HTMLInputElement,
    HTMLSelectElement: domWindow.HTMLSelectElement,
    HTMLButtonElement: domWindow.HTMLButtonElement,
    Node: domWindow.Node,
    Element: domWindow.Element,
    Event: domWindow.Event,
    MouseEvent: domWindow.MouseEvent,
    KeyboardEvent: domWindow.KeyboardEvent,
    Text: domWindow.Text,
    localStorage: domWindow.localStorage,
    requestAnimationFrame: (callback: FrameRequestCallback) =>
      setTimeout(() => callback(Date.now()), 0),
    cancelAnimationFrame: (id: number) => clearTimeout(id),
    IS_REACT_ACT_ENVIRONMENT: true,
  };
  for (const [key, value] of Object.entries(globals)) {
    Object.defineProperty(globalThis, key, {
      configurable: true,
      value,
      writable: true,
    });
  }

  globalThis.fetch = async (input, init) => {
    const rawUrl =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.toString()
          : input.url;
    const url = new URL(rawUrl, "http://localhost");
    if (url.pathname === "/api/config/list") {
      listCalls += 1;
      if (delayNextList) {
        delayNextList = false;
        await new Promise<void>((resolve) => {
          releasePendingList = resolve;
        });
      }
      const entries =
        url.searchParams.get("prefix") === "other"
          ? []
          : [
              {
                path: "channels.show_tool_calls",
                category: "channels",
                kind: "bool",
                type_hint: "bool",
                value: savedValue,
                populated: true,
                is_secret: false,
              },
            ];
      return new Response(
        JSON.stringify({ entries }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (url.pathname === "/api/config" && init?.method === "OPTIONS") {
      return new Response("{}", {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }
    if (url.pathname === "/api/channels") {
      return new Response("[]", {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }
    if (url.pathname === "/api/config/sections") {
      return new Response(
        JSON.stringify({ sections: [{ key: "channels", label: "Channels" }] }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (url.pathname === "/api/config" && init?.method === "PATCH") {
      const ops = JSON.parse(String(init.body)) as Array<{
        path: string;
        value?: unknown;
      }>;
      patches.push(ops);
      if (delayNextPatch) {
        delayNextPatch = false;
        await new Promise<void>((resolve) => {
          releasePendingPatch = resolve;
        });
      }
      if (failNextPatch) {
        failNextPatch = false;
        return new Response("mock gateway rejected save", { status: 500 });
      }
      const update = ops.find((op) => op.path === "channels.show_tool_calls");
      if (update && typeof update.value === "boolean") {
        savedValue = update.value;
      }
      return new Response(
        JSON.stringify({
          saved: true,
          results: ops.map((op) => ({
            op: "replace",
            path: op.path,
            value: op.value,
            populated: true,
          })),
          warnings: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response("not found", { status: 404 });
  };

  let root:
    | { render: (children: React.ReactNode) => void; unmount: () => void }
    | undefined;
  try {
    process.env.JITI_TSCONFIG_PATHS = "1";
    const jiti = createJiti(import.meta.url, {
      fsCache: false,
      jsx: { runtime: "automatic" },
    });
    const [
      { ConfigDraftProvider },
      { default: FieldForm },
      { default: UnsavedChangesBanner },
    ] = await Promise.all([
        jiti.import<typeof import("./draftStore.tsx")>("./draftStore.tsx"),
        jiti.import<typeof import("../components/sections/FieldForm.tsx")>(
          "../components/sections/FieldForm.tsx",
        ),
        jiti.import<typeof import("../components/layout/UnsavedChangesBanner.tsx")>(
          "../components/layout/UnsavedChangesBanner.tsx",
        ),
      ]);

    function FormHarness() {
      const [prefix, setPrefix] = React.useState("channels");
      return React.createElement(
        React.Fragment,
        null,
        React.createElement(
          "button",
          {
            onClick: () =>
              setPrefix((current) =>
                current === "channels" ? "other" : "channels",
              ),
          },
          "Toggle section",
        ),
        React.createElement(FieldForm, {
          prefix,
          includePath: (path: string) =>
            path === "channels.show_tool_calls",
          scopeActionsToIncludedPaths: true,
        }),
        React.createElement(UnsavedChangesBanner),
      );
    }

    const { createRoot } = await import("react-dom/client");
    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
      root?.render(
        React.createElement(
          ConfigDraftProvider,
          null,
          React.createElement(FormHarness),
        ),
      );
      await flushEffects();
    });

    assert.ok(root, "expected the config form to render");
    assert.equal(listCalls, 1, "expected the initial field list to load");

    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    assert.match(domWindow.document.body.textContent ?? "", /1 unsaved change/);

    const originalSwitch =
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      );
    assert.ok(originalSwitch, "expected the field switch to render");
    originalSwitch.focus();
    delayNextList = true;
    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });

    assert.equal(savedValue, true);
    assert.equal(patches.length, 1);
    assert.ok(listCalls >= 2, "expected Save all to refresh the field baseline");
    assert.ok(releasePendingList, "expected the baseline refresh to be pending");
    originalSwitch.focus();
    assert.strictEqual(
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      ),
      originalSwitch,
      "background refresh should keep the switch DOM node mounted",
    );
    assert.strictEqual(
      domWindow.document.activeElement,
      originalSwitch,
      "background refresh should preserve field focus while loading",
    );
    assert.doesNotMatch(
      domWindow.document.body.innerHTML,
      /animate-spin/,
      "background refresh should not replace the form with a spinner",
    );
    await act(async () => {
      originalSwitch.click();
    });
    assert.equal(
      originalSwitch.getAttribute("aria-checked"),
      "false",
      "an edit made during the background refresh should remain visible",
    );
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
      "an edit matching the old baseline must remain staged until refresh completes",
    );
    const finishFirstRefresh = releasePendingList;
    releasePendingList = undefined;
    assert.ok(finishFirstRefresh, "expected the delayed baseline request");
    await act(async () => {
      finishFirstRefresh();
      await flushEffects();
    });
    assert.strictEqual(
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      ),
      originalSwitch,
      "refresh completion should keep the same switch DOM node mounted",
    );
    assert.equal(
      domWindow.document
        .querySelector<HappyHTMLButtonElement>('button[role="switch"]')
        ?.getAttribute("aria-checked"),
      "false",
    );
    assert.strictEqual(
      domWindow.document.activeElement,
      originalSwitch,
      "background refresh should preserve field focus through completion",
    );
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
      "the edit should be compared with the refreshed server value",
    );

    // Save the edit against the refreshed baseline before exercising the
    // separate in-flight PATCH and section-reload race below.
    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });
    assert.equal(savedValue, false);
    assert.equal(patches.length, 2);
    assert.match(
      domWindow.document.body.textContent ?? "",
      /No unsaved changes/,
    );
    assert.equal(findButton(domWindow.document, "Save").disabled, true);

    await act(async () => {
      originalSwitch.click();
    });
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
      "a subsequent edit must be compared with the refreshed baseline",
    );

    delayNextPatch = true;
    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });
    assert.equal(patches.length, 3);
    assert.ok(releasePendingPatch, "expected the second patch to be pending");

    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    await act(async () => {
      findButton(domWindow.document, "Toggle section").click();
      await flushEffects();
    });
    await act(async () => {
      findButton(domWindow.document, "Toggle section").click();
      await flushEffects();
    });
    assert.equal(
      domWindow.document
        .querySelector<HappyHTMLButtonElement>('button[role="switch"]')
        ?.getAttribute("aria-checked"),
      "false",
      "a prefix reload must preserve a draft matching the old baseline while the save is in flight",
    );
    const finishSectionSave = releasePendingPatch;
    releasePendingPatch = undefined;
    assert.ok(finishSectionSave, "expected the delayed section save");
    await act(async () => {
      finishSectionSave();
      await flushEffects();
    });

    assert.equal(savedValue, true);
    assert.equal(
      domWindow.document
        .querySelector<HappyHTMLButtonElement>('button[role="switch"]')
        ?.getAttribute("aria-checked"),
      "false",
      "the newer in-flight edit should remain staged after the save completes",
    );
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
    );

    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });
    assert.equal(savedValue, false);
    assert.match(
      domWindow.document.body.textContent ?? "",
      /No unsaved changes/,
    );
    assert.equal(findButton(domWindow.document, "Save").disabled, true);

    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
      "a subsequent edit must use the value persisted by the follow-up save",
    );

    delayNextPatch = true;
    failNextPatch = true;
    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });
    assert.equal(patches.length, 5);
    assert.ok(releasePendingPatch, "expected the failing patch to be pending");

    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    const finishFailedSave: (() => void) | undefined =
      releasePendingPatch as (() => void) | undefined;
    releasePendingPatch = undefined;
    assert.ok(finishFailedSave, "expected the delayed failing save");
    await act(async () => {
      finishFailedSave();
      await flushEffects();
    });

    assert.equal(savedValue, false, "the failed patch must not change the server value");
    assert.equal(
      domWindow.document
        .querySelector<HappyHTMLButtonElement>('button[role="switch"]')
        ?.getAttribute("aria-checked"),
      "true",
      "the latest input should remain visible after the failed save",
    );
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
      "the failed value should remain staged for retry",
    );
    assert.match(
      domWindow.document.body.textContent ?? "",
      /mock gateway rejected save/,
      "the failed-save error should remain visible with the unsaved draft",
    );

    // A local group save can start a foreground reload while global Save all
    // is pending. The newer background reload must clear that spinner state.
    delayNextPatch = true;
    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });
    assert.equal(patches.length, 6);
    assert.ok(releasePendingPatch, "expected the overlapping global save");
    delayNextList = true;
    await act(async () => {
      findButton(domWindow.document, "Save").click();
      await flushEffects();
    });
    assert.equal(patches.length, 7);
    assert.ok(releasePendingList, "expected the group save reload to be pending");
    assert.match(
      domWindow.document.body.innerHTML,
      /animate-spin/,
      "the group save should show its foreground loading state before refresh replacement",
    );

    const finishOverlappingSave: (() => void) | undefined =
      releasePendingPatch as (() => void) | undefined;
    releasePendingPatch = undefined;
    assert.ok(finishOverlappingSave, "expected the delayed global save");
    await act(async () => {
      finishOverlappingSave();
      await flushEffects();
    });
    const finishGroupReload: (() => void) | undefined =
      releasePendingList as (() => void) | undefined;
    releasePendingList = undefined;
    assert.ok(finishGroupReload, "expected the shared group reload request");
    await act(async () => {
      finishGroupReload();
      await flushEffects();
    });
    assert.doesNotMatch(
      domWindow.document.body.innerHTML,
      /animate-spin/,
      "the latest background refresh should clear an older foreground loading state",
    );
    assert.ok(
      domWindow.document.querySelector('button[role="switch"]'),
      "the form should render after the overlapping save refresh completes",
    );
  } finally {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    globalThis.fetch = previousFetch;
    for (const key of globalKeys) {
      const descriptor = previous.get(key);
      if (descriptor) {
        Object.defineProperty(globalThis, key, descriptor);
      } else {
        Reflect.deleteProperty(globalThis, key);
      }
    }
    domWindow.close();
  }
});
