/**
 * Slot mutation hooks (M3, US-001).
 *
 * One React Query mutation per slot lifecycle endpoint. Each hook
 * invalidates the `["slots"]` query so `SlotSidebar` re-renders the
 * fresh list — no manual cache patching, which keeps optimistic UI
 * decisions in the components that own the interaction (e.g. the
 * sidebar's selected-after-create logic).
 *
 * Wire shapes mirror `crates/zeroclaw-gateway/src/api_slots.rs`:
 *   POST   /api/slots                    SlotCreateRequest -> SlotResponse
 *   PATCH  /api/slots/:id                SlotPatchRequest -> SlotResponse
 *   DELETE /api/slots/:id                -> 204
 *   POST   /api/slots/:id/duplicate      SlotDuplicateRequest -> SlotResponse
 */
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

// Mirrors the gateway's `SlotResponse`; extend in lock-step with the
// backend struct rather than reinventing it. M4a will replace
// `agent_config` with the persona-aware shape.
export interface SlotResponse {
  id: string;
  session_id: string;
  title: string;
  state: "idle" | "running" | "waiting_approval" | "error";
  message_count: number;
  dirty: boolean;
  workspace?: string;
  created_at: number;
  updated_at: number;
  agent_config?: SlotAgentConfig;
}

export interface SlotAgentConfig {
  provider?: string | null;
  model?: string | null;
  mode?: "normal" | "trust" | "yolo";
  personality?: string | null;
  persona_preset?: string | null;
}

export interface SlotCreateRequest {
  title?: string;
  session_id?: string;
  workspace?: string;
  agent_config?: SlotAgentConfig;
}

export interface SlotPatchRequest {
  title?: string;
  workspace?: string | null;
  agent_config?: SlotAgentConfig;
}

export interface SlotDuplicateRequest {
  title?: string;
  include_history?: boolean;
}

/** Create a new slot. */
export function useCreateSlot() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (req: SlotCreateRequest = {}) =>
      apiFetch<SlotResponse>("/api/slots", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(req),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["slots"] });
    },
  });
}

/** Rename or otherwise patch an existing slot. */
export function useRenameSlot() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ id, ...patch }: SlotPatchRequest & { id: string }) =>
      apiFetch<SlotResponse>(`/api/slots/${encodeURIComponent(id)}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(patch),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["slots"] });
    },
  });
}

/** Delete a slot. Resolves with the deleted id so callers can clear
 *  selection state. */
export function useDeleteSlot() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      // 204 No Content — pass `json: false` so apiFetch doesn't try to
      // parse an empty body.
      await apiFetch(`/api/slots/${encodeURIComponent(id)}`, {
        method: "DELETE",
        json: false,
      });
      return id;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["slots"] });
    },
  });
}

/** Clone a slot. */
export function useDuplicateSlot() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({
      id,
      ...req
    }: SlotDuplicateRequest & { id: string }) =>
      apiFetch<SlotResponse>(`/api/slots/${encodeURIComponent(id)}/duplicate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(req),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["slots"] });
    },
  });
}
