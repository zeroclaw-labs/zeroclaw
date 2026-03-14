/**
 * 2-Layer Document Editor for MoA.
 *
 * Architecture:
 *   Layer 1 (Viewer): Read-only iframe rendering the original PDF→HTML
 *     output (pdf2htmlEX for layout-preserving display, or PyMuPDF/Hancom
 *     HTML). Always shows the unmodified original.
 *   Layer 2 (Editor): Tiptap rich-text editor working on Markdown/JSON.
 *     Opens on "Edit" click in a side-by-side split pane to the right
 *     of the viewer.
 *
 * Data flow:
 *   Upload → pdf2htmlEX produces viewer.html (Layer 1)
 *          → PyMuPDF produces content.md  (Layer 2)
 *   Edit   → Tiptap modifies content.md
 *   Save   → content.md + Tiptap JSON persisted
 *          → viewer.html stays as original (no re-render)
 *
 * Save options:
 *   1. MoA에 저장 (SQLite FTS5) — stored in ~/.moa/moa_documents.db
 *   2. 하드디스크에 저장 — user picks a folder, saves .md + .html
 *   Both can be selected simultaneously.
 *   "저장하지 않음" — sends to LLM only, no local persistence.
 *
 * Supports: PDF (digital + image), HWP, HWPX, DOC, DOCX, XLS, XLSX, PPT, PPTX
 */

