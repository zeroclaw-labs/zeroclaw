// Markdown remains canonical in the native conversation. This derived view is
// used only when handing approved outbound text to the SimpleX CLI.
import {Lexer} from 'marked';

const MESSAGE_CHARS = 12000;

function pieces(tokens, inherited = '') {
  const result = [];
  const add = (text, mark = inherited, literal = false) => result.push({text, mark, literal});
  for (const token of tokens) {
    switch (token.type) {
      case 'strong': result.push(...pieces(token.tokens, '*')); break;
      case 'em': result.push(...pieces(token.tokens, inherited || '_')); break;
      case 'del': result.push(...pieces(token.tokens, inherited || '~')); break;
      // The target cannot nest formatting or represent arbitrary fenced code.
      // Keep code and HTML source verbatim instead of rewriting their contents.
      case 'code': case 'codespan': case 'html': add(token.raw, '', true); break;
      case 'heading':
        result.push(...pieces(token.tokens, '*'));
        add(token.raw.match(/\n*$/)[0], ''); break;
      case 'paragraph':
        result.push(...pieces(token.tokens, inherited));
        add(token.raw.match(/\n*$/)[0], ''); break;
      case 'text':
        if (token.tokens) result.push(...pieces(token.tokens, inherited));
        else add(token.raw);
        break;
      case 'link': case 'image': {
        const label = token.tokens ? pieces(token.tokens, inherited) : [{text: token.text, mark: inherited}];
        result.push(...label);
        if (label.map(part => part.text).join('') !== token.href) add(` (${token.href})`, '');
        break;
      }
      case 'blockquote': {
        const inner = pieces(token.tokens, inherited);
        add('> ', '');
        for (const part of inner) result.push({...part, text: part.text.replace(/\n(?=.)/g, '\n> ')});
        if (token.raw.endsWith('\n') && !inner.at(-1)?.text.endsWith('\n')) add('\n', '');
        break;
      }
      case 'list':
        for (const [index, item] of token.items.entries()) {
          add(token.ordered ? `${Number(token.start) + index}. ` : '• ', '');
          if (item.task) add(item.checked ? '[x] ' : '[ ] ', '');
          result.push(...pieces(item.tokens, inherited));
          add('\n', '');
        }
        break;
      case 'table':
        for (const [index, row] of [token.header, ...token.rows].entries()) {
          row.forEach((cell, column) => {
            if (column) add(' | ', '');
            result.push(...pieces(cell.tokens, index === 0 ? '*' : inherited));
          });
          add('\n', '');
        }
        break;
      case 'checkbox': break; // The enclosing list item owns the checkbox prefix.
      case 'def': break; // Resolved links retain their actual destination above.
      case 'br': add('\n', ''); break;
      default: add(token.raw ?? token.text ?? '');
    }
  }
  return result;
}

function render(markdown, maxChars) {
  const parts = [];
  for (const part of pieces(Lexer.lex(markdown, {gfm: true}))) {
    const previous = parts.at(-1);
    if (previous && previous.mark === part.mark && previous.literal === part.literal) previous.text += part.text;
    else parts.push({...part});
  }
  const messages = [];
  let current = '', size = 0;
  const flush = () => {if (current) messages.push({msgContent: {type: 'text', text: current}}); current = ''; size = 0;};
  const append = text => {current += text; size += [...text].length;};
  for (const [index, part] of parts.entries()) {
    let mark = part.mark;
    // Native spans need their own word boundary. Flatten intraword emphasis
    // rather than inserting spaces into identifiers or splitting a word.
    const previous = parts[index - 1]?.text.at(-1) ?? '';
    const next = parts[index + 1]?.text[0] ?? '';
    if (/[\p{L}\p{N}_]/u.test(previous) || /[\p{L}\p{N}_]/u.test(next) || part.text.includes(mark)) mark = '';
    if (!mark) {
      const chars = [...part.text];
      // Short code spans stay together even when a message boundary is close.
      if (part.literal && chars.length <= maxChars && size + chars.length > maxChars) flush();
      for (let offset = 0; offset < chars.length;) {
        if (size === maxChars) flush();
        const chunk = chars.slice(offset, offset + maxChars - size).join('');
        offset += [...chunk].length; append(chunk);
      }
      continue;
    }
    for (const line of part.text.split(/(\n)/)) {
      if (!line) continue;
      if (line === '\n' || !line.trim()) {
        for (const char of line) {if (size === maxChars) flush(); append(char);}
        continue;
      }
      const [, leading, content, trailing] = line.match(/^(\s*)(.*?)(\s*)$/);
      const chars = [...content];
      for (const char of leading) {if (size === maxChars) flush(); append(char);}
      if (current && !/\s$/.test(current)) {if (size === maxChars) flush(); append(' ');}
      for (let offset = 0; offset < chars.length;) {
        if (maxChars - size < 3) flush();
        const chunk = chars.slice(offset, offset + maxChars - size - 2);
        const [, left, core, right] = chunk.join('').match(/^(\s*)(.*?)(\s*)$/);
        append(left + (core ? mark + core + mark : '') + right); offset += chunk.length;
        if (offset < chars.length) flush();
      }
      for (const char of trailing) {if (size === maxChars) flush(); append(char);}
    }
  }
  flush();
  return messages;
}

// SimpleX limits encoded messages, not Unicode characters. Keep the normal
// chunk size when it fits; re-render with a proven per-character byte bound
// for emoji, non-ASCII text, or JSON escapes, retaining span boundaries.
export function simplexTextMessages(markdown) {
  const messages = render(markdown, MESSAGE_CHARS);
  if (messages.every(message => Buffer.byteLength(JSON.stringify(message)) <= 14500)) return messages;
  let cost = 1;
  for (const message of messages)
    for (const char of message.msgContent.text)
      cost = Math.max(cost, Buffer.byteLength(JSON.stringify(char)) - 2);
  return render(markdown, Math.min(MESSAGE_CHARS, Math.floor(13500 / cost)));
}
