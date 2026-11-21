import assert from "node:assert/strict";
import test from "node:test";
import { Window, type Event as HappyEvent, type HTMLInputElement as HappyInput } from "happy-dom";
import { createJiti } from "jiti";
import * as React from "react";
import { act } from "react";
import type { PatchOp, ValidationWarning } from "../../lib/api.ts";

const flushEffects = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

test("config saves and toggles retain warnings only for the relevant result", async () => {
  const domWindow = new Window();
  const container = domWindow.document.createElement("div");
  domWindow.document.body.appendChild(container);
  const globalKeys = [
    "window", "document", "navigator", "HTMLElement", "HTMLInputElement",
    "HTMLSelectElement", "HTMLButtonElement", "Node", "Element", "Event",
    "MouseEvent", "KeyboardEvent", "Text", "localStorage", "getComputedStyle",
    "requestAnimationFrame", "cancelAnimationFrame", "IS_REACT_ACT_ENVIRONMENT",
  ] as const;
  const previous = new Map<string, PropertyDescriptor | undefined>();
  for (const key of globalKeys) previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
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
    getComputedStyle: domWindow.getComputedStyle.bind(domWindow),
    requestAnimationFrame: (callback: FrameRequestCallback) => setTimeout(() => callback(Date.now()), 0),
    cancelAnimationFrame: (id: number) => clearTimeout(id),
    IS_REACT_ACT_ENVIRONMENT: true,
  };
  for (const [key, value] of Object.entries(globals)) {
    Object.defineProperty(globalThis, key, { configurable: true, value, writable: true });
  }

  const warning: ValidationWarning = {
    code: "pre_existing_validation_error",
    path: "nodes.mdns.peer_ttl_secs",
    message: "fixture retained diagnostic",
  };
  const otherWarning: ValidationWarning = {
    code: "pre_existing_validation_error",
    path: "gateway.host",
    message: "different retained diagnostic",
  };
  const ratePrefix = "cost.rates.providers.models.openai.fixture-model";
  const ratePath = `${ratePrefix}.input_per_mtok`;
  const values: Record<string, string> = {
    "browser.enabled": "false",
    "browser.timeout_ms": "5000",
    [ratePath]: "1",
  };
  let mode: "warning" | "other" | "none" | "error" = "warning";
  let failNextReload = false;
  const previousFetch = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const url = String(input);
    if (url === "/api/config" && init?.method === "OPTIONS") return Response.json({});
    if (url === "/api/config/sections") {
      return Response.json({ sections: [{ key: "browser", label: "Browser" }] });
    }
    if (url === "/api/config/list?prefix=browser") {
      if (failNextReload) {
        failNextReload = false;
        return Response.json({ code: "reload_failed", message: "fixture reload failed" }, { status: 500 });
      }
      return Response.json({
        entries: Object.entries(values).filter(([path]) => path.startsWith("browser.")).map(([path, value]) => ({
          path, value, populated: true, is_secret: false,
          kind: path.endsWith("enabled") ? "bool" : "integer",
          type_hint: path.endsWith("enabled") ? "bool" : "u64",
        })),
      });
    }
    if (url === "/api/config/map-keys?path=cost.rates.providers.models.openai") {
      return Response.json({ keys: ["fixture-model"] });
    }
    if (url === "/api/config/templates") return Response.json({ templates: [] });
    if (url === "/api/config/catalog/models?provider=openai") {
      return Response.json({ models: [], model_provider: "openai", local: false, live: false });
    }
    if (url === `/api/config/list?prefix=${encodeURIComponent(ratePrefix)}`) {
      return Response.json({ entries: [{
        path: ratePath, value: values[ratePath], populated: true, is_secret: false,
        kind: "integer", type_hint: "u64",
      }] });
    }
    if ((url === "/api/config/prop" && init?.method === "PUT") ||
        (url.startsWith("/api/config/prop?") && init?.method === "DELETE") ||
        (url.startsWith("/api/config/init") && init?.method === "POST")) {
      if (mode === "error") {
        return Response.json({ code: "invalid_format", message: "fixture save failed" }, { status: 400 });
      }
      return Response.json({ path: "fixture.path", initialized: ["fixture"], warnings: mode === "warning" ? [warning] : mode === "other" ? [otherWarning] : [] });
    }
    assert.equal(url, "/api/config");
    assert.equal(init?.method, "PATCH");
    if (mode === "error") {
      return Response.json({ code: "invalid_format", message: "fixture save failed" }, { status: 400 });
    }
    const ops = JSON.parse(String(init.body)) as PatchOp[];
    for (const op of ops) values[op.path] = String(op.value);
    return Response.json({ saved: true, results: ops, warnings: mode === "warning" ? [warning] : mode === "other" ? [otherWarning] : [] });
  };

  let root: { render: (children: React.ReactNode) => void; unmount: () => void } | undefined;
  try {
    const { createRoot } = await import("react-dom/client");
    const jiti = createJiti(import.meta.url, {
      fsCache: false,
      jsx: { runtime: "automatic" },
      alias: { "@": new URL("../../", import.meta.url).pathname },
    });
    const { default: FieldForm } = await jiti.import<typeof import("./FieldForm.tsx")>("./FieldForm.tsx");
    const { default: CostRatesEditor } = await jiti.import<typeof import("./CostRatesEditor.tsx")>(
      "./CostRatesEditor.tsx",
    );
    const { default: Banner } = await jiti.import<typeof import("../layout/UnsavedChangesBanner.tsx")>(
      "../layout/UnsavedChangesBanner.tsx",
    );
    const { default: Presenter } = await jiti.import<typeof import("../ConfigSaveWarnings.tsx")>(
      "../ConfigSaveWarnings.tsx",
    );
    const { ConfigDraftProvider } = await jiti.import<typeof import("../../lib/draftStore.tsx")>(
      "../../lib/draftStore.tsx",
    );
    const api = await jiti.import<typeof import("../../lib/api.ts")>("../../lib/api.ts");
    const auth = await jiti.import<typeof import("../../lib/auth.ts")>("../../lib/auth.ts");
    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
      root?.render(React.createElement(ConfigDraftProvider, null,
        React.createElement("div", { id: "banner-host" }, React.createElement(Banner)),
        React.createElement("div", { id: "form-host" },
          React.createElement(FieldForm, { prefix: "browser", title: "Browser" })),
        React.createElement("div", { id: "presenter-host" }, React.createElement(Presenter, {
          warnings: [
            { code: "security_audit_disabled_drops_certificate_record", path: "security.audit",
              message: "fallback not shown" },
            { code: "unknown_fixture", path: "fixture.path", message: "unknown fallback" },
          ],
        })),
      ));
      await flushEffects();
    });
    const banner = container.querySelector("#banner-host") as unknown as HTMLElement;
    const form = container.querySelector("#form-host") as unknown as HTMLElement;
    const presenter = container.querySelector("#presenter-host") as unknown as HTMLElement;
    const button = (host: HTMLElement, text: string): HTMLButtonElement => {
      const found = Array.from(host.querySelectorAll("button")).find((item) => item.textContent?.trim() === text);
      assert.ok(found, `expected button ${text}: ${host.textContent}`);
      return found;
    };
    const click = async (host: HTMLElement, text: string) => {
      await act(async () => { button(host, text).click(); await flushEffects(); });
    };
    const edit = async (value: string) => {
      await act(async () => {
        const input = domWindow.document.getElementById("browser.timeout_ms") as HappyInput | null;
        assert.ok(input);
        const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set;
        assert.ok(setter);
        setter.call(input, value);
        input.dispatchEvent(new domWindow.Event("input", { bubbles: true }) as HappyEvent);
        input.dispatchEvent(new domWindow.Event("change", { bubbles: true }) as HappyEvent);
      });
    };

    assert.match(presenter.textContent ?? "", /Audit logging is off/);
    assert.match(presenter.textContent ?? "", /unknown fallback/);
    assert.doesNotMatch(presenter.textContent ?? "", /fallback not shown/);

    // Writers without their own warning display use the persistent banner.
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    mode = "other";
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    assert.match(banner.textContent ?? "", /different retained diagnostic/);
    assert.doesNotMatch(banner.textContent ?? "", /fixture retained diagnostic/);
    mode = "none";
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    assert.equal(banner.textContent, "", "a later clean write clears an old global warning");
    mode = "warning";
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    await act(async () => {
      await api.patchConfig([{ op: "replace", path: "fixture.path", value: true }]);
      await api.deleteProp("fixture.path");
      await api.initSection("fixture");
      await flushEffects();
    });
    assert.equal((banner.textContent?.match(/fixture retained diagnostic/g) ?? []).length, 1);
    await edit("5500");
    assert.match(banner.textContent ?? "", /unsaved change/);
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    await click(banner, "Discard all");
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    mode = "none";
    await edit("5500");
    await click(form, "Save");
    assert.equal(banner.textContent, "", "a clean local save clears an old global warning");
    mode = "warning";
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    await click(banner, "Dismiss warnings");
    mode = "none";
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    assert.equal(banner.textContent, "");
    mode = "error";
    await act(async () => {
      await assert.rejects(api.putProp("fixture.path", true));
      await flushEffects();
    });
    assert.equal(banner.textContent, "");
    mode = "warning";

    // Warning text belongs to the authority that received it, including when
    // an old response arrives after the session loses config access.
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    await act(async () => {
      domWindow.dispatchEvent(new domWindow.StorageEvent("storage", { key: "unrelated-chat" }));
      await flushEffects();
    });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    await act(async () => {
      domWindow.dispatchEvent(new domWindow.StorageEvent("storage", { key: "zeroclaw_token" }));
      await flushEffects();
    });
    assert.equal(banner.textContent, "");
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    await act(async () => {
      domWindow.dispatchEvent(new domWindow.StorageEvent("storage"));
      await flushEffects();
    });
    assert.equal(banner.textContent, "");
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    const session = {
      kind: "oidc" as const, provider: "oidc.fixture", principal_id: "fixture-user",
      display_name: "Fixture", admin: false, expires_at: 4_000_000_000,
      csrf_token: "fixture-csrf", features: { chat: true, operator_pages: true, config: true },
    };
    await act(async () => { auth.setOidcSession(session); await flushEffects(); });
    assert.equal(banner.textContent, "");
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    await act(async () => {
      domWindow.dispatchEvent(new domWindow.StorageEvent("storage", { key: "zeroclaw_token" }));
      await flushEffects();
    });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    const mockFetch = globalThis.fetch;
    let finishWrite!: (response: Response) => void;
    const delayed = new Promise<Response>((resolve) => { finishWrite = resolve; });
    globalThis.fetch = async () => delayed;
    const pendingWrite = api.patchConfig([{ op: "replace", path: "fixture.path", value: true }]);
    await act(async () => {
      auth.setOidcSession({ ...session, features: { ...session.features, config: false } });
      // Even an intervening return to the same session object must not make
      // an old response authoritative again.
      auth.setOidcSession(session);
      globalThis.fetch = mockFetch;
      await api.putProp("fixture.path", true);
      finishWrite(Response.json({ saved: true, results: [], warnings: [warning] }));
      assert.deepEqual((await pendingWrite).warnings, []);
      await flushEffects();
    });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/,
      "a stale response must not clear the current authority's warning");
    await click(banner, "Dismiss warnings");
    globalThis.fetch = mockFetch;
    await act(async () => { auth.setOidcSession(null); await flushEffects(); });

    let finishPairingWrite!: (response: Response) => void;
    const delayedPairingWrite = new Promise<Response>((resolve) => { finishPairingWrite = resolve; });
    globalThis.fetch = async () => delayedPairingWrite;
    const pendingPairingWrite = api.patchConfig([{ op: "replace", path: "fixture.path", value: true }]);
    await act(async () => {
      domWindow.dispatchEvent(new domWindow.StorageEvent("storage", { key: "zeroclaw_token" }));
      finishPairingWrite(Response.json({ saved: true, results: [], warnings: [warning] }));
      assert.deepEqual((await pendingPairingWrite).warnings, []);
      await flushEffects();
    });
    assert.equal(banner.textContent, "");
    globalThis.fetch = mockFetch;

    await act(async () => { auth.setToken("fixture-token-a"); await flushEffects(); });
    let finishLocalPairingWrite!: (response: Response) => void;
    const delayedLocalPairingWrite = new Promise<Response>((resolve) => { finishLocalPairingWrite = resolve; });
    globalThis.fetch = async () => delayedLocalPairingWrite;
    const pendingLocalPairingWrite = api.patchConfig([{ op: "replace", path: "fixture.path", value: true }]);
    await act(async () => {
      auth.setToken("fixture-token-b");
      auth.setToken("fixture-token-a");
      finishLocalPairingWrite(Response.json({ saved: true, results: [], warnings: [warning] }));
      assert.deepEqual((await pendingLocalPairingWrite).warnings, []);
      await flushEffects();
    });
    assert.equal(banner.textContent, "");
    globalThis.fetch = mockFetch;
    await act(async () => { await api.putProp("fixture.path", true); await flushEffects(); });
    await act(async () => { auth.setToken("fixture-token-a"); await flushEffects(); });
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    await act(async () => { auth.clearToken(); await flushEffects(); });
    assert.equal(banner.textContent, "");

    await edit("6000");
    await click(form, "Save");
    assert.match(form.textContent ?? "", /Saved 1 field/);
    assert.match(
      form.querySelector('[role="status"]')?.textContent ?? "",
      /nodes\.mdns\.peer_ttl_secs.*fixture retained diagnostic/,
    );
    assert.equal(form.querySelector('[role="status"]')?.getAttribute("aria-live"), "polite");
    assert.doesNotMatch(banner.textContent ?? "", /fixture retained diagnostic/);
    await act(async () => { auth.setOidcSession(session); await flushEffects(); });
    assert.equal(form.querySelector('[role="status"]'), null);
    await act(async () => { auth.setOidcSession(null); await flushEffects(); });
    await edit("6500");
    failNextReload = true;
    await click(form, "Save");
    assert.match(form.textContent ?? "", /fixture reload failed/);
    assert.match(form.querySelector('[role="status"]')?.textContent ?? "", /fixture retained diagnostic/);
    mode = "none";
    await edit("7000");
    await click(form, "Save");
    assert.match(form.textContent ?? "", /Saved 1 field/);
    assert.equal(form.querySelector('[role="status"]'), null);
    mode = "warning";
    await edit("8000");
    await click(form, "Save");
    mode = "error";
    await edit("9000");
    await click(form, "Save");
    assert.match(form.textContent ?? "", /fixture save failed/);
    assert.equal(form.querySelector('[role="status"]'), null);

    mode = "warning";
    await click(banner, "Save all");
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    assert.doesNotMatch(banner.textContent ?? "", /unsaved change/);
    await click(banner, "Dismiss warnings");
    assert.equal(banner.textContent, "");
    await edit("10000");
    await click(banner, "Save all");
    assert.match(banner.textContent ?? "", /fixture retained diagnostic/);
    await edit("11000");
    assert.match(banner.textContent ?? "", /unsaved change/);
    assert.doesNotMatch(banner.textContent ?? "", /fixture retained diagnostic/);
    mode = "none";
    await click(banner, "Save all");
    assert.equal(banner.textContent, "");
    await edit("12000");
    mode = "error";
    await click(banner, "Save all");
    assert.match(banner.textContent ?? "", /fixture save failed/);
    assert.doesNotMatch(banner.textContent ?? "", /fixture retained diagnostic/);
    await click(banner, "Discard all");
    assert.equal(banner.textContent, "");

    mode = "warning";
    await click(form, "disabled");
    assert.equal(button(form, "enabled").getAttribute("aria-pressed"), "true");
    assert.match(form.querySelector('[role="status"]')?.textContent ?? "", /fixture retained diagnostic/);
    mode = "none";
    await click(form, "enabled");
    assert.equal(form.querySelector('[role="status"]'), null);
    mode = "warning";
    await click(form, "disabled");
    await click(form, "Dismiss warnings");
    assert.equal(form.querySelector('[role="status"]'), null);

    let savedRates = 0;
    await act(async () => {
      root?.render(React.createElement(ConfigDraftProvider, null,
        React.createElement(CostRatesEditor, {
          category: "models", providerType: "openai", onSaved: () => { savedRates += 1; },
        }),
      ));
      await flushEffects();
    });
    const rateRow = Array.from(container.querySelectorAll("button"))
      .find((item) => item.textContent?.includes("models.openai.fixture-model"));
    assert.ok(rateRow, `expected rate row: ${container.textContent}`);
    await act(async () => { rateRow.click(); await flushEffects(); });
    const editRate = async (value: string) => {
      await act(async () => {
        const input = domWindow.document.getElementById(ratePath) as HappyInput | null;
        assert.ok(input);
        const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set;
        assert.ok(setter);
        setter.call(input, value);
        input.dispatchEvent(new domWindow.Event("input", { bubbles: true }) as HappyEvent);
        input.dispatchEvent(new domWindow.Event("change", { bubbles: true }) as HappyEvent);
      });
    };
    mode = "warning";
    await editRate("2");
    await click(container as unknown as HTMLElement, "Save");
    assert.equal(values[ratePath], "2");
    assert.equal(savedRates, 1);
    assert.match(container.querySelector('[role="status"]')?.textContent ?? "", /fixture retained diagnostic/);
    assert.equal((domWindow.document.getElementById(ratePath) as HappyInput).value, "2");
    assert.equal(button(container as unknown as HTMLElement, "Save").disabled, true);
    mode = "none";
    await editRate("3");
    await click(container as unknown as HTMLElement, "Save");
    assert.equal(values[ratePath], "3");
    assert.equal(savedRates, 2);
    assert.equal(container.querySelector('[role="status"]'), null);
  } finally {
    await act(async () => { root?.unmount(); });
    globalThis.fetch = previousFetch;
    for (const key of globalKeys) {
      const descriptor = previous.get(key);
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    }
    await domWindow.happyDOM.close();
  }
});
