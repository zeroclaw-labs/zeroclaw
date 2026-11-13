import assert from "node:assert/strict";
import test from "node:test";
import { isOptionTypeHint, parseNumberDraft } from "./numberInput.logic.ts";

test("empty optional integer and float drafts clear to null", () => {
  // Clearing either optional numeric kind means unset, not explicit zero.
  assert.strictEqual(parseNumberDraft("Option<u64>", ""), null);
  assert.strictEqual(parseNumberDraft("Option<f64>", ""), null);
});

test("whitespace-only optional drafts clear to null", () => {
  // Whitespace-only clears have the same unset meaning for integer and float fields.
  for (const typeHint of ["Option<u64>", "Option<f64>"]) {
    for (const raw of ["   ", "\t", " \n "]) {
      assert.strictEqual(parseNumberDraft(typeHint, raw), null);
    }
  }
});

test("explicit zero stays a number for optional fields", () => {
  // Explicit zero remains distinct from an empty optional draft.
  for (const typeHint of ["Option<u64>", "Option<f64>"]) {
    for (const raw of ["0", "0.0", " 0 "]) {
      assert.strictEqual(parseNumberDraft(typeHint, raw), 0);
    }
  }
});

test("positive optional values parse unchanged", () => {
  // Positive integer and fractional values retain their numeric values.
  assert.strictEqual(parseNumberDraft("Option<u64>", "500"), 500);
  assert.strictEqual(parseNumberDraft("Option<f64>", "2.5"), 2.5);
});

test("required numbers keep the existing parse", () => {
  // Required and empty hints preserve Number(raw), including empty-to-zero.
  assert.strictEqual(parseNumberDraft("u64", ""), 0);
  assert.strictEqual(parseNumberDraft("u64", "   "), 0);
  assert.strictEqual(parseNumberDraft("f64", ""), 0);
  assert.strictEqual(parseNumberDraft("", ""), 0);
  assert.strictEqual(parseNumberDraft("u16", "42617"), 42617);
});

test("non-numeric input is forwarded as text for server validation", () => {
  // Both optional and required malformed values remain server-validated text.
  assert.strictEqual(parseNumberDraft("Option<u64>", "abc"), "abc");
  assert.strictEqual(parseNumberDraft("u64", "abc"), "abc");
});

test("option detection ignores whitespace and rejects other hints", () => {
  // Detection follows the Option prefix while rejecting unrelated signatures.
  assert.strictEqual(isOptionTypeHint("Option<u64>"), true);
  assert.strictEqual(isOptionTypeHint("Option < f64 >"), true);
  assert.strictEqual(isOptionTypeHint("Option<Vec<String>>"), true);
  assert.strictEqual(isOptionTypeHint("u64"), false);
  assert.strictEqual(isOptionTypeHint(""), false);
  assert.strictEqual(isOptionTypeHint("NonOption<u64>"), false);
});

test("cleared optional draft keeps an explicit null in the PATCH body", () => {
  // JSON serialization must preserve null so the server can clear the key.
  const body = JSON.stringify({
    op: "replace",
    path: "goal.default_token_limit",
    value: parseNumberDraft("Option<u64>", ""),
  });
  assert.strictEqual(
    body,
    '{"op":"replace","path":"goal.default_token_limit","value":null}',
  );
});
