// Tests for the docs site's root 404 redirect (xtask/src/cmd/mdbook/root_404.js,
// written into 404.html by `gen-root-404`): which missing paths go to the same
// page under the stable alias, and which stay on the not-found message. The
// script runs unmodified in a VM context with the `target` and `locales`
// values 404.html sets before it.
//
//   node --test scripts/docs/root_404_test.mjs

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const SOURCE = readFileSync(
  new URL("../../xtask/src/cmd/mdbook/root_404.js", import.meta.url),
  "utf8",
);

// Where the 404 page sends `url`, or null when it stays on the message.
function redirect(url, { target = "stable", locales = ["en", "fr", "zh-CN"] } = {}) {
  const parsed = new URL(url, "https://docs.example");
  let replaced = null;
  const location = {
    pathname: parsed.pathname,
    search: parsed.search,
    hash: parsed.hash,
    replace: (next) => {
      replaced = next;
    },
  };
  vm.runInNewContext(SOURCE, { location, target, locales });
  return replaced;
}

test("a pruned release goes to the same page under the stable alias", () => {
  assert.equal(redirect("/v0.7.5/en/setup/linux.html"), "/stable/en/setup/linux.html");
  assert.equal(redirect("/v0.8.4/zh-CN/index.html"), "/stable/zh-CN/index.html");
});

test("every prerelease tag form the deploy workflow accepts is a version", () => {
  // The deploy workflow validates tags as v<major>.<minor>.<patch>[-<pre>]
  // with <pre> in [a-zA-Z0-9._-], and parse_version takes any of them.
  for (const tag of ["v0.8.0-beta_1", "v0.8.0-beta.1", "v0.8.0-rc-2", "v0.8.0-RC1", "v1.2.3-a.b_c-d"]) {
    assert.equal(redirect(`/${tag}/en/setup/linux.html`), "/stable/en/setup/linux.html", tag);
  }
});

test("a missing master page and the pre-versioned layout go to the alias too", () => {
  assert.equal(redirect("/master/fr/gone.html"), "/stable/fr/gone.html");
  assert.equal(redirect("/en/setup/linux.html"), "/stable/en/setup/linux.html");
});

test("a trailing slash, the query and the anchor are kept", () => {
  assert.equal(redirect("/v0.8.0-beta_1/fr/"), "/stable/fr/");
  assert.equal(redirect("/v0.7.5/en/setup/linux.html?ref=old#install"), "/stable/en/setup/linux.html?ref=old#install");
});

test("without a stable pointer the target is master", () => {
  assert.equal(redirect("/v0.7.5/en/setup/linux.html", { target: "master" }), "/master/en/setup/linux.html");
});

test("other missing paths stay on the not-found message", () => {
  for (const url of [
    "/stable/en/missing.html", // already under the alias: no loop
    "/v0.7.5/de/setup/linux.html", // a locale the stable release doesn't have
    "/api/zeroclaw/missing.html",
    "/v0.8/en/setup/linux.html", // not a version
    "/V0.8.0/en/setup/linux.html",
    "/v0.8.0+build/en/setup/linux.html",
    "/v0.8.0-beta_1/",
    "/",
  ]) {
    assert.equal(redirect(url), null, url);
  }
});
