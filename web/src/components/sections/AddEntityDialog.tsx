// "+ Add" dialog for the Config master-detail navigator.
//
// Reuses the existing add-alias flow (`selectSectionItem`) and the shared
// `SectionPicker` so no authoring logic is duplicated:
//
//   one_tier_alias_map (e.g. agents, skill_bundles)
//     → just ask for an alias name, then selectSectionItem(section, alias).
//   typed_family_map (e.g. providers, channels)
//     → pick a TYPE via SectionPicker, then ask for an alias, then
//       selectSectionItem(section, type, alias).
//
// On success it calls onCreated(url) with the new entity's form URL so the
// parent can navigate (the existing dispatch then renders the right
// editor). Alias validation matches the wizard's rules verbatim — kept in
// sync with `zeroclaw_config::helpers::validate_alias_key`.

import { useEffect, useRef, useState } from "react";
import { ArrowLeft, X } from "lucide-react";
import {
  ApiError,
  getMapKeys,
  selectSectionItem,
  type SectionInfo,
} from "../../lib/api";
import SectionPicker from "./SectionPicker";
import { Button } from "@/components/ui";

function suggestAlias(aliases: string[]): string {
  const used = new Set(aliases);
  if (!used.has("default")) return "default";
  for (let i = 2; i < 100; i += 1) {
    const candidate = `default_${i}`;
    if (!used.has(candidate)) return candidate;
  }
  return "default_100";
}

function validateAlias(alias: string): string | null {
  if (/^(?!_)(?!.*__)(?!.*_$)[a-z0-9_]{1,63}$/.test(alias)) return null;
  return "Alias must use lowercase letters, digits, or single underscores only; no hyphens, dots, spaces, leading/trailing underscores, or double underscores.";
}

interface AddEntityDialogProps {
  section: SectionInfo;
  onClose: () => void;
  /** Called with the new entity's form URL once created. */
  onCreated: (url: string) => void;
}

export default function AddEntityDialog({
  section,
  onClose,
  onCreated,
}: AddEntityDialogProps) {
  const isTyped = section.shape === "typed_family_map";
  // For typed sections, step 1 is choosing a type. One-tier sections skip
  // straight to the alias step.
  const [type, setType] = useState<string | null>(isTyped ? null : "");
  const [alias, setAlias] = useState("");
  const [existing, setExisting] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const onAliasStep = type !== null;

  // Load existing aliases (for the placeholder suggestion + dup avoidance)
  // once a type is chosen (typed) or immediately (one-tier).
  useEffect(() => {
    if (!onAliasStep) return;
    const mapPath = isTyped ? `${section.key}.${type}` : section.key;
    let cancelled = false;
    getMapKeys(mapPath)
      .then((r) => {
        if (!cancelled) setExisting(r.keys);
      })
      .catch(() => {
        if (!cancelled) setExisting([]);
      });
    inputRef.current?.focus();
    return () => {
      cancelled = true;
    };
  }, [onAliasStep, isTyped, section.key, type]);

  // Esc closes.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const submit = async () => {
    const trimmed = alias.trim() || suggestAlias(existing);
    const validationError = validateAlias(trimmed);
    if (validationError) {
      setError(validationError);
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      if (isTyped) {
        await selectSectionItem(section.key, type as string, trimmed);
        onCreated(
          `/config/${encodeURIComponent(section.key)}/${encodeURIComponent(type as string)}/${encodeURIComponent(trimmed)}`,
        );
      } else {
        await selectSectionItem(section.key, trimmed);
        onCreated(
          `/config/${encodeURIComponent(section.key)}/${encodeURIComponent(trimmed)}`,
        );
      }
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.envelope.message
          : e instanceof Error
            ? e.message
            : String(e),
      );
      setSubmitting(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-lg rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-5 shadow-xl flex flex-col gap-4 max-h-[85vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between gap-3">
          <h2 className="text-base font-semibold text-pc-text">
            Add to {section.label}
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="btn-icon flex-shrink-0"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {error && (
          <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error">
            {error}
          </div>
        )}

        {!onAliasStep ? (
          // Step 1 (typed sections): choose the type via the shared picker.
          <SectionPicker
            sectionKey={section.key}
            help={section.help}
            onPick={(item) => {
              setType(item.key);
              setAlias("");
              setError(null);
            }}
          />
        ) : (
          // Step 2 (or only step for one-tier): name the alias.
          <div className="flex flex-col gap-3">
            {isTyped && (
              <button
                type="button"
                onClick={() => {
                  setType(null);
                  setError(null);
                }}
                className="self-start flex items-center gap-1 text-xs text-pc-text-muted hover:text-pc-text"
              >
                <ArrowLeft className="h-3.5 w-3.5" />
                Choose a different type ({type})
              </button>
            )}
            <p className="text-xs text-pc-text-secondary leading-relaxed">
              A short stable name you'll use elsewhere in config to point at
              this entry. Lowercase letters, digits, single underscores; 1–63
              chars; no leading/trailing/double underscores, dots, hyphens, or
              spaces.
            </p>
            <div className="flex items-center gap-2">
              <input
                ref={inputRef}
                type="text"
                className="input-electric flex-1 px-3 py-2 text-sm"
                placeholder={suggestAlias(existing)}
                value={alias}
                onChange={(e) => {
                  setAlias(e.target.value);
                  setError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void submit();
                }}
              />
              <Button
                variant="primary"
                size="sm"
                onClick={() => void submit()}
                disabled={submitting}
                className="flex-shrink-0"
              >
                {submitting ? "Adding…" : "Add"}
              </Button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
