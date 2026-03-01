import { useRef, useCallback, useLayoutEffect, useState } from 'react';

interface Props {
  rawToml: string;
  onChange: (raw: string) => void;
  disabled?: boolean;
}

/** Minimal TOML syntax highlighter — returns HTML with colored spans. */
function highlightToml(text: string): string {
  return text
    .split('\n')
    .map((line) => {
      // Full-line comment
      if (/^\s*#/.test(line)) {
        return `<span class="toml-comment">${esc(line)}</span>`;
      }

      // Section header [section] or [[array]]
      if (/^\s*\[{1,2}[^\]]*\]{1,2}\s*$/.test(line)) {
        return `<span class="toml-section">${esc(line)}</span>`;
      }

      // key = value
      const kv = line.match(/^(\s*)([\w."-]+)(\s*=\s*)(.*)/);
      if (kv) {
        const [, indent, key, eq, val] = kv;
        return `${esc(indent!)}<span class="toml-key">${esc(key!)}</span>${esc(eq!)}${highlightValue(val!)}`;
      }

      return esc(line);
    })
    .join('\n');
}

/** Highlight a TOML value fragment. */
function highlightValue(val: string): string {
  const trimmed = val.trim();

  // Inline comment after value
  const commentIdx = findInlineComment(val);
  if (commentIdx !== -1) {
    const before = val.slice(0, commentIdx);
    const comment = val.slice(commentIdx);
    return `${highlightValue(before)}<span class="toml-comment">${esc(comment)}</span>`;
  }

  // String (double or single quoted)
  if (/^".*"$/.test(trimmed) || /^'.*'$/.test(trimmed)) {
    return `<span class="toml-string">${esc(val)}</span>`;
  }
  // Boolean
  if (trimmed === 'true' || trimmed === 'false') {
    return `<span class="toml-bool">${esc(val)}</span>`;
  }
  // Number (int / float)
  if (/^[+-]?(\d[\d_]*\.?[\d_]*([eE][+-]?\d+)?|0x[\da-fA-F_]+|0o[0-7_]+|0b[01_]+|inf|nan)$/.test(trimmed)) {
    return `<span class="toml-number">${esc(val)}</span>`;
  }

  return esc(val);
}

/** Find index of an inline comment (# not inside a string). */
function findInlineComment(val: string): number {
  let inStr: string | null = null;
  for (let i = 0; i < val.length; i++) {
    const ch = val[i]!;
    if (inStr) {
      if (ch === inStr && val[i - 1] !== '\\') inStr = null;
    } else if (ch === '"' || ch === "'") {
      inStr = ch;
    } else if (ch === '#') {
      return i;
    }
  }
  return -1;
}

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export default function ConfigRawEditor({ rawToml, onChange, disabled }: Props) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const preRef = useRef<HTMLPreElement>(null);
  const gutterRef = useRef<HTMLDivElement>(null);
  const [lineCount, setLineCount] = useState(() => rawToml.split('\n').length);

  const syncScroll = useCallback(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    if (preRef.current) {
      preRef.current.scrollTop = ta.scrollTop;
      preRef.current.scrollLeft = ta.scrollLeft;
    }
    if (gutterRef.current) {
      gutterRef.current.scrollTop = ta.scrollTop;
    }
  }, []);

  // Keep line count in sync
  useLayoutEffect(() => {
    setLineCount(rawToml.split('\n').length);
  }, [rawToml]);

  const lineNumbers = Array.from({ length: lineCount }, (_, i) => i + 1);

  return (
    <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
      {/* Header bar — preserved from original */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-gray-800 bg-gray-800/50">
        <span className="text-xs text-gray-400 font-medium uppercase tracking-wider">
          TOML Configuration
        </span>
        <span className="text-xs text-gray-500">
          {lineCount} lines
        </span>
      </div>

      {/* Editor area */}
      <div style={{ position: 'relative', display: 'flex', minHeight: 500 }}>
        {/* Line number gutter */}
        <div
          ref={gutterRef}
          aria-hidden
          style={{
            overflow: 'hidden',
            flexShrink: 0,
            width: 48,
            paddingTop: 16,
            paddingBottom: 16,
            background: '#030712',
            borderRight: '1px solid #1f2937',
            userSelect: 'none',
            fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
            fontSize: '0.875rem',
            lineHeight: '1.25rem',
          }}
        >
          {lineNumbers.map((n) => (
            <div
              key={n}
              style={{
                textAlign: 'right',
                paddingRight: 12,
                color: '#4b5563',
                height: '1.25rem',
              }}
            >
              {n}
            </div>
          ))}
        </div>

        {/* Stacked layers: highlighted pre (behind) + transparent textarea (front) */}
        <div style={{ position: 'relative', flex: 1, overflow: 'hidden' }}>
          {/* Syntax-highlighted underlay */}
          <pre
            ref={preRef}
            aria-hidden
            style={{
              position: 'absolute',
              top: 0,
              left: 0,
              right: 0,
              bottom: 0,
              margin: 0,
              padding: 16,
              overflow: 'auto',
              fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
              fontSize: '0.875rem',
              lineHeight: '1.25rem',
              color: '#e5e7eb',
              background: '#030712',
              whiteSpace: 'pre',
              pointerEvents: 'none',
              tabSize: 4,
            }}
            dangerouslySetInnerHTML={{ __html: highlightToml(rawToml) + '\n' }}
          />

          {/* Editable textarea overlay */}
          <textarea
            ref={textareaRef}
            value={rawToml}
            onChange={(e) => onChange(e.target.value)}
            onScroll={syncScroll}
            disabled={disabled}
            spellCheck={false}
            aria-label="Raw TOML configuration editor"
            style={{
              position: 'relative',
              display: 'block',
              width: '100%',
              minHeight: 500,
              margin: 0,
              padding: 16,
              border: 'none',
              outline: 'none',
              resize: 'vertical',
              fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
              fontSize: '0.875rem',
              lineHeight: '1.25rem',
              color: 'transparent',
              caretColor: '#e5e7eb',
              background: 'transparent',
              whiteSpace: 'pre',
              overflow: 'auto',
              tabSize: 4,
            }}
            className="focus:ring-2 focus:ring-blue-500 focus:ring-inset disabled:opacity-50"
          />
        </div>
      </div>

      {/* Syntax colors */}
      <style>{`
        .toml-comment { color: #6b7280; font-style: italic; }
        .toml-section { color: #60a5fa; font-weight: 600; }
        .toml-key { color: #a78bfa; }
        .toml-string { color: #34d399; }
        .toml-bool { color: #f472b6; }
        .toml-number { color: #fbbf24; }
      `}</style>
    </div>
  );
}
