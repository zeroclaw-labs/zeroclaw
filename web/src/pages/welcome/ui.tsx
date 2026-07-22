/**
 * Shared visual primitives for the /welcome wizard.
 *
 * The wizard renders fullscreen outside the dashboard Layout, so it carries
 * its own fixed palette (pure black, terracotta accent, ivory text) via
 * inline styles instead of the themed pc-* tokens.
 */
import { useState, type CSSProperties, type ReactNode } from "react";
import { AlertCircle, ArrowLeft, ArrowRight, RotateCw } from "lucide-react";

export const C = {
  bg: "#000000",
  surface: "#0a0a0a",
  raised: "#111110",
  border: "#232220",
  borderStrong: "#3a3835",
  text: "#FAF9F5",
  muted: "#9c988f",
  faint: "#5f5c55",
  accent: "#D97757",
  accentSoft: "rgba(217, 119, 87, 0.12)",
  accentBorder: "rgba(217, 119, 87, 0.45)",
  error: "#e5695e",
  errorSoft: "rgba(229, 105, 94, 0.1)",
  ok: "#7fb069",
} as const;

export const INPUT_STYLE: CSSProperties = {
  width: "100%",
  padding: "10px 12px",
  borderRadius: 8,
  border: `1px solid ${C.border}`,
  background: C.raised,
  color: C.text,
  fontSize: 14,
  outline: "none",
  transition: "border-color 120ms ease, box-shadow 120ms ease",
};

/** Focus/blur handlers giving every input the terracotta focus ring. */
export function focusRing() {
  return {
    onFocus: (e: React.FocusEvent<HTMLElement>) => {
      e.currentTarget.style.borderColor = C.accentBorder;
      e.currentTarget.style.boxShadow = `0 0 0 3px ${C.accentSoft}`;
    },
    onBlur: (e: React.FocusEvent<HTMLElement>) => {
      e.currentTarget.style.borderColor = C.border;
      e.currentTarget.style.boxShadow = "none";
    },
  };
}

export function StepTitle({
  kicker,
  title,
  sub,
}: {
  kicker: string;
  title: string;
  sub?: ReactNode;
}) {
  return (
    <header style={{ marginBottom: 28 }} className="wlc-fade-up">
      <div
        style={{
          color: C.accent,
          fontSize: 12,
          letterSpacing: "0.18em",
          textTransform: "uppercase",
          fontWeight: 600,
          marginBottom: 10,
        }}
      >
        {kicker}
      </div>
      <h1
        style={{
          color: C.text,
          fontSize: 30,
          fontWeight: 600,
          letterSpacing: "-0.02em",
          lineHeight: 1.15,
          margin: 0,
        }}
      >
        {title}
      </h1>
      {sub ? (
        <p
          style={{
            color: C.muted,
            fontSize: 14.5,
            lineHeight: 1.6,
            marginTop: 10,
            maxWidth: 560,
          }}
        >
          {sub}
        </p>
      ) : null}
    </header>
  );
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label style={{ display: "block", marginBottom: 18 }}>
      <div
        style={{
          color: C.muted,
          fontSize: 11.5,
          letterSpacing: "0.14em",
          textTransform: "uppercase",
          fontWeight: 600,
          marginBottom: 7,
        }}
      >
        {label}
      </div>
      {children}
      {hint ? (
        <div style={{ color: C.faint, fontSize: 12.5, marginTop: 6, lineHeight: 1.5 }}>
          {hint}
        </div>
      ) : null}
    </label>
  );
}

export function TextInput(
  props: React.InputHTMLAttributes<HTMLInputElement>,
) {
  return <input {...props} {...focusRing()} style={{ ...INPUT_STYLE, ...props.style }} />;
}

export function TextArea(
  props: React.TextareaHTMLAttributes<HTMLTextAreaElement>,
) {
  return (
    <textarea
      {...props}
      {...focusRing()}
      style={{ ...INPUT_STYLE, resize: "vertical", minHeight: 96, ...props.style }}
    />
  );
}

/** Selectable card (provider picker, voice presets, …). Real button → keyboard friendly. */
export function OptionCard({
  selected,
  onSelect,
  title,
  blurb,
  badge,
  disabled,
}: {
  selected: boolean;
  onSelect: () => void;
  title: string;
  blurb?: string;
  badge?: string;
  disabled?: boolean;
}) {
  const [hover, setHover] = useState(false);
  return (
    <button
      type="button"
      disabled={disabled}
      aria-pressed={selected}
      onClick={onSelect}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      {...focusRing()}
      style={{
        textAlign: "left",
        padding: "14px 16px",
        borderRadius: 10,
        cursor: disabled ? "not-allowed" : "pointer",
        opacity: disabled ? 0.45 : 1,
        background: selected ? C.accentSoft : hover ? C.raised : C.surface,
        border: `1px solid ${selected ? C.accentBorder : C.border}`,
        transition: "background 120ms ease, border-color 120ms ease",
        display: "block",
        width: "100%",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ color: selected ? C.accent : C.text, fontSize: 14.5, fontWeight: 600 }}>
          {title}
        </span>
        {badge ? (
          <span
            style={{
              fontSize: 10.5,
              fontWeight: 600,
              letterSpacing: "0.08em",
              textTransform: "uppercase",
              color: C.accent,
              background: C.accentSoft,
              border: `1px solid ${C.accentBorder}`,
              borderRadius: 999,
              padding: "2px 8px",
            }}
          >
            {badge}
          </span>
        ) : null}
      </div>
      {blurb ? (
        <div style={{ color: C.muted, fontSize: 12.5, marginTop: 5, lineHeight: 1.5 }}>
          {blurb}
        </div>
      ) : null}
    </button>
  );
}

