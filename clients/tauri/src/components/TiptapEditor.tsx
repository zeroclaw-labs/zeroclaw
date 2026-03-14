/**
 * Tiptap-based rich text editor for the MoA 2-layer document stack.
 *
 * Layer 2: Structure-based editing. Loads Markdown (extracted from PDF
 * by PyMuPDF/Marker), renders as WYSIWYG via Tiptap, and exports back
 * to Markdown on save. The companion DocumentViewer (Layer 1) shows the
 * original pdf2htmlEX layout; this editor handles all modifications.
 *
 * Extensions: StarterKit (paragraphs, headings, bold, italic, lists,
 * blockquote, code, horizontal rule), Table, Underline, TextAlign,
 * Placeholder, tiptap-markdown bridge.
 */

import { useEffect, useCallback, forwardRef, useImperativeHandle, useRef } from "react";
import { useEditor, EditorContent } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { Table } from "@tiptap/extension-table";
import TableRow from "@tiptap/extension-table-row";
import TableCell from "@tiptap/extension-table-cell";
import TableHeader from "@tiptap/extension-table-header";
import Underline from "@tiptap/extension-underline";
import TextAlign from "@tiptap/extension-text-align";
import Placeholder from "@tiptap/extension-placeholder";
import { Markdown } from "tiptap-markdown";
import { type Locale } from "../lib/i18n";

// ── Public handle exposed via ref ────────────────────────────────
export interface TiptapEditorHandle {
  /** Get current content as Markdown string */
  getMarkdown: () => string;
  /** Get current content as Tiptap JSON */
  getJSON: () => Record<string, unknown>;
  /** Get current content as HTML */
  getHTML: () => string;
  /** Replace editor content with Markdown */
  setMarkdown: (md: string) => void;
  /** Focus the editor */
  focus: () => void;
}

interface TiptapEditorProps {
  /** Initial Markdown content to load */
  initialMarkdown?: string;
  locale: Locale;
  /** Called on every content change with updated Markdown */
  onChange?: (markdown: string) => void;
  /** Additional CSS class */
  className?: string;
  /** Whether the editor should be read-only */
  readOnly?: boolean;
}

