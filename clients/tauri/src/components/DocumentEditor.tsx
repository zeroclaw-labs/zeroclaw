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

// Tauri invoke for local commands (PyMuPDF, etc.)
let tauriInvoke: ((cmd: string, args?: Record<string, unknown>) => Promise<unknown>) | null = null;
try {
  // Dynamic import for Tauri environment
  const tauri = (window as Record<string, unknown>).__TAURI__;
  if (tauri && typeof (tauri as Record<string, unknown>).invoke === "function") {
    tauriInvoke = (tauri as Record<string, (cmd: string, args?: Record<string, unknown>) => Promise<unknown>>).invoke;
  }
} catch {
  // Not in Tauri environment (web mode)
}

/** Check if running inside Tauri desktop app */
function isTauriApp(): boolean {
  return tauriInvoke !== null;
}

/** PDF type: digital (has text) or image (scanned/no text) */
type PdfType = "digital" | "image" | "unknown";

/** Office document extensions processed via Hancom API */
const OFFICE_EXTENSIONS = [".hwp", ".hwpx", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx"];

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

  // Handle file upload — routes to appropriate processing pipeline:
  // 1. Digital PDF → local PyMuPDF (Tauri sidecar, no server needed)
  // 2. Image PDF → R2 pre-signed URL → Railway → Upstage (operator key on server)
  // 3. Office docs (HWP, DOCX, etc.) → Hancom DocsConverter API via /api/document/process
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
        ? `${file.name} 처리 중...`
        : `Processing ${file.name}...`
    );

    try {
      const isPdf = ext === ".pdf";
      const isOffice = OFFICE_EXTENSIONS.includes(ext);

      if (isPdf) {
        // Route PDF based on type
        await handlePdfUpload(file);
      } else if (isOffice) {
        // Office docs → Hancom API via local agent
        await handleOfficeUpload(file);
      } else {
        throw new Error(`Unsupported: ${ext}`);
      }

      setUploadProgress("");
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Upload failed";
      setError(msg);
      setUploadProgress("");
    } finally {
      setIsUploading(false);
    }
  }, [locale, onDocumentUpdate]);

  // PDF upload: try local PyMuPDF first (digital), fall back to R2→Upstage (image)
  const handlePdfUpload = useCallback(async (file: File) => {
    // Step 1: Try local PyMuPDF conversion for digital PDFs
    if (isTauriApp() && tauriInvoke) {
      setUploadProgress(
        locale === "ko"
          ? "디지털 PDF 로컬 변환 시도 중 (PyMuPDF)..."
          : "Trying local digital PDF conversion (PyMuPDF)..."
      );

      try {
        // Save file to temp path for PyMuPDF processing
        const arrayBuf = await file.arrayBuffer();
        const tempDir = await tauriInvoke("plugin:path|temp_dir") as string;
        const tempPath = `${tempDir}/moa_pdf_upload_${Date.now()}.pdf`;

        // Write file via Tauri FS
        const { writeFile } = await import("@tauri-apps/plugin-fs");
        await writeFile(tempPath, new Uint8Array(arrayBuf));

        const result = await tauriInvoke("convert_pdf_local", {
          filePath: tempPath,
        }) as { success: boolean; html: string; markdown: string; page_count: number; engine: string };

        if (result.success && result.html && result.html.length > 100) {
          // Digital PDF converted successfully locally
          applyDocumentResult({
            html: result.html,
            markdown: result.markdown,
            doc_type: "digital_pdf",
            engine: result.engine || "pymupdf4llm",
            page_count: result.page_count,
          }, file.name);
          return;
        }
        // If output is too short, likely image PDF → fall through to R2 flow
      } catch {
        // PyMuPDF not available or conversion failed → try R2 flow
      }
    }

    // Step 2: Image PDF → R2 pre-signed URL → Railway → Upstage
    setUploadProgress(
      locale === "ko"
        ? "이미지 PDF 처리 중 (Upstage OCR)..."
        : "Processing image PDF (Upstage OCR)..."
    );

    const serverUrl = apiClient.getServerUrl();
    const token = apiClient.getToken();

    // Step 2a: Get pre-signed R2 upload URL from Railway
    setUploadProgress(
      locale === "ko"
        ? "업로드 URL 발급 중..."
        : "Requesting upload URL..."
    );

    const urlResp = await fetch(`${serverUrl}/api/document/upload-url`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({
        filename: file.name,
        content_type: file.type || "application/pdf",
        estimated_pages: 1,
      }),
    });

    if (!urlResp.ok) {
      const data = await urlResp.json().catch(() => ({ error: "Failed to get upload URL" }));
      throw new Error(data.error || `Upload URL request failed (${urlResp.status})`);
    }

    const { upload_url, object_key } = await urlResp.json();

    // Step 2b: Upload file directly to R2
    setUploadProgress(
      locale === "ko"
        ? "파일 업로드 중 (R2)..."
        : "Uploading file (R2)..."
    );

    const uploadResp = await fetch(upload_url, {
      method: "PUT",
      headers: {
        "Content-Type": file.type || "application/pdf",
      },
      body: file,
    });

    if (!uploadResp.ok) {
      throw new Error(
        locale === "ko"
          ? `R2 업로드 실패 (HTTP ${uploadResp.status})`
          : `R2 upload failed (HTTP ${uploadResp.status})`
      );
    }

    // Step 2c: Tell Railway to process the file from R2 via Upstage
    setUploadProgress(
      locale === "ko"
        ? "OCR 문서 변환 중 (Upstage)..."
        : "OCR document conversion (Upstage)..."
    );

    const processResp = await fetch(`${serverUrl}/api/document/process-r2`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({
        object_key,
        filename: file.name,
        estimated_pages: 1,
      }),
    });

    if (!processResp.ok) {
      const data = await processResp.json().catch(() => ({ error: "Processing failed" }));
      throw new Error(data.error || `R2 processing failed (${processResp.status})`);
    }

    const result = await processResp.json();
    applyDocumentResult(result, file.name);
  }, [locale, onDocumentUpdate]);

  // Office document upload → Hancom API via local agent's /api/document/process
  const handleOfficeUpload = useCallback(async (file: File) => {
    setUploadProgress(
      locale === "ko"
        ? "오피스 문서 변환 중 (한컴 변환기)..."
        : "Converting office document (Hancom converter)..."
    );

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
    applyDocumentResult(result, file.name);
  }, [locale, onDocumentUpdate]);

  // Apply conversion result to the editor
  const applyDocumentResult = useCallback((result: Record<string, unknown>, fileName: string) => {
    const html = (result.html as string) || "";
    const markdown = (result.markdown as string) || "";

    setDoc({
      html,
      markdown,
      fileName,
      docType: (result.doc_type as string) || "unknown",
      engine: (result.engine as string) || "unknown",
      isModified: false,
    });

    if (editorRef.current) {
      editorRef.current.innerHTML = html;
    }

    if (onDocumentUpdate && markdown) {
      onDocumentUpdate(markdown, html);
    }
  }, [onDocumentUpdate]);

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
            {doc.docType === "digital_pdf" && (locale === "ko" ? "디지털 PDF (PyMuPDF 로컬 변환)" : "Digital PDF (PyMuPDF local conversion)")}
            {doc.docType === "image_pdf" && (locale === "ko" ? "이미지 PDF (Upstage OCR)" : "Image PDF (Upstage OCR)")}
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
                ? "이미지 PDF: Upstage OCR (서버) | 디지털 PDF: PyMuPDF (로컬) | 오피스: 한컴 변환"
                : "Image PDF: Upstage OCR (server) | Digital PDF: PyMuPDF (local) | Office: Hancom converter"}
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
