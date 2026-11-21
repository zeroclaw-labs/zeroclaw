import remarkGfm from 'remark-gfm';
import remarkParse from 'remark-parse';
import { unified } from 'unified';

export const markdownParser = unified().use(remarkParse).use(remarkGfm);

interface ReadableNode {
  type: string;
  value?: string;
  identifier?: string;
  alt?: string | null;
  children?: readonly ReadableNode[];
}

const blockTypes = new Set(['paragraph', 'listItem', 'tableRow', 'tableCell', 'blockquote', 'heading']);

function visibleText(
  node: ReadableNode, definitions: Map<string, ReadableNode>, references: Set<string>,
): string {
  switch (node.type) {
    case 'text':
    case 'inlineCode':
      return node.value ?? '';
    case 'html':
      return (node.value ?? '').replace(/<!--[\s\S]*?-->|<\/?[a-z][\w:-]*(?=[\s/>])(?:[^"'<>]|"[^"]*"|'[^']*')*>/gi, '');
    case 'code':
      return '\n' + (node.value ?? '') + '\n';
    case 'image':
    case 'imageReference':
      return node.alt ?? '';
    case 'break':
    case 'thematicBreak':
      return '\n';
    case 'footnoteReference':
      if (node.identifier && definitions.has(node.identifier)) references.add(node.identifier);
      return '';
    case 'definition':
    case 'footnoteDefinition':
      return '';
    default: {
      const text = node.children?.map(child => visibleText(child, definitions, references)).join('') ?? '';
      return blockTypes.has(node.type) ? '\n' + text + '\n' : text;
    }
  }
}

/** Speak visible Markdown content, without markup or link destinations. */
export function speechText(markdown: string): string {
  const root = markdownParser.parse(markdown);
  const definitions = new Map<string, ReadableNode>();
  function collect(node: ReadableNode) {
    if (node.type === 'footnoteDefinition' && node.identifier && !definitions.has(node.identifier)) {
      definitions.set(node.identifier, node);
    }
    node.children?.forEach(collect);
  }
  collect(root);
  const references = new Set<string>();
  const main = visibleText(root, definitions, references);
  const notes: string[] = [];
  for (const id of references) {
    const note = definitions.get(id)!;
    notes.push(note.children?.map(child => visibleText(child, definitions, references)).join('') ?? '');
  }
  return [main, ...notes].join('\n').replace(/[ \t]+/g, ' ').replace(/ *\n */g, '\n').replace(/\n{3,}/g, '\n\n').trim().replace(/(?:\s*🦀\uFE0F?)+\s*$/u, '').trim();
}