export const TiptapEditor = forwardRef<TiptapEditorHandle, TiptapEditorProps>(
  function TiptapEditor({ initialMarkdown, locale, onChange, className, readOnly }, ref) {
    const hasSetInitial = useRef(false);

    const editor = useEditor({
      extensions: [
        StarterKit.configure({
          heading: { levels: [1, 2, 3, 4] },
        }),
        Table.configure({ resizable: true }),
        TableRow,
        TableCell,
        TableHeader,
        Underline,
        TextAlign.configure({
          types: ["heading", "paragraph"],
        }),
        Placeholder.configure({
          placeholder: locale === "ko"
            ? "여기에서 문서를 편집하세요..."
            : "Edit your document here...",
        }),
        Markdown.configure({
          html: true,
          transformPastedText: true,
          transformCopiedText: true,
        }),
      ],
      editable: !readOnly,
      content: "",
      onUpdate: ({ editor: ed }) => {
        if (onChange) {
          // tiptap-markdown extension adds getMarkdown() to editor.storage.markdown
          try {
            const md = (ed.storage.markdown as { getMarkdown: () => string }).getMarkdown();
            onChange(md);
          } catch {
            // Fallback if tiptap-markdown extension is not loaded
            onChange(ed.getHTML());
          }
        }
      },
    });

    // Load initial markdown once editor is ready
    useEffect(() => {
      if (editor && initialMarkdown && !hasSetInitial.current) {
        hasSetInitial.current = true;
        editor.commands.setContent(initialMarkdown);
      }
    }, [editor, initialMarkdown]);

    // Expose imperative handle
    const getMarkdown = useCallback((): string => {
      if (!editor) return "";
      try {
        return (editor.storage.markdown as { getMarkdown: () => string }).getMarkdown();
      } catch {
        return editor.getHTML();
      }
    }, [editor]);

    const getJSON = useCallback((): Record<string, unknown> => {
      if (!editor) return {};
      return editor.getJSON() as Record<string, unknown>;
    }, [editor]);

    const getHTML = useCallback((): string => {
      if (!editor) return "";
      return editor.getHTML();
    }, [editor]);

    const setMarkdown = useCallback((md: string) => {
      if (!editor) return;
      hasSetInitial.current = true;
      editor.commands.setContent(md);
    }, [editor]);

    const focusEditor = useCallback(() => {
      editor?.commands.focus();
    }, [editor]);

    useImperativeHandle(ref, () => ({
      getMarkdown,
      getJSON,
      getHTML,
      setMarkdown,
      focus: focusEditor,
    }), [getMarkdown, getJSON, getHTML, setMarkdown, focusEditor]);

    if (!editor) return null;

    return (
      <div className={`tiptap-editor-wrapper ${className || ""}`}>
        {/* Formatting toolbar */}
        <div className="tiptap-toolbar">
          {/* Text formatting */}
          <button
            className={`tiptap-btn ${editor.isActive("bold") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleBold().run()}
            title="Bold (Ctrl+B)"
          >
            <strong>B</strong>
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("italic") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleItalic().run()}
            title="Italic (Ctrl+I)"
          >
            <em>I</em>
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("underline") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleUnderline().run()}
            title="Underline (Ctrl+U)"
          >
            <u>U</u>
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("strike") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleStrike().run()}
            title="Strikethrough"
          >
            <s>S</s>
          </button>

          <span className="tiptap-divider" />

          {/* Headings */}
          <button
            className={`tiptap-btn ${editor.isActive("heading", { level: 1 }) ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleHeading({ level: 1 }).run()}
            title="Heading 1"
          >
            H1
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("heading", { level: 2 }) ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}
            title="Heading 2"
          >
            H2
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("heading", { level: 3 }) ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleHeading({ level: 3 }).run()}
            title="Heading 3"
          >
            H3
          </button>

          <span className="tiptap-divider" />

          {/* Lists */}
          <button
            className={`tiptap-btn ${editor.isActive("bulletList") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleBulletList().run()}
            title={locale === "ko" ? "글머리 기호 목록" : "Bullet list"}
          >
            {locale === "ko" ? "목록" : "List"}
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("orderedList") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleOrderedList().run()}
            title={locale === "ko" ? "번호 목록" : "Numbered list"}
          >
            1.
          </button>

          <span className="tiptap-divider" />

          {/* Block elements */}
          <button
            className={`tiptap-btn ${editor.isActive("blockquote") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleBlockquote().run()}
            title={locale === "ko" ? "인용" : "Blockquote"}
          >
            &ldquo;
          </button>
          <button
            className={`tiptap-btn ${editor.isActive("codeBlock") ? "active" : ""}`}
            onClick={() => editor.chain().focus().toggleCodeBlock().run()}
            title={locale === "ko" ? "코드 블록" : "Code block"}
          >
            {"</>"}
          </button>
          <button
            className="tiptap-btn"
            onClick={() => editor.chain().focus().setHorizontalRule().run()}
            title={locale === "ko" ? "수평선" : "Horizontal rule"}
          >
            ---
          </button>

          <span className="tiptap-divider" />

          {/* Text alignment */}
          <button
            className={`tiptap-btn ${editor.isActive({ textAlign: "left" }) ? "active" : ""}`}
            onClick={() => editor.chain().focus().setTextAlign("left").run()}
            title={locale === "ko" ? "왼쪽 정렬" : "Align left"}
          >
            {"\u2261"}
          </button>
          <button
            className={`tiptap-btn ${editor.isActive({ textAlign: "center" }) ? "active" : ""}`}
            onClick={() => editor.chain().focus().setTextAlign("center").run()}
            title={locale === "ko" ? "가운데 정렬" : "Align center"}
          >
            {"\u2550"}
          </button>
          <button
            className={`tiptap-btn ${editor.isActive({ textAlign: "right" }) ? "active" : ""}`}
            onClick={() => editor.chain().focus().setTextAlign("right").run()}
            title={locale === "ko" ? "오른쪽 정렬" : "Align right"}
          >
            {"\u2262"}
          </button>

          <span className="tiptap-divider" />

          {/* Table */}
          <button
            className="tiptap-btn"
            onClick={() => editor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()}
            title={locale === "ko" ? "표 삽입" : "Insert table"}
          >
            {locale === "ko" ? "표" : "Table"}
          </button>

          <span className="tiptap-divider" />

          {/* Undo/Redo */}
          <button
            className="tiptap-btn"
            onClick={() => editor.chain().focus().undo().run()}
            disabled={!editor.can().undo()}
            title="Undo (Ctrl+Z)"
          >
            {"\u21A9"}
          </button>
          <button
            className="tiptap-btn"
            onClick={() => editor.chain().focus().redo().run()}
            disabled={!editor.can().redo()}
            title="Redo (Ctrl+Y)"
          >
            {"\u21AA"}
          </button>
        </div>

        {/* Editor content area */}
        <EditorContent editor={editor} className="tiptap-content" />
      </div>
    );
  }
);
