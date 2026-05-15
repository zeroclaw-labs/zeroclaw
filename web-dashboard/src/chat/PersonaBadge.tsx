/**
 * Persona badge (M4a, US-005).
 *
 * Renders next to the slot title in the sidebar so the operator can
 * see at a glance which persona (or provider/model pair) a slot is
 * configured to use. The badge ladders down through three states:
 *
 *   1. `agent_config.persona_preset` set      → show preset name
 *   2. `agent_config.provider` or `model` set → show "<provider>/<model>"
 *   3. neither set                            → render nothing (slot
 *      is using the gateway's inherited defaults; no need to clutter
 *      the row)
 */
import type { SlotAgentConfig } from "@/chat/slotMutations";

interface PersonaBadgeProps {
  config?: SlotAgentConfig;
}

export function PersonaBadge({ config }: PersonaBadgeProps) {
  if (!config) return null;

  const preset = config.persona_preset?.trim();
  if (preset) {
    return <Pill aria-label={`Persona ${preset}`}>{preset}</Pill>;
  }

  const provider = config.provider?.trim();
  const model = config.model?.trim();
  if (provider || model) {
    const label = [provider, model].filter(Boolean).join("/");
    return <Pill aria-label={`Provider ${label}`}>{label}</Pill>;
  }

  return null;
}

function Pill({ children, ...rest }: React.HTMLAttributes<HTMLSpanElement>) {
  return (
    <span
      {...rest}
      className="text-[9px] font-mono px-1.5 py-0.5 rounded truncate max-w-[120px]"
      style={{
        background: "var(--color-surface-muted)",
        color: "var(--color-text-muted)",
        border: "1px solid var(--color-border)",
      }}
    >
      {children}
    </span>
  );
}
