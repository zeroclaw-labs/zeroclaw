// Pending tool-approval card. Default-deny on timeout: the gateway's
// WsApprovalChannel denies after 120s, so when timeout_at passes the
// store evicts the card and the agent loop sees a deterministic deny.

import { useEffect, useState } from "react";
import { Check, X, ShieldCheck, Loader2 } from "lucide-react";
import {
  useApproveTool,
  type ApprovalDecision,
  type PendingApproval,
} from "@/tools/approvalQueue";

interface ApprovalCardProps {
  approval: PendingApproval;
  /** Optional compact variant for the Board page (no args preview). */
  compact?: boolean;
}

const ARGS_PREVIEW_LIMIT = 280;

export function ApprovalCard({ approval, compact = false }: ApprovalCardProps) {
  const approve = useApproveTool();
  const [pendingDecision, setPendingDecision] =
    useState<ApprovalDecision | null>(null);
  const remainingSecs = useCountdown(approval.timeout_at);

  const fire = (decision: ApprovalDecision) => {
    if (approve.isPending) return;
    setPendingDecision(decision);
    approve.mutate(
      {
        slot_id: approval.slot_id,
        request_id: approval.request_id,
        decision,
      },
      // Reset on settle so a failed approval re-enables the buttons.
      // Card stays mounted until the store removes it (success/timeout).
      { onSettled: () => setPendingDecision(null) },
    );
  };

  const trimmedArgs = truncate(approval.arguments_summary, ARGS_PREVIEW_LIMIT);

  return (
    <div
      role="region"
      aria-label="Tool approval request"
      className="rounded border px-3 py-2 my-2"
      style={{
        background: "var(--color-surface-muted)",
        borderColor: "var(--color-border)",
        color: "var(--color-text)",
      }}
    >
      <div className="flex items-center justify-between gap-2 mb-1">
        <div className="flex items-center gap-1 text-xs font-semibold">
          <ShieldCheck size={12} aria-hidden="true" />
          <span>Tool approval needed</span>
          <code
            className="font-mono ml-1 px-1 rounded"
            style={{ background: "var(--color-surface)" }}
          >
            {approval.tool_name}
          </code>
        </div>
        <span
          className="text-[10px] tabular-nums"
          style={{ color: "var(--color-text-muted)" }}
          aria-label={`Auto-deny in ${remainingSecs} seconds`}
        >
          {remainingSecs}s
        </span>
      </div>
      {!compact && trimmedArgs.length > 0 ? (
        <pre
          className="text-[11px] mb-2 p-2 rounded whitespace-pre-wrap break-words font-mono"
          style={{
            background: "var(--color-surface)",
            color: "var(--color-text-muted)",
            maxHeight: "140px",
            overflow: "auto",
          }}
        >
          {trimmedArgs}
        </pre>
      ) : null}
      <div className="flex items-center gap-2">
        <DecisionButton
          decision="approve"
          tone="accent"
          icon={<Check size={12} aria-hidden="true" />}
          label="Approve"
          ariaLabel={`Approve ${approval.tool_name}`}
          disabled={approve.isPending}
          isActive={pendingDecision === "approve"}
          onClick={() => fire("approve")}
        />
        <DecisionButton
          decision="deny"
          tone="default"
          icon={<X size={12} aria-hidden="true" />}
          label="Deny"
          ariaLabel={`Deny ${approval.tool_name}`}
          disabled={approve.isPending}
          isActive={pendingDecision === "deny"}
          onClick={() => fire("deny")}
        />
        <DecisionButton
          decision="always"
          tone="default"
          icon={<ShieldCheck size={12} aria-hidden="true" />}
          label="Always"
          ariaLabel={`Always approve ${approval.tool_name}`}
          disabled={approve.isPending}
          isActive={pendingDecision === "always"}
          onClick={() => fire("always")}
        />
        {approve.isError ? (
          <span
            role="alert"
            className="text-[11px] ml-auto"
            style={{ color: "var(--color-text-muted)" }}
          >
            {approve.error instanceof Error
              ? approve.error.message
              : "Failed"}
          </span>
        ) : null}
      </div>
    </div>
  );
}

interface DecisionButtonProps {
  decision: ApprovalDecision;
  tone: "accent" | "default";
  icon: React.ReactNode;
  label: string;
  ariaLabel: string;
  disabled: boolean;
  isActive: boolean;
  onClick: () => void;
}

function DecisionButton({
  tone,
  icon,
  label,
  ariaLabel,
  disabled,
  isActive,
  onClick,
}: DecisionButtonProps) {
  const isAccent = tone === "accent";
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={ariaLabel}
      className="inline-flex items-center gap-1 text-xs px-2 py-1 rounded border disabled:opacity-50"
      style={{
        borderColor: "var(--color-border)",
        background: isAccent
          ? "var(--color-accent)"
          : "var(--color-surface)",
        color: isAccent ? "var(--color-surface)" : "var(--color-text)",
      }}
    >
      {isActive ? (
        <Loader2 size={12} aria-hidden="true" className="animate-spin" />
      ) : (
        icon
      )}
      {label}
    </button>
  );
}

function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return `${text.slice(0, max)}\n…[truncated ${text.length - max} chars]`;
}

function useCountdown(deadlineMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(id);
  }, []);
  return Math.max(0, Math.ceil((deadlineMs - now) / 1000));
}
