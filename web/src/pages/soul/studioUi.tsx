/**
 * Shared design tokens + small styled primitives for Soul Studio.
 * Design language: pure black, terracotta accents, ivory text.
 */

import type { CSSProperties, ReactNode } from 'react';

export const S = {
  bg: '#000000',
  surface: '#0a0a0a',
  surfaceRaised: '#121110',
  border: 'rgba(250, 249, 245, 0.10)',
  borderStrong: 'rgba(250, 249, 245, 0.18)',
  text: '#FAF9F5',
  muted: 'rgba(250, 249, 245, 0.58)',
  faint: 'rgba(250, 249, 245, 0.34)',
  accent: '#D97757',
  accentSoft: 'rgba(217, 119, 87, 0.16)',
  accentBorder: 'rgba(217, 119, 87, 0.45)',
  danger: '#f87171',
  dangerSoft: 'rgba(239, 68, 68, 0.10)',
} as const;

export function SectionCard({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <section
      className="rounded-xl border p-4 flex flex-col gap-3"
      style={{ background: S.surface, borderColor: S.border }}
    >
      <div>
        <h3
          className="text-[11px] font-semibold uppercase tracking-[0.14em]"
          style={{ color: S.accent }}
        >
          {title}
        </h3>
        {hint && (
          <p className="mt-1 text-xs leading-relaxed" style={{ color: S.faint }}>
            {hint}
          </p>
        )}
      </div>
      {children}
    </section>
  );
}

const inputBase: CSSProperties = {
  background: S.surfaceRaised,
  border: `1px solid ${S.border}`,
  color: S.text,
  caretColor: S.accent,
};

export function TextInput({
  value,
  onChange,
  placeholder,
  onKeyDown,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  onKeyDown?: (e: React.KeyboardEvent<HTMLInputElement>) => void;
}) {
  return (
    <input
      type="text"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={onKeyDown}
      placeholder={placeholder}
      className="w-full rounded-lg px-3 py-2 text-sm outline-none focus:border-[#D97757]"
      style={inputBase}
    />
  );
}

export function TextArea({
  value,
  onChange,
  placeholder,
  rows = 3,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  rows?: number;
}) {
  return (
    <textarea
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      rows={rows}
      className="w-full resize-y rounded-lg px-3 py-2 text-sm leading-relaxed outline-none focus:border-[#D97757]"
      style={inputBase}
    />
  );
}

/** A temperament axis slider with the two pole labels. */
export function AxisSlider({
  left,
  right,
  value,
  onChange,
}: {
  left: string;
  right: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center justify-between text-xs">
        <span style={{ color: value <= 50 ? S.text : S.faint }}>{left}</span>
        <span style={{ color: value > 50 ? S.text : S.faint }}>{right}</span>
      </div>
      <input
        type="range"
        min={0}
        max={100}
        step={5}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="w-full"
        style={{ accentColor: S.accent }}
        aria-label={`${left} to ${right}`}
      />
    </div>
  );
}

export function Chip({
  label,
  selected,
  onClick,
  onRemove,
}: {
  label: string;
  selected: boolean;
  onClick?: () => void;
  onRemove?: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onRemove ?? onClick}
      className="rounded-full border px-3 py-1 text-xs transition-colors"
      style={{
        background: selected ? S.accentSoft : 'transparent',
        borderColor: selected ? S.accentBorder : S.border,
        color: selected ? S.accent : S.muted,
      }}
      title={onRemove ? 'Remove' : undefined}
    >
      {label}
      {onRemove && <span className="ml-1.5 opacity-70">×</span>}
    </button>
  );
}

export function Toggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className="flex items-center gap-2.5 text-left text-sm"
      style={{ color: checked ? S.text : S.muted }}
    >
      <span
        className="relative inline-flex h-4.5 w-8 shrink-0 items-center rounded-full transition-colors"
        style={{
          background: checked ? S.accent : S.surfaceRaised,
          border: `1px solid ${checked ? S.accent : S.borderStrong}`,
          height: 18,
          width: 32,
        }}
      >
        <span
          className="absolute h-3 w-3 rounded-full transition-all"
          style={{
            background: checked ? '#000' : S.faint,
            left: checked ? 15 : 3,
          }}
        />
      </span>
      {label}
    </button>
  );
}

export function ErrorNote({ children }: { children: ReactNode }) {
  return (
    <div
      className="rounded-lg border p-3 text-sm"
      style={{
        background: S.dangerSoft,
        borderColor: 'rgba(239, 68, 68, 0.25)',
        color: S.danger,
      }}
    >
      {children}
    </div>
  );
}
