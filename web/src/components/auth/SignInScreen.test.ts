import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import React, { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { OidcMethod } from "../../lib/authSession.ts";
import type { SignOutNotice } from "../../hooks/useAuth.ts";

const fakeWindow = Object.assign(new EventTarget(), {
  location: {
    protocol: "https:",
    host: "zc.example",
    hostname: "zc.example",
    port: "",
    pathname: "/",
    search: "",
  },
});
// The jiti CLI compiles JSX with the classic runtime, which needs React in scope.
Object.assign(globalThis, { window: fakeWindow, React });
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;
// The pairing dialog asks for a pairing code on mount; this gateway has none.
globalThis.fetch = async () => new Response("", { status: 404 });

const { PairingDialog, SignInScreen } = await import("./SignInScreen.tsx");

const onPair = async () => {};
const corp: OidcMethod = { alias: "corp", provider: "oidc.corp" };
const okta: OidcMethod = { alias: "okta", provider: "oidc.okta" };

function render(providers: OidcMethod[], notice: SignOutNotice | null = null): string {
  return renderToStaticMarkup(
    createElement(SignInScreen, { onPair, providers, onSignIn: () => {}, notice }),
  );
}

const count = (html: string, needle: string) => html.split(needle).length - 1;

test("sign-in off: exactly today's pairing dialog", () => {
  const today = renderToStaticMarkup(createElement(PairingDialog, { onPair }));
  const off = render([]);
  assert.equal(off, today);
  assert.equal(count(off, "data-signin"), 0);
  assert.equal(count(off, "Sign in with"), 0);
  assert.equal(count(off, "or pair this browser"), 0);
  // Today's first paint: title, status line, one input, one button.
  assert.ok(off.includes("Checking pairing status…"));
  assert.equal(count(off, "<form"), 1);
  assert.equal(count(off, "<input"), 1);
  assert.equal(count(off, "<button"), 1);
});

test("sign-in on: a button per provider above the unchanged pairing form", () => {
  const html = render([corp, okta]);
  assert.equal(count(html, "data-signin-provider="), 2);
  assert.ok(html.includes("Sign in with corp"));
  assert.ok(html.includes("Sign in with okta"));
  assert.ok(html.includes("or pair this browser"));
  assert.ok(html.indexOf("Sign in with corp") < html.indexOf("<form"));
  assert.equal(count(html, "<form"), 1);
  assert.equal(count(html, "<input"), 1);
  assert.equal(count(html, "<button"), 3);
});

test("after sign-out the screen says the provider session may remain", () => {
  const html = render([corp], { kind: "signed-out", provider: "corp" });
  assert.ok(html.includes("signed out of ZeroClaw. You may still be signed in at corp."));
  const ended = render([corp], { kind: "session-ended" });
  assert.ok(ended.includes("Your session has ended."));
});

test("until the gateway says how to sign in, the screen offers no pairing form", async () => {
  const screen = (methods: "checking" | "failed", viaRelay: boolean, providers: OidcMethod[]) =>
    renderToStaticMarkup(
      createElement(SignInScreen, {
        onPair,
        providers,
        onSignIn: () => {},
        notice: null,
        viaRelay,
        methods,
        onRetryMethods: () => {},
      }),
    );
  for (const methods of ["checking", "failed"] as const) {
    for (const viaRelay of [false, true]) {
      for (const providers of [[], [corp]]) {
        const html = screen(methods, viaRelay, providers);
        assert.equal(count(html, "<form"), 0, `${methods} ${viaRelay}`);
        assert.equal(count(html, "<input"), 0, `${methods} ${viaRelay}`);
        assert.equal(count(html, "pairing code"), 0, `${methods} ${viaRelay}`);
        assert.equal(count(html, "data-signin-provider="), 0, `${methods} ${viaRelay}`);
        assert.equal(count(html, "data-methods-retry"), methods === "failed" ? 1 : 0);
        assert.equal(count(html, "data-relay-notice"), viaRelay ? 1 : 0);
      }
    }
  }
  assert.ok(screen("failed", false, []).includes("check how to sign in"));
  assert.ok(screen("checking", false, []).includes("Checking how to sign in"));
});

test("through a relay the screen offers sign-in alone, never pairing", async () => {
  const relayed = (providers: OidcMethod[], notice: SignOutNotice | null = null) =>
    createElement(SignInScreen, { onPair, providers, onSignIn: () => {}, notice, viaRelay: true });
  for (const providers of [[corp, okta], [corp], []]) {
    const html = renderToStaticMarkup(relayed(providers));
    assert.equal(count(html, "data-relay-notice"), 1);
    assert.ok(html.includes("its operator can read and change this session"));
    // No pairing form, code or divider leading to one.
    assert.equal(count(html, "<form"), 0);
    assert.equal(count(html, "<input"), 0);
    assert.equal(count(html, "pairing code"), 0);
    assert.equal(count(html, "pairing status"), 0);
    assert.equal(count(html, "or pair this browser"), 0);
    assert.equal(count(html, "data-signin-provider="), providers.length);
    // With no provider, the screen says sign-in is missing instead.
    assert.equal(count(html, "data-relay-no-sign-in"), providers.length === 0 ? 1 : 0);
  }
  assert.ok(
    renderToStaticMarkup(relayed([])).includes("configured for this relay"),
  );
  const ended = renderToStaticMarkup(relayed([corp], { kind: "session-ended" }));
  assert.ok(ended.includes("Your session has ended."));
  assert.equal(count(ended, "<input"), 0);

  // Mounted, it asks the gateway for no pairing code either.
  const asked: string[] = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = async (input: RequestInfo | URL) => {
    asked.push(String(input));
    return new Response("", { status: 404 });
  };
  try {
    for (const providers of [[corp], []]) {
      let renderer: ReactTestRenderer | undefined;
      await act(async () => {
        renderer = create(relayed(providers));
      });
      await act(async () => {
        renderer!.unmount();
      });
    }
  } finally {
    globalThis.fetch = realFetch;
  }
  assert.deepEqual(asked, []);

  // A direct connection is unchanged: no relay notice.
  assert.equal(count(render([corp]), "data-relay-notice"), 0);
  assert.equal(count(render([]), "data-relay-notice"), 0);
});

test("the notice shows even if sign-in was turned off meanwhile", () => {
  const html = render([], { kind: "session-ended" });
  assert.ok(html.includes("Your session has ended."));
  assert.equal(count(html, "data-signin-provider="), 0);
});

test("choosing a provider starts its sign-in once", async () => {
  const started: string[] = [];
  let renderer: ReactTestRenderer | undefined;
  await act(async () => {
    renderer = create(
      createElement(SignInScreen, {
        onPair,
        providers: [corp, okta],
        onSignIn: (alias: string) => started.push(alias),
        notice: null,
      }),
    );
  });
  assert.ok(renderer);
  const button = (alias: string) =>
    renderer!.root.find(
      (node) => node.type === "button" && node.props["data-signin-provider"] === alias,
    );
  await act(async () => {
    button("okta").props.onClick();
  });
  assert.deepEqual(started, ["okta"]);
  assert.equal(button("okta").props.disabled, true);
  assert.equal(button("corp").props.disabled, true);
  await act(async () => {
    renderer!.unmount();
  });
});

test("every shipped locale has the sign-in strings, placeholders intact", () => {
  const source = readFileSync(new URL("../../lib/i18n.ts", import.meta.url), "utf8");
  const blocks = new Map<string, string>();
  const header = /^ {2}([a-z]{2}): \{$/gm;
  const starts = [...source.matchAll(header)];
  starts.forEach((match, i) => {
    const end = starts[i + 1]?.index ?? source.length;
    blocks.set(match[1] ?? "", source.slice(match.index ?? 0, end));
  });
  assert.equal(blocks.size, 31);
  const en = blocks.get("en") ?? "";
  const keys = [...en.matchAll(/^ {4}'(auth\.(?:signin|account|reauth|sign_out|not_available)[a-z_.]*)'/gm)].map(
    (m) => m[1] ?? "",
  );
  assert.equal(keys.length, 22);
  for (const [locale, block] of blocks) {
    for (const key of keys) {
      const line = block.split("\n").find((l) => l.startsWith(`    '${key}':`));
      assert.ok(line, `${locale} lacks ${key}`);
      const enLine = en.split("\n").find((l) => l.startsWith(`    '${key}':`)) ?? "";
      if (enLine.includes("{provider}")) {
        assert.ok(line.includes("{provider}"), `${locale} ${key} lost {provider}`);
      }
    }
  }
});
