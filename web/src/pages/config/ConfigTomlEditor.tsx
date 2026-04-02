import { useRef, useCallback } from 'react';
import { t } from '@/lib/i18n';

// ---------------------------------------------------------------------------
// Lightweight zero-dependency TOML syntax highlighter.
// ---------------------------------------------------------------------------
function highlightToml(raw: string): string {
  const lines = raw.split('\n');
  const result: string[] = [];

  for (const line of lines) {
    const escaped = line
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');

    if (/^\s*\[/.test(escaped)) {
      result.push(`<span style="color:#67e8f9;font-weight:600">${escaped}</span>`);
      continue;
    }

    if (/^\s*#/.test(escaped)) {
      result.push(`<span style="color:#52525b;font-style:italic">${escaped}</span>`);
      continue;
    }

    const kvMatch = escaped.match(/^(\s*)([A-Za-z0-9_\-.]+)(\s*=\s*)(.*)$/);
    if (kvMatch) {
      const [, indent, key, eq, rawValue] = kvMatch;
      const value = colorValue(rawValue ?? '');
      result.push(
        `${indent}<span style="color:#a78bfa">${key}</span>`
        + `<span style="color:#71717a">${eq}</span>${value}`
      );
      continue;
    }

    result.push(escaped);
  }

  return result.join('\n') + '\n';
}

function colorValue(v: string): string {
  const trimmed = v.trim();
  const commentIdx = findUnquotedHash(trimmed);
  if (commentIdx !== -1) {
    const valueCore = trimmed.slice(0, commentIdx).trimEnd();
    const comment = `<span style="color:#52525b;font-style:italic">${trimmed.slice(commentIdx)}</span>`;
    const leading = v.slice(0, v.indexOf(trimmed));
    return leading + colorScalar(valueCore) + ' ' + comment;
  }
  return colorScalar(v);
}

function findUnquotedHash(s: string): number {
  let inSingle = false;
  let inDouble = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "'" && !inDouble) inSingle = !inSingle;
    else if (c === '"' && !inSingle) inDouble = !inDouble;
    else if (c === '#' && !inSingle && !inDouble) return i;
  }
  return -1;
}

function colorScalar(v: string): string {
  const tr = v.trim();
  if (tr === 'true' || tr === 'false')
    return `<span style="color:#34d399">${v}</span>`;
  if (/^-?\d[\d_]*(\.[\d_]*)?([eE][+-]?\d+)?$/.test(tr))
    return `<span style="color:#fbbf24">${v}</span>`;
  if (tr.startsWith('"') || tr.startsWith("'"))
    return `<span style="color:#86efac">${v}</span>`;
  if (tr.startsWith('[') || tr.startsWith('{'))
    return `<span style="color:#e2e8f0">${v}</span>`;
  if (/^\d{4}-\d{2}-\d{2}/.test(tr))
    return `<span style="color:#fb923c">${v}</span>`;
  return v;
}

interface ConfigTomlEditorProps {
  value: string;
  onChange: (v: string) => void;
}

export default function ConfigTomlEditor({ value, onChange }: ConfigTomlEditorProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const preRef = useRef<HTMLPreElement>(null);

  const syncScroll = useCallback(() => {
    if (preRef.current && textareaRef.current) {
      preRef.current.scrollTop = textareaRef.current.scrollTop;
      preRef.current.scrollLeft = textareaRef.current.scrollLeft;
    }
  }, []);

  return (
    <div className="card overflow-hidden rounded-2xl flex flex-col flex-1 min-h-0">
      <div className="flex items-center justify-between px-4 py-2.5 border-b" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-accent-glow)' }}>
        <span className="text-[10px] font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-muted)' }}>
          {t('config.toml_label')}
        </span>
        <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
          {value.split('\n').length} {t('config.lines')}
        </span>
      </div>
      <div className="relative flex-1 min-h-0 overflow-hidden">
        <pre
          ref={preRef}
          aria-hidden="true"
          className="absolute inset-0 text-sm p-4 font-mono overflow-auto whitespace-pre pointer-events-none m-0"
          style={{ background: 'var(--pc-bg-base)', tabSize: 4 }}
          dangerouslySetInnerHTML={{ __html: highlightToml(value) }}
        />
        <textarea
          ref={textareaRef}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onScroll={syncScroll}
          onKeyDown={(e) => {
            if (e.key === 'Tab') {
              e.preventDefault();
              const el = e.currentTarget;
              const start = el.selectionStart;
              const end = el.selectionEnd;
              onChange(value.slice(0, start) + '  ' + value.slice(end));
              requestAnimationFrame(() => { el.selectionStart = el.selectionEnd = start + 2; });
            }
          }}
          spellCheck={false}
          className="absolute inset-0 w-full h-full text-sm p-4 resize-none focus:outline-none font-mono caret-white"
          style={{ background: 'transparent', color: 'transparent', tabSize: 4 }}
        />
      </div>
    </div>
  );
}
