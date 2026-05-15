/**
 * Slot settings drawer (M4a, US-005).
 *
 * Quick tab — apply a persona preset (one click stamps provider,
 * model, personality, mode onto the slot via `PATCH /api/slots/:id`).
 *
 * Advanced tab — per-field overrides for users who want to mix and
 * match outside any saved preset:
 *   * Provider — dropdown sourced from `/api/providers`
 *   * Model — text input (free-form; provider entries already have
 *     their default model, this overrides per-slot)
 *   * Mode — Normal / Trust / Yolo radio
 *   * Personality — dropdown sourced from `/api/personality` index
 *     (filtered to the allowlisted set the runtime knows how to load)
 *
 * The drawer is a fixed-position panel slid in from the right of the
 * viewport. Closing it triggers a `["slots"]` invalidation so the
 * sidebar re-renders the persona badge.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { X } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import {
  type SlotAgentConfig,
  type SlotResponse,
  useRenameSlot,
} from "@/chat/slotMutations";
import { useProviders } from "@/models/providersQuery";
import {
  type PersonaPreset,
  type SlotMode,
  usePersonas,
} from "@/personas/personasQuery";
import { usePersonalityIndex } from "@/personas/personalityFilesQuery";

interface SettingsDrawerProps {
  /** When non-null, the drawer is open for this slot. Pass `null` to close. */
  slot: SlotResponse | null;
  onClose: () => void;
}

type Tab = "quick" | "advanced";

export function SettingsDrawer({ slot, onClose }: SettingsDrawerProps) {
  const [tab, setTab] = useState<Tab>("quick");
  const dialogRef = useRef<HTMLDivElement>(null);

  // Reset tab to Quick when the drawer reopens for a new slot.
  useEffect(() => {
    if (slot) setTab("quick");
  }, [slot?.id]);

  // Esc closes; click-outside closes via the overlay onClick.
  useEffect(() => {
    if (!slot) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [slot, onClose]);

  if (!slot) return null;

  return (
    <div
      className="fixed inset-0 z-40 flex justify-end"
      role="dialog"
      aria-modal="true"
      aria-label="Slot settings"
    >
      <button
        type="button"
        aria-label="Close settings"
        className="flex-1 bg-black/30"
        onClick={onClose}
      />
      <div
        ref={dialogRef}
        className="flex flex-col w-[360px] max-w-[90vw] border-l overflow-y-auto"
        style={{
          background: "var(--color-surface)",
          borderColor: "var(--color-border)",
        }}
      >
        <header
          className="flex items-center justify-between px-4 py-3 border-b"
          style={{ borderColor: "var(--color-border)" }}
        >
          <div>
            <div className="text-xs uppercase tracking-wider opacity-60">
              Slot settings
            </div>
            <div className="text-sm font-semibold truncate max-w-[240px]">
              {slot.title}
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="p-1 rounded hover:bg-[color:var(--color-surface-muted)]"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </header>

        <nav
          className="flex border-b text-sm"
          style={{ borderColor: "var(--color-border)" }}
        >
          <TabButton active={tab === "quick"} onClick={() => setTab("quick")}>
            Quick
          </TabButton>
          <TabButton
            active={tab === "advanced"}
            onClick={() => setTab("advanced")}
          >
            Advanced
          </TabButton>
        </nav>

        <div className="flex-1 p-4 space-y-4">
          {tab === "quick" ? (
            <QuickTab slot={slot} onApplied={onClose} />
          ) : (
            <AdvancedTab slot={slot} onApplied={onClose} />
          )}
        </div>
      </div>
    </div>
  );
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex-1 px-4 py-2 text-center"
      style={{
        background: active ? "var(--color-surface-muted)" : "transparent",
        borderBottom: active
          ? "2px solid var(--color-accent)"
          : "2px solid transparent",
      }}
    >
      {children}
    </button>
  );
}

interface TabPaneProps {
  slot: SlotResponse;
  onApplied: () => void;
}

