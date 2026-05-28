// Chat history export — Markdown / JSON / HTML / plaintext renderers for a
// session's `PersistedChatBubble[]`. Pattern adapted from claude-code's
// `web/lib/export/` (4 mirror files), collapsed into a single TS module
// against zeroclaw's chat shape.
//
// Used by the chat UI's "Export" menu. Pure functions with no side-effects;
// the caller is responsible for triggering the download via [`downloadText`].

import type { PersistedChatBubble } from './chatHistoryStorage';

/**
 * Shape accepted by the exporters. Compatible with both
 * `PersistedChatBubble` (timestamp: string) and the in-memory
 * `ChatMessage` shape used by `AgentContext` (timestamp: Date), plus any
 * `toolCall` structure that has at least a `name`. Lets callers export
 * either persisted history or live messages without an adapter step.
 *
 * Defined here rather than imported from a sibling so consumers do not
 * need a separate file to bring it in. Previously the type was referenced
 * by the four exporter signatures below without ever being declared,
 * which slipped past the local typecheck but broke the production
 * `tsc -b && vite build` in the release workflow (see PR #11).
 */
export interface ExportableMessage {
  role: PersistedChatBubble['role'];
  content: string;
  thinking?: string;
  toolCall?: { name: string; args?: unknown; output?: string };
  timestamp: string | Date;
}

export interface ExportOptions {
  /** Include `[YYYY-MM-DD HH:MM:SS]` per message. Default: `true`. */
  includeTimestamps?: boolean;
  /** Include the model "thinking" block when present. Default: `false`. */
  includeThinking?: boolean;
  /** Include tool-call arguments + output. Default: `true`. */
  includeToolCalls?: boolean;
  /** Cap tool-output length per message (chars). `0` disables. Default: `2000`. */
  truncateToolOutput?: number;
  /** Title shown at the top of the export. Defaults to "ZeroClaw chat — <sid>". */
  title?: string;
}

const DEFAULTS: Required<Omit<ExportOptions, 'title'>> = {
  includeTimestamps: true,
  includeThinking: false,
  includeToolCalls: true,
  truncateToolOutput: 2000,
};

function resolve(opts: ExportOptions): Required<ExportOptions> {
  return {
    title: opts.title ?? 'ZeroClaw chat',
    ...DEFAULTS,
    ...opts,
  } as Required<ExportOptions>;
}

function truncate(text: string, max: number): string {
  if (max <= 0 || text.length <= max) return text;
  return `${text.slice(0, max)}\n…[truncated ${text.length - max} chars]`;
}

function roleLabel(role: PersistedChatBubble['role']): string {
  return role === 'user' ? 'User' : 'Agent';
}

function fmtTime(ts: string | Date): string {
  try {
    const d = ts instanceof Date ? ts : new Date(ts);
    return d.toISOString().replace('T', ' ').slice(0, 19);
  } catch {
    return String(ts);
  }
}

// ─── Markdown ────────────────────────────────────────────────────────────────

export function toMarkdown(
  messages: ExportableMessage[],
  opts: ExportOptions = {},
): string {
  const o = resolve(opts);
  const lines: string[] = [
    `# ${o.title}`,
    '',
    `**Messages:** ${messages.length}`,
    '',
    '---',
    '',
  ];
  for (const m of messages) {
    const head = o.includeTimestamps
      ? `### ${roleLabel(m.role)} — _${fmtTime(m.timestamp)}_`
      : `### ${roleLabel(m.role)}`;
    lines.push(head, '');
    if (o.includeThinking && m.thinking) {
      lines.push('> **Thinking**', '> ', `> ${m.thinking.replace(/\n/g, '\n> ')}`, '');
    }
    lines.push(m.content || '*(empty)*', '');
    if (o.includeToolCalls && m.toolCall) {
      const args = m.toolCall.args !== undefined
        ? `\n\`\`\`json\n${JSON.stringify(m.toolCall.args, null, 2)}\n\`\`\``
        : '';
      lines.push(`**Tool call:** \`${m.toolCall.name}\`${args}`, '');
      if (m.toolCall.output) {
        lines.push(
          '```',
          truncate(m.toolCall.output, o.truncateToolOutput),
          '```',
          '',
        );
      }
    }
    lines.push('---', '');
  }
  lines.push('*Exported from ZeroClaw*');
  return lines.join('\n');
}

// ─── JSON ────────────────────────────────────────────────────────────────────