export function ErrorNote({
  message,
  onRetry,
}: {
  message: string;
  onRetry?: () => void;
}) {
  return (
    <div
      role="alert"
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: 10,
        padding: "12px 14px",
        borderRadius: 8,
        border: `1px solid ${C.error}44`,
        background: C.errorSoft,
        color: C.error,
        fontSize: 13.5,
        lineHeight: 1.5,
        marginBottom: 16,
      }}
    >
      <AlertCircle size={16} style={{ flexShrink: 0, marginTop: 2 }} />
      <div style={{ flex: 1, wordBreak: "break-word" }}>{message}</div>
      {onRetry ? (
        <button
          type="button"
          onClick={onRetry}
          {...focusRing()}
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            color: C.text,
            background: "transparent",
            border: `1px solid ${C.borderStrong}`,
            borderRadius: 6,
            padding: "4px 10px",
            fontSize: 12.5,
            cursor: "pointer",
            flexShrink: 0,
          }}
        >
          <RotateCw size={12} /> Retry
        </button>
      ) : null}
    </div>
  );
}

export function LoadingNote({ label }: { label: string }) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        color: C.muted,
        fontSize: 13.5,
        padding: "14px 0",
      }}
    >
      <span className="wlc-spinner" aria-hidden="true" />
      {label}
    </div>
  );
}

export function PrimaryButton({
  children,
  disabled,
  busy,
  onClick,
  type = "submit",
  big,
}: {
  children: ReactNode;
  disabled?: boolean;
  busy?: boolean;
  onClick?: () => void;
  type?: "submit" | "button";
  big?: boolean;
}) {
  const blocked = Boolean(disabled) || Boolean(busy);
  return (
    <button
      type={type}
      disabled={blocked}
      onClick={onClick}
      {...focusRing()}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 8,
        background: blocked ? "#6b4335" : C.accent,
        color: "#0a0a0a",
        border: "1px solid transparent",
        borderRadius: big ? 12 : 8,
        padding: big ? "16px 34px" : "10px 22px",
        fontSize: big ? 17 : 14,
        fontWeight: 600,
        cursor: blocked ? "not-allowed" : "pointer",
        transition: "background 120ms ease, transform 120ms ease",
      }}
    >
      {busy ? <span className="wlc-spinner wlc-spinner-dark" aria-hidden="true" /> : null}
      {children}
      {!busy && type === "submit" ? <ArrowRight size={big ? 18 : 15} /> : null}
    </button>
  );
}

export function GhostButton({
  children,
  onClick,
  type = "button",
}: {
  children: ReactNode;
  onClick?: () => void;
  type?: "button" | "submit";
}) {
  return (
    <button
      type={type}
      onClick={onClick}
      {...focusRing()}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 7,
        background: "transparent",
        color: C.muted,
        border: `1px solid ${C.border}`,
        borderRadius: 8,
        padding: "10px 18px",
        fontSize: 14,
        cursor: "pointer",
        transition: "color 120ms ease, border-color 120ms ease",
      }}
    >
      {children}
    </button>
  );
}

/**
 * Standard step footer: Back on the left, optional Skip + Continue on the
 * right. Rendered inside each step's <form>, so Enter anywhere in the panel
 * submits (Continue).
 */
export function StepFooter({
  onBack,
  backHidden,
  continueLabel = "Continue",
  continueDisabled,
  busy,
  onSkip,
  skipLabel = "Skip for now",
}: {
  onBack: () => void;
  backHidden?: boolean;
  continueLabel?: string;
  continueDisabled?: boolean;
  busy?: boolean;
  onSkip?: () => void;
  skipLabel?: string;
}) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        marginTop: 32,
        paddingTop: 20,
        borderTop: `1px solid ${C.border}`,
      }}
    >
      <div>
        {!backHidden ? (
          <GhostButton onClick={onBack}>
            <ArrowLeft size={15} /> Back
          </GhostButton>
        ) : null}
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
        {onSkip ? <GhostButton onClick={onSkip}>{skipLabel}</GhostButton> : null}
        <PrimaryButton disabled={continueDisabled} busy={busy}>
          {continueLabel}
        </PrimaryButton>
      </div>
    </div>
  );
}
