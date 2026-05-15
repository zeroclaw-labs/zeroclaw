// 4-lane Kanban auto-sorting slots by state. Pending approvals win
// over backend state (a running slot with a pending approval lands in
// Needs Approval). Errored slots get a strip below the grid.

import { Link, useNavigate } from "react-router-dom";
import { Wrench, Bell, Activity, Pause, AlertCircle, Hourglass } from "lucide-react";
import { useSlotsQuery } from "@/chat/slotsQuery";
import { PersonaBadge } from "@/chat/PersonaBadge";
import { useSlotEvents } from "@/lib/slotEvents";
import {
  useApprovalQueue,
  useApproveTool,
  type PendingApproval,
} from "@/tools/approvalQueue";
import { useStallDetection } from "@/board/useStallDetection";
import type { SlotResponse } from "@/chat/slotMutations";
import { ThemeSwitcher } from "@/theme/ThemeSwitcher";
import { useControlUiBootstrap } from "@/app/ControlUiBootstrapProvider";
import { SectionNav } from "@/app/SectionNav";

type Lane = "needs_approval" | "your_turn" | "working" | "idle";

const LANE_META: Record<
  Lane,
  { title: string; icon: React.ComponentType<{ size?: number; "aria-hidden"?: boolean }> }
> = {
  needs_approval: { title: "Needs Approval", icon: Bell },
  your_turn: { title: "Your Turn", icon: Wrench },
  working: { title: "Working", icon: Activity },
  idle: { title: "Idle", icon: Pause },
};

const LANE_ORDER: Lane[] = ["needs_approval", "your_turn", "working", "idle"];

export function BoardPage() {
  const bootstrap = useControlUiBootstrap();
  const { data, isLoading, error } = useSlotsQuery();
  const approvals = useApprovalQueue();

  const slots = data?.slots ?? [];

  // Open every slot's chat channel so the approval queue + toast host
  // (both `subscribeAll` listeners) hear permission events without
  // needing the chat view mounted.
  const channels = ["slots", ...slots.map((s) => `chat:${s.id}`)];
  useSlotEvents({ channels, onEvent: () => {} });

  const lanes = computeLanes(slots, approvals);
  const erroredSlots = slots.filter((s) => s.state === "error");

  return (
    <div className="flex flex-col h-full">
      <header
        className="flex items-center justify-between gap-2 px-4 py-3 border-b"
        style={{ borderColor: "var(--color-border)" }}
      >
        <div className="flex items-center gap-3 text-sm">
          <span className="font-semibold">{bootstrap.assistant_identity.name}</span>
          <span style={{ color: "var(--color-text-muted)" }}>·</span>
          <SectionNav layout="inline" />
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs opacity-50">v{bootstrap.server_version}</span>
          <ThemeSwitcher />
        </div>
      </header>

      <main className="flex-1 overflow-auto p-3">
        {isLoading ? (
          <p className="text-sm opacity-60 text-center mt-8">Loading slots…</p>
        ) : error ? (
          <p className="text-sm text-red-600 text-center mt-8" role="alert">
            Failed to load slots: {String(error)}
          </p>
        ) : slots.length === 0 ? (
          <p className="text-sm opacity-60 text-center mt-8">
            No slots yet. Open <Link to="/chat" className="underline">Chat</Link>{" "}
            and click <strong>+ New</strong> to create one.
          </p>
        ) : (
          <>
            <div
              className="grid gap-3"
              style={{
                gridTemplateColumns: `repeat(${LANE_ORDER.length}, minmax(0, 1fr))`,
              }}
            >
              {LANE_ORDER.map((lane) => (
                <BoardLane
                  key={lane}
                  lane={lane}
                  slots={lanes[lane]}
                  approvalsByLane={approvals}
                />
              ))}
            </div>
            {erroredSlots.length > 0 ? (
              <ErroredStrip slots={erroredSlots} />
            ) : null}
          </>
        )}
      </main>
    </div>
  );
}

interface BoardLaneProps {
  lane: Lane;
  slots: SlotResponse[];
  approvalsByLane: PendingApproval[];
}

function BoardLane({ lane, slots, approvalsByLane }: BoardLaneProps) {
  const Icon = LANE_META[lane].icon;
  return (
    <section
      aria-label={LANE_META[lane].title}
      className="flex flex-col rounded border min-h-[200px]"
      style={{
        borderColor: "var(--color-border)",
        background: "var(--color-surface)",
      }}
    >
      <header
        className="flex items-center gap-2 px-3 py-2 border-b text-xs uppercase tracking-wider"
        style={{
          borderColor: "var(--color-border)",
          color: "var(--color-text-muted)",
        }}
      >
        <Icon size={12} aria-hidden={true} />
        <span>{LANE_META[lane].title}</span>
        <span className="ml-auto tabular-nums">{slots.length}</span>
      </header>
      <ul className="flex-1 flex flex-col gap-2 p-2">
        {slots.length === 0 ? (
          <li
            className="text-xs italic opacity-50 text-center pt-4"
            data-testid={`board-lane-empty-${lane}`}
          >
            Empty
          </li>
        ) : (
          slots.map((slot) => (
            <li key={slot.id}>
              <SlotCard
                slot={slot}
                lane={lane}
                approvals={approvalsByLane.filter((a) => a.slot_id === slot.id)}
              />
            </li>
          ))
        )}
      </ul>
    </section>
  );
}

