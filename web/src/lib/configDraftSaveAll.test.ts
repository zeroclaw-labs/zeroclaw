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
  let delayNextPatch = false;
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
      return new Response(
        JSON.stringify({
          entries: [
            {
              path: "channels.show_tool_calls",
              category: "channels",
              kind: "bool",
              type_hint: "bool",
              value: savedValue,
              populated: true,
              is_secret: false,
            },
          ],
        }),
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

    const { createRoot } = await import("react-dom/client");
    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
      root?.render(
        React.createElement(
          ConfigDraftProvider,
          null,
          React.createElement(
            React.Fragment,
            null,
            React.createElement(FieldForm, {
              prefix: "channels",
              includePath: (path: string) =>
                path === "channels.show_tool_calls",
              scopeActionsToIncludedPaths: true,
            }),
            React.createElement(UnsavedChangesBanner),
          ),
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

    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });

    assert.equal(savedValue, true);
    assert.equal(patches.length, 1);
    assert.ok(listCalls >= 2, "expected Save all to refresh the field baseline");
    assert.match(
      domWindow.document.body.textContent ?? "",
      /No unsaved changes/,
    );
    assert.equal(
      domWindow.document
        .querySelector<HappyHTMLButtonElement>('button[role="switch"]')
        ?.getAttribute("aria-checked"),
      "true",
    );
    const groupSaveButton = findButton(domWindow.document, "Save");
    assert.equal(groupSaveButton.disabled, true);

    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    assert.match(
      domWindow.document.body.textContent ?? "",
      /1 unsaved change/,
      "a change back to the old value must be compared with the new baseline",
    );

    delayNextPatch = true;
    await act(async () => {
      findButton(domWindow.document, "Save all").click();
      await flushEffects();
    });
    assert.equal(patches.length, 2);
    assert.ok(releasePendingPatch, "expected the second patch to be pending");

    await act(async () => {
      domWindow.document.querySelector<HappyHTMLButtonElement>(
        'button[role="switch"]',
      )?.click();
    });
    await act(async () => {
      releasePendingPatch?.();
      await flushEffects();
    });

    assert.equal(savedValue, false);
    assert.equal(
      domWindow.document
        .querySelector<HappyHTMLButtonElement>('button[role="switch"]')
        ?.getAttribute("aria-checked"),
      "true",
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
    assert.equal(savedValue, true);
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
      "a subsequent edit must use the value persisted by the second save",
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
