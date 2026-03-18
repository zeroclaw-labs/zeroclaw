import { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import {
  Send, Bot, User, AlertCircle, Copy, Check, SquarePen,
  FileText, FileDown, Volume2, VolumeX, Mic, MicOff,
  Paperclip, FolderOpen, Github,
} from 'lucide-react';
import { marked } from 'marked';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';

// Configure marked for safe rendering
marked.setOptions({
  breaks: true,
  gfm: true,
});

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK =
  'Tool execution completed, but no final response text was returned.';

function makeMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;

  fallbackMessageIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}_${Math.random()
    .toString(36)
    .slice(2, 10)}`;
}

/** Render markdown string to sanitized HTML */
function renderMarkdown(content: string): string {
  try {
    return marked.parse(content, { async: false }) as string;
  } catch {
    // Fallback: escape HTML and preserve whitespace
    return content
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\n/g, '<br>');
  }
}

/**
 * Detect the BCP-47 language tag from text content using Unicode script
 * analysis and common word/pattern heuristics. Returns a lang code suitable
 * for Web Speech API (e.g. 'ko-KR', 'en-US', 'ja-JP').
 */
function detectLanguage(text: string): string {
  // Strip markdown / code blocks so they don't skew detection
  const clean = text
    .replace(/```[\s\S]*?```/g, '')
    .replace(/`[^`]+`/g, '')
    .replace(/https?:\/\/\S+/g, '')
    .replace(/[#*_~>\[\]()!|`]/g, '')
    .trim();

  if (!clean) return navigator.language || 'en-US';

  // Count characters by Unicode script range
  let ko = 0, ja = 0, zh = 0, cyrillic = 0, arabic = 0, thai = 0;
  let devanagari = 0, greek = 0, latin = 0, vietnamese = 0;

  for (const ch of clean) {
    const cp = ch.codePointAt(0) ?? 0;
    // Korean Hangul
    if ((cp >= 0xAC00 && cp <= 0xD7AF) || (cp >= 0x1100 && cp <= 0x11FF) ||
        (cp >= 0x3130 && cp <= 0x318F)) { ko++; continue; }
    // Japanese Hiragana / Katakana
    if ((cp >= 0x3040 && cp <= 0x309F) || (cp >= 0x30A0 && cp <= 0x30FF) ||
        (cp >= 0x31F0 && cp <= 0x31FF)) { ja++; continue; }
    // CJK Unified (shared by zh/ja/ko — counted separately below)
    if (cp >= 0x4E00 && cp <= 0x9FFF) { zh++; continue; }
    // Cyrillic
    if (cp >= 0x0400 && cp <= 0x04FF) { cyrillic++; continue; }
    // Arabic
    if (cp >= 0x0600 && cp <= 0x06FF) { arabic++; continue; }
    // Thai
    if (cp >= 0x0E00 && cp <= 0x0E7F) { thai++; continue; }
    // Devanagari (Hindi etc.)
    if (cp >= 0x0900 && cp <= 0x097F) { devanagari++; continue; }
    // Greek
    if (cp >= 0x0370 && cp <= 0x03FF) { greek++; continue; }
    // Vietnamese diacritics on Latin letters
    if ('àáảãạăắằẳẵặâấầẩẫậèéẻẽẹêếềểễệìíỉĩịòóỏõọôốồổỗộơớờởỡợùúủũụưứừửữựỳýỷỹỵđ'
        .includes(ch.toLowerCase())) { vietnamese++; continue; }
    // Basic Latin
    if (cp >= 0x0041 && cp <= 0x007A) { latin++; continue; }
  }

  // If Japanese kana present, CJK chars are likely kanji → add to ja
  if (ja > 0) ja += zh;
  // If Korean present with CJK, likely hanja
  else if (ko > 0 && zh > 0 && ja === 0) ko += zh;

  // Build scored candidates
  const scores: [string, number][] = [
    ['ko-KR', ko],
    ['ja-JP', ja],
    ['zh-CN', (ja === 0 && ko === 0) ? zh : 0],
    ['ru-RU', cyrillic],
    ['ar-SA', arabic],
    ['th-TH', thai],
    ['hi-IN', devanagari],
    ['el-GR', greek],
    ['vi-VN', vietnamese],
  ];

  // Pick highest non-Latin script
  scores.sort((a, b) => b[1] - a[1]);
  if (scores[0][1] > 0) return scores[0][0];

  // All Latin — use word-level heuristics for European languages
  const lower = clean.toLowerCase();
  // French
  if (/\b(le|la|les|un|une|des|est|sont|avec|dans|pour|que|qui|nous|vous|ils|elles|ce|cette|je|tu|il|elle|ne|pas|mais|ou|et|donc|car)\b/.test(lower))
    return 'fr-FR';
  // Spanish
  if (/\b(el|los|las|una|unos|unas|es|son|está|están|con|por|para|que|como|pero|más|este|esta|yo|tú|él|ella|nosotros)\b/.test(lower))
    return 'es-ES';
  // German
  if (/\b(der|die|das|ein|eine|ist|sind|haben|mit|und|oder|aber|für|von|ich|du|er|sie|wir|ihr|nicht|auch|noch)\b/.test(lower))
    return 'de-DE';
  // Portuguese
  if (/\b(o|os|as|um|uma|uns|umas|é|são|está|estão|com|por|para|que|como|mas|mais|este|esta|eu|tu|ele|ela|nós|vocês)\b/.test(lower))
    return 'pt-BR';
  // Italian
  if (/\b(il|lo|la|gli|le|un|uno|una|è|sono|con|per|che|come|ma|più|questo|questa|io|tu|lui|lei|noi|voi|loro|anche|non)\b/.test(lower))
    return 'it-IT';
  // Indonesian / Malay
  if (/\b(yang|dan|di|ini|itu|dengan|untuk|dari|pada|adalah|tidak|akan|sudah|bisa|kami|mereka|saya|anda)\b/.test(lower))
    return 'id-ID';
  // Turkish
  if (/\b(bir|ve|bu|da|de|için|ile|ben|sen|biz|onlar|değil|var|yok|olan|gibi|ama|çok|daha)\b/.test(lower))
    return 'tr-TR';

  // Default: English
  return 'en-US';
}

/** Create a SpeechRecognition instance (cross-browser) */
function createSpeechRecognition(lang: string) {
  const SpeechRecognition =
    (window as unknown as { SpeechRecognition?: new () => SpeechRecognition }).SpeechRecognition ??
    (window as unknown as { webkitSpeechRecognition?: new () => SpeechRecognition }).webkitSpeechRecognition;
  if (!SpeechRecognition) return null;
  const recognition = new SpeechRecognition();
  recognition.lang = lang;
  recognition.interimResults = false;
  recognition.continuous = true;
  return recognition;
}

/** Convert markdown content to a simple HTML document for export */
function markdownToHtmlDoc(content: string, title = 'Export'): string {
  const body = renderMarkdown(content);
  return `<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>${title}</title>
<style>body{font-family:sans-serif;max-width:800px;margin:2em auto;padding:0 1em;line-height:1.6}
pre{background:#f4f4f4;padding:1em;overflow-x:auto;border-radius:4px}
code{background:#f4f4f4;padding:0.2em 0.4em;border-radius:3px}
blockquote{border-left:4px solid #ddd;margin:0;padding:0 1em;color:#666}</style>
</head><body>${body}</body></html>`;
}

/** Export content as a .doc (HTML-based) file */
function exportToDoc(content: string) {
  const html = markdownToHtmlDoc(content, 'Document Export');
  const blob = new Blob(
    [`<html xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:w="urn:schemas-microsoft-com:office:word" xmlns="http://www.w3.org/TR/REC-html40">
<head><meta charset="utf-8"><title>Export</title></head><body>${renderMarkdown(content)}</body></html>`],
    { type: 'application/msword' }
  );
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `export_${Date.now()}.doc`;
  a.click();
  URL.revokeObjectURL(url);
}

/** Export content as a PDF via print dialog */
function exportToPdf(content: string) {
  const html = markdownToHtmlDoc(content, 'PDF Export');
  const win = window.open('', '_blank');
  if (!win) return;
  win.document.write(html);
  win.document.close();
  // Small delay to allow styles to load
  setTimeout(() => {
    win.print();
    // Close after print dialog is handled
    win.addEventListener('afterprint', () => win.close());
  }, 400);
}

/** TTS: read content aloud using Web Speech API */
function speakContent(content: string, lang: string, onEnd?: () => void) {
  // Strip markdown syntax for cleaner speech
  const plain = content
    .replace(/```[\s\S]*?```/g, ' (code block) ')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/\*([^*]+)\*/g, '$1')
    .replace(/#{1,6}\s*/g, '')
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/!\[([^\]]*)\]\([^)]+\)/g, '$1')
    .replace(/[-*_]{3,}/g, '')
    .trim();

  if (!plain) return;
  window.speechSynthesis.cancel();
  const utterance = new SpeechSynthesisUtterance(plain);
  utterance.lang = lang;
  utterance.rate = 1.0;
  if (onEnd) utterance.onend = onEnd;
  if (onEnd) utterance.onerror = onEnd;
  window.speechSynthesis.speak(utterance);
}

