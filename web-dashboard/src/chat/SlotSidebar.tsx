import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

/**
 * Slot sidebar (M3 scaffold).
 *
 * Ports OpenClaw's `chat-sidebar-raw.ts` semantics (plan §12
 * translation): list of slots, state indicators, keyboard nav,
 * context menu actions. This M3 commit lands the list + empty state;
 * create/rename/delete/duplicate and keyboard nav follow in sub-commits.
 */

interface SlotSummary {
  id: string;
  session_id: string;
  title: string;
  state: "idle" | "running" | "waiting_approval" | "error";
  message_count: number;
  dirty: boolean;
  workspace?: string;
}

interface SlotListResponse {
  slots: SlotSummary[];
}

async function fetchSlots(): Promise<SlotListResponse> {
  return apiFetch<SlotListResponse>("/api/slots");
}

export function SlotSidebar() {
  const { data, isLoading, error, refetch } = useQuery({
    queryKey: ["slots"],
    queryFn: fetchSlots,
    // Poll every 5s while the subscribe-mode WS wiring lands in a
    // follow-up M3 sub-commit. Switches to pure WS-driven diffs once
    // `useDashboardWs` is in place.
    refetchInterval: 5_000,
  });

  if (isLoading) {
    return <div className="p-4 text-xs opacity-60">Loading slots…</div>;
  }
  if (error) {
    return (
      <div className="p-4 text-xs text-red-600">
        Failed to load slots: {String(error)}
        <button
          type="button"
          className="mt-2 block underline"
          onClick={() => {
            void refetch();
          }}
        >
          Retry
        </button>
      </div>
    );
  }

  const slots = data?.slots ?? [];
  if (slots.length === 0) {
    return (
      <div className="p-4 text-xs opacity-60">
        No slots yet. Create one via{" "}
        <code className="font-mono">POST /api/slots</code> to get started.
      </div>
    );
  }

  return (
    <ul className="flex-1 overflow-y-auto">
      {slots.map((slot) => (
        <li
          key={slot.id}
          className="px-4 py-2 text-sm border-b cursor-pointer hover:bg-[color:var(--color-surface-muted)]"
          style={{ borderColor: "var(--color-border)" }}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="truncate">{slot.title}</span>
            <SlotStateBadge state={slot.state} />
          </div>
          {slot.workspace ? (
            <div className="text-[10px] opacity-50 mt-0.5">{slot.workspace}</div>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

function SlotStateBadge({ state }: { state: SlotSummary["state"] }) {
  const label =
    state === "idle"
      ? ""
      : state === "running"
        ? "…"
        : state === "waiting_approval"
          ? "?"
          : "!";
  if (!label) return null;
  return (
    <span
      className="text-[10px] px-1.5 py-0.5 rounded"
      style={{
        background: "var(--color-surface-muted)",
        color: "var(--color-text-muted)",
      }}
    >
      {label}
    </span>
  );
}
