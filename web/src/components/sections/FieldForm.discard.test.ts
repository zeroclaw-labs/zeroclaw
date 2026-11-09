import assert from "node:assert/strict";
import test from "node:test";
import {
  Window,
  type HTMLInputElement as HappyInput,
  type Event as HappyEvent,
} from "happy-dom";
import { createJiti } from "jiti";
import * as React from "react";
import { act } from "react";
import type { PatchOp } from "../../lib/api.ts";

const flushEffects = () =>
  new Promise<void>((resolve) => setTimeout(resolve, 0));

const GLOBAL_KEYS = [
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
  "getComputedStyle",
  "requestAnimationFrame",
  "cancelAnimationFrame",
  "IS_REACT_ACT_ENVIRONMENT",
] as const;

async function withForm(
  run: (ctx: {
    container: Element;
    patches: PatchOp[][];
    values: Record<string, string>;
    button: (text: string) => HTMLButtonElement;
    editField: (path: string, value: string) => Promise<void>;
    fieldValue: (path: string) => string;
  }) => Promise<void>,
) {
  const domWindow = new Window();
  const container = domWindow.document.createElement("div");
  domWindow.document.body.appendChild(container);
  const previous = new Map<string, PropertyDescriptor | undefined>();
  for (const key of GLOBAL_KEYS) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
  }
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
    getComputedStyle: domWindow.getComputedStyle.bind(domWindow),
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

  const previousFetch = globalThis.fetch;
  const patches: PatchOp[][] = [];
  const values: Record<string, string> = {
    "browser.enabled": "true",
    "browser.session_name": "discard-saved",
    "browser.native_webdriver_url": "http://127.0.0.1:9515",
  };
  globalThis.fetch = async (input, init) => {
    const url = String(input);
    if (url === "/api/config" && init?.method === "OPTIONS") {
      return Response.json({});
    }
    if (url === "/api/config/sections") {
      return Response.json({
        sections: [{ key: "browser", label: "Browser" }],
      });
    }
    if (url === "/api/config/list?prefix=browser") {
      return Response.json({
        entries: Object.entries(values).map(([path, value]) => ({
          path,
          value,
          populated: true,
          is_secret: false,
          kind: path.endsWith("enabled") ? "bool" : "string",
          type_hint: path.endsWith("enabled") ? "bool" : "string",
        })),
      });
    }
    assert.equal(url, "/api/config");
    assert.equal(init?.method, "PATCH");
    const ops = JSON.parse(String(init.body)) as PatchOp[];
    patches.push(ops);
    for (const op of ops) values[op.path] = String(op.value);
    return Response.json({ saved: true, results: ops, warnings: [] });
  };

  let root:
    | { render: (children: React.ReactNode) => void; unmount: () => void }
    | undefined;
  try {
    const { createRoot } = await import("react-dom/client");
    const jiti = createJiti(import.meta.url, {
      fsCache: false,
      jsx: { runtime: "automatic" },
      alias: { "@": new URL("../../", import.meta.url).pathname },
    });
    const { default: FieldForm } =
      await jiti.import<typeof import("./FieldForm.tsx")>("./FieldForm.tsx");
    const { default: UnsavedChangesBanner } = await jiti.import<
      typeof import("../layout/UnsavedChangesBanner.tsx")
    >("../layout/UnsavedChangesBanner.tsx");
    const { ConfigDraftProvider } = await jiti.import<
      typeof import("../../lib/draftStore.tsx")
    >("../../lib/draftStore.tsx");
    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
      root?.render(
        React.createElement(
          ConfigDraftProvider,
          null,
          React.createElement(UnsavedChangesBanner),
          React.createElement(FieldForm, {
            prefix: "browser",
            title: "Browser",
          }),
        ),
      );
      await flushEffects();
    });
    const button = (text: string) => {
      const result = Array.from(container.querySelectorAll("button")).find(
        (b) => b.textContent?.trim() === text,
      );
      assert.ok(result, `expected button ${text}: ${container.textContent}`);
      return result as HTMLButtonElement;
    };
    const editField = async (path: string, value: string) => {
      await act(async () => {
        const input = domWindow.document.getElementById(path) as HappyInput | null;
        assert.ok(input, `expected input ${path}`);
        const setter = Object.getOwnPropertyDescriptor(
          Object.getPrototypeOf(input),
          "value",
        )?.set;
        assert.ok(setter);
        setter.call(input, value);
        input.dispatchEvent(
          new domWindow.Event("input", { bubbles: true }) as HappyEvent,
        );
        input.dispatchEvent(
          new domWindow.Event("change", { bubbles: true }) as HappyEvent,
        );
      });
    };
    const fieldValue = (path: string) => {
      const input = domWindow.document.getElementById(path) as HappyInput | null;
      assert.ok(input, `expected input ${path}`);
      return input.value;
    };
    await run({
      container: container as unknown as Element,
      patches,
      values,
      button,
      editField,
      fieldValue,
    });
  } finally {
    await act(async () => {
      root?.unmount();
    });
    globalThis.fetch = previousFetch;
    for (const key of GLOBAL_KEYS) {
      const descriptor = previous.get(key);
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    }
    await domWindow.happyDOM.close();
  }
}

