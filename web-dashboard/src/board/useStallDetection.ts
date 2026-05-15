// Stall = state===running AND no chat delta in 30s. When stalled,
// GET /api/slots/:id at most once per 15s and invalidate ['slots']
// if the server-side state diverged.

import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import {
  getSlotEventBus,
  type SlotBusEvent,
} from "@/lib/slotEvents";
import { apiFetch } from "@/lib/apiFetch";
import { SLOTS_QUERY_KEY } from "@/chat/slotsQuery";
import type { SlotResponse } from "@/chat/slotMutations";

const STALL_THRESHOLD_MS = 30_000;
const HEALTH_POLL_MS = 15_000;

interface DeltaTracker {
  lastDeltaAt: Map<string, number>;
  lastHealthAt: Map<string, number>;
}

const tracker: DeltaTracker = {
  lastDeltaAt: new Map(),
  lastHealthAt: new Map(),
};

let busDisposeSingleton: (() => void) | null = null;

function ensureBusListener(): void {
  if (busDisposeSingleton !== null) return;
  const bus = getSlotEventBus();
  busDisposeSingleton = bus.subscribeAll((event: SlotBusEvent) => {
    // `slot` events reset the timer too — a fresh state transition
    // shouldn't immediately count as stalled.
    if ((event.type === "chat" || event.type === "slot") && event.slot_id) {
      tracker.lastDeltaAt.set(event.slot_id, Date.now());
    }
  });
}

export interface StallStatus {
  stalled: boolean;
  /** Ms since last delta, or null if none seen. */
  sinceMs: number | null;
}

// Re-renders every 5s while the slot is running so the threshold
// crossing is visible without waiting for the next bus event.
export function useStallDetection(slot: SlotResponse): StallStatus {
  const qc = useQueryClient();
  const [, force] = useState(0);
  const lastInvalidatedAt = useRef(0);

  useEffect(() => {
    ensureBusListener();
  }, []);

  // Ref-tracked so a state flip doesn't stomp the interval.
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  useEffect(() => {
    if (slot.state !== "running") {
      if (intervalRef.current !== null) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
      return;
    }
    if (intervalRef.current !== null) return;
    intervalRef.current = setInterval(() => force((n) => n + 1), 5_000);
    return () => {
      if (intervalRef.current !== null) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [slot.state]);

  // Seed from `slot.updated_at` if no delta has been observed locally.
  // Without this, a slot that was already running before BoardPage
  // mounted (or that has not emitted a chat delta yet) reports
  // sinceMs=null forever and never trips the threshold check.
  // `updated_at` is a unix-second epoch — convert to ms.
  if (!tracker.lastDeltaAt.has(slot.id)) {
    tracker.lastDeltaAt.set(slot.id, slot.updated_at * 1000);
  }
  const last = tracker.lastDeltaAt.get(slot.id);
  const now = Date.now();
  const sinceMs = last === undefined ? null : now - last;
  const stalled =
    slot.state === "running" &&
    sinceMs !== null &&
    sinceMs > STALL_THRESHOLD_MS;

  // Health poll: only when stalled, throttled to once per HEALTH_POLL_MS.
  useEffect(() => {
    if (!stalled) return;
    const lastHealth = tracker.lastHealthAt.get(slot.id) ?? 0;
    if (now - lastHealth < HEALTH_POLL_MS) return;
    tracker.lastHealthAt.set(slot.id, now);
    let cancelled = false;
    void (async () => {
      try {
        const fresh = await apiFetch<SlotResponse>(
          `/api/slots/${encodeURIComponent(slot.id)}`,
        );
        if (cancelled) return;
        if (
          fresh.state !== slot.state &&
          Date.now() - lastInvalidatedAt.current > 5_000
        ) {
          lastInvalidatedAt.current = Date.now();
          void qc.invalidateQueries({ queryKey: SLOTS_QUERY_KEY });
        }
      } catch {
        // Best-effort; surfacing here would just spam the user.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [stalled, slot.id, slot.state, now, qc]);

  return { stalled, sinceMs };
}