function QuickTab({ slot, onApplied }: TabPaneProps) {
  const personas = usePersonas();
  const renameSlot = useRenameSlot();
  const qc = useQueryClient();
  const list = personas.data?.personas ?? [];

  const handleApply = (preset: PersonaPreset) => {
    const next: SlotAgentConfig = {
      provider: preset.provider,
      model: preset.model ?? null,
      personality: preset.personality ?? null,
      mode: preset.mode,
      persona_preset: preset.name,
    };
    renameSlot.mutate(
      { id: slot.id, agent_config: next },
      {
        onSuccess: () => {
          void qc.invalidateQueries({ queryKey: ["slots"] });
          onApplied();
        },
      },
    );
  };

  if (personas.isLoading) {
    return <div className="text-xs opacity-60">Loading presets…</div>;
  }
  if (personas.error) {
    return (
      <div className="text-xs text-red-600">
        Failed to load personas: {String(personas.error)}
      </div>
    );
  }
  if (list.length === 0) {
    return (
      <div className="text-xs opacity-60">
        No persona presets configured. The gateway seeds four defaults on first
        list — try refreshing.
      </div>
    );
  }

  return (
    <ul className="space-y-2">
      {list.map((preset) => {
        const isCurrent = slot.agent_config?.persona_preset === preset.name;
        return (
          <li key={preset.name}>
            <button
              type="button"
              onClick={() => handleApply(preset)}
              disabled={renameSlot.isPending}
              className="w-full text-left p-3 rounded border transition-colors"
              style={{
                borderColor: isCurrent
                  ? "var(--color-accent)"
                  : "var(--color-border)",
                background: isCurrent
                  ? "var(--color-surface-muted)"
                  : "var(--color-surface)",
              }}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="font-medium text-sm">{preset.name}</span>
                <span className="text-[10px] opacity-60 font-mono">
                  {preset.provider}
                  {preset.model ? ` · ${preset.model}` : ""}
                </span>
              </div>
              {preset.description ? (
                <div className="text-xs opacity-70 mt-1">{preset.description}</div>
              ) : null}
            </button>
          </li>
        );
      })}
    </ul>
  );
}

function AdvancedTab({ slot, onApplied }: TabPaneProps) {
  const providers = useProviders();
  const personality = usePersonalityIndex();
  const renameSlot = useRenameSlot();
  const qc = useQueryClient();

  const initial = slot.agent_config ?? {};
  const [providerId, setProviderId] = useState(initial.provider ?? "");
  const [model, setModel] = useState(initial.model ?? "");
  const [mode, setMode] = useState<SlotMode>(
    (initial.mode as SlotMode | undefined) ?? "normal",
  );
  const [personalityFile, setPersonalityFile] = useState(
    initial.personality ?? "",
  );

  const personalityOptions = useMemo(
    () => personality.data?.files ?? [],
    [personality.data],
  );

  const handleSave = () => {
    const next: SlotAgentConfig = {
      provider: providerId.trim() || null,
      model: model.trim() || null,
      mode,
      personality: personalityFile.trim() || null,
      // Advanced edits clear the preset link — the slot's identity is
      // now hand-rolled rather than tied to a named preset.
      persona_preset: null,
    };
    renameSlot.mutate(
      { id: slot.id, agent_config: next },
      {
        onSuccess: () => {
          void qc.invalidateQueries({ queryKey: ["slots"] });
          onApplied();
        },
      },
    );
  };

  return (
    <div className="space-y-3">
      <Field label="Provider">
        <select
          value={providerId}
          onChange={(e) => setProviderId(e.target.value)}
          className="w-full bg-transparent border rounded px-2 py-1 text-sm"
          style={{ borderColor: "var(--color-border)" }}
        >
          <option value="">(inherit gateway default)</option>
          {(providers.data?.providers ?? []).map((p) => (
            <option key={p.id} value={p.id}>
              {p.display_name}
              {p.is_fallback ? " (fallback)" : ""}
            </option>
          ))}
        </select>
      </Field>

      <Field label="Model">
        <input
          type="text"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          placeholder="(use provider default)"
          className="w-full bg-transparent border rounded px-2 py-1 text-sm"
          style={{ borderColor: "var(--color-border)" }}
        />
      </Field>

      <Field label="Mode">
        <div className="flex gap-2 text-sm">
          {(["normal", "trust", "yolo"] as const).map((value) => (
            <label
              key={value}
              className="flex items-center gap-1 cursor-pointer"
            >
              <input
                type="radio"
                name="mode"
                value={value}
                checked={mode === value}
                onChange={() => setMode(value)}
              />
              <span className="capitalize">{value}</span>
            </label>
          ))}
        </div>
      </Field>

      <Field label="Personality">
        <select
          value={personalityFile}
          onChange={(e) => setPersonalityFile(e.target.value)}
          className="w-full bg-transparent border rounded px-2 py-1 text-sm"
          style={{ borderColor: "var(--color-border)" }}
        >
          <option value="">(default identity stack)</option>
          {personalityOptions.map((entry) => (
            <option key={entry.filename} value={entry.filename}>
              {entry.filename}
              {entry.exists ? "" : " (empty)"}
            </option>
          ))}
        </select>
      </Field>

      <button
        type="button"
        onClick={handleSave}
        disabled={renameSlot.isPending}
        className="w-full px-3 py-2 rounded text-sm font-medium disabled:opacity-50"
        style={{
          background: "var(--color-accent)",
          color: "var(--color-surface)",
        }}
      >
        {renameSlot.isPending ? "Saving…" : "Save"}
      </button>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span className="text-xs uppercase tracking-wider opacity-60 block mb-1">
        {label}
      </span>
      {children}
    </label>
  );
}
