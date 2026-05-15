/**
 * Shared `["slots"]` query (M3 PR #5 review fix).
 *
 * Both `SlotSidebar` and `ChatPage`'s `ChatPane` need the slot list.
 * React Query keys on the `queryKey` alone — registering two
 * `useQuery({ queryKey: ["slots"] })` calls with different `queryFn`s
 * or different options is a footgun: whichever observer mounts first
 * wins, and the other registration's options are silently ignored.
 *
 * Centralising the query here gives us a single source of truth for
 * (a) the fetch function, (b) the polling interval. Components import
 * this hook instead of inlining `useQuery({ queryKey: ["slots"] })`.
 */
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";
import type { SlotResponse } from "@/chat/slotMutations";

export interface SlotListResponse {
  slots: SlotResponse[];
}

async function fetchSlots(): Promise<SlotListResponse> {
  return apiFetch<SlotListResponse>("/api/slots");
}

export const SLOTS_QUERY_KEY = ["slots"] as const;

/**
 * Polled (5s) slot list. The interval drops out once subscribe-mode WS
 * lands in M4b and pushes incremental diffs.
 */
export function useSlotsQuery() {
  return useQuery({
    queryKey: SLOTS_QUERY_KEY,
    queryFn: fetchSlots,
    refetchInterval: 5_000,
  });
}
