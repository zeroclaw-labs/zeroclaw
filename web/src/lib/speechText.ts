import remarkGfm from 'remark-gfm';
import remarkParse from 'remark-parse';
import { unified } from 'unified';

export const markdownParser = unified().use(remarkParse).use(remarkGfm);

interface ReadableNode {
  type: string;
  value?: string;
  alt?: string | null;
  children?: readonly ReadableNode[];
}

const blockTypes = new Set(['paragraph', 'listItem', 'tableRow', 'tableCell', 'blockquote', 'heading']);

function visibleText(node: ReadableNode): string {
  switch (node.type) {
    case 'text':
    case 'inlineCode':
      return node.value ?? '';
    case 'code':
      return '\n' + (node.value ?? '') + '\n';
    case 'image':
    case 'imageReference':
      return node.alt ?? '';
    case 'break':
    case 'thematicBreak':
      return '\n';
    case 'html':
    case 'definition':
    case 'footnoteDefinition':
      return '';
    default: {
      const text = node.children?.map(visibleText).join('') ?? '';
      return blockTypes.has(node.type) ? '\n' + text + '\n' : text;
    }
  }
}

/** Speak visible Markdown content, without markup or link destinations. */
export function speechText(markdown: string): string {
  return visibleText(markdownParser.parse(markdown)).replace(/[ \t]+/g, ' ').replace(/ *\n */g, '\n').replace(/\n{3,}/g, '\n\n').trim().replace(/(?:\s*🦀\uFE0F?)+\s*$/u, '').trim();
}
