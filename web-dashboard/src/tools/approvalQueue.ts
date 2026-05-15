// Pending tool approvals tracked by (slot_id, request_id). Singleton
// store so chat view + Board + toast host all see the same queue.
// 120s timeout matches `WsApprovalChannel` in api_slots.rs.

import { useSyncExternalStore } from "react";
import { useMutation } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";
import { getSlotEventBus, type SlotBusEvent } from "@/lib/slotEvents";

export type ApprovalDecision = "approve" | "deny" | "always";

export interface PendingApproval {
  slot_id: string;
  request_id: string;
  tool_name: string;
  arguments_summary: string;
  /** Wall-clock ms when the bus event arrived locally. */
  received_at: number;
  /** `received_at + timeout_secs * 1000` — the queue auto-evicts past this. */
  timeout_at: number;
}

interface State {
  pending: PendingApproval[];
}

type Subscriber = () => void;

class ApprovalQueueStore {
  private state: State = { pending: [] };
  private subscribers = new Set<Subscriber>();
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  private busDispose: (() => void) | null = null;

  attachToBus(): void {
    if (this.busDispose !== null) return;
    // `subscribeAll` because slot ids aren't known up-front. Other
    // consumers (sidebar `slots`, chat `chat:<id>`, Board per-slot)
    // open the channels at the wire level.
    this.busDispose = getSlotEventBus().subscribeAll((event) =>
      this.handleEvent(event),
    );
  }

  /** Detach from the bus. Used by tests; production code never calls this. */
  detachFromBus(): void {
    if (this.busDispose) {
      this.busDispose();
      this.busDispose = null;
    }
  }

  getSnapshot = (): State => this.state;

  subscribe = (sub: Subscriber): (() => void) => {
    this.subscribers.add(sub);
    return () => {
      this.subscribers.delete(sub);
    };
  };

  private notify(): void {
    for (const s of this.subscribers) s();
  }

  /** Public for optimistic UI: directly remove an approval. */
  remove(slotId: string, requestId: string): void {
    const next = this.state.pending.filter(
      (p) => !(p.slot_id === slotId && p.request_id === requestId),
    );
    if (next.length === this.state.pending.length) return;
    this.state = { ...this.state, pending: next };
    this.clearTimer(slotId, requestId);
    this.notify();
  }

  private handleEvent(event: SlotBusEvent): void {
    if (event.type === "permission_request") {
      const now = Date.now();
      const pending: PendingApproval = {
        slot_id: event.slot_id,
        request_id: event.data.request_id,
        tool_name: event.data.tool_name,
        arguments_summary: event.data.arguments_summary,
        received_at: now,
        timeout_at: now + Math.max(1, event.data.timeout_secs) * 1000,
      };
      // Replace if a duplicate (slot_id, request_id) somehow re-arrives;
      // otherwise append.
      const existingIdx = this.state.pending.findIndex(
        (p) =>
          p.slot_id === pending.slot_id && p.request_id === pending.request_id,
      );
      const nextPending =
        existingIdx >= 0
          ? this.state.pending.map((p, i) => (i === existingIdx ? pending : p))
          : [...this.state.pending, pending];
      this.state = { ...this.state, pending: nextPending };
      this.scheduleTimeout(pending);
      this.notify();
      return;
    }
    if (event.type === "approval_response") {
      this.remove(event.slot_id, event.data.request_id);
      return;
    }
  }

  private scheduleTimeout(pending: PendingApproval): void {
    const key = timerKey(pending.slot_id, pending.request_id);
    this.clearTimer(pending.slot_id, pending.request_id);
    const ms = Math.max(0, pending.timeout_at - Date.now());
    const handle = setTimeout(() => {
      this.remove(pending.slot_id, pending.request_id);
    }, ms);
    this.timers.set(key, handle);
  }

  private clearTimer(slotId: string, requestId: string): void {
    const key = timerKey(slotId, requestId);
    const handle = this.timers.get(key);
    if (handle !== undefined) {
      clearTimeout(handle);
      this.timers.delete(key);
    }
  }
}

function timerKey(slotId: string, requestId: string): string {
  return `${slotId}::${requestId}`;
}

let storeSingleton: ApprovalQueueStore | null = null;

function getStore(): ApprovalQueueStore {
  if (storeSingleton === null) {
    storeSingleton = new ApprovalQueueStore();
    storeSingleton.attachToBus();
  }
  return storeSingleton;
}

// ── Hooks ───────────────────────────────────────────────────────────

/** Snapshot of all pending approvals, ordered by `received_at` ascending. */
export function useApprovalQueue(): PendingApproval[] {
  const store = getStore();
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  return state.pending;
}

/** Pending approvals scoped to a specific slot. */
export function useApprovalsForSlot(slotId: string | undefined): PendingApproval[] {
  const all = useApprovalQueue();
  if (!slotId) return [];
  return all.filter((p) => p.slot_id === slotId);
}

/**
 * React Query mutation that posts the operator's decision to
 * `POST /api/slots/:id/approve` and optimistically removes the matching
 * pending entry on success. The canonical removal still happens via the
 * `approval_response` event from the bus; this is a UI smoothness win
 * for the case where the WS event arrives milliseconds after the HTTP
 * response.
 */
export function useApproveTool() {
  return useMutation({
    mutationFn: async (vars: {
      slot_id: string;
      request_id: string;
      decision: ApprovalDecision;
    }) => {
      await apiFetch(`/api/slots/${encodeURIComponent(vars.slot_id)}/approve`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          request_id: vars.request_id,
          decision: vars.decision,
        }),
      });
      return vars;
    },
    onSuccess: (vars) => {
      getStore().remove(vars.slot_id, vars.request_id);
    },
  });
}

