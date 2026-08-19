import type { QuickstartFieldDescriptor } from "../../lib/api.ts";

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
