import assert from "node:assert/strict";
import test from "node:test";
import React, { Fragment, createElement } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";

// The notice's text comes from the i18n module, which reads `window` when it loads.
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

const { MainColumn } = await import("./MainColumn.tsx");

type Host = { type: string; props: Record<string, unknown>; children: Host[] | null };

/** The host elements the column renders: what the browser lays out. */
async function column(viaRelay: boolean): Promise<Host | Host[] | null> {
  let tree: ReactTestRenderer | undefined;
  await act(async () => {
    tree = create(
      createElement(MainColumn, {
        viaRelay,
        children: createElement(
          Fragment,
          null,
          createElement("header", { "data-probe": "header" }),
          createElement("main", { "data-probe": "main" }),
        ),
      }),
    );
  });
  assert.ok(tree);
  return tree.toJSON() as Host | Host[] | null;
}

function windowHeightColumn(rendered: Host | Host[] | null): Host {
  // One element at the top, and it is the column: nothing above it pushes the page past the window.
  assert.ok(rendered && !Array.isArray(rendered), "the column renders one top-level element");
  assert.equal(rendered.type, "div");
  const classes = String(rendered.props.className ?? "").split(/\s+/);
  assert.ok(classes.includes("h-screen"), `column classes: ${rendered.props.className}`);
  return rendered;
}

test("through a relay the notice is the first row of the window-height column, above the header", async () => {
  const rows = windowHeightColumn(await column(true)).children ?? [];
  assert.equal(rows[0]?.type, "p");
  assert.equal(rows[0]?.props["data-relay-notice"], "");
  assert.deepEqual(
    rows.slice(1).map((row) => row.props["data-probe"]),
    ["header", "main"],
  );
});

test("a direct connection gets the same column with no notice", async () => {
  const rows = windowHeightColumn(await column(false)).children ?? [];
  assert.deepEqual(
    rows.map((row) => row.props["data-probe"]),
    ["header", "main"],
  );
});
