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

// ---------------------------------------------------------------------------
// Free browser-native STT / TTS helpers (Web Speech API — zero cost)
// ---------------------------------------------------------------------------

/* eslint-disable @typescript-eslint/no-explicit-any */
function createSpeechRecognition(lang: string) {
  const W = window as any;
  const SR = W.SpeechRecognition ?? W.webkitSpeechRecognition;
  if (!SR) return null;
  const r = new SR();
  r.lang = lang;
  r.interimResults = true;
  r.continuous = true;
  r.maxAlternatives = 1;
  return r;
}
/* eslint-enable @typescript-eslint/no-explicit-any */

interface TtsSettings {
  rate: number;    // 0.5 – 2.0 (default 1.0)
  pitch: number;   // 0.0 – 2.0 (default 1.0)
  volume: number;  // 0.0 – 1.0 (default 1.0)
  voiceURI: string | null; // selected voice URI, null = system default
}

const TTS_STORAGE_KEY = "zeroclaw_tts_settings";

function loadTtsSettings(): TtsSettings {
  try {
    const stored = localStorage.getItem(TTS_STORAGE_KEY);
    if (stored) return { ...{ rate: 1.0, pitch: 1.0, volume: 1.0, voiceURI: null }, ...JSON.parse(stored) };
  } catch { /* ignore */ }
  return { rate: 1.0, pitch: 1.0, volume: 1.0, voiceURI: null };
}

function saveTtsSettings(s: TtsSettings) {
  localStorage.setItem(TTS_STORAGE_KEY, JSON.stringify(s));
}

function speakText(text: string, lang: string, onEnd?: () => void, settings?: TtsSettings) {
  const plain = text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/#{1,6}\s*/g, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/[-*_]{3,}/g, "")
    .trim();
  if (!plain) { onEnd?.(); return; }
  window.speechSynthesis.cancel();
  const u = new SpeechSynthesisUtterance(plain);
  u.lang = lang;
  const s = settings ?? loadTtsSettings();
  u.rate = s.rate;
  u.pitch = s.pitch;
  u.volume = s.volume;
  if (s.voiceURI) {
    const voice = window.speechSynthesis.getVoices().find(v => v.voiceURI === s.voiceURI);
    if (voice) u.voice = voice;
  }
  if (onEnd) { u.onend = onEnd; u.onerror = onEnd; }
  window.speechSynthesis.speak(u);
}

/** Detect BCP-47 tag from text (simplified — covers CJK + Latin top languages). */
function detectLang(text: string): string {
  let ko = 0, ja = 0, zh = 0, latin = 0;
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0;
    if ((cp >= 0xAC00 && cp <= 0xD7AF) || (cp >= 0x1100 && cp <= 0x11FF)) { ko++; continue; }
    if ((cp >= 0x3040 && cp <= 0x309F) || (cp >= 0x30A0 && cp <= 0x30FF)) { ja++; continue; }
    if ((cp >= 0x4E00 && cp <= 0x9FFF)) { zh++; continue; }
    if ((cp >= 0x41 && cp <= 0x5A) || (cp >= 0x61 && cp <= 0x7A)) latin++;
  }
  if (ko > 0) return "ko-KR";
  if (ja > 0) return "ja-JP";
  if (zh > 0) return "zh-CN";
  if (latin > 0) return "en-US";
  return navigator.language || "en-US";
}

interface ChatProps {
  chat: ChatSession | null;
  locale: Locale;
  isConnected: boolean;
  onSendMessage: (content: string, attachments?: AttachmentFile[]) => Promise<void>;
  onRetry: (messages: ChatMessage[]) => void;
  onOpenSettings: () => void;
  onOpenInterpreter?: () => void;
  onNewChat?: () => void;
  onToggleSidebar: () => void;
  sidebarOpen: boolean;
  currentDeviceName?: string;
  currentModel?: string;
}

