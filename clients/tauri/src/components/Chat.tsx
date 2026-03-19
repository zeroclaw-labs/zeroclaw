import { useState, useRef, useEffect, useCallback, type FormEvent, type KeyboardEvent } from "react";
import { t, type Locale } from "../lib/i18n";
import { renderMarkdown } from "../lib/markdown";
import type { ChatSession, ChatMessage } from "../lib/storage";
import { deriveChatTitle } from "../lib/storage";
import {
  createAttachment,
  isDocumentFile,
  isLargeImagePdfBatch,
  estimatePageCount,
  formatFileSize,
  SUPPORTED_CHAT_EXTENSIONS,
  type AttachmentFile,
} from "../lib/chat-attachments";
import { apiClient } from "../lib/api";
import { isTauri } from "../lib/tauri-bridge";

interface ChatProps {
  chat: ChatSession | null;
  locale: Locale;
  isConnected: boolean;
  onSendMessage: (content: string, attachments?: AttachmentFile[]) => Promise<void>;
  onRetry: (messages: ChatMessage[]) => void;
  onOpenSettings: () => void;
  onOpenInterpreter: () => void;
  onToggleSidebar: () => void;
  sidebarOpen: boolean;
}

export function Chat({
  chat,
  locale,
  isConnected,
  onSendMessage,
  onRetry,
  onOpenSettings,
  onOpenInterpreter,
  onToggleSidebar,
  sidebarOpen,
}: ChatProps) {
  const [input, setInput] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [attachments, setAttachments] = useState<AttachmentFile[]>([]);
  const [docInfoDismissed, setDocInfoDismissed] = useState(false);
  const [ocrConsent, setOcrConsent] = useState<{
    show: boolean;
    pendingFiles: AttachmentFile[];
  }>({ show: false, pendingFiles: [] });

  // Workspace connect state (moved from Sidebar)
  const [showGitHubInput, setShowGitHubInput] = useState(false);
  const [gitHubUrl, setGitHubUrl] = useState("");
  const [workspaceStatus, setWorkspaceStatus] = useState<string | null>(null);
  const [workspaceLoading, setWorkspaceLoading] = useState(false);
  const [connectedWorkspace, setConnectedWorkspace] = useState<string | null>(
    () => apiClient.getWorkspacePath(),
  );

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const messages = chat?.messages ?? [];

  const scrollToBottom = useCallback(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, []);

  useEffect(() => {
    scrollToBottom();
  }, [messages.length, scrollToBottom]);

  useEffect(() => {
    if (!isLoading) {
      textareaRef.current?.focus();
    }
  }, [isLoading]);

  // ── File attachment handling ─────────────────────────────────────

  const handleFileSelect = useCallback(
    (files: FileList | null) => {
      if (!files) return;

      const newAttachments: AttachmentFile[] = [];
      for (let i = 0; i < files.length; i++) {
        const file = files[i];
        const ext = "." + file.name.split(".").pop()?.toLowerCase();
        if (!SUPPORTED_CHAT_EXTENSIONS.includes(ext)) continue;

        const attachment = createAttachment(file);
        // Estimate page count for PDFs
        if (ext === ".pdf") {
          attachment.pageCount = estimatePageCount(file);
        }
        newAttachments.push(attachment);
      }

      if (newAttachments.length === 0) return;

      // Show document info toast for office/PDF files
      const hasDocFiles = newAttachments.some((a) => isDocumentFile(a.file));
      if (hasDocFiles) {
        setDocInfoDismissed(false);
      }

      setAttachments((prev) => [...prev, ...newAttachments]);
    },
    [],
  );

  const handleRemoveAttachment = useCallback((id: string) => {
    setAttachments((prev) => prev.filter((a) => a.id !== id));
  }, []);

  const handleAttachClick = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  // ── Submit handling ─────────────────────────────────────────────

  const handleSubmit = useCallback(
    async (e?: FormEvent) => {
      e?.preventDefault();
      const trimmed = input.trim();
      if ((!trimmed && attachments.length === 0) || isLoading) return;

      // Check for large image PDF batch needing OCR consent
      const imagePdfs = attachments.filter(
        (a) => a.type === "text_pdf" && a.pageCount && a.pageCount >= 1,
      );
      // Re-classify: estimate which are image PDFs (heuristic based on file size per page)
      const likelyImagePdfs = imagePdfs.filter((a) => {
        const bytesPerPage = a.file.size / (a.pageCount || 1);
        return bytesPerPage > 50_000; // >50KB/page likely image PDF
      });

      if (likelyImagePdfs.length > 0 && isLargeImagePdfBatch(likelyImagePdfs)) {
        // Mark these as large image PDFs and show consent
        const updatedAttachments = attachments.map((a) => {
          if (likelyImagePdfs.some((ip) => ip.id === a.id)) {
            return { ...a, type: "image_pdf_large" as const, isImagePdf: true };
          }
          return a;
        });
        setOcrConsent({ show: true, pendingFiles: updatedAttachments });
        return;
      }

      await doSend(trimmed, attachments);
    },
    [input, isLoading, attachments],
  );

  const doSend = useCallback(
    async (text: string, files: AttachmentFile[]) => {
      setInput("");
      setAttachments([]);
      setDocInfoDismissed(false);
      setIsLoading(true);

      if (textareaRef.current) {
        textareaRef.current.style.height = "auto";
      }

      try {
        await onSendMessage(text, files.length > 0 ? files : undefined);
      } finally {
        setIsLoading(false);
      }
    },
    [onSendMessage],
  );

  const handleOcrConsent = useCallback(
    (agreed: boolean) => {
      if (agreed) {
        doSend(input.trim(), ocrConsent.pendingFiles);
      }
      setOcrConsent({ show: false, pendingFiles: [] });
    },
    [input, ocrConsent.pendingFiles, doSend],
  );

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSubmit();
      }
    },
    [handleSubmit],
  );

  const handleTextareaInput = useCallback(() => {
    const el = textareaRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 120) + "px";
    }
  }, []);

  // ── Workspace handlers (moved from Sidebar) ──────────────────

  const handleConnectFolder = useCallback(async () => {
    if (workspaceLoading) return;
    try {
      if (isTauri()) {
        const { open } = await import("@tauri-apps/plugin-dialog");
        const selected = await open({ directory: true, multiple: false });
        if (!selected) return;
        const dirPath = typeof selected === "string" ? selected : selected[0];
        if (!dirPath) return;
        setWorkspaceLoading(true);
        const resolved = await apiClient.setWorkspaceDir(dirPath);
        setConnectedWorkspace(resolved);
        setWorkspaceStatus(locale === "ko" ? "폴더가 연결되었습니다" : "Folder connected");
        setTimeout(() => setWorkspaceStatus(null), 3000);
      }
    } catch (err) {
      setWorkspaceStatus(err instanceof Error ? err.message : "Error");
      setTimeout(() => setWorkspaceStatus(null), 4000);
    } finally {
      setWorkspaceLoading(false);
    }
  }, [workspaceLoading, locale]);

  const handleConnectGitHub = useCallback(async () => {
    const url = gitHubUrl.trim();
    if (!url || workspaceLoading) return;
    setWorkspaceLoading(true);
    setWorkspaceStatus(null);
    try {
      const resolved = await apiClient.connectGitHubRepo(url);
      setConnectedWorkspace(resolved);
      setWorkspaceStatus(locale === "ko" ? "저장소가 연결되었습니다" : "Repository connected");
      setGitHubUrl("");
      setShowGitHubInput(false);
      setTimeout(() => setWorkspaceStatus(null), 3000);
    } catch (err) {
      setWorkspaceStatus(err instanceof Error ? err.message : "Error");
      setTimeout(() => setWorkspaceStatus(null), 4000);
    } finally {
      setWorkspaceLoading(false);
    }
  }, [gitHubUrl, workspaceLoading, locale]);

  const handleDisconnectWorkspace = useCallback(() => {
    apiClient.disconnectWorkspace();
    setConnectedWorkspace(null);
    setWorkspaceStatus(null);
  }, []);

  const canSend =
    (input.trim().length > 0 || attachments.length > 0) && !isLoading && isConnected;

  // Check if any attachments are document files (for info toast)
  const hasDocAttachments = attachments.some((a) => isDocumentFile(a.file));

  return (
    <div className="chat-container">
      {/* Header */}
      <div className="chat-header">
        <button
          className="chat-header-toggle"
          onClick={onToggleSidebar}
          aria-label="Toggle sidebar"
        >
          {sidebarOpen ? "\u2715" : "\u2630"}
        </button>
        <span className="chat-header-title">
          {chat
            ? (chat.title && !["MoA", "New Chat"].includes(chat.title)
              ? chat.title
              : (chat.messages.length > 0
                ? deriveChatTitle(chat.messages)
                : t("new_chat", locale)))
            : t("app_title", locale)}
        </span>
        <div className="chat-header-status">
          <div className={`status-dot ${isConnected ? "connected" : ""}`} />
          <span>{isConnected ? t("connected", locale) : t("disconnected", locale)}</span>
        </div>
      </div>

      {/* Messages */}
      <div className="chat-messages">
        {messages.length === 0 ? (
          <div className="chat-welcome">
            <div className="chat-welcome-icon">M</div>
            <h2>{t("welcome_title", locale)}</h2>
            <p>{t("welcome_subtitle", locale)}</p>
            <p>
              {isConnected
                ? t("welcome_hint", locale)
                : t("not_connected_hint", locale)}
            </p>
            {!isConnected && (
              <button className="chat-welcome-connect" onClick={onOpenSettings}>
                {t("login", locale)}
              </button>
            )}
          </div>
        ) : (
          <div className="chat-messages-inner">
            {messages.map((msg) => (
              <MessageBubble
                key={msg.id}
                message={msg}
                locale={locale}
                onRetry={
                  msg.role === "error"
                    ? () => onRetry(messages)
                    : undefined
                }
              />
            ))}

            {isLoading && (
              <div className="thinking-indicator">
                <div className="thinking-avatar">
                  <span style={{ color: "#fff", fontSize: 14, fontWeight: 600 }}>M</span>
                </div>
                <div className="thinking-dots">
                  <span />
                  <span />
                  <span />
                </div>
              </div>
            )}

            <div ref={messagesEndRef} />
          </div>
        )}
      </div>

      {/* OCR Consent Dialog */}
      {ocrConsent.show && (
        <div className="chat-ocr-consent-overlay">
          <div className="chat-ocr-consent-dialog">
            <p>
              {locale === "ko"
                ? "이미지 PDF 문서가 대량으로 업로드되었으므로 LLM에 이미지 그대로 전송하여 멀티모달로 처리하는 것보다 정확성과 비용 및 시간 측면에서 OCR 전문 툴을 사용하는 것이 훨씬 효과적입니다. 다만 OCR 처리에는 A4장당 0.02달러(30원)의 크레딧이 소진됩니다. 계속 진행하는데 동의하시나요?"
                : "A large batch of image PDFs was uploaded. Using a specialized OCR tool (Upstage Document Parser) is more accurate and cost-effective than sending raw images to the LLM. OCR processing costs $0.02 (30 KRW) per A4 page in credits. Do you agree to proceed?"}
            </p>
            <div className="chat-ocr-consent-buttons">
              <button
                className="chat-ocr-consent-btn agree"
                onClick={() => handleOcrConsent(true)}
              >
                {locale === "ko" ? "동의" : "Agree"}
              </button>
              <button
                className="chat-ocr-consent-btn decline"
                onClick={() => handleOcrConsent(false)}
              >
                {locale === "ko" ? "거부" : "Decline"}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Input */}
      <div className="chat-input-area">
        {/* Document info toast */}
        {hasDocAttachments && !docInfoDismissed && (
          <div className="chat-doc-info-toast">
            <p>
              {locale === "ko"
                ? "첨부하신 문서를 이용자님과 AI가 함께 보면서 최고의 이해도로 대화를 하고자 하시거나 문서 중 일부를 정확하게 특정해서 대화를 하고자 하시거나, 문서를 수정/저장하거나 다른 형식으로 변환하고자 하실 경우에는 '문서작업' 카테고리에서 진행하세요."
                : "If you want to view the document together with AI for the best understanding, pinpoint specific parts of the document, or edit/save/convert the document, please use the 'Document Work' category instead."}
            </p>
            <button
              className="chat-doc-info-dismiss"
              onClick={() => setDocInfoDismissed(true)}
            >
              ✕
            </button>
          </div>
        )}

        {/* Attachment preview bar */}
        {attachments.length > 0 && (
          <div className="chat-attachments-bar">
            {attachments.map((a) => (
              <div key={a.id} className="chat-attachment-chip">
                <span className="chat-attachment-icon">
                  {a.type === "image" ? "🖼" : "📄"}
                </span>
                <span className="chat-attachment-name">{a.name}</span>
                <span className="chat-attachment-size">
                  {formatFileSize(a.file.size)}
                </span>
                <button
                  className="chat-attachment-remove"
                  onClick={() => handleRemoveAttachment(a.id)}
                  aria-label="Remove"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Connected workspace indicator */}
        {connectedWorkspace && (
          <div className="chat-workspace-indicator">
            <div className="chat-workspace-dot" />
            <span className="chat-workspace-path" title={connectedWorkspace}>
              {connectedWorkspace.split("/").pop() || connectedWorkspace}
            </span>
            <button
              className="chat-workspace-disconnect"
              onClick={handleDisconnectWorkspace}
              title={locale === "ko" ? "연결 해제" : "Disconnect"}
            >
              {"\u2715"}
            </button>
          </div>
        )}

        {/* GitHub URL input (expandable) */}
        {showGitHubInput && (
          <div className="chat-github-input-row">
            <input
              className="chat-github-input"
              type="text"
              value={gitHubUrl}
              onChange={(e) => setGitHubUrl(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleConnectGitHub(); }}
              placeholder={t("connect_github_placeholder", locale)}
            />
            <button
              className="chat-github-ok-btn"
              disabled={!gitHubUrl.trim() || workspaceLoading}
              onClick={handleConnectGitHub}
            >
              {workspaceLoading ? "..." : "OK"}
            </button>
            <button
              className="chat-github-cancel-btn"
              onClick={() => { setShowGitHubInput(false); setGitHubUrl(""); }}
            >
              {"\u2715"}
            </button>
          </div>
        )}

        {/* Workspace status message */}
        {workspaceStatus && (
          <div className={`chat-workspace-status ${workspaceStatus.includes("Error") || workspaceStatus.includes("failed") ? "error" : "success"}`}>
            {workspaceStatus}
          </div>
        )}

        <form onSubmit={handleSubmit} className="chat-input-wrapper">
          {/* File attachment button */}
          <button
            type="button"
            className="chat-attach-btn"
            onClick={handleAttachClick}
            disabled={!isConnected || isLoading}
            aria-label={locale === "ko" ? "파일 첨부" : "Attach file"}
            title={locale === "ko" ? "파일 첨부" : "Attach file"}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
            </svg>
          </button>

          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept={SUPPORTED_CHAT_EXTENSIONS.join(",")}
            onChange={(e) => {
              handleFileSelect(e.target.files);
              e.target.value = ""; // reset for re-selecting same file
            }}
            style={{ display: "none" }}
          />

          <textarea
            ref={textareaRef}
            className="chat-input"
            placeholder={t("type_message", locale)}
            value={input}
            onChange={(e) => {
              setInput(e.target.value);
              handleTextareaInput();
            }}
            onKeyDown={handleKeyDown}
            rows={1}
            disabled={!isConnected}
          />
          <button
            type="submit"
            className="chat-send-btn"
            disabled={!canSend}
            aria-label={t("send", locale)}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <line x1="22" y1="2" x2="11" y2="13" />
              <polygon points="22 2 15 22 11 13 2 9 22 2" />
            </svg>
          </button>
        </form>

        {/* Action buttons row: folder, github, microphone */}
        <div className="chat-action-buttons">
          <button
            type="button"
            className="chat-action-btn"
            onClick={handleConnectFolder}
            disabled={workspaceLoading || !isConnected}
            title={locale === "ko" ? "폴더 연결" : "Connect folder"}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
            </svg>
            <span>{locale === "ko" ? "폴더 연결" : "Folder"}</span>
          </button>
          <button
            type="button"
            className="chat-action-btn"
            onClick={() => setShowGitHubInput((prev) => !prev)}
            disabled={workspaceLoading || !isConnected}
            title={locale === "ko" ? "GitHub 연결" : "Connect GitHub"}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M9 19c-5 1.5-5-2.5-7-3m14 6v-3.87a3.37 3.37 0 0 0-.94-2.61c3.14-.35 6.44-1.54 6.44-7A5.44 5.44 0 0 0 20 4.77 5.07 5.07 0 0 0 19.91 1S18.73.65 16 2.48a13.38 13.38 0 0 0-7 0C6.27.65 5.09 1 5.09 1A5.07 5.07 0 0 0 5 4.77a5.44 5.44 0 0 0-1.5 3.78c0 5.42 3.3 6.61 6.44 7A3.37 3.37 0 0 0 9 18.13V22" />
            </svg>
            <span>GitHub</span>
          </button>
          <button
            type="button"
            className="chat-action-btn chat-mic-btn"
            onClick={onOpenInterpreter}
            disabled={!isConnected}
            title={locale === "ko" ? "음성 입력 (STT/TTS)" : "Voice input (STT/TTS)"}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
              <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
              <line x1="12" y1="19" x2="12" y2="23" />
              <line x1="8" y1="23" x2="16" y2="23" />
            </svg>
            <span>{locale === "ko" ? "음성" : "Voice"}</span>
          </button>
        </div>
      </div>
    </div>
  );
}

/* --- MessageBubble sub-component --- */

interface MessageBubbleProps {
  message: ChatMessage;
  locale: Locale;
  onRetry?: () => void;
}

function MessageBubble({ message, locale, onRetry }: MessageBubbleProps) {
  const isUser = message.role === "user";
  const isError = message.role === "error";

  return (
    <div className={`message ${message.role}`}>
      {!isUser && (
        <div className="message-avatar">
          {isError ? "!" : "M"}
        </div>
      )}
      <div className="message-content">
        {isUser ? (
          <div className="message-bubble">{message.content}</div>
        ) : (
          <div
            className="message-bubble"
            dangerouslySetInnerHTML={{
              __html: isError
                ? escapeForHtml(message.content)
                : renderMarkdown(message.content),
            }}
          />
        )}
        {message.model && (
          <div className="message-model">{t("model", locale)}: {message.model}</div>
        )}
        {isError && onRetry && (
          <button className="message-retry-btn" onClick={onRetry}>
            {t("retry", locale)}
          </button>
        )}
      </div>
    </div>
  );
}

function escapeForHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\n/g, "<br>");
}
