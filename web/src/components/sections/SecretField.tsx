import { useState } from "react";
import { Eye, EyeOff, X } from "lucide-react";
import { t } from "../../lib/i18n";

// A populated secret displays a constant mask and a Change button. The stored
// value and its length are never sent to the browser. Change edits a new draft
// only; the reveal toggle can reveal that draft, never the saved key. It uses a masked
// input with a reveal/hide eye toggle and a cancel control that reverts to the
// set state. An unset secret renders the input directly with no cancel.
//
// The draft contract is unchanged: an empty value means "keep the stored
// secret" (handled by handleSave's `e.is_secret && raw.length === 0` guard), so
// cancelling clears the draft back to empty.
export function SecretField({
  inputId,
  populated,
  value,
  onChange,
  multiline = false,
}: {
  inputId: string;
  populated: boolean;
  value: string;
  onChange: (next: string) => void;
  multiline?: boolean;
}) {
  // Start in change mode when a populated field already carries a staged draft
  // value (operator typed a replacement, navigated away, came back). Otherwise
  // the pending edit would hide behind the "set" indicator and the operator
  // could not see or read back their own unsaved change.
  const [changing, setChanging] = useState(populated && value.length > 0);
  const [revealed, setRevealed] = useState(false);

  const editing = !populated || changing;

  if (!editing) {
    return (
      <div className="flex items-center gap-2">
        <span aria-hidden="true" className="font-mono text-sm">********</span>
        <button
          type="button"
          onClick={() => {
            setChanging(true);
            setRevealed(false);
          }}
          className="btn-secondary text-sm px-3 py-1.5 inline-flex items-center gap-1"
        >
          {t("fieldform.secret_change")}
        </button>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <div className="relative flex-1">
        {multiline ? (
          <textarea
            id={inputId}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className={`input-electric w-full px-3 py-2 pr-10 text-sm font-mono min-h-[10rem] resize-y${
              revealed ? "" : " secret-mask"
            }`}
            placeholder={t("fieldform.secret_enter_placeholder")}
            autoComplete="off"
            spellCheck={false}
          />
        ) : (
          <input
            id={inputId}
            type={revealed ? "text" : "password"}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 pr-10 text-sm"
            placeholder={t("fieldform.secret_enter_placeholder")}
            autoComplete="off"
          />
        )}
        <button
          type="button"
          onClick={() => setRevealed((r) => !r)}
          title={revealed ? t("fieldform.secret_hide") : t("fieldform.secret_reveal")}
          aria-label={revealed ? t("fieldform.secret_hide") : t("fieldform.secret_reveal")}
          aria-pressed={revealed}
          className={
            multiline
              ? "btn-icon absolute right-1.5 top-2"
              : "btn-icon absolute right-1.5 top-1/2 -translate-y-1/2"
          }
        >
          {revealed ? (
            <EyeOff className="h-4 w-4" />
          ) : (
            <Eye className="h-4 w-4" />
          )}
        </button>
      </div>
      {populated && (
        <button
          type="button"
          onClick={() => {
            onChange("");
            setChanging(false);
            setRevealed(false);
          }}
          title={t("fieldform.secret_cancel_change")}
          aria-label={t("fieldform.secret_cancel_change")}
          className="btn-icon flex-shrink-0"
        >
          <X className="h-4 w-4" />
        </button>
      )}
    </div>
  );
}