/** Copy button component */
function CopyButton({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for older browsers
      const textarea = document.createElement('textarea');
      textarea.value = content;
      textarea.style.position = 'fixed';
      textarea.style.opacity = '0';
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [content]);

  return (
    <button
      onClick={handleCopy}
      className="inline-flex items-center gap-1 text-xs text-gray-500 hover:text-gray-300 transition-colors px-2 py-1 rounded hover:bg-gray-700/50"
      title="Copy as Markdown"
    >
      {copied ? (
        <>
          <Check className="h-3.5 w-3.5 text-green-400" />
          <span className="text-green-400">Copied</span>
        </>
      ) : (
        <>
          <Copy className="h-3.5 w-3.5" />
          <span>Copy</span>
        </>
      )}
    </button>
  );
}

/** Action buttons for agent messages: Copy, Doc export, PDF export, Listen */
function MessageActions({ content, lang }: { content: string; lang: string }) {
  const [speaking, setSpeaking] = useState(false);

  const handleListen = useCallback(() => {
    if (speaking) {
      window.speechSynthesis.cancel();
      setSpeaking(false);
    } else {
      setSpeaking(true);
      speakContent(content, lang, () => setSpeaking(false));
    }
  }, [content, lang, speaking]);

  // Stop speech if component unmounts
  useEffect(() => {
    return () => {
      if (speaking) window.speechSynthesis.cancel();
    };
  }, [speaking]);

  const btnClass =
    'inline-flex items-center gap-1 text-xs text-gray-500 hover:text-gray-300 transition-colors px-2 py-1 rounded hover:bg-gray-700/50';

  return (
    <div className="flex items-center gap-0.5 flex-wrap">
      <CopyButton content={content} />
      <button onClick={() => exportToDoc(content)} className={btnClass} title="Export to Doc">
        <FileText className="h-3.5 w-3.5" />
        <span>Doc</span>
      </button>
      <button onClick={() => exportToPdf(content)} className={btnClass} title="Export to PDF">
        <FileDown className="h-3.5 w-3.5" />
        <span>PDF</span>
      </button>
      <button onClick={handleListen} className={btnClass} title={speaking ? 'Stop listening' : 'Listen'}>
        {speaking ? (
          <>
            <VolumeX className="h-3.5 w-3.5 text-blue-400" />
            <span className="text-blue-400">Stop</span>
          </>
        ) : (
          <>
            <Volume2 className="h-3.5 w-3.5" />
            <span>Listen</span>
          </>
        )}
      </button>
    </div>
  );
}