export function Chat({
  chat,
  locale,
  isConnected,
  onSendMessage,
  onRetry,
  onOpenSettings,
  onOpenInterpreter: _onOpenInterpreter,
  onNewChat,
  onToggleSidebar,
  sidebarOpen,
  currentDeviceName = "",
  currentModel = "",
}: ChatProps) {
  const [input, setInput] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [attachments, setAttachments] = useState<AttachmentFile[]>([]);
  const [docInfoDismissed, setDocInfoDismissed] = useState(false);
  const [ocrConsent, setOcrConsent] = useState<{
    show: boolean;
    pendingFiles: AttachmentFile[];
  }>({ show: false, pendingFiles: [] });

  // ── Free browser-native STT/TTS state ──
  const [listening, setListening] = useState(false);
  const [voiceMode, setVoiceMode] = useState(false);
  const [chatLang, setChatLang] = useState(() => navigator.language || "en-US");
  const recognitionRef = useRef<ReturnType<typeof createSpeechRecognition> | null>(null);
  const voiceModeRef = useRef(false);
  const chatLangRef = useRef(chatLang);
  // Prevents STT from restarting while TTS is playing (echo suppression)
  const isSpeakingRef = useRef(false);
  // TTS voice settings
  const [ttsSettings, setTtsSettings] = useState<TtsSettings>(loadTtsSettings);
  const [showTtsSettings, setShowTtsSettings] = useState(false);
  const [availableVoices, setAvailableVoices] = useState<SpeechSynthesisVoice[]>([]);
  const ttsSettingsRef = useRef(ttsSettings);
  useEffect(() => { ttsSettingsRef.current = ttsSettings; }, [ttsSettings]);
  useEffect(() => { voiceModeRef.current = voiceMode; }, [voiceMode]);
  useEffect(() => { chatLangRef.current = chatLang; }, [chatLang]);

  // Load available TTS voices
  useEffect(() => {
    const loadVoices = () => setAvailableVoices(window.speechSynthesis.getVoices());
    loadVoices();
    window.speechSynthesis.onvoiceschanged = loadVoices;
    return () => { window.speechSynthesis.onvoiceschanged = null; };
  }, []);

  // Cleanup STT + TTS on unmount
  useEffect(() => {
    return () => { recognitionRef.current?.stop(); window.speechSynthesis.cancel(); };
  }, []);

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

  // ── STT: start listening (internal helper) ──
  const startListening = useCallback((lang: string) => {
    // Never start STT while TTS is still playing
    if (isSpeakingRef.current) return;

    const recognition = createSpeechRecognition(lang);
    if (!recognition) return;
    let finalTranscript = "";
    recognition.onresult = (event: { results: SpeechRecognitionResultList }) => {
      // ── Echo suppression: ignore all STT input while TTS is speaking ──
      // Without this, the speaker output feeds back into the microphone,
      // gets transcribed, and auto-sent as a new question → infinite loop.
      if (isSpeakingRef.current) return;

      let interim = "";
      finalTranscript = "";
      for (let i = 0; i < event.results.length; i++) {
        const r = event.results[i];
        if (!r?.[0]) continue;
        if (r.isFinal) finalTranscript += r[0].transcript;
        else interim += r[0].transcript;
      }
      setInput(finalTranscript + interim);
    };
    recognition.onerror = (e: { error?: string }) => {
      if (e.error === "no-speech" || e.error === "aborted") return;
      setListening(false);
      setVoiceMode(false);
    };
    recognition.onend = () => {
      // Don't restart or auto-send while TTS is playing (echo suppression)
      if (isSpeakingRef.current) {
        setListening(false);
        return;
      }
      if (voiceModeRef.current && finalTranscript.trim()) {
        setInput(finalTranscript.trim());
        // Auto-send on next tick
        setTimeout(() => {
          const btn = document.querySelector("[data-voice-send]") as HTMLButtonElement | null;
          btn?.click();
        }, 50);
      } else if (voiceModeRef.current) {
        setTimeout(() => {
          if (voiceModeRef.current && !isSpeakingRef.current) startListening(chatLangRef.current);
        }, 300);
      } else {
        setListening(false);
      }
    };
    recognitionRef.current = recognition;
    recognition.start();
    setListening(true);
  }, []);

  // Toggle voice mode on/off
  const toggleVoiceMode = useCallback(() => {
    if (listening || voiceMode) {
      isSpeakingRef.current = false;
      recognitionRef.current?.stop();
      window.speechSynthesis.cancel();
      setListening(false);
      setVoiceMode(false);
      return;
    }
    setVoiceMode(true);
    startListening(chatLang);
  }, [listening, voiceMode, chatLang, startListening]);

  // ── Auto-TTS: when voice mode is on, read the latest agent response aloud ──
  const prevMsgCountRef = useRef(messages.length);
  useEffect(() => {
    if (!voiceModeRef.current) { prevMsgCountRef.current = messages.length; return; }
    if (messages.length > prevMsgCountRef.current) {
      const last = messages[messages.length - 1];
      if (last && last.role === "assistant") {
        // ── Echo suppression: fully stop STT before TTS starts ──
        // 1. Set speaking flag FIRST (blocks onresult from processing)
        isSpeakingRef.current = true;
        // 2. Stop recognition (async — onresult/onend may still fire)
        recognitionRef.current?.stop();
        recognitionRef.current = null;
        setListening(false);
        // 3. Clear any partially-recognized text that might be echo
        setInput("");
        // 4. Small delay to ensure STT is fully stopped before TTS audio plays
        setTimeout(() => {
          speakText(last.content, chatLangRef.current, () => {
            isSpeakingRef.current = false;
            // Resume listening after TTS finishes
            if (voiceModeRef.current) {
              setTimeout(() => startListening(chatLangRef.current), 200);
            }
          }, ttsSettingsRef.current);
        }, 100);
      }
    }
    prevMsgCountRef.current = messages.length;
  }, [messages.length, startListening]);

  const doSend = useCallback(
    async (text: string, files: AttachmentFile[]) => {
      // Detect language from user input for TTS
      const detected = detectLang(text);
      if (detected !== chatLang) setChatLang(detected);

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
    [onSendMessage, chatLang],
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
        {onNewChat && (
          <button
            className="chat-header-new-chat"
            onClick={onNewChat}
            title={locale === "ko" ? "새 대화" : "New Chat"}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 20h9" />
              <path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z" />
            </svg>
          </button>
        )}
        {(currentDeviceName || currentModel) && (
          <div className="chat-header-info">
            {currentDeviceName && <span className="chat-header-device">{"\uD83D\uDCF1"} {currentDeviceName}</span>}
            {currentModel && <span className="chat-header-model">{"\uD83E\uDD16"} {currentModel}</span>}
          </div>
        )}
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
            data-voice-send
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
            className={`chat-action-btn chat-mic-btn${voiceMode ? " chat-mic-active" : ""}`}
            onClick={toggleVoiceMode}
            disabled={!isConnected}
            title={locale === "ko"
              ? (voiceMode ? "음성 모드 중지" : "음성 모드 (무료 STT/TTS)")
              : (voiceMode ? "Stop voice mode" : "Voice mode (free STT/TTS)")}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              {voiceMode || listening ? (
                <>{/* MicOff icon */}
                  <line x1="1" y1="1" x2="23" y2="23" />
                  <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                  <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
                  <line x1="12" y1="19" x2="12" y2="23" />
                  <line x1="8" y1="23" x2="16" y2="23" />
                </>
              ) : (
                <>{/* Mic icon */}
                  <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                  <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                  <line x1="12" y1="19" x2="12" y2="23" />
                  <line x1="8" y1="23" x2="16" y2="23" />
                </>
              )}
            </svg>
            <span>{voiceMode
              ? (locale === "ko" ? "중지" : "Stop")
              : (locale === "ko" ? "음성" : "Voice")}</span>
          </button>
          <button
            type="button"
            className="chat-action-btn"
            onClick={() => setShowTtsSettings(prev => !prev)}
            title={locale === "ko" ? "음성 설정" : "Voice settings"}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
            <span>{locale === "ko" ? "음성 설정" : "TTS"}</span>
          </button>
        </div>

        {/* TTS Voice Settings Panel */}
        {showTtsSettings && (
          <div className="tts-settings-panel" style={{
            padding: "12px 16px",
            background: "var(--bg-secondary, #1e1e2e)",
            borderRadius: "8px",
            marginTop: "8px",
            display: "flex",
            flexDirection: "column",
            gap: "10px",
            fontSize: "13px",
          }}>
            <div style={{ fontWeight: 600, marginBottom: "2px", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <span>{locale === "ko" ? "TTS 음성 설정" : "TTS Voice Settings"}</span>
              <button
                type="button"
                onClick={() => setShowTtsSettings(false)}
                style={{
                  background: "none",
                  border: "none",
                  color: "var(--text-secondary, #888)",
                  cursor: "pointer",
                  fontSize: "18px",
                  lineHeight: 1,
                  padding: "0 4px",
                }}
                title={locale === "ko" ? "닫기" : "Close"}
              >
                {"\u2715"}
              </button>
            </div>

            {/* Speed / Rate */}
            <label style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span style={{ minWidth: "60px" }}>{locale === "ko" ? "속도" : "Speed"}</span>
              <input type="range" min="0.5" max="2.0" step="0.1"
                value={ttsSettings.rate}
                onChange={e => {
                  const s = { ...ttsSettings, rate: parseFloat(e.target.value) };
                  setTtsSettings(s); saveTtsSettings(s);
                }}
                style={{ flex: 1 }}
              />
              <span style={{ minWidth: "32px", textAlign: "right" }}>{ttsSettings.rate.toFixed(1)}x</span>
            </label>

            {/* Pitch */}
            <label style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span style={{ minWidth: "60px" }}>{locale === "ko" ? "톤" : "Pitch"}</span>
              <input type="range" min="0.0" max="2.0" step="0.1"
                value={ttsSettings.pitch}
                onChange={e => {
                  const s = { ...ttsSettings, pitch: parseFloat(e.target.value) };
                  setTtsSettings(s); saveTtsSettings(s);
                }}
                style={{ flex: 1 }}
              />
              <span style={{ minWidth: "32px", textAlign: "right" }}>{ttsSettings.pitch.toFixed(1)}</span>
            </label>

            {/* Volume */}
            <label style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span style={{ minWidth: "60px" }}>{locale === "ko" ? "볼륨" : "Volume"}</span>
              <input type="range" min="0.0" max="1.0" step="0.1"
                value={ttsSettings.volume}
                onChange={e => {
                  const s = { ...ttsSettings, volume: parseFloat(e.target.value) };
                  setTtsSettings(s); saveTtsSettings(s);
                }}
                style={{ flex: 1 }}
              />
              <span style={{ minWidth: "32px", textAlign: "right" }}>{Math.round(ttsSettings.volume * 100)}%</span>
            </label>

            {/* Voice Selection */}
            <label style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <span style={{ minWidth: "60px" }}>{locale === "ko" ? "목소리" : "Voice"}</span>
              <select
                value={ttsSettings.voiceURI ?? ""}
                onChange={e => {
                  const s = { ...ttsSettings, voiceURI: e.target.value || null };
                  setTtsSettings(s); saveTtsSettings(s);
                }}
                style={{
                  flex: 1,
                  background: "var(--bg-primary, #11111b)",
                  color: "var(--text-primary, #cdd6f4)",
                  border: "1px solid var(--border-color, #45475a)",
                  borderRadius: "4px",
                  padding: "4px 8px",
                  fontSize: "12px",
                }}
              >
                <option value="">{locale === "ko" ? "시스템 기본" : "System default"}</option>
                {availableVoices
                  .filter(v => {
                    const vl = v.lang.toLowerCase();
                    const cl = chatLang.toLowerCase().slice(0, 2);
                    return vl.startsWith(cl);
                  })
                  .map(v => (
                    <option key={v.voiceURI} value={v.voiceURI}>
                      {v.name} ({v.lang})
                    </option>
                  ))}
                {/* Show all voices if none match current language */}
                {availableVoices.filter(v => v.lang.toLowerCase().startsWith(chatLang.toLowerCase().slice(0, 2))).length === 0 &&
                  availableVoices.map(v => (
                    <option key={v.voiceURI} value={v.voiceURI}>
                      {v.name} ({v.lang})
                    </option>
                  ))
                }
              </select>
            </label>

            {/* Test button */}
            <button
              type="button"
              onClick={() => speakText(
                locale === "ko" ? "안녕하세요, 이것은 음성 테스트입니다." : "Hello, this is a voice test.",
                chatLang,
                undefined,
                ttsSettings,
              )}
              style={{
                alignSelf: "flex-end",
                padding: "4px 12px",
                background: "var(--accent-color, #89b4fa)",
                color: "var(--bg-primary, #11111b)",
                border: "none",
                borderRadius: "4px",
                cursor: "pointer",
                fontSize: "12px",
                fontWeight: 600,
              }}
            >
              {locale === "ko" ? "테스트" : "Test"}
            </button>
          </div>
        )}
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
          <div className="message-bubble" style={{ whiteSpace: "pre-wrap" }}>{message.content}</div>
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