test("Discard all restores mounted fields so a later section save excludes them", async () => {
  await withForm(async ({ container, patches, button, editField, fieldValue }) => {
    await editField("browser.session_name", "discarded-edit");
    assert.equal(fieldValue("browser.session_name"), "discarded-edit");
    assert.equal(button("Save").disabled, false);
    await act(async () => {
      button("Discard all").click();
      await flushEffects();
    });
    assert.equal(fieldValue("browser.session_name"), "discard-saved");
    assert.equal(button("Save").disabled, true);
    assert.match(container.textContent ?? "", /No unsaved changes/);
    assert.equal(patches.length, 0);

    await editField(
      "browser.native_webdriver_url",
      "http://127.0.0.1:9516",
    );
    await act(async () => {
      button("Save").click();
      await flushEffects();
    });
    assert.deepEqual(patches, [
      [
        {
          op: "replace",
          path: "browser.native_webdriver_url",
          value: "http://127.0.0.1:9516",
        },
      ],
    ]);
  });
});

test("Discard all after an edit and a persisted toggle keeps the toggle", async () => {
  await withForm(async ({ patches, button, editField, fieldValue }) => {
    await editField("browser.session_name", "discarded-edit");
    await act(async () => {
      button("enabled").click();
      await flushEffects();
    });
    assert.deepEqual(patches, [
      [{ op: "replace", path: "browser.enabled", value: false }],
    ]);
    await act(async () => {
      button("Discard all").click();
      await flushEffects();
    });
    assert.equal(fieldValue("browser.session_name"), "discard-saved");
    assert.equal(button("disabled").getAttribute("aria-pressed"), "false");
    await editField(
      "browser.native_webdriver_url",
      "http://127.0.0.1:9516",
    );
    await act(async () => {
      button("Save").click();
      await flushEffects();
    });
    assert.deepEqual(patches[patches.length - 1], [
      {
        op: "replace",
        path: "browser.native_webdriver_url",
        value: "http://127.0.0.1:9516",
      },
    ]);
    assert.equal(button("disabled").getAttribute("aria-pressed"), "false");
  });
});

test("Discard all after a persisted toggle then an edit keeps the toggle", async () => {
  await withForm(async ({ patches, button, editField, fieldValue }) => {
    await act(async () => {
      button("enabled").click();
      await flushEffects();
    });
    assert.deepEqual(patches, [
      [{ op: "replace", path: "browser.enabled", value: false }],
    ]);
    await editField("browser.session_name", "discarded-edit");
    await act(async () => {
      button("Discard all").click();
      await flushEffects();
    });
    assert.equal(fieldValue("browser.session_name"), "discard-saved");
    assert.equal(button("disabled").getAttribute("aria-pressed"), "false");
    assert.equal(button("Save").disabled, true);
  });
});

test("unsaved edits stay until Discard all", async () => {
  await withForm(async ({ patches, button, editField, fieldValue }) => {
    await editField("browser.session_name", "keep-until-save");
    assert.equal(fieldValue("browser.session_name"), "keep-until-save");
    await act(async () => {
      button("Save").click();
      await flushEffects();
    });
    assert.deepEqual(patches, [
      [
        {
          op: "replace",
          path: "browser.session_name",
          value: "keep-until-save",
        },
      ],
    ]);
  });
});
