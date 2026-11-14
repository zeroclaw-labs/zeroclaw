import assert from "node:assert/strict";
import test from "node:test";
import { nextXmppPolicyKey } from "./xmppPolicy.ts";

test("long valid aliases create bounded policy keys", () => {
  for (const length of [1, 58, 59, 63]) {
    const key = nextXmppPolicyKey("a".repeat(length), []);
    assert.match(key, /^[a-zA-Z0-9_-]{1,63}$/);
    assert.ok(key.startsWith("xmpp_"));
  }
});
test("collision suffixes stay within the limit across digit boundaries", () => {
  const keys: string[] = [];
  for (let index = 0; index < 14; index += 1) {
    const key = nextXmppPolicyKey("a".repeat(63), keys);
    assert.ok(key.length <= 63);
    assert.ok(!keys.includes(key));
    keys.push(key);
  }
  assert.ok(keys[10]?.endsWith("_11"));
});
test("short aliases retain their existing names and collision behavior", () => {
  assert.equal(nextXmppPolicyKey("synthetic-bot", []), "xmpp_synthetic-bot");
  assert.equal(nextXmppPolicyKey("synthetic-bot", ["xmpp_synthetic-bot"]), "xmpp_synthetic-bot_2");
});