/** Rendered markdown message component */
function MarkdownMessage({ content }: { content: string }) {
  const html = useMemo(() => renderMarkdown(content), [content]);

  return (
    <div
      className="markdown-body text-sm break-words"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

export default function AgentChat() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [listening, setListening] = useState(false);
  const [attachMenuOpen, setAttachMenuOpen] = useState(false);
  const [chatLang, setChatLang] = useState(() => navigator.language || 'en-US');

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingContentRef = useRef('');
  const recognitionRef = useRef<ReturnType<typeof createSpeechRecognition> | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };

    ws.onClose = () => {
      setConnected(false);
    };

    ws.onError = () => {
      setError('Connection error. Attempting to reconnect...');
    };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'history': {
          const restored: ChatMessage[] = (msg.messages ?? [])
            .filter((entry) => entry.content?.trim())
            .map((entry) => ({
              id: makeMessageId(),
              role: (entry.role === 'user' ? 'user' : 'agent') as 'user' | 'agent',
              content: entry.content.trim(),
              timestamp: new Date(),
            }));

          setMessages(restored);
          setTyping(false);
          pendingContentRef.current = '';
          break;
        }

        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          break;

        case 'message':
        case 'done': {
          const content = (msg.full_response ?? msg.content ?? pendingContentRef.current ?? '').trim();
          const finalContent = content || EMPTY_DONE_FALLBACK;

          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: finalContent,
              timestamp: new Date(),
            },
          ]);

          pendingContentRef.current = '';
          setTyping(false);
          break;
        }

        case 'tool_call':
          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: `\`[Tool Call]\` **${msg.name ?? 'unknown'}**\n\`\`\`json\n${JSON.stringify(msg.args ?? {}, null, 2)}\n\`\`\``,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'tool_result':
          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: `\`[Tool Result]\`\n\`\`\`\n${msg.output ?? ''}\n\`\`\``,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'error': {
          const errorText = msg.message ?? 'Unknown error';
          const isApiKeyError =
            msg.code === 'missing_api_key' || msg.code === 'provider_auth_error';
          const displayContent = isApiKeyError
            ? `**[API Key Error]** ${errorText}\n\nPlease configure your API key in Settings → Integrations.`
            : `**[Error]** ${errorText}`;

          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: displayContent,
              timestamp: new Date(),
            },
          ]);
          setTyping(false);
          pendingContentRef.current = '';
          break;
        }
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing]);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    // Detect language from user's message and update session language
    const detected = detectLanguage(trimmed);
    setChatLang(detected);

    setMessages((prev) => [
      ...prev,
      {
        id: makeMessageId(),
        role: 'user',
        content: trimmed,
        timestamp: new Date(),
      },
    ]);

    try {
      wsRef.current.sendMessage(trimmed);
      setTyping(true);
      pendingContentRef.current = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }

    setInput('');
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleNewChat = () => {
    if (!wsRef.current) return;
    setMessages([]);
    setTyping(false);
    setError(null);
    pendingContentRef.current = '';
    wsRef.current.resetSession();
    inputRef.current?.focus();
  };

  // --- STT (Speech-to-Text) ---
  const toggleListening = useCallback(() => {
    if (listening) {
      recognitionRef.current?.stop();
      setListening(false);
      return;
    }

    const recognition = createSpeechRecognition(chatLang);
    if (!recognition) {
      setError('Speech recognition is not supported in this browser.');
      return;
    }

    recognition.onresult = (event: { results: SpeechRecognitionResultList }) => {
      let transcript = '';
      for (let i = 0; i < event.results.length; i++) {
        transcript += event.results[i][0].transcript;
      }
      setInput((prev) => (prev ? prev + ' ' + transcript : transcript));
    };

    recognition.onerror = () => {
      setListening(false);
    };

    recognition.onend = () => {
      setListening(false);
    };

    recognitionRef.current = recognition;
    recognition.start();
    setListening(true);
  }, [listening, chatLang]);

  // Cleanup STT on unmount
  useEffect(() => {
    return () => {
      recognitionRef.current?.stop();
    };
  }, []);

  // Close attach menu when clicking outside
  useEffect(() => {
    if (!attachMenuOpen) return;
    const handleClick = () => setAttachMenuOpen(false);
    // Delay to avoid closing immediately on the same click
    const timer = setTimeout(() => document.addEventListener('click', handleClick), 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('click', handleClick);
    };
  }, [attachMenuOpen]);

  // --- File attachment handler ---
  const handleFileAttach = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFileSelected = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    if (!files?.length) return;
    const names = Array.from(files).map((f) => f.name).join(', ');
    setInput((prev) => (prev ? prev + `\n[Attached: ${names}]` : `[Attached: ${names}]`));
    // Reset so same file can be re-selected
    e.target.value = '';
    setAttachMenuOpen(false);
  }, []);

  // --- Local folder connection ---
  const handleFolderConnect = useCallback(async () => {
    setAttachMenuOpen(false);
    try {
      if (!('showDirectoryPicker' in window)) {
        setError('Folder selection is not supported in this browser. Use Chrome or Edge.');
        return;
      }
      const dirHandle = await (window as unknown as { showDirectoryPicker: () => Promise<FileSystemDirectoryHandle> }).showDirectoryPicker();
      setInput((prev) => (prev ? prev + `\n[Local folder: ${dirHandle.name}]` : `[Local folder: ${dirHandle.name}]`));
    } catch {
      // User cancelled - ignore
    }
  }, []);

  // --- GitHub repo connection ---
  const handleGithubConnect = useCallback(() => {
    setAttachMenuOpen(false);
    const repo = prompt('Enter GitHub repository URL or owner/repo:');
    if (repo?.trim()) {
      setInput((prev) => (prev ? prev + `\n[GitHub: ${repo.trim()}]` : `[GitHub: ${repo.trim()}]`));
    }
  }, []);

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Chat header with New Chat button */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-gray-800 bg-gray-900/80">
        <div className="flex items-center gap-2">
          <Bot className="h-5 w-5 text-gray-400" />
          <span className="text-sm font-medium text-gray-300">Agent Chat</span>
        </div>
        <button
          onClick={handleNewChat}
          className="inline-flex items-center gap-1.5 text-sm text-gray-400 hover:text-white px-3 py-1.5 rounded-lg hover:bg-gray-700/60 transition-colors"
          title="New Chat"
        >
          <SquarePen className="h-4 w-4" />
          <span>New Chat</span>
        </button>
      </div>

      {/* Connection status bar */}
      {error && (
        <div className="px-4 py-2 bg-red-900/30 border-b border-red-700 flex items-center gap-2 text-sm text-red-300">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-gray-500">
            <Bot className="h-12 w-12 mb-3 text-gray-600" />
            <p className="text-lg font-medium">ZeroClaw Agent</p>
            <p className="text-sm mt-1">Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`flex items-start gap-3 ${
              msg.role === 'user' ? 'flex-row-reverse' : ''
            }`}
          >
            <div
              className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${
                msg.role === 'user'
                  ? 'bg-blue-600'
                  : 'bg-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <User className="h-4 w-4 text-white" />
              ) : (
                <Bot className="h-4 w-4 text-white" />
              )}
            </div>
            <div
              className={`max-w-[75%] rounded-xl px-4 py-3 ${
                msg.role === 'user'
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-800 text-gray-100 border border-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
              ) : (
                <MarkdownMessage content={msg.content} />
              )}
              <div className={`flex items-center justify-between mt-2 ${
                msg.role === 'user' ? '' : 'border-t border-gray-700/50 pt-1.5'
              }`}>
                <p
                  className={`text-xs ${
                    msg.role === 'user' ? 'text-blue-200' : 'text-gray-500'
                  }`}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
                {msg.role === 'agent' && (
                  <MessageActions content={msg.content} lang={chatLang} />
                )}
              </div>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3">
            <div className="flex-shrink-0 w-8 h-8 rounded-full bg-gray-700 flex items-center justify-center">
              <Bot className="h-4 w-4 text-white" />
            </div>
            <div className="bg-gray-800 border border-gray-700 rounded-xl px-4 py-3">
              <div className="flex items-center gap-1">
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '0ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '150ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '300ms' }} />
              </div>
              <p className="text-xs text-gray-500 mt-1">Typing...</p>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t border-gray-800 bg-gray-900 p-4">
        <div className="flex items-center gap-2 max-w-4xl mx-auto">
          {/* Left: Attachment menu */}
          <div className="relative flex-shrink-0">
            <button
              onClick={() => setAttachMenuOpen((v) => !v)}
              className="p-2.5 rounded-xl text-gray-400 hover:text-white hover:bg-gray-700/60 transition-colors"
              title="Attach file / Connect source"
            >
              <Paperclip className="h-5 w-5" />
            </button>
            {attachMenuOpen && (
              <div className="absolute bottom-full left-0 mb-2 bg-gray-800 border border-gray-700 rounded-xl shadow-xl py-1 min-w-[200px] z-50">
                <button
                  onClick={handleFileAttach}
                  className="w-full flex items-center gap-2.5 px-4 py-2.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
                >
                  <Paperclip className="h-4 w-4" />
                  <span>Attach File</span>
                </button>
                <button
                  onClick={handleFolderConnect}
                  className="w-full flex items-center gap-2.5 px-4 py-2.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
                >
                  <FolderOpen className="h-4 w-4" />
                  <span>Connect Local Folder</span>
                </button>
                <button
                  onClick={handleGithubConnect}
                  className="w-full flex items-center gap-2.5 px-4 py-2.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
                >
                  <Github className="h-4 w-4" />
                  <span>Connect GitHub</span>
                </button>
              </div>
            )}
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={handleFileSelected}
            />
          </div>

          {/* Center: Text input */}
          <div className="flex-1 relative">
            <input
              ref={inputRef}
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={connected ? 'Type a message...' : 'Connecting...'}
              disabled={!connected}
              className="w-full bg-gray-800 border border-gray-700 rounded-xl px-4 py-3 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-50"
            />
          </div>

          {/* Right: Mic (STT) button */}
          <button
            onClick={toggleListening}
            disabled={!connected}
            className={`flex-shrink-0 p-2.5 rounded-xl transition-colors ${
              listening
                ? 'bg-red-600 text-white animate-pulse'
                : 'text-gray-400 hover:text-white hover:bg-gray-700/60'
            } disabled:opacity-50 disabled:cursor-not-allowed`}
            title={listening ? 'Stop recording' : 'Voice input'}
          >
            {listening ? <MicOff className="h-5 w-5" /> : <Mic className="h-5 w-5" />}
          </button>

          {/* Send button */}
          <button
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="flex-shrink-0 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-xl p-3 transition-colors"
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <div className="flex items-center justify-center mt-2 gap-2">
          <span
            className={`inline-block h-2 w-2 rounded-full ${
              connected ? 'bg-green-500' : 'bg-red-500'
            }`}
          />
          <span className="text-xs text-gray-500">
            {connected ? 'Connected' : 'Disconnected'}
          </span>
        </div>
      </div>
    </div>
  );
}
