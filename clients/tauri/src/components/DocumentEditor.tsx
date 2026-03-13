/**
 * WYSIWYG Document Editor component for MoA.
 *
 * Displays converted HTML documents in an editable rich-text editor.
 * When the user edits, the HTML is re-converted to Markdown and sent
 * back to the AI for understanding.
 *
 * Supports:
 * - Split view: Source HTML / Visual preview
 * - Toolbar: Bold, Italic, Headings (H1-H3), Lists, Tables
 * - Save/Load: Persist to local filesystem
 * - Export: HTML and Markdown formats
 * - Document upload: PDF, HWP, DOCX, XLSX, PPTX
 */

import { useState, useRef, useCallback, useEffect } from "react";
import { type Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

interface DocumentEditorProps {
  locale: Locale;
  onBack: () => void;
  onToggleSidebar: () => void;
  sidebarOpen: boolean;
  /** Optional initial HTML content to display */
  initialHtml?: string;
  /** Callback when document is saved/updated — sends Markdown to AI */
  onDocumentUpdate?: (markdown: string, html: string) => void;
}

type ViewMode = "visual" | "source" | "split";

interface DocumentState {
  html: string;
  markdown: string;
  fileName: string;
  docType: string;
  engine: string;
  isModified: boolean;
}

// Supported document extensions for upload
const SUPPORTED_EXTENSIONS = [
  ".pdf",
  ".hwp", ".hwpx",
  ".doc", ".docx",
  ".xls", ".xlsx",
  ".ppt", ".pptx",
];

export function DocumentEditor({
  locale,
  onBack,
  onToggleSidebar,
  sidebarOpen,
  initialHtml,
  onDocumentUpdate,
}: DocumentEditorProps) {
  const [viewMode, setViewMode] = useState<ViewMode>("visual");
  const [isUploading, setIsUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [doc, setDoc] = useState<DocumentState>({
    html: initialHtml || "",
    markdown: "",
    fileName: "",
    docType: "",
    engine: "",
    isModified: false,
  });

  const editorRef = useRef<HTMLDivElement>(null);
  const sourceRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Initialize with initial HTML if provided
  useEffect(() => {
    if (initialHtml && editorRef.current) {
      editorRef.current.innerHTML = initialHtml;
    }
  }, [initialHtml]);

  // Handle file upload
  const handleFileUpload = useCallback(async (file: File) => {
    const ext = "." + file.name.split(".").pop()?.toLowerCase();
    if (!SUPPORTED_EXTENSIONS.includes(ext)) {
      setError(
        locale === "ko"
          ? `지원되지 않는 파일 형식입니다: ${ext}. 지원 형식: ${SUPPORTED_EXTENSIONS.join(", ")}`
          : `Unsupported file format: ${ext}. Supported: ${SUPPORTED_EXTENSIONS.join(", ")}`
      );
      return;
    }

    setIsUploading(true);
    setError(null);
    setUploadProgress(
      locale === "ko"
        ? `${file.name} 업로드 중...`
        : `Uploading ${file.name}...`
    );

    try {
      // For PDF files, determine if it's a digital or image PDF
      // and route to the appropriate processing pipeline
      const isPdf = ext === ".pdf";

      if (isPdf) {
        setUploadProgress(
          locale === "ko"
            ? "PDF 유형 분석 중... (디지털/이미지 판별)"
            : "Analyzing PDF type... (digital/image detection)"
        );
      }

      // Upload file to the local agent's document processing tool
      const formData = new FormData();
      formData.append("file", file);

      const serverUrl = apiClient.getServerUrl();
      const token = apiClient.getToken();

      const response = await fetch(`${serverUrl}/api/document/process`, {
        method: "POST",
        headers: {
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        body: formData,
      });

      if (!response.ok) {
        const data = await response.json().catch(() => ({ error: "Upload failed" }));
        throw new Error(data.error || `Processing failed (${response.status})`);
      }

      const result = await response.json();

      setDoc({
        html: result.html || "",
        markdown: result.markdown || "",
        fileName: file.name,
        docType: result.doc_type || "unknown",
        engine: result.engine || "unknown",
        isModified: false,
      });

      // Set visual editor content
      if (editorRef.current) {
        editorRef.current.innerHTML = result.html || "";
      }

      setUploadProgress("");

      // Notify AI about the document
      if (onDocumentUpdate && result.markdown) {
        onDocumentUpdate(result.markdown, result.html || "");
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Upload failed";
      setError(msg);
      setUploadProgress("");
    } finally {
      setIsUploading(false);
    }
  }, [locale, onDocumentUpdate]);

  // Handle visual editor changes
  const handleEditorInput = useCallback(() => {
    if (!editorRef.current) return;

    const html = editorRef.current.innerHTML;
    const markdown = htmlToMarkdown(html);

    setDoc((prev) => ({
      ...prev,
      html,
      markdown,
      isModified: true,
    }));
  }, []);

  // Handle source HTML changes
  const handleSourceChange = useCallback((newHtml: string) => {
    setDoc((prev) => ({
      ...prev,
      html: newHtml,
      markdown: htmlToMarkdown(newHtml),
      isModified: true,
    }));

    // Update visual editor
    if (editorRef.current) {
      editorRef.current.innerHTML = newHtml;
    }
  }, []);

  // Save and send to AI
  const handleSave = useCallback(() => {
    if (onDocumentUpdate) {
      onDocumentUpdate(doc.markdown, doc.html);
    }
    setDoc((prev) => ({ ...prev, isModified: false }));
  }, [doc.markdown, doc.html, onDocumentUpdate]);

  // Toolbar formatting commands
  const execCmd = useCallback((command: string, value?: string) => {
    document.execCommand(command, false, value);
    editorRef.current?.focus();
    handleEditorInput();
  }, [handleEditorInput]);

  // File drop handler
  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    const file = e.dataTransfer.files[0];
    if (file) handleFileUpload(file);
  }, [handleFileUpload]);

  // Export as Markdown file
  const handleExportMarkdown = useCallback(() => {
    const blob = new Blob([doc.markdown], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = doc.fileName.replace(/\.[^.]+$/, ".md") || "document.md";
    a.click();
    URL.revokeObjectURL(url);
  }, [doc.markdown, doc.fileName]);

  // Export as HTML file
  const handleExportHtml = useCallback(() => {
    const blob = new Blob([doc.html], { type: "text/html" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = doc.fileName.replace(/\.[^.]+$/, ".html") || "document.html";
    a.click();
    URL.revokeObjectURL(url);
  }, [doc.html, doc.fileName]);

  return (
    <div className="document-editor-page">
      {/* Header */}
      <div className="chat-header">
        <button className="header-toggle-btn" onClick={onToggleSidebar}>
          {sidebarOpen ? "\u2715" : "\u2630"}
        </button>
        <div className="header-title">
          {doc.fileName
            ? `${locale === "ko" ? "문서 편집기" : "Document Editor"} — ${doc.fileName}`
            : (locale === "ko" ? "문서 편집기" : "Document Editor")}
        </div>
        {doc.isModified && (
          <span className="modified-badge">
            {locale === "ko" ? "수정됨" : "Modified"}
          </span>
        )}
      </div>

      {/* Toolbar */}
      <div className="editor-toolbar">
        {/* File operations */}
        <input
          ref={fileInputRef}
          type="file"
          accept={SUPPORTED_EXTENSIONS.join(",")}
          style={{ display: "none" }}
          onChange={(e) => {
            const file = e.target.files?.[0];
            if (file) handleFileUpload(file);
          }}
        />
        <button
          className="toolbar-btn"
          onClick={() => fileInputRef.current?.click()}
          disabled={isUploading}
          title={locale === "ko" ? "문서 업로드" : "Upload document"}
        >
          {locale === "ko" ? "업로드" : "Upload"}
        </button>

        <span className="toolbar-divider" />

        {/* Formatting */}
        <button className="toolbar-btn" onClick={() => execCmd("bold")} title="Bold (Ctrl+B)">
          <strong>B</strong>
        </button>
        <button className="toolbar-btn" onClick={() => execCmd("italic")} title="Italic (Ctrl+I)">
          <em>I</em>
        </button>
        <button className="toolbar-btn" onClick={() => execCmd("underline")} title="Underline (Ctrl+U)">
          <u>U</u>
        </button>

        <span className="toolbar-divider" />

        {/* Headings */}
        <button className="toolbar-btn" onClick={() => execCmd("formatBlock", "h1")} title="Heading 1">
          H1
        </button>
        <button className="toolbar-btn" onClick={() => execCmd("formatBlock", "h2")} title="Heading 2">
          H2
        </button>
        <button className="toolbar-btn" onClick={() => execCmd("formatBlock", "h3")} title="Heading 3">
          H3
        </button>
        <button className="toolbar-btn" onClick={() => execCmd("formatBlock", "p")} title="Paragraph">
          P
        </button>

        <span className="toolbar-divider" />

        {/* Lists */}
        <button className="toolbar-btn" onClick={() => execCmd("insertUnorderedList")} title={locale === "ko" ? "글머리 기호 목록" : "Bullet list"}>
          {locale === "ko" ? "목록" : "List"}
        </button>
        <button className="toolbar-btn" onClick={() => execCmd("insertOrderedList")} title={locale === "ko" ? "번호 목록" : "Numbered list"}>
          1.
        </button>

        <span className="toolbar-divider" />

        {/* View mode */}
        <div className="view-mode-toggle">
          <button
            className={`view-btn ${viewMode === "visual" ? "active" : ""}`}
            onClick={() => setViewMode("visual")}
          >
            {locale === "ko" ? "편집" : "Edit"}
          </button>
          <button
            className={`view-btn ${viewMode === "source" ? "active" : ""}`}
            onClick={() => setViewMode("source")}
          >
            {locale === "ko" ? "소스" : "Source"}
          </button>
          <button
            className={`view-btn ${viewMode === "split" ? "active" : ""}`}
            onClick={() => setViewMode("split")}
          >
            {locale === "ko" ? "분할" : "Split"}
          </button>
        </div>

        <span className="toolbar-divider" />

        {/* Save & Export */}
        {doc.html && (
          <>
            <button
              className="toolbar-btn save-btn"
              onClick={handleSave}
              disabled={!doc.isModified}
              title={locale === "ko" ? "저장하고 AI에게 전달" : "Save and send to AI"}
            >
              {locale === "ko" ? "저장" : "Save"}
            </button>
            <button
              className="toolbar-btn"
              onClick={handleExportMarkdown}
              title={locale === "ko" ? "마크다운으로 내보내기" : "Export as Markdown"}
            >
              .md
            </button>
            <button
              className="toolbar-btn"
              onClick={handleExportHtml}
              title={locale === "ko" ? "HTML로 내보내기" : "Export as HTML"}
            >
              .html
            </button>
          </>
        )}
      </div>

      {/* Document info bar */}
      {doc.docType && (
        <div className="doc-info-bar">
          <span className="doc-info-type">
            {doc.docType === "digital_pdf" && (locale === "ko" ? "디지털 PDF (로컬 추출)" : "Digital PDF (local extraction)")}
            {doc.docType === "image_pdf" && (locale === "ko" ? "이미지 PDF (Upstage OCR + Gemini 교정)" : "Image PDF (Upstage OCR + Gemini correction)")}
            {doc.docType.startsWith("office_") && (locale === "ko" ? `오피스 문서 (한컴 변환기)` : `Office document (Hancom converter)`)}
          </span>
          <span className="doc-info-engine">{doc.engine}</span>
        </div>
      )}

      {/* Upload progress */}
      {uploadProgress && (
        <div className="upload-progress">
          <span className="upload-spinner" />
          {uploadProgress}
        </div>
      )}

      {/* Error display */}
      {error && (
        <div className="editor-error" onClick={() => setError(null)}>
          {error}
        </div>
      )}

      {/* Editor area */}
      <div
        className={`editor-content ${viewMode}`}
        onDrop={handleDrop}
        onDragOver={(e) => e.preventDefault()}
      >
        {!doc.html && !isUploading && (
          <div
            className="editor-dropzone"
            onClick={() => fileInputRef.current?.click()}
          >
            <div className="dropzone-icon">+</div>
            <p>
              {locale === "ko"
                ? "문서를 드래그 앤 드롭하거나 클릭하여 업로드"
                : "Drag & drop a document or click to upload"}
            </p>
            <p className="dropzone-formats">
              PDF, HWP, HWPX, DOC, DOCX, XLS, XLSX, PPT, PPTX
            </p>
            <p className="dropzone-note">
              {locale === "ko"
                ? "이미지 PDF: Upstage OCR + Gemini 교정 | 디지털 PDF: 로컬 추출 | 오피스: 한컴 변환"
                : "Image PDF: Upstage OCR + Gemini | Digital PDF: local extraction | Office: Hancom converter"}
            </p>
          </div>
        )}

        {doc.html && (viewMode === "visual" || viewMode === "split") && (
          <div
            ref={editorRef}
            className="visual-editor"
            contentEditable
            onInput={handleEditorInput}
            suppressContentEditableWarning
            dangerouslySetInnerHTML={{ __html: doc.html }}
          />
        )}

        {doc.html && (viewMode === "source" || viewMode === "split") && (
          <textarea
            ref={sourceRef}
            className="source-editor"
            value={doc.html}
            onChange={(e) => handleSourceChange(e.target.value)}
            spellCheck={false}
          />
        )}
      </div>
    </div>
  );
}

/**
 * Simple HTML to Markdown converter (client-side).
 * Used when user edits HTML in the WYSIWYG editor — the Markdown
 * version is what gets sent to the AI for understanding.
 */
function htmlToMarkdown(html: string): string {
  let md = html;

  // Headings
  for (let i = 6; i >= 1; i--) {
    const prefix = "#".repeat(i);
    md = md.replace(new RegExp(`<h${i}[^>]*>`, "gi"), `\n${prefix} `);
    md = md.replace(new RegExp(`</h${i}>`, "gi"), "\n");
  }

  // Paragraphs and breaks
  md = md.replace(/<br\s*\/?>/gi, "\n");
  md = md.replace(/<p[^>]*>/gi, "\n");
  md = md.replace(/<\/p>/gi, "\n");

  // Bold and italic
  md = md.replace(/<(strong|b)[^>]*>/gi, "**");
  md = md.replace(/<\/(strong|b)>/gi, "**");
  md = md.replace(/<(em|i)[^>]*>/gi, "*");
  md = md.replace(/<\/(em|i)>/gi, "*");

  // Lists
  md = md.replace(/<ul[^>]*>/gi, "\n");
  md = md.replace(/<\/ul>/gi, "\n");
  md = md.replace(/<ol[^>]*>/gi, "\n");
  md = md.replace(/<\/ol>/gi, "\n");
  md = md.replace(/<li[^>]*>/gi, "- ");
  md = md.replace(/<\/li>/gi, "\n");

  // Strip remaining tags
  md = md.replace(/<[^>]+>/g, "");

  // HTML entities
  md = md.replace(/&amp;/g, "&");
  md = md.replace(/&lt;/g, "<");
  md = md.replace(/&gt;/g, ">");
  md = md.replace(/&quot;/g, '"');
  md = md.replace(/&nbsp;/g, " ");

  // Clean up whitespace
  md = md.replace(/\n{3,}/g, "\n\n");

  return md.trim();
}
