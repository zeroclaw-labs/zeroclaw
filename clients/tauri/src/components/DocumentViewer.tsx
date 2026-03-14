/**
 * Read-only document viewer for original PDF/Office HTML output.
 *
 * Renders the pdf2htmlEX (or PyMuPDF/Hancom) generated HTML inside a
 * sandboxed iframe so absolute-positioning CSS from the converter does
 * not interfere with the app layout. The viewer is strictly read-only —
 * all editing happens in the companion TiptapEditor.
 *
 * Uses srcdoc for secure rendering without needing allow-same-origin.
 */

import { useMemo } from "react";
import { type Locale } from "../lib/i18n";

interface DocumentViewerProps {
  /** Raw HTML string produced by pdf2htmlEX / PyMuPDF / Hancom */
  html: string;
  locale: Locale;
  /** Optional CSS class for the container */
  className?: string;
}

export function DocumentViewer({ html, locale, className }: DocumentViewerProps) {
  // Build a self-contained HTML page with dark-mode-aware styling
  const srcdoc = useMemo(() => {
    if (!html) return "";
    return `<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  html, body {
    margin: 0;
    padding: 16px;
    background: #0f1117;
    color: #e4e5f1;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Noto Sans KR", sans-serif;
    font-size: 14px;
    line-height: 1.6;
  }
  /* Allow pdf2htmlEX absolute positioning to work */
  body { position: relative; }
  /* Basic table styling for non-pdf2htmlEX HTML */
  table { border-collapse: collapse; margin: 8px 0; }
  td, th { border: 1px solid #2a2d45; padding: 4px 8px; }
  th { background: #1e2030; font-weight: 600; }
  img { max-width: 100%; height: auto; }
  a { color: #6366f1; }
  /* Scrollbar styling */
  ::-webkit-scrollbar { width: 6px; }
  ::-webkit-scrollbar-track { background: transparent; }
  ::-webkit-scrollbar-thumb { background: #363a58; border-radius: 3px; }
</style>
</head>
<body>${html}</body>
</html>`;
  }, [html]);

  if (!html) {
    return (
      <div className={`doc-viewer-empty ${className || ""}`}>
        <p>
          {locale === "ko"
            ? "문서를 업로드하면 여기에 원본 미리보기가 표시됩니다."
            : "Upload a document to see the original preview here."}
        </p>
      </div>
    );
  }

  return (
    <iframe
      className={`doc-viewer-iframe ${className || ""}`}
      srcDoc={srcdoc}
      sandbox="allow-same-origin"
      title={locale === "ko" ? "원본 문서 미리보기" : "Original document preview"}
    />
  );
}
