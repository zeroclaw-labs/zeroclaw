import assert from "node:assert/strict";
import { test } from "node:test";

import type { ListResponseEntry } from "../../lib/api.ts";
import {
  groupSecretMapEntries,
  secretMapEntryKey,
  secretMapKeyError,
} from "./secretMap.logic.ts";

function entry(path: string, kind: string, is_secret: boolean): ListResponseEntry {
  return { path, category: "Mcp", kind, type_hint: "", populated: false, is_secret };
}

test("empty secret map still yields a container group", () => {
  const grouped = groupSecretMapEntries([
    entry("mcp.servers.acme.command", "string", false),
    entry("mcp.servers.acme.env", "secret-map", true),
  ]);
  assert.deepEqual([...grouped.keys()], ["mcp.servers.acme.env"]);
  assert.deepEqual(grouped.get("mcp.servers.acme.env"), []);
});

test("entries group under their container and siblings stay out", () => {
  const grouped = groupSecretMapEntries([
    entry("mcp.servers.acme.env", "secret-map", true),
    entry("mcp.servers.acme.env.API_KEY", "string", true),
    entry("mcp.servers.acme.env.x.dotted", "string", true),
    entry("mcp.servers.acme.headers", "secret-map", true),
    entry("mcp.servers.acme.headers.Authorization", "string", true),
    // Not secret: never treated as a map entry even if the path matched.
    entry("mcp.servers.acme.env.bogus", "string", false),
    entry("mcp.servers.acme.environment", "string", true),
  ]);
  assert.deepEqual(
    grouped.get("mcp.servers.acme.env")?.map((e) => e.path),
    ["mcp.servers.acme.env.API_KEY", "mcp.servers.acme.env.x.dotted"],
  );
  assert.deepEqual(
    grouped.get("mcp.servers.acme.headers")?.map((e) => e.path),
    ["mcp.servers.acme.headers.Authorization"],
  );
});

test("entry key is relative to the container and keeps dots", () => {
  assert.equal(
    secretMapEntryKey("mcp.servers.acme.env", "mcp.servers.acme.env.x.dotted"),
    "x.dotted",
  );
});

test("new entry names are validated loosely", () => {
  assert.equal(secretMapKeyError("", []), "fieldform.secret_map_key_invalid");
  assert.equal(secretMapKeyError("HAS SPACE", []), "fieldform.secret_map_key_invalid");
  assert.equal(secretMapKeyError("A=B", []), "fieldform.secret_map_key_invalid");
  assert.equal(secretMapKeyError("TAB\tKEY", []), "fieldform.secret_map_key_invalid");
  assert.equal(secretMapKeyError("API_KEY", ["API_KEY"]), "fieldform.secret_map_key_exists");
  for (const ok of ["GITHUB_TOKEN", "Authorization", "X-Api-Key", "x.dotted"]) {
    assert.equal(secretMapKeyError(ok, []), null, ok);
  }
});
