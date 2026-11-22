import { createPortal } from "react-dom";
import { Link } from "react-router-dom";
import { Bell } from "lucide-react";
import { t } from "@/lib/i18n";

/** Render directly from the task owner's pending request; no parallel queue. */
export default function WorkspaceAttention({
  to,
  label,
  onOpen,
}: {
  to: string;
  label: string;
  onOpen?: () => void;
}) {
  const target = document.getElementById("workspace-attention");
  return target
    ? createPortal(
        <div role="status"><Link
          to={to}
          onClick={onOpen}
          className="flex items-center gap-2 border-b border-pc-border bg-pc-elevated px-4 py-2 text-sm text-pc-accent"
        >
          <Bell className="h-4 w-4 shrink-0" />
          <span>
            {t("workspace.needs_attention")} · {label}
          </span>
        </Link></div>,
        target,
      )
    : null;
}
