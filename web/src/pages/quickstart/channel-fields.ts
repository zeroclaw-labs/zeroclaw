import type { QuickstartFieldDescriptor } from "../../lib/api.ts";

export type ChannelFieldMode = "existing" | "fresh";

export interface ChannelFieldState {
  mode: ChannelFieldMode;
  type: string;
  descriptors: QuickstartFieldDescriptor[];
  fields: Record<string, string>;
}

export type ChannelFieldAction =
  | { kind: "mode-changed"; mode: ChannelFieldMode }
  | { kind: "channel-type-changed"; channelType: string }
  | {
      kind: "descriptors-loaded";
      channelType: string;
      descriptors: readonly QuickstartFieldDescriptor[];
    }
  | { kind: "field-changed"; key: string; value: string };

export function initialChannelFieldState(
  mode: ChannelFieldMode,
): ChannelFieldState {
  return { mode, type: "", descriptors: [], fields: {} };
}

/** Seed channel form state from the explicit defaults returned by the daemon. */
export function initialChannelFieldValues(
  descriptors: readonly QuickstartFieldDescriptor[],
  current: Readonly<Record<string, string>> = {},
): Record<string, string> {
  const values: Record<string, string> = { ...current };
  for (const descriptor of descriptors) {
    if (
      !Object.prototype.hasOwnProperty.call(values, descriptor.key) &&
      descriptor.default !== null
    ) {
      values[descriptor.key] = descriptor.default;
    }
  }
  return values;
}

/** Apply one user or descriptor update at the channel form state boundary. */
export function channelFieldStateReducer(
  state: ChannelFieldState,
  action: ChannelFieldAction,
): ChannelFieldState {
  switch (action.kind) {
    case "mode-changed":
      return { ...state, mode: action.mode };
    case "channel-type-changed":
      return {
        ...state,
        type: action.channelType,
        descriptors: [],
        fields: {},
      };
    case "descriptors-loaded":
      if (state.type !== action.channelType) return state;
      return {
        ...state,
        descriptors: [...action.descriptors],
        fields: initialChannelFieldValues(action.descriptors, state.fields),
      };
    case "field-changed":
      return {
        ...state,
        fields: { ...state.fields, [action.key]: action.value },
      };
  }
}
