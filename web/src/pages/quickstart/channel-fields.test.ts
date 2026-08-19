import test from "node:test";
import assert from "node:assert/strict";
import type { QuickstartFieldDescriptor } from "../../lib/api.ts";
import { initialChannelFieldValues } from "./channel-fields.ts";

function descriptor(
  key: string,
  defaultValue: string | null,
): QuickstartFieldDescriptor {
  return {
    key,
    label: key,
    help: "",
    kind: "string",
    is_secret: false,
    enum_variants: null,
    required: true,
    default: defaultValue,
  };
}

test("initializes channel fields from descriptor defaults", () => {
  assert.deepEqual(
    initialChannelFieldValues([
      descriptor("port", "8090"),
      descriptor("secret", null),
    ]),
    { port: "8090" },
  );
});

test("does not invent values for descriptors without defaults", () => {
  assert.deepEqual(initialChannelFieldValues([descriptor("secret", null)]), {});
});

test("preserves edited values across a fresh-existing-fresh transition", () => {
  const fields = [descriptor("port", "8090"), descriptor("secret", null)];
  const edited = initialChannelFieldValues(fields);
  edited.port = "9123";
  edited.secret = "user-entered-secret";

  // The existing-mode view hides, but does not discard, fresh-mode state.
  assert.deepEqual(initialChannelFieldValues(fields, edited), {
    port: "9123",
    secret: "user-entered-secret",
  });
});