import { useState, useRef, useCallback, useEffect } from "react";
import { type Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";
import { DocumentViewer } from "./DocumentViewer";
import { TiptapEditor, type TiptapEditorHandle } from "./TiptapEditor";

// Tauri invoke for local commands (PyMuPDF, pdf2htmlEX, etc.)
let tauriInvoke: ((cmd: string, args?: Record<string, unknown>) => Promise<unknown>) | null = null;
try {
  const tauri = (window as unknown as Record<string, unknown>).__TAURI__;
  if (tauri && typeof (tauri as Record<string, unknown>).invoke === "function") {
    tauriInvoke = (tauri as Record<string, (cmd: string, args?: Record<string, unknown>) => Promise<unknown>>).invoke;
  }
} catch {
  // Not in Tauri environment (web mode)
}

function isTauriApp(): boolean {
  return tauriInvoke !== null;
}

/** Save target options */
type SaveTarget = "moa" | "disk";

interface SaveDialogState {
  open: boolean;
  targets: Set<SaveTarget>;
  diskPath: string | null;
  saving: boolean;
  result: string | null;
}

/** Office document extensions processed via Hancom API */
const OFFICE_EXTENSIONS = [".hwp", ".hwpx", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx"];

interface DocumentEditorProps {
  locale: Locale;
  onBack: () => void;
  onToggleSidebar: () => void;
  sidebarOpen: boolean;
  /** Optional initial HTML content to display in the viewer */
  initialHtml?: string;
  /** Callback when document is saved/updated — sends Markdown to AI */
  onDocumentUpdate?: (markdown: string, html: string) => void;
  /** Optional file name to auto-load a previously saved document */
  initialFileName?: string;
}

interface DocumentState {
  /** Original HTML from pdf2htmlEX / converter — never modified after initial set */
  viewerHtml: string;
  /** Editable Markdown from PyMuPDF structure extraction */
  markdown: string;
  /** Tiptap JSON for structured persistence */
  tiptapJson: Record<string, unknown> | null;
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
  initialFileName,
}: DocumentEditorProps) {
  // Editor open/close state — starts closed (viewer only)
  const [editorOpen, setEditorOpen] = useState(false);
  const [isUploading, setIsUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [doc, setDoc] = useState<DocumentState>({
    viewerHtml: initialHtml || "",
    markdown: "",
    tiptapJson: null,
    fileName: "",
    docType: "",
    engine: "",
    isModified: false,
  });

  const tiptapRef = useRef<TiptapEditorHandle>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Saved documents list for the "Open" picker
  const [savedDocs, setSavedDocs] = useState<string[]>([]);
  const [showSavedDocs, setShowSavedDocs] = useState(false);

  // Save dialog state
  const [saveDialog, setSaveDialog] = useState<SaveDialogState>({
    open: false,
    targets: new Set<SaveTarget>(["moa"]),
    diskPath: null,
    saving: false,
    result: null,
  });
  // Ref to always access latest saveDialog in async callbacks (avoids stale closure)
  const saveDialogRef = useRef(saveDialog);
  saveDialogRef.current = saveDialog;

  // Fetch saved documents list on mount (Tauri only)
  useEffect(() => {
    if (!isTauriApp() || !tauriInvoke) return;
    tauriInvoke("list_documents", {})
      .then((result) => {
        const r = result as { documents: string[] };
        setSavedDocs(r.documents || []);
      })
      .catch(() => {});
  }, []);

  // Auto-load a saved document if initialFileName is provided
  useEffect(() => {
    if (!initialFileName || !isTauriApp() || !tauriInvoke) return;
    tauriInvoke("load_document", { fileName: initialFileName })
      .then((result) => {
        const r = result as { success: boolean; markdown: string; tiptap_json?: string };
        if (r.success && r.markdown) {
          setDoc({
            viewerHtml: "",
            markdown: r.markdown,
            tiptapJson: r.tiptap_json ? JSON.parse(r.tiptap_json) : null,
            fileName: initialFileName,
            docType: "saved",
            engine: "local",
            isModified: false,
          });
          setEditorOpen(true);
        }
      })
      .catch(() => {});
  }, [initialFileName]);

  // Load a previously saved document by name
  const handleLoadSavedDocument = useCallback(async (name: string) => {
    if (!isTauriApp() || !tauriInvoke) return;
    setShowSavedDocs(false);
    setIsUploading(true);
    setUploadProgress(locale === "ko" ? "문서 불러오는 중..." : "Loading document...");
    try {
      const result = await tauriInvoke("load_document", { fileName: name }) as {
        success: boolean;
        markdown: string;
        tiptap_json?: string;
      };
      if (result.success && result.markdown) {
        setDoc({
          viewerHtml: "",
          markdown: result.markdown,
          tiptapJson: result.tiptap_json ? JSON.parse(result.tiptap_json) : null,
          fileName: name,
          docType: "saved",
          engine: "local",
          isModified: false,
        });
        setEditorOpen(true);
        if (onDocumentUpdate) {
          onDocumentUpdate(result.markdown, "");
        }
      } else {
        setError(
          locale === "ko"
            ? "문서를 불러올 수 없습니다. 파일이 비어있거나 손상되었을 수 있습니다."
            : "Could not load document. The file may be empty or corrupted."
        );
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load document");
    } finally {
      setIsUploading(false);
      setUploadProgress("");
    }
  }, [locale, onDocumentUpdate]);

  // ── Apply conversion result (sets both viewer HTML and editor Markdown) ──
  const applyDualResult = useCallback((result: {
    viewer_html: string;
    markdown: string;
    doc_type: string;
    engine: string;
    page_count: number;
  }, fileName: string) => {
    setDoc({
      viewerHtml: result.viewer_html,
      markdown: result.markdown,
      tiptapJson: null,
      fileName,
      docType: result.doc_type,
      engine: result.engine,
      isModified: false,
    });

    // Notify parent with initial markdown
    if (onDocumentUpdate && result.markdown) {
      onDocumentUpdate(result.markdown, result.viewer_html);
    }
  }, [onDocumentUpdate]);

  // ── PDF upload: 2-layer pipeline ──────────────────────────────────
  // Step 1: pdf2htmlEX → viewer HTML (layout-preserving)
  // Step 2: PyMuPDF   → Markdown (structure for Tiptap)
  // Fallback for image PDF: R2 → Upstage OCR
  const handlePdfUpload = useCallback(async (file: File) => {
    if (isTauriApp() && tauriInvoke) {
      setUploadProgress(
        locale === "ko"
          ? "PDF 변환 중 (뷰어: pdf2htmlEX + 편집: PyMuPDF)..."
          : "Converting PDF (Viewer: pdf2htmlEX + Editor: PyMuPDF)..."
      );

      try {
        const arrayBuf = await file.arrayBuffer();

        // Write file via Tauri invoke (binary data as base64)
        // Chunked encoding to avoid stack overflow with spread operator on large files
        const bytes = new Uint8Array(arrayBuf);
        const chunkSize = 8192;
        let binaryStr = "";
        for (let i = 0; i < bytes.length; i += chunkSize) {
          const chunk = bytes.subarray(i, i + chunkSize);
          binaryStr += String.fromCharCode.apply(null, Array.from(chunk));
        }
        const base64 = btoa(binaryStr);
        // write_temp_file returns the generated temp file path
        const tempPath = await tauriInvoke!("write_temp_file", {
          base64Data: base64,
          extension: "pdf",
        }) as string;

        // Run both conversions: pdf2htmlEX (viewer) + PyMuPDF (editor)
        try {
          const result = await tauriInvoke("convert_pdf_dual", {
            filePath: tempPath,
          }) as {
            success: boolean;
            viewer_html: string;
            markdown: string;
            page_count: number;
            engine: string;
          };

          if (result.success && result.viewer_html && result.viewer_html.length > 100) {
            applyDualResult({
              viewer_html: result.viewer_html,
              markdown: result.markdown,
              doc_type: "digital_pdf",
              engine: result.engine || "pdf2htmlEX+pymupdf",
              page_count: result.page_count,
            }, file.name);
            return;
          }

          // Fallback: try PyMuPDF-only conversion (pdf2htmlEX not available)
          setUploadProgress(
            locale === "ko"
              ? "PyMuPDF 로컬 변환 중..."
              : "Trying PyMuPDF local conversion..."
          );

          const fallback = await tauriInvoke("convert_pdf_local", {
            filePath: tempPath,
          }) as {
            success: boolean;
            html: string;
            markdown: string;
            page_count: number;
            engine: string;
          };

          if (fallback.success && fallback.html && fallback.html.length > 100) {
            applyDualResult({
              viewer_html: fallback.html,
              markdown: fallback.markdown,
              doc_type: "digital_pdf",
              engine: fallback.engine || "pymupdf4llm",
              page_count: fallback.page_count,
            }, file.name);
            return;
          }
        } finally {
          // Clean up the temp file after conversion (success or failure)
          tauriInvoke!("cleanup_temp_file", { filePath: tempPath }).catch(() => {});
        }
      } catch {
        // Local conversion not available → fall through to server
      }
    }

    // Image PDF fallback → R2 pre-signed URL → Railway → Upstage OCR
    setUploadProgress(
      locale === "ko"
        ? "이미지 PDF 처리 중 (Upstage OCR)..."
        : "Processing image PDF (Upstage OCR)..."
    );

    const serverUrl = apiClient.getServerUrl();
    const token = apiClient.getToken();

    // Get pre-signed R2 upload URL
    setUploadProgress(
      locale === "ko" ? "업로드 URL 발급 중..." : "Requesting upload URL..."
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

    // Upload file to R2
    setUploadProgress(
      locale === "ko" ? "파일 업로드 중 (R2)..." : "Uploading file (R2)..."
    );

    const uploadResp = await fetch(upload_url, {
      method: "PUT",
      headers: { "Content-Type": file.type || "application/pdf" },
      body: file,
    });

    if (!uploadResp.ok) {
      throw new Error(
        locale === "ko"
          ? `R2 업로드 실패 (HTTP ${uploadResp.status})`
          : `R2 upload failed (HTTP ${uploadResp.status})`
      );
    }

    // Process via Upstage OCR
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
      body: JSON.stringify({ object_key, filename: file.name, estimated_pages: 1 }),
    });

    if (!processResp.ok) {
      const data = await processResp.json().catch(() => ({ error: "Processing failed" }));
      throw new Error(data.error || `R2 processing failed (${processResp.status})`);
    }

    const result = await processResp.json();
    // Server returns html + markdown; use html for viewer, markdown for editor
    applyDualResult({
      viewer_html: result.html || "",
      markdown: result.markdown || "",
      doc_type: result.doc_type || "image_pdf",
      engine: result.engine || "upstage",
      page_count: result.page_count || 0,
    }, file.name);
  }, [locale, applyDualResult]);

  // ── Office upload → Hancom API ────────────────────────────────────
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
      headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}) },
      body: formData,
    });

    if (!response.ok) {
      const data = await response.json().catch(() => ({ error: "Upload failed" }));
      throw new Error(data.error || `Processing failed (${response.status})`);
    }

    const result = await response.json();
    applyDualResult({
      viewer_html: result.html || "",
      markdown: result.markdown || "",
      doc_type: result.doc_type || "office",
      engine: result.engine || "hancom",
      page_count: result.page_count || 0,
    }, file.name);
  }, [locale, applyDualResult]);

  // ── File upload routing ──────────────────────────────────────────
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
    setEditorOpen(false);
    setUploadProgress(
      locale === "ko" ? `${file.name} 처리 중...` : `Processing ${file.name}...`
    );

    try {
      const isPdf = ext === ".pdf";
      const isOffice = OFFICE_EXTENSIONS.includes(ext);

      if (isPdf) {
        await handlePdfUpload(file);
      } else if (isOffice) {
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
  }, [locale, handlePdfUpload, handleOfficeUpload]);

  // ── "Edit" button: open Tiptap editor pane ───────────────────────
  const handleOpenEditor = useCallback(() => {
    setEditorOpen(true);
    // Load markdown into Tiptap — retry until ref is available (editor may
    // need a render cycle to mount after setEditorOpen(true))
    const tryLoad = (attempts: number) => {
      if (tiptapRef.current) {
        tiptapRef.current.setMarkdown(doc.markdown);
        tiptapRef.current.focus();
      } else if (attempts < 10) {
        setTimeout(() => tryLoad(attempts + 1), 50);
      }
    };
    setTimeout(() => tryLoad(0), 0);
  }, [doc.markdown]);

  // ── Close editor pane ─────────────────────────────────────────────
  const handleCloseEditor = useCallback(() => {
    setEditorOpen(false);
  }, []);

  // ── Editor content change handler ─────────────────────────────────
  const handleEditorChange = useCallback((markdown: string) => {
    setDoc((prev) => ({
      ...prev,
      markdown,
      isModified: true,
    }));
  }, []);

  // ── Save dialog: open ──────────────────────────────────────────
  const handleOpenSaveDialog = useCallback(() => {
    setSaveDialog((prev) => ({
      ...prev,
      open: true,
      result: null,
    }));
  }, []);

  // ── Save dialog: close ─────────────────────────────────────────
  const handleCloseSaveDialog = useCallback(() => {
    setSaveDialog((prev) => ({
      ...prev,
      open: false,
      saving: false,
      result: null,
    }));
  }, []);

  // ── Save dialog: toggle save target ────────────────────────────
  const handleToggleSaveTarget = useCallback((target: SaveTarget) => {
    setSaveDialog((prev) => {
      const next = new Set(prev.targets);
      if (next.has(target)) {
        next.delete(target);
      } else {
        next.add(target);
      }
      return { ...prev, targets: next };
    });
  }, []);

  // ── Save dialog: pick folder for disk save ─────────────────────
  const handlePickFolder = useCallback(async () => {
    if (!isTauriApp() || !tauriInvoke) return;

    try {
      // Use Tauri dialog plugin to pick a folder
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        directory: true,
        multiple: false,
        title: locale === "ko" ? "저장 폴더 선택" : "Select save folder",
      });
      // open() may return string, string[], or null depending on config
      if (selected) {
        const path = Array.isArray(selected) ? selected[0] : selected;
        if (path) {
          setSaveDialog((prev) => ({ ...prev, diskPath: path }));
        }
      }
    } catch {
      // Dialog not available — let user type a path manually
    }
  }, [locale]);

  // ── Execute save with selected targets ─────────────────────────
  const handleSave = useCallback(async () => {
    if (!tiptapRef.current) return;

    const markdown = tiptapRef.current.getMarkdown();
    const tiptapJson = tiptapRef.current.getJSON();
    const editorHtml = tiptapRef.current.getHTML();

    setSaveDialog((prev) => ({ ...prev, saving: true, result: null }));

    const results: string[] = [];
    const { targets, diskPath } = saveDialogRef.current;

    // Update document state
    setDoc((prev) => ({
      ...prev,
      markdown,
      tiptapJson,
      isModified: false,
    }));

    if (isTauriApp() && tauriInvoke && doc.fileName) {
      // Save to MoA (SQLite FTS5)
      if (targets.has("moa")) {
        try {
          await tauriInvoke("save_document_to_sqlite", {
            fileName: doc.fileName,
            markdown,
            html: editorHtml,
            tiptapJson: JSON.stringify(tiptapJson),
            docType: doc.docType,
            engine: doc.engine,
          });
          results.push(locale === "ko" ? "MoA에 저장 완료" : "Saved to MoA");
        } catch (e) {
          results.push(
            locale === "ko"
              ? `MoA 저장 실패: ${e}`
              : `MoA save failed: ${e}`
          );
        }

        // Also save to filesystem for backward compat
        try {
          await tauriInvoke("save_document", {
            fileName: doc.fileName,
            markdown,
            tiptapJson: JSON.stringify(tiptapJson),
            editorHtml,
          });
          // Refresh saved docs list
          tauriInvoke("list_documents", {})
            .then((r) => setSavedDocs((r as { documents: string[] }).documents || []))
            .catch(() => {});
        } catch {
          // Filesystem save failed — SQLite has the data
        }
      }

      // Save to hard disk (user-chosen directory)
      if (targets.has("disk")) {
        if (!diskPath) {
          results.push(
            locale === "ko"
              ? "저장 폴더를 선택해주세요"
              : "Please select a save folder first"
          );
        } else {
          try {
            const diskResult = await tauriInvoke("save_document_to_disk", {
              dirPath: diskPath,
              fileName: doc.fileName,
              markdown,
              html: editorHtml,
            }) as { success: boolean; markdown_path: string };
            results.push(
              locale === "ko"
                ? `하드디스크에 저장 완료: ${diskResult.markdown_path}`
                : `Saved to disk: ${diskResult.markdown_path}`
            );
          } catch (e) {
            results.push(
              locale === "ko"
                ? `하드디스크 저장 실패: ${e}`
                : `Disk save failed: ${e}`
            );
          }
        }
      }
    }

    // Always notify parent (AI context) regardless of save target
    if (onDocumentUpdate) {
      onDocumentUpdate(markdown, editorHtml);
    }

    setSaveDialog((prev) => ({
      ...prev,
      saving: false,
      result: results.join(" | "),
    }));

    // Auto-close dialog after short delay on success
    if (results.length > 0 && results.every((r) => r.includes("완료") || r.includes("Saved"))) {
      setTimeout(() => {
        setSaveDialog((prev) => ({ ...prev, open: false, result: null }));
      }, 1500);
    }
  }, [doc.fileName, doc.docType, doc.engine, onDocumentUpdate, locale]);

  // ── Send to LLM only (no save) ────────────────────────────────
  const handleSendToLlmOnly = useCallback(() => {
    if (!tiptapRef.current) return;

    const markdown = tiptapRef.current.getMarkdown();
    const editorHtml = tiptapRef.current.getHTML();

    setDoc((prev) => ({
      ...prev,
      markdown,
      isModified: false,
    }));

    if (onDocumentUpdate) {
      onDocumentUpdate(markdown, editorHtml);
    }

    setSaveDialog((prev) => ({
      ...prev,
      open: false,
      result: null,
    }));
  }, [onDocumentUpdate]);

  // ── Export as Markdown ────────────────────────────────────────────
  const handleExportMarkdown = useCallback(() => {
    const md = tiptapRef.current?.getMarkdown() || doc.markdown;
    const blob = new Blob([md], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = doc.fileName.replace(/\.[^.]+$/, ".md") || "document.md";
    a.click();
    URL.revokeObjectURL(url);
  }, [doc.markdown, doc.fileName]);

  // ── Export as HTML ────────────────────────────────────────────────
  const handleExportHtml = useCallback(() => {
    const html = tiptapRef.current?.getHTML() || doc.viewerHtml;
    const blob = new Blob([html], { type: "text/html" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = doc.fileName.replace(/\.[^.]+$/, ".html") || "document.html";
    a.click();
    URL.revokeObjectURL(url);
  }, [doc.viewerHtml, doc.fileName]);

  // ── File drop handler ─────────────────────────────────────────────
  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    const file = e.dataTransfer.files[0];
    if (file) handleFileUpload(file);
  }, [handleFileUpload]);

  // ── Render ────────────────────────────────────────────────────────
  return (
    <div className="document-editor-page">
      {/* Header */}
      <div className="chat-header">
        <button className="chat-header-toggle" onClick={onToggleSidebar}>
          {sidebarOpen ? "\u2715" : "\u2630"}
        </button>
        <div className="chat-header-title">
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

      {/* Top toolbar */}
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

        {/* Open saved document (Tauri only) */}
        {isTauriApp() && savedDocs.length > 0 && (
          <div style={{ position: "relative", display: "inline-block" }}>
            <button
              className="toolbar-btn"
              onClick={() => setShowSavedDocs((p) => !p)}
              disabled={isUploading}
              title={locale === "ko" ? "저장된 문서 열기" : "Open saved document"}
            >
              {locale === "ko" ? "열기" : "Open"}
            </button>
            {showSavedDocs && (
              <div className="saved-docs-dropdown">
                {savedDocs.map((name) => (
                  <button
                    key={name}
                    className="saved-doc-item"
                    onClick={() => handleLoadSavedDocument(name)}
                  >
                    {name}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        <span className="toolbar-divider" />

        {/* Edit toggle */}
        {doc.viewerHtml && !editorOpen && (
          <button
            className="toolbar-btn edit-toggle-btn"
            onClick={handleOpenEditor}
            title={locale === "ko" ? "수정하기 — 에디터 열기" : "Edit — open editor"}
          >
            {locale === "ko" ? "수정하기" : "Edit"}
          </button>
        )}
        {editorOpen && (
          <button
            className="toolbar-btn"
            onClick={handleCloseEditor}
            title={locale === "ko" ? "에디터 닫기" : "Close editor"}
          >
            {locale === "ko" ? "에디터 닫기" : "Close Editor"}
          </button>
        )}

        <span className="toolbar-divider" />

        {/* Save & Export (visible when document is loaded or opened from saved) */}
        {(doc.viewerHtml || doc.fileName) && (
          <>
            <button
              className="toolbar-btn save-btn"
              onClick={handleOpenSaveDialog}
              disabled={!editorOpen}
              title={locale === "ko" ? "저장 옵션 선택" : "Save options"}
            >
              {locale === "ko" ? "저장하기" : "Save"}
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

        {/* Back button */}
        <div style={{ marginLeft: "auto" }}>
          <button className="toolbar-btn" onClick={onBack}>
            {locale === "ko" ? "뒤로" : "Back"}
          </button>
        </div>
      </div>

      {/* Document info bar */}
      {doc.docType && (
        <div className="doc-info-bar">
          <span className="doc-info-type">
            {doc.docType === "digital_pdf" && (locale === "ko"
              ? "디지털 PDF (뷰어: pdf2htmlEX | 편집: PyMuPDF)"
              : "Digital PDF (Viewer: pdf2htmlEX | Editor: PyMuPDF)")}
            {doc.docType === "image_pdf" && (locale === "ko"
              ? "이미지 PDF (Upstage OCR)"
              : "Image PDF (Upstage OCR)")}
            {doc.docType.startsWith("office") && (locale === "ko"
              ? "오피스 문서 (한컴 변환기)"
              : "Office document (Hancom converter)")}
          </span>
          <span className="doc-info-engine">{doc.engine}</span>
          {editorOpen && (
            <span className="doc-info-mode">
              {locale === "ko" ? "| 뷰어 + 에디터 모드" : "| Viewer + Editor mode"}
            </span>
          )}
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

      {/* Main content area: Viewer (left) + Editor (right) */}
      <div
        className={`doc-split-container ${editorOpen ? "editor-visible" : "viewer-only"}`}
        onDrop={handleDrop}
        onDragOver={(e) => e.preventDefault()}
      >
        {/* Empty state / dropzone */}
        {!doc.viewerHtml && !isUploading && (
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
                ? "디지털 PDF: pdf2htmlEX+PyMuPDF (로컬) | 이미지 PDF: Upstage OCR (서버) | 오피스: 한컴 변환"
                : "Digital PDF: pdf2htmlEX+PyMuPDF (local) | Image PDF: Upstage OCR (server) | Office: Hancom converter"}
            </p>
          </div>
        )}

        {/* Layer 1: Viewer (always visible when document loaded) */}
        {doc.viewerHtml && (
          <div className={`doc-viewer-pane ${editorOpen ? "split" : "full"}`}>
            <div className="pane-header">
              <span className="pane-label">
                {locale === "ko" ? "원본 미리보기 (읽기 전용)" : "Original Preview (Read-only)"}
              </span>
            </div>
            <DocumentViewer
              html={doc.viewerHtml}
              locale={locale}
            />
          </div>
        )}

        {/* Layer 2: Tiptap Editor (visible when "Edit" is clicked) */}
        {doc.viewerHtml && editorOpen && (
          <div className="doc-editor-pane">
            <div className="pane-header">
              <span className="pane-label">
                {locale === "ko" ? "편집기 (Markdown 기반)" : "Editor (Markdown-based)"}
              </span>
            </div>
            <TiptapEditor
              ref={tiptapRef}
              initialMarkdown={doc.markdown}
              locale={locale}
              onChange={handleEditorChange}
            />
          </div>
        )}
      </div>

      {/* ── Save Dialog Overlay ──────────────────────────────────── */}
      {saveDialog.open && (
        <div className="save-dialog-overlay" onClick={handleCloseSaveDialog}>
          <div className="save-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="save-dialog-header">
              <h3>{locale === "ko" ? "저장 옵션" : "Save Options"}</h3>
              <button className="save-dialog-close" onClick={handleCloseSaveDialog}>
                {"\u2715"}
              </button>
            </div>

            <div className="save-dialog-body">
              {/* Option 1: Save to MoA (SQLite) */}
              <label className="save-option">
                <input
                  type="checkbox"
                  checked={saveDialog.targets.has("moa")}
                  onChange={() => handleToggleSaveTarget("moa")}
                />
                <div className="save-option-info">
                  <span className="save-option-title">
                    {locale === "ko" ? "MoA에 저장" : "Save to MoA"}
                  </span>
                  <span className="save-option-desc">
                    {locale === "ko"
                      ? "로컬 SQLite (FTS5 전문 검색 지원) — 나중에 MoA에서 문서를 검색/열기 가능"
                      : "Local SQLite (FTS5 full-text search) — searchable and reopenable from MoA"}
                  </span>
                </div>
              </label>

              {/* Option 2: Save to hard disk */}
              <label className="save-option">
                <input
                  type="checkbox"
                  checked={saveDialog.targets.has("disk")}
                  onChange={() => handleToggleSaveTarget("disk")}
                />
                <div className="save-option-info">
                  <span className="save-option-title">
                    {locale === "ko" ? "하드디스크에 저장" : "Save to Disk"}
                  </span>
                  <span className="save-option-desc">
                    {locale === "ko"
                      ? "로컬 하드디스크에 .md + .html 파일로 저장"
                      : "Save as .md + .html files on local disk"}
                  </span>
                </div>
              </label>

              {/* Folder picker (shown when disk is selected) */}
              {saveDialog.targets.has("disk") && (
                <div className="save-disk-path">
                  <button
                    className="toolbar-btn"
                    onClick={handlePickFolder}
                    type="button"
                  >
                    {locale === "ko" ? "폴더 선택" : "Choose Folder"}
                  </button>
                  <span className="save-disk-path-display">
                    {saveDialog.diskPath
                      ? saveDialog.diskPath
                      : (locale === "ko" ? "폴더를 선택해주세요" : "No folder selected")}
                  </span>
                </div>
              )}

              {/* Result message */}
              {saveDialog.result && (
                <div className="save-dialog-result">
                  {saveDialog.result}
                </div>
              )}
            </div>

            <div className="save-dialog-footer">
              {/* Save button */}
              <button
                className="toolbar-btn save-btn"
                onClick={handleSave}
                disabled={saveDialog.saving || saveDialog.targets.size === 0}
              >
                {saveDialog.saving
                  ? (locale === "ko" ? "저장 중..." : "Saving...")
                  : (locale === "ko" ? "저장하기" : "Save")}
              </button>

              {/* Send to LLM only (no save) */}
              <button
                className="toolbar-btn save-llm-only-btn"
                onClick={handleSendToLlmOnly}
                disabled={saveDialog.saving}
                title={locale === "ko"
                  ? "로컬에 저장하지 않고 LLM에게만 전송"
                  : "Send to LLM only without saving locally"}
              >
                {locale === "ko" ? "저장하지 않음 (LLM에만 전송)" : "Don't Save (Send to LLM only)"}
              </button>

              <button
                className="toolbar-btn"
                onClick={handleCloseSaveDialog}
              >
                {locale === "ko" ? "취소" : "Cancel"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
