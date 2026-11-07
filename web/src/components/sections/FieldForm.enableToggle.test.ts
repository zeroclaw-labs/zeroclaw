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

for (const concurrentSave of [false, true]) {
  test(`FieldForm keeps the persisted toggle out of Save${concurrentSave ? " during a concurrent save" : ""}`, async () => {
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
      "getComputedStyle",
      "requestAnimationFrame",
      "cancelAnimationFrame",
      "IS_REACT_ACT_ENVIRONMENT",
    ] as const;
    const previous = new Map<string, PropertyDescriptor | undefined>();
    for (const key of globalKeys) {
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
      "browser.enabled": "false",
      "browser.timeout_ms": "5000",
    };
    let finishToggle: (() => void) | undefined;
    let holdToggle = concurrentSave;
    globalThis.fetch = async (input, init) => {
      const url = String(input);
      if (url === "/api/config" && init?.method === "OPTIONS") {
        return Response.json({});
      }
      if (url === "/api/config/list?prefix=browser") {
        return Response.json({
          entries: Object.entries(values).map(([path, value]) => ({
            path,
            value,
            populated: true,
            is_secret: false,
            kind: path.endsWith("enabled") ? "bool" : "integer",
            type_hint: path.endsWith("enabled") ? "bool" : "u64",
          })),
        });
      }
      assert.equal(url, "/api/config");
      assert.equal(init?.method, "PATCH");
      const ops = JSON.parse(String(init.body)) as PatchOp[];
      patches.push(ops);
      if (holdToggle && ops.some((op) => op.path === "browser.enabled")) {
        await new Promise<void>((resolve) => {
          finishToggle = resolve;
        });
      }
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
      const { ConfigDraftProvider } = await jiti.import<
        typeof import("../../lib/draftStore.tsx")
      >("../../lib/draftStore.tsx");
      root = createRoot(container as unknown as HTMLElement);
      await act(async () => {
        root?.render(
          React.createElement(
            ConfigDraftProvider,
            null,
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
        return result;
      };
      const editTimeout = async (value: string) => {
        await act(async () => {
          const input = domWindow.document.getElementById(
            "browser.timeout_ms",
          ) as HappyInput | null;
          assert.ok(input);
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
      assert.equal(button("Save").disabled, true);
      assert.match(container.textContent ?? "", /No unsaved changes/);
      await act(async () => {
        button("disabled").click();
        await flushEffects();
      });
      assert.deepEqual(patches[0], [
        { op: "replace", path: "browser.enabled", value: true },
      ]);
      if (concurrentSave) {
        await editTimeout("9000");
        await act(async () => {
          button("Save").click();
          await flushEffects();
        });
        assert.deepEqual(patches[1], [
          { op: "replace", path: "browser.timeout_ms", value: 9000 },
        ]);
        assert.ok(finishToggle);
        await act(async () => {
          finishToggle?.();
          await flushEffects();
        });
      }
      assert.equal(button("enabled").getAttribute("aria-pressed"), "true");
      assert.equal(button("Save").disabled, true);
      assert.match(
        container.textContent ?? "",
        concurrentSave ? /Saved 1 field/ : /No unsaved changes/,
      );
      holdToggle = false;
      await editTimeout("12000");
      await act(async () => {
        button("Save").click();
        await flushEffects();
      });
      assert.deepEqual(patches[patches.length - 1], [
        { op: "replace", path: "browser.timeout_ms", value: 12000 },
      ]);
      assert.equal(button("enabled").getAttribute("aria-pressed"), "true");
      assert.equal(button("Save").disabled, true);
      assert.match(container.textContent ?? "", /Saved 1 field/);
    } finally {
      await act(async () => {
        root?.unmount();
      });
      globalThis.fetch = previousFetch;
      for (const key of globalKeys) {
        const descriptor = previous.get(key);
        if (descriptor) Object.defineProperty(globalThis, key, descriptor);
        else Reflect.deleteProperty(globalThis, key);
      }
      await domWindow.happyDOM.close();
    }
  });
}