interface SlotCardProps {
  slot: SlotResponse;
  lane: Lane;
  approvals: PendingApproval[];
}

function SlotCard({ slot, lane, approvals }: SlotCardProps) {
  const navigate = useNavigate();
  const approve = useApproveTool();
  const stall = useStallDetection(slot);
  const mostRecent = approvals[approvals.length - 1];

  const handleApprove = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!mostRecent) return;
    approve.mutate({
      slot_id: slot.id,
      request_id: mostRecent.request_id,
      decision: "approve",
    });
  };

  return (
    <article
      data-slot-id={slot.id}
      data-board-lane={lane}
      onClick={() => navigate(`/chat/${encodeURIComponent(slot.id)}`)}
      className="rounded border px-2 py-2 text-sm cursor-pointer hover:bg-[color:var(--color-surface-muted)]"
      style={{
        borderColor: "var(--color-border)",
        background: "var(--color-surface)",
      }}
    >
      <div className="flex items-center gap-1">
        <span className="truncate flex-1 font-medium">{slot.title}</span>
        <PersonaBadge config={slot.agent_config} />
      </div>
      {slot.workspace ? (
        <div
          className="text-[10px] mt-0.5 truncate"
          style={{ color: "var(--color-text-muted)" }}
        >
          {slot.workspace}
        </div>
      ) : null}
      <div
        className="flex items-center gap-2 mt-1 text-[11px]"
        style={{ color: "var(--color-text-muted)" }}
      >
        <span className="tabular-nums">{slot.message_count} msg</span>
        {stall.stalled ? (
          <span
            data-testid={`stalled-${slot.id}`}
            className="inline-flex items-center gap-0.5"
            title="No deltas in 30s"
            style={{ color: "var(--color-text)" }}
          >
            <Hourglass size={11} aria-hidden="true" />
            stalled
          </span>
        ) : null}
      </div>
      {lane === "needs_approval" && mostRecent ? (
        <div className="mt-2 flex items-center gap-1">
          <button
            type="button"
            onClick={handleApprove}
            disabled={approve.isPending}
            className="text-xs px-2 py-1 rounded border disabled:opacity-50"
            style={{
              borderColor: "var(--color-border)",
              background: "var(--color-accent)",
              color: "var(--color-surface)",
            }}
            aria-label={`Approve ${mostRecent.tool_name} on ${slot.title}`}
          >
            Approve {mostRecent.tool_name}
          </button>
          <Link
            to={`/chat/${encodeURIComponent(slot.id)}`}
            onClick={(e) => e.stopPropagation()}
            className="text-xs underline"
            style={{ color: "var(--color-text-muted)" }}
          >
            review
          </Link>
        </div>
      ) : null}
    </article>
  );
}

function ErroredStrip({ slots }: { slots: SlotResponse[] }) {
  return (
    <section
      aria-label="Errored"
      className="mt-4 rounded border"
      style={{ borderColor: "var(--color-border)" }}
    >
      <header
        className="flex items-center gap-2 px-3 py-2 border-b text-xs uppercase tracking-wider"
        style={{
          borderColor: "var(--color-border)",
          color: "var(--color-text-muted)",
        }}
      >
        <AlertCircle size={12} aria-hidden="true" />
        <span>Errored</span>
        <span className="ml-auto tabular-nums">{slots.length}</span>
      </header>
      <ul className="flex flex-wrap gap-2 p-2">
        {slots.map((s) => (
          <li key={s.id}>
            <Link
              to={`/chat/${encodeURIComponent(s.id)}`}
              className="text-xs px-2 py-1 rounded border underline"
              style={{ borderColor: "var(--color-border)" }}
            >
              {s.title}
            </Link>
          </li>
        ))}
      </ul>
    </section>
  );
}

// ── Lane assignment ─────────────────────────────────────────────────

function computeLanes(
  slots: SlotResponse[],
  approvals: PendingApproval[],
): Record<Lane, SlotResponse[]> {
  const slotsWithApprovals = new Set(approvals.map((a) => a.slot_id));
  const lanes: Record<Lane, SlotResponse[]> = {
    needs_approval: [],
    your_turn: [],
    working: [],
    idle: [],
  };
  for (const slot of slots) {
    // Errored slots only render in ErroredStrip; skip lane assignment
    // even if they happen to carry a pending approval (otherwise the
    // slot would duplicate into Needs Approval AND the strip).
    if (slot.state === "error") continue;
    if (slotsWithApprovals.has(slot.id)) {
      lanes.needs_approval.push(slot);
      continue;
    }
    if (slot.state === "waiting_approval") {
      lanes.your_turn.push(slot);
      continue;
    }
    if (slot.state === "running") {
      lanes.working.push(slot);
      continue;
    }
    if (slot.state === "idle") {
      lanes.idle.push(slot);
      continue;
    }
  }
  // Sort within each lane: most-recently-updated first.
  for (const key of LANE_ORDER) {
    lanes[key].sort((a, b) => b.updated_at - a.updated_at);
  }
  return lanes;
}
