// Tests for docs/book/theme/version-selector.js: the label, the selected
// entry and each version's link, from the version-less stable copy, a pinned
// release and master. The script runs unmodified in a VM context with just
// enough of a DOM for it to build its menu.
//
//   node --test scripts/docs/version_selector_test.mjs

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const SOURCE = readFileSync(
  new URL("../../docs/book/theme/version-selector.js", import.meta.url),
  "utf8",
);

const VERSIONS = {
  stable: "v0.8.7",
  versions: [
    { tag: "master", label: "master (dev)" },
    { tag: "v0.8.7", label: "v0.8.7 (stable)" },
    { tag: "v0.8.6", label: "v0.8.6" },
  ],
};

class FakeElement {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.attributes = {};
    this.style = {};
    this.classes = new Set();
    this.classList = { add: (name) => this.classes.add(name) };
    this.textContent = "";
  }
  setAttribute(name, value) {
    this.attributes[name] = String(value);
  }
  appendChild(child) {
    this.children.push(child);
    return child;
  }
  prepend(child) {
    this.children.unshift(child);
  }
  addEventListener() {}
  contains() {
    return false;
  }
}

// Run the script at `pathname` and return what it rendered: the button's
// label, the selected entry's tag and each entry's link, keyed by tag.
async function render(pathname, { hash = "", versions = VERSIONS } = {}) {
  const menuRight = new FakeElement("div");
  const fetched = [];
  const context = {
    window: { location: { pathname, hash } },
    document: {
      querySelector: (selector) =>
        selector === ".menu-bar .right-buttons" ? menuRight : null,
      createElement: (tag) => new FakeElement(tag),
      addEventListener() {},
    },
    fetch: async (url) => {
      fetched.push(url);
      return { ok: true, json: async () => versions };
    },
    console: { debug() {} },
  };
  vm.runInNewContext(SOURCE, context);
  // The menu is built after fetch() and json() resolve.
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setImmediate(resolve));

  const wrapper = menuRight.children[0];
  if (!wrapper) return { fetched, rendered: false };
  const [button, list] = wrapper.children;
  const entries = list.children.map((li) => ({
    tag: versions.versions.find((v) => v.label === li.children[0].textContent).tag,
    selected: li.classes.has("theme-selected"),
    href: li.children[0].href,
  }));
  return {
    fetched,
    rendered: true,
    label: button.textContent,
    selected: entries.filter((e) => e.selected).map((e) => e.tag),
    links: Object.fromEntries(entries.map((e) => [e.tag, e.href])),
  };
}

test("the stable alias shows the stable release and links to itself", async () => {
  const shown = await render("/stable/en/setup/linux.html");
  assert.deepEqual(shown.fetched, ["/versions.json"]);
  assert.equal(shown.label, "v0.8.7 (stable)");
  assert.deepEqual(shown.selected, ["v0.8.7"]);
  assert.deepEqual(shown.links, {
    master: "/master/en/setup/linux.html",
    "v0.8.7": "/stable/en/setup/linux.html",
    "v0.8.6": "/v0.8.6/en/setup/linux.html",
  });
});

test("a pinned older release links its stable entry to the alias", async () => {
  const shown = await render("/v0.8.6/en/setup/linux.html");
  assert.equal(shown.label, "v0.8.6");
  assert.deepEqual(shown.selected, ["v0.8.6"]);
  assert.equal(shown.links["v0.8.7"], "/stable/en/setup/linux.html");
  assert.equal(shown.links["v0.8.6"], "/v0.8.6/en/setup/linux.html");
});

test("the stable release's own directory still links Stable to the alias", async () => {
  const shown = await render("/v0.8.7/en/setup/linux.html");
  assert.equal(shown.label, "v0.8.7 (stable)");
  assert.deepEqual(shown.selected, ["v0.8.7"]);
  assert.equal(shown.links["v0.8.7"], "/stable/en/setup/linux.html");
});

test("master shows itself and links Stable to the alias", async () => {
  const shown = await render("/master/en/setup/linux.html");
  assert.equal(shown.label, "master (dev)");
  assert.deepEqual(shown.selected, ["master"]);
  assert.equal(shown.links.master, "/master/en/setup/linux.html");
  assert.equal(shown.links["v0.8.7"], "/stable/en/setup/linux.html");
});

test("links keep the page anchor and a project-page base path", async () => {
  const shown = await render("/zeroclaw/stable/ja/setup/linux.html", { hash: "#install" });
  assert.deepEqual(shown.fetched, ["/zeroclaw/versions.json"]);
  assert.equal(shown.links["v0.8.6"], "/zeroclaw/v0.8.6/ja/setup/linux.html#install");
  assert.equal(shown.links["v0.8.7"], "/zeroclaw/stable/ja/setup/linux.html#install");
});

test("without a stable release in versions.json, the alias falls back to its own name", async () => {
  const versions = { versions: VERSIONS.versions };
  const shown = await render("/v0.8.6/en/index.html", { versions });
  assert.equal(shown.links["v0.8.7"], "/v0.8.7/en/index.html");
  const alias = await render("/stable/en/index.html", { versions });
  assert.equal(alias.label, "stable");
  assert.deepEqual(alias.selected, []);
});

test("a page outside a versioned path renders nothing", async () => {
  const shown = await render("/index.html");
  assert.equal(shown.rendered, false);
  assert.deepEqual(shown.fetched, []);
});
