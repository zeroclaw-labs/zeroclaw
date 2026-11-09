import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

/** Speak the visible Markdown content, never its formatting or link targets. */
export function speechText(markdown: string): string {
  const html = renderToStaticMarkup(createElement(Markdown, { remarkPlugins: [remarkGfm], children: markdown }));
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const blocks = new Set(['P', 'DIV', 'LI', 'TR', 'TH', 'TD', 'PRE', 'BLOCKQUOTE', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6']);
  const text = (node: Node): string => {
    if (node.nodeType === 3) return node.textContent ?? '';
    if (node.nodeType !== 1) return '';
    const element = node as Element;
    if (element.tagName === 'IMG') return element.getAttribute('alt') ?? '';
    if (element.tagName === 'BR' || element.tagName === 'HR') return '\n';
    const content = Array.from(node.childNodes, text).join('');
    return blocks.has(element.tagName) ? `\n${content}\n` : content;
  };
  return Array.from(doc.body.childNodes, text).join('').replace(/[ \t]+/g, ' ').replace(/ *\n */g, '\n').replace(/\n{3,}/g, '\n\n').trim().replace(/(?:\s*🦀\uFE0F?)+\s*$/u, '').trim();
}
