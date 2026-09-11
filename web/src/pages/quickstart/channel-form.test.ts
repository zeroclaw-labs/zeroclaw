import assert from "node:assert/strict";
import test from "node:test";
import {
  Window,
  type Document as HappyDocument,
  type Event as HappyEvent,
  type HTMLButtonElement as HappyHTMLButtonElement,
  type HTMLInputElement as HappyHTMLInputElement,
  type HTMLSelectElement as HappyHTMLSelectElement,
} from "happy-dom";
import { createJiti } from "jiti";
import * as React from "react";
import { act } from "react";
import type { QuickstartFieldDescriptor, QuickstartState } from "../../lib/api.ts";
import type { ChannelAddFormProps } from "./ChannelAddForm.tsx";

const descriptor = (
  key: string,
  label: string,
  defaultValue: string | null,
  isSecret = false,
): QuickstartFieldDescriptor => ({
  key,
  label,
  help: "",
  kind: "string",
  is_secret: isSecret,
  enum_variants: null,
  required: true,
  default: defaultValue,
});

const state: QuickstartState = {
  quickstart_completed: false,
  agents: [],
  risk_profiles: [],
  runtime_profiles: [],
  model_providers: [],
  channels: [],
  unassigned_channels: [],
  storage: [],
  model_provider_types: [],
  channel_types: [
    { kind: "webhook", display_name: "Webhook", local: false },
  ],
  risk_presets: [],
  runtime_presets: [],
  memory_kinds: [],
  personality_files: [],
};

function flushEffects(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function findButton(document: HappyDocument, text: string): HappyHTMLButtonElement {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  assert.ok(button, `expected button ${text}`);
  return button as HappyHTMLButtonElement;
}

function findInput(document: HappyDocument, text: string): HappyHTMLInputElement {
  const label = Array.from(document.querySelectorAll("label")).find(
    (candidate) => candidate.textContent?.includes(text),
  );
  assert.ok(label, `expected labeled input ${text}`);
  const input = label.querySelector("input");
  assert.ok(input, `expected input under label ${text}`);
  return input as HappyHTMLInputElement;
}

function findInputOrNull(
  document: HappyDocument,
  text: string,
): HappyHTMLInputElement | null {
  const label = Array.from(document.querySelectorAll("label")).find(
    (candidate) => candidate.textContent?.includes(text),
  );
  return (label?.querySelector("input") as HappyHTMLInputElement | null) ?? null;
}

function setInputValue(input: HappyHTMLInputElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(
    Object.getPrototypeOf(input),
    "value",
  )?.set;
  assert.ok(setter, "expected a native input value setter");
  setter.call(input, value);
  input.dispatchEvent(
    new Event("input", { bubbles: true }) as unknown as HappyEvent,
  );
  input.dispatchEvent(
    new Event("change", { bubbles: true }) as unknown as HappyEvent,
  );
}

test("ChannelAddForm preserves edited fields across real mode interactions", async () => {
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

  let root:
    | { render: (children: React.ReactNode) => void; unmount: () => void }
    | undefined;
  try {
    const { createRoot } = await import("react-dom/client");
    const jiti = createJiti(import.meta.url, {
      fsCache: false,
      moduleCache: false,
      jsx: { runtime: "automatic" },
    });
    const module = await jiti.import<typeof import("./ChannelAddForm.tsx")>(
      "./ChannelAddForm.tsx",
    );
    const ChannelAddForm = module.ChannelAddForm as React.ComponentType<
      ChannelAddFormProps
    >;
    let loadCount = 0;
    const loadFields: ChannelAddFormProps["loadFields"] = async () => {
      loadCount += 1;
      return {
        fields: [
          descriptor("port", "Port", "8090"),
          descriptor("secret", "Secret", null, true),
        ],
      };
    };

    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
      root?.render(
        React.createElement(ChannelAddForm, {
          state,
          inConfig: new Set<string>(),
          inFlight: new Set<string>(),
          reusable: ["webhook.default"],
          onAdd: () => {},
          onCancel: () => {},
          loadFields,
        }),
      );
    });

    await act(async () => {
      findButton(domWindow.document, "Create new").click();
    });

    const typeSelect = domWindow.document.querySelector(
      "select",
    ) as HappyHTMLSelectElement | null;
    assert.ok(typeSelect, "expected channel type selector");
    await act(async () => {
      typeSelect.value = "webhook";
      typeSelect.dispatchEvent(new domWindow.Event("change", { bubbles: true }));
      await flushEffects();
    });

    const port = findInput(domWindow.document, "Port");
    const secret = findInput(domWindow.document, "Secret");
    await act(async () => {
      setInputValue(port, "9123");
      setInputValue(secret, "user-entered-secret");
    });

    await act(async () => {
      findButton(domWindow.document, "Use existing").click();
      await flushEffects();
    });

    assert.ok(
      domWindow.document.querySelector('option[value="webhook.default"]'),
      "expected existing-channel control after switching modes",
    );
    assert.equal(
      findInputOrNull(domWindow.document, "Port"),
      null,
      "fresh channel inputs must be absent in existing mode",
    );
    assert.equal(
      findInputOrNull(domWindow.document, "Secret"),
      null,
      "fresh channel inputs must be absent in existing mode",
    );

    await act(async () => {
      findButton(domWindow.document, "Create new").click();
      await flushEffects();
    });

    const livePort = findInput(domWindow.document, "Port");
    const liveSecret = findInput(domWindow.document, "Secret");
    assert.equal(livePort.value, "9123");
    assert.equal(liveSecret.value, "user-entered-secret");
    assert.equal(loadCount, 1, "mode changes must not reload channel descriptors");
  } finally {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
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
