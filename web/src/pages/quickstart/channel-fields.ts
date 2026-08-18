import type { QuickstartFieldDescriptor } from "../../lib/api.ts";

/** Seed channel form state from the explicit defaults returned by the daemon. */
export function initialChannelFieldValues(
  descriptors: readonly QuickstartFieldDescriptor[],
): Record<string, string> {
  const values: Record<string, string> = {};
  for (const descriptor of descriptors) {
    if (descriptor.default !== null) {
      values[descriptor.key] = descriptor.default;
    }
  }
  return values;
}
