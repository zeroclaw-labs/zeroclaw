/**
 * Slot sidebar (M3, US-001 + US-002).
 *
 * Ports OpenClaw's `chat-sidebar-raw.ts` semantics (plan §12 translation
 * table): scrollable list with create/rename/delete/duplicate, hover-
 * revealed action buttons, inline rename. Ordering (newest-updated
 * first) comes from the backend; the sidebar is a thin presenter.
 *
 * The hooks live in `slotMutations.ts` so a future Board page (M4b) or
 * keyboard palette can reuse them without lifting the sidebar's
 * presentational state.
 */
import { useState, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { Plus, Pencil, Copy, Trash2, Check, X } from "lucide-react";
import { apiFetch } from "@/lib/apiFetch";
import {
  useCreateSlot,
  useRenameSlot,
  useDeleteSlot,
  useDuplicateSlot,
  type SlotResponse,
} from "@/chat/slotMutations";

interface SlotListResponse {
  slots: SlotResponse[];
}

async function fetchSlots(): Promise<SlotListResponse> {
  return apiFetch<SlotListResponse>("/api/slots");
}

interface SlotSidebarProps {
  activeSlotId?: string;
  onSelectSlot: (slotId: string) => void;
  /** Called after a successful delete so the parent can clear active id. */
  onSlotDeleted?: (slotId: string) => void;
}

export function SlotSidebar({
  activeSlotId,
  onSelectSlot,
  onSlotDeleted,
}: SlotSidebarProps) {
  const { data, isLoading, error, refetch } = useQuery({
    queryKey: ["slots"],
    queryFn: fetchSlots,
    // Polled hydration. Once the subscribe-mode WS hook lands, this
    // becomes WS-driven and the interval drops out.
    refetchInterval: 5_000,
  });

  const createSlot = useCreateSlot();
  const renameSlot = useRenameSlot();
  const deleteSlot = useDeleteSlot();
  const duplicateSlot = useDuplicateSlot();
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");

  const handleCreate = () => {
    createSlot.mutate(
      {},
      {
        onSuccess: (slot) => {
          onSelectSlot(slot.id);
        },
      },
    );
  };

  const startRename = (slot: SlotResponse) => {
    setRenamingId(slot.id);
    setRenameDraft(slot.title);
  };

  const commitRename = (id: string) => {
    const next = renameDraft.trim();
    const current = data?.slots.find((s) => s.id === id);
    if (next.length === 0 || next === current?.title) {
      setRenamingId(null);
      return;
    }
    renameSlot.mutate(
      { id, title: next },
      { onSettled: () => setRenamingId(null) },
    );
  };

  const handleDelete = (slot: SlotResponse) => {
    const ok = window.confirm(
      `Delete slot "${slot.title}"? This removes the slot row; the underlying conversation history is preserved (delete that separately via the Sessions API).`,
    );
    if (!ok) return;
    deleteSlot.mutate(slot.id, {
      onSuccess: () => {
        if (onSlotDeleted) onSlotDeleted(slot.id);
      },
    });
  };

  const handleDuplicate = (slot: SlotResponse) => {
    duplicateSlot.mutate(
      { id: slot.id, include_history: false },
      {
        onSuccess: (clone) => {
          onSelectSlot(clone.id);
        },
      },
    );
  };

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div
        className="flex items-center justify-between px-3 py-2 border-b"
        style={{ borderColor: "var(--color-border)" }}
      >
        <span className="text-xs uppercase tracking-wider opacity-60">
          Slots
        </span>
        <button
          type="button"
          onClick={handleCreate}
          disabled={createSlot.isPending}
          className="inline-flex items-center gap-1 text-xs px-2 py-1 rounded border hover:bg-[color:var(--color-surface-muted)] disabled:opacity-50"
          style={{ borderColor: "var(--color-border)" }}
          aria-label="Create new slot"
        >
          <Plus size={12} aria-hidden="true" />
          New
        </button>
      </div>

      {isLoading ? (
        <div className="p-4 text-xs opacity-60">Loading slots…</div>
      ) : error ? (
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
      ) : (data?.slots.length ?? 0) === 0 ? (
        <div className="p-4 text-xs opacity-60">
          No slots yet. Click <span className="font-medium">+ New</span> to
          start a fresh conversation.
        </div>
      ) : (
        <ul className="flex-1 overflow-y-auto">
          {data!.slots.map((slot) => {
            const isActive = slot.id === activeSlotId;
            const isRenaming = slot.id === renamingId;
            return (
              <li
                key={slot.id}
                className="group px-3 py-2 text-sm border-b"
                style={{
                  borderColor: "var(--color-border)",
                  background: isActive
                    ? "var(--color-surface-muted)"
                    : undefined,
                  borderLeft: isActive
                    ? "2px solid var(--color-accent)"
                    : "2px solid transparent",
                }}
              >
                {isRenaming ? (
                  <RenameRow
                    draft={renameDraft}
                    onChange={setRenameDraft}
                    onCommit={() => commitRename(slot.id)}
                    onCancel={() => setRenamingId(null)}
                  />
                ) : (
                  <SlotRow
                    slot={slot}
                    isActive={isActive}
                    onSelect={() => onSelectSlot(slot.id)}
                    onRename={() => startRename(slot)}
                    onDuplicate={() => handleDuplicate(slot)}
                    onDelete={() => handleDelete(slot)}
                  />
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

interface SlotRowProps {
  slot: SlotResponse;
  isActive: boolean;
  onSelect: () => void;
  onRename: () => void;
  onDuplicate: () => void;
  onDelete: () => void;
}

function SlotRow({
  slot,
  isActive,
  onSelect,
  onRename,
  onDuplicate,
  onDelete,
}: SlotRowProps) {
  return (
    <div className="flex items-center gap-1">
      <button
        type="button"
        onClick={onSelect}
        className="flex-1 text-left truncate cursor-pointer"
        aria-current={isActive ? "page" : undefined}
      >
        <div className="flex items-center justify-between gap-2">
          <span className="truncate">{slot.title}</span>
          <SlotStateBadge state={slot.state} />
        </div>
        {slot.workspace ? (
          <div className="text-[10px] opacity-50 mt-0.5">{slot.workspace}</div>
        ) : null}
      </button>
      <div
        className="flex items-center opacity-0 group-hover:opacity-100 transition-opacity"
        // Stop propagation so clicking an action doesn't also trigger
        // selection from the parent button.
        onClick={(e) => e.stopPropagation()}
      >
        <IconButton label="Rename slot" onClick={onRename}>
          <Pencil size={12} aria-hidden="true" />
        </IconButton>
        <IconButton label="Duplicate slot" onClick={onDuplicate}>
          <Copy size={12} aria-hidden="true" />
        </IconButton>
        <IconButton label="Delete slot" onClick={onDelete}>
          <Trash2 size={12} aria-hidden="true" />
        </IconButton>
      </div>
    </div>
  );
}

interface RenameRowProps {
  draft: string;
  onChange: (next: string) => void;
  onCommit: () => void;
  onCancel: () => void;
}

function RenameRow({ draft, onChange, onCommit, onCancel }: RenameRowProps) {
  const handleKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      onCommit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      onCancel();
    }
  };
  return (
    <div className="flex items-center gap-1">
      <input
        autoFocus
        value={draft}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={handleKey}
        onBlur={onCommit}
        className="flex-1 bg-transparent border rounded px-1 py-0.5 text-sm focus:outline-none"
        style={{ borderColor: "var(--color-border)" }}
        aria-label="Rename slot"
      />
      <IconButton label="Save" onClick={onCommit}>
        <Check size={12} aria-hidden="true" />
      </IconButton>
      <IconButton label="Cancel" onClick={onCancel}>
        <X size={12} aria-hidden="true" />
      </IconButton>
    </div>
  );
}

function IconButton({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={label}
      aria-label={label}
      className="p-1 rounded hover:bg-[color:var(--color-surface-muted)]"
    >
      {children}
    </button>
  );
}

function SlotStateBadge({ state }: { state: SlotResponse["state"] }) {
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

