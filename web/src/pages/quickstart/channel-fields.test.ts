import test from "node:test";
import assert from "node:assert/strict";
import type { QuickstartFieldDescriptor } from "../../lib/api.ts";
import {
  channelFieldStateReducer,
  initialChannelFieldState,
  initialChannelFieldValues,
} from "./channel-fields.ts";

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

test("does not overwrite existing field values when merging defaults", () => {
  const fields = [descriptor("port", "8090"), descriptor("secret", null)];
  const edited = { port: "9123", secret: "user-entered-secret" };

  assert.deepEqual(initialChannelFieldValues(fields, edited), {
    port: "9123",
    secret: "user-entered-secret",
  });
});

test("preserves edited values across the fresh-existing-fresh transition", () => {
  const descriptors = [descriptor("port", "8090"), descriptor("secret", null)];
  let state = initialChannelFieldState("fresh");

  state = channelFieldStateReducer(state, {
    kind: "channel-type-changed",
    channelType: "webhook",
  });
  state = channelFieldStateReducer(state, {
    kind: "descriptors-loaded",
    channelType: "webhook",
    descriptors,
  });
  state = channelFieldStateReducer(state, {
    kind: "field-changed",
    key: "port",
    value: "9123",
  });
  state = channelFieldStateReducer(state, {
    kind: "field-changed",
    key: "secret",
    value: "user-entered-secret",
  });
  state = channelFieldStateReducer(state, {
    kind: "mode-changed",
    mode: "existing",
  });
  state = channelFieldStateReducer(state, {
    kind: "mode-changed",
    mode: "fresh",
  });
  state = channelFieldStateReducer(state, {
    kind: "descriptors-loaded",
    channelType: "webhook",
    descriptors,
  });

  assert.equal(state.mode, "fresh");
  assert.deepEqual(state.fields, {
    port: "9123",
    secret: "user-entered-secret",
  });
  assert.deepEqual(state.descriptors, descriptors);
});