export function toJSON(
  messages: ExportableMessage[],
  opts: ExportOptions = {},
): string {
  const o = resolve(opts);
  const filtered = messages.map((m) => {
    const out: Record<string, unknown> = {
      role: m.role,
      content: m.content,
      // Normalise Date → ISO string for stable JSON output across runtimes.
      timestamp: m.timestamp instanceof Date ? m.timestamp.toISOString() : m.timestamp,
    };
    if (o.includeThinking && m.thinking) out.thinking = m.thinking;
    if (o.includeToolCalls && m.toolCall) {
      const tc = { ...m.toolCall };
      if (o.truncateToolOutput > 0 && tc.output) {
        tc.output = truncate(tc.output, o.truncateToolOutput);
      }
      out.toolCall = tc;
    }
    if (!o.includeTimestamps) delete out.timestamp;
    return out;
  });
  return JSON.stringify(
    {
      title: o.title,
      exported_at: new Date().toISOString(),
      messages: filtered,
    },
    null,
    2,
  );
}

// ─── Plaintext ───────────────────────────────────────────────────────────────

export function toPlaintext(
  messages: ExportableMessage[],
  opts: ExportOptions = {},
): string {
  const o = resolve(opts);
  const lines: string[] = [o.title, '='.repeat(o.title.length), ''];
  for (const m of messages) {
    const head = o.includeTimestamps
      ? `[${fmtTime(m.timestamp)}] ${roleLabel(m.role)}:`
      : `${roleLabel(m.role)}:`;
    lines.push(head);
    if (o.includeThinking && m.thinking) {
      lines.push('(thinking)', m.thinking);
    }
    lines.push(m.content || '(empty)');
    if (o.includeToolCalls && m.toolCall) {
      lines.push(`-- tool: ${m.toolCall.name}`);
      if (m.toolCall.args !== undefined) {
        lines.push(JSON.stringify(m.toolCall.args));
      }
      if (m.toolCall.output) {
        lines.push(truncate(m.toolCall.output, o.truncateToolOutput));
      }
    }
    lines.push('');
  }
  return lines.join('\n');
}

// ─── HTML ────────────────────────────────────────────────────────────────────

function esc(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

export function toHTML(
  messages: ExportableMessage[],
  opts: ExportOptions = {},
): string {
  const o = resolve(opts);
  const body = messages
    .map((m) => {
      const time = o.includeTimestamps
        ? `<time>${esc(fmtTime(m.timestamp))}</time>`
        : '';
      const thinking =
        o.includeThinking && m.thinking
          ? `<details class="thinking"><summary>Thinking</summary><pre>${esc(m.thinking)}</pre></details>`
          : '';
      const tool =
        o.includeToolCalls && m.toolCall
          ? `<details class="tool"><summary>Tool: ${esc(m.toolCall.name)}</summary>${
              m.toolCall.args !== undefined
                ? `<pre>${esc(JSON.stringify(m.toolCall.args, null, 2))}</pre>`
                : ''
            }${
              m.toolCall.output
                ? `<pre>${esc(truncate(m.toolCall.output, o.truncateToolOutput))}</pre>`
                : ''
            }</details>`
          : '';
      return `<article class="msg msg-${m.role}">
  <header><strong>${roleLabel(m.role)}</strong>${time}</header>
  ${thinking}
  <div class="content">${esc(m.content || '(empty)')}</div>
  ${tool}
</article>`;
    })
    .join('\n');

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>${esc(o.title)}</title>
<style>
  body { font: 14px/1.5 -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; max-width: 820px; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.5rem; border-bottom: 1px solid #ddd; padding-bottom: .5rem; }
  .msg { border: 1px solid #eaeaea; border-radius: 8px; padding: 1rem; margin-bottom: 1rem; }
  .msg-user { background: #fafafa; }
  .msg-agent { background: #fff; }
  .msg header { display: flex; justify-content: space-between; margin-bottom: .5rem; color: #666; font-size: .85rem; }
  .msg time { font-variant-numeric: tabular-nums; }
  .msg .content { white-space: pre-wrap; }
  details { margin-top: .75rem; }
  details summary { cursor: pointer; font-size: .85rem; color: #555; }
  details pre { background: #f4f4f4; padding: .5rem; border-radius: 4px; overflow-x: auto; font-size: .8rem; }
</style>
</head>
<body>
<h1>${esc(o.title)}</h1>
<p>${messages.length} messages — exported ${esc(new Date().toISOString())}</p>
${body}
<footer><small>Exported from ZeroClaw</small></footer>
</body>
</html>
`;
}

// ─── Download helper ─────────────────────────────────────────────────────────

/** Trigger a browser download of `content` as `filename`. Caller picks
 *  the mime; common choices: `text/markdown`, `application/json`,
 *  `text/html`, `text/plain`. */
export function downloadText(filename: string, content: string, mime: string): void {
  const blob = new Blob([content], { type: `${mime};charset=utf-8` });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // Give the browser a tick to start the download before we revoke.
  setTimeout(() => URL.revokeObjectURL(url), 100);
}
