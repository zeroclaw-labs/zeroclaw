'use client';

import { useState, useEffect, useRef } from 'react';
import { useRouter } from 'next/navigation';
import Link from 'next/link';
import { Send, Bot, User, AlertCircle, ArrowLeft, Settings, LogOut, Mic, MicOff, Monitor, ChevronDown } from 'lucide-react';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';
import { getToken, clearToken, isAuthenticated } from '@/lib/auth';
import { getMyDevices, type UserDevice } from '@/lib/gateway-api';

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK = 'Tool execution completed, but no final response text was returned.';

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

function speakText(text: string, lang: string, onEnd?: () => void) {
  const plain = text
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/#{1,6}\s*/g, '')
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .trim();
  if (!plain) { onEnd?.(); return; }
  window.speechSynthesis.cancel();
  const u = new SpeechSynthesisUtterance(plain);
  u.lang = lang;
  if (onEnd) { u.onend = onEnd; u.onerror = onEnd; }
  window.speechSynthesis.speak(u);
}

function detectLang(text: string): string {
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0;
    if ((cp >= 0xAC00 && cp <= 0xD7AF) || (cp >= 0x1100 && cp <= 0x11FF)) return 'ko-KR';
    if ((cp >= 0x3040 && cp <= 0x309F) || (cp >= 0x30A0 && cp <= 0x30FF)) return 'ja-JP';
    if (cp >= 0x4E00 && cp <= 0x9FFF) return 'zh-CN';
  }
  return navigator.language || 'en-US';
}

function makeMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;
  fallbackMessageIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

export default function ChatPage() {
  const router = useRouter();
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [authChecked, setAuthChecked] = useState(false);

  const [listening, setListening] = useState(false);
  const [voiceMode, setVoiceMode] = useState(false);
  const [chatLang, setChatLang] = useState('en-US');
  const recognitionRef = useRef<ReturnType<typeof createSpeechRecognition> | null>(null);
  const voiceModeRef = useRef(false);
  const chatLangRef = useRef(chatLang);

  // Device selection state
  const [devices, setDevices] = useState<UserDevice[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(null);
  const [showDeviceMenu, setShowDeviceMenu] = useState(false);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingContentRef = useRef('');

  // Auth gate: redirect to /auth if not authenticated
  useEffect(() => {
    if (!isAuthenticated()) {
      router.replace('/auth?redirect=/chat');
      return;
    }
    setAuthChecked(true);
  }, [router]);

  // Fetch user devices
  useEffect(() => {
    if (!authChecked) return;
    let cancelled = false;
    const fetchDevices = async () => {
      const devs = await getMyDevices();
      if (!cancelled) setDevices(devs);
    };
    fetchDevices();
    const interval = setInterval(fetchDevices, 10000);
    return () => { cancelled = true; clearInterval(interval); };
  }, [authChecked]);

  // Close device menu on outside click
  useEffect(() => {
    if (!showDeviceMenu) return;
    const handler = () => setShowDeviceMenu(false);
    document.addEventListener('click', handler);
    return () => document.removeEventListener('click', handler);
  }, [showDeviceMenu]);

  // Keep refs in sync
  useEffect(() => { voiceModeRef.current = voiceMode; }, [voiceMode]);
  useEffect(() => { chatLangRef.current = chatLang; }, [chatLang]);
  useEffect(() => { return () => { recognitionRef.current?.stop(); window.speechSynthesis.cancel(); }; }, []);

  // Set browser language on mount
  useEffect(() => { setChatLang(navigator.language || 'en-US'); }, []);

  const startListening = (lang: string) => {
    const recognition = createSpeechRecognition(lang);
    if (!recognition) return;
    let finalTranscript = '';
    recognition.onresult = (event: { results: SpeechRecognitionResultList }) => {
      let interim = '';
      finalTranscript = '';
      for (let i = 0; i < event.results.length; i++) {
        const r = event.results[i];
        if (!r?.[0]) continue;
        if (r.isFinal) finalTranscript += r[0].transcript;
        else interim += r[0].transcript;
      }
      setInput(finalTranscript + interim);
    };
    recognition.onerror = (e: { error?: string }) => {
      if (e.error === 'no-speech' || e.error === 'aborted') return;
      setListening(false); setVoiceMode(false);
    };
    recognition.onend = () => {
      if (voiceModeRef.current && finalTranscript.trim()) {
        setInput(finalTranscript.trim());
        setTimeout(() => {
          const btn = document.querySelector('[data-voice-send]') as HTMLButtonElement | null;
          btn?.click();
        }, 50);
      } else if (voiceModeRef.current) {
        setTimeout(() => { if (voiceModeRef.current) startListening(chatLangRef.current); }, 300);
      } else {
        setListening(false);
      }
    };
    recognitionRef.current = recognition;
    recognition.start();
    setListening(true);
  };

  const toggleVoiceMode = () => {
    if (listening || voiceMode) {
      recognitionRef.current?.stop();
      window.speechSynthesis.cancel();
      setListening(false); setVoiceMode(false);
      return;
    }
    setVoiceMode(true);
    startListening(chatLang);
  };

  // WebSocket connection (only after auth is confirmed)
  useEffect(() => {
    if (!authChecked) return;

    const ws = new WebSocketClient();

    ws.onOpen = () => { setConnected(true); setError(null); };
    ws.onClose = () => { setConnected(false); };
    ws.onError = () => { setError('Connection error. Attempting to reconnect...'); };

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
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: finalContent, timestamp: new Date() }]);
          pendingContentRef.current = '';
          setTyping(false);
          // Auto-TTS in voice mode
          if (voiceModeRef.current && finalContent !== EMPTY_DONE_FALLBACK) {
            recognitionRef.current?.stop();
            speakText(finalContent, chatLangRef.current, () => {
              if (voiceModeRef.current) startListening(chatLangRef.current);
            });
          }
          break;
        }
        case 'tool_call':
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: `[Tool Call] ${msg.name ?? 'unknown'}(${JSON.stringify(msg.args ?? {})})`, timestamp: new Date() }]);
          break;
        case 'tool_result':
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: `[Tool Result] ${msg.output ?? ''}`, timestamp: new Date() }]);
          break;
        case 'error':
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: `[Error] ${msg.message ?? 'Unknown error'}`, timestamp: new Date() }]);
          setTyping(false);
          pendingContentRef.current = '';
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;
    return () => { ws.disconnect(); };
  }, [authChecked]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing]);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;
    const detected = detectLang(trimmed);
    if (detected !== chatLang) setChatLang(detected);
    setMessages((prev) => [...prev, { id: makeMessageId(), role: 'user', content: trimmed, timestamp: new Date() }]);
    try {
      const extra: Record<string, string> = {};
      if (selectedDeviceId) extra.target_device_id = selectedDeviceId;
      wsRef.current.sendMessage(trimmed, extra);
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

  const handleLogout = () => {
    clearToken();
    wsRef.current?.disconnect();
    router.replace('/auth');
  };

  // Show nothing while checking auth
  if (!authChecked) {
    return (
      <div className="flex items-center justify-center h-screen bg-dark-950">
        <div className="flex flex-col items-center gap-3">
          <div className="h-8 w-8 border-2 border-primary-500 border-t-transparent rounded-full animate-spin" />
          <p className="text-sm text-dark-400">Loading...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-screen bg-dark-950">
      {/* Chat header bar */}
      <div className="flex items-center justify-between border-b border-dark-800/50 bg-dark-900/80 backdrop-blur-xl px-4 py-3 flex-shrink-0">
        <div className="flex items-center gap-3">
          <Link
            href="/"
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all"
            aria-label="Back to home"
          >
            <ArrowLeft className="h-4 w-4" />
          </Link>
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20">
            <span className="text-sm font-bold text-primary-400">M</span>
          </div>
          <div>
            <h2 className="text-sm font-semibold text-dark-100">MoA Agent</h2>
            <div className="flex items-center gap-1.5">
              <div className={`h-1.5 w-1.5 rounded-full ${connected ? 'bg-green-400' : 'bg-red-400'}`} />
              <span className="text-xs text-dark-400">
                {connected ? 'Connected' : 'Disconnected'}
              </span>
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {/* Device selector */}
          {devices.length > 0 && (
            <div className="relative" onClick={(e) => e.stopPropagation()}>
              <button
                onClick={() => setShowDeviceMenu((v) => !v)}
                className="flex items-center gap-1.5 h-8 px-2.5 rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all text-xs border border-dark-700/50"
              >
                <Monitor className="h-3.5 w-3.5" />
                <span className="max-w-[100px] truncate">
                  {selectedDeviceId
                    ? devices.find((d) => d.device_id === selectedDeviceId)?.device_name ?? 'Device'
                    : 'Auto'}
                </span>
                <ChevronDown className="h-3 w-3" />
              </button>
              {showDeviceMenu && (
                <div className="absolute right-0 top-full mt-1 w-56 bg-dark-800 border border-dark-700 rounded-xl shadow-2xl z-50 py-1 overflow-hidden">
                  <button
                    onClick={() => { setSelectedDeviceId(null); setShowDeviceMenu(false); }}
                    className={`w-full text-left px-3 py-2 text-xs flex items-center gap-2 hover:bg-dark-700 transition-colors ${!selectedDeviceId ? 'text-primary-400 bg-primary-500/10' : 'text-dark-300'}`}
                  >
                    <span className="w-1.5 h-1.5 rounded-full bg-primary-400" />
                    Auto (best available)
                  </button>
                  {devices.map((dev) => (
                    <button
                      key={dev.device_id}
                      onClick={() => { setSelectedDeviceId(dev.device_id); setShowDeviceMenu(false); }}
                      className={`w-full text-left px-3 py-2 text-xs flex items-center gap-2 hover:bg-dark-700 transition-colors ${selectedDeviceId === dev.device_id ? 'text-primary-400 bg-primary-500/10' : 'text-dark-300'}`}
                    >
                      <span className={`w-1.5 h-1.5 rounded-full ${dev.is_online ? 'bg-green-400' : 'bg-dark-500'}`} />
                      <span className="flex-1 truncate">{dev.device_name}</span>
                      {dev.platform && <span className="text-dark-500 text-[10px]">{dev.platform}</span>}
                      {!dev.is_online && <span className="text-dark-500 text-[10px]">offline</span>}
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}
          <Link
            href="/workspace/dashboard"
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all"
            aria-label="Workspace"
          >
            <Settings className="h-4 w-4" />
          </Link>
          <button
            onClick={handleLogout}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-red-400 transition-all"
            aria-label="Logout"
          >
            <LogOut className="h-4 w-4" />
          </button>
        </div>
      </div>

      {/* Connection error banner */}
      {error && (
        <div className="px-4 py-2 bg-red-900/30 border-b border-red-700 flex items-center gap-2 text-sm text-red-300 flex-shrink-0">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />{error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4 custom-scrollbar">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-dark-500">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20 mb-4">
              <Bot className="h-8 w-8 text-primary-400" />
            </div>
            <p className="text-lg font-semibold text-dark-200">MoA Agent</p>
            <p className="text-sm mt-1 text-dark-400">Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg) => (
          <div key={msg.id} className={`flex items-start gap-3 chat-bubble-enter ${msg.role === 'user' ? 'flex-row-reverse' : ''}`}>
            <div className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${msg.role === 'user' ? 'bg-primary-500' : 'bg-dark-700 border border-dark-600'}`}>
              {msg.role === 'user' ? <User className="h-4 w-4 text-white" /> : <Bot className="h-4 w-4 text-primary-400" />}
            </div>
            <div className={`max-w-[75%] rounded-2xl px-4 py-3 ${msg.role === 'user' ? 'bg-primary-500 text-white rounded-br-md' : 'bg-dark-800 text-dark-100 border border-dark-700/50 rounded-bl-md'}`}>
              <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
              <p className={`text-xs mt-1 ${msg.role === 'user' ? 'text-primary-200/60' : 'text-dark-500'}`}>
                {msg.timestamp.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
              </p>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3 chat-bubble-enter">
            <div className="flex-shrink-0 w-8 h-8 rounded-full bg-dark-700 border border-dark-600 flex items-center justify-center">
              <Bot className="h-4 w-4 text-primary-400" />
            </div>
            <div className="bg-dark-800 border border-dark-700/50 rounded-2xl rounded-bl-md px-5 py-4">
              <div className="typing-indicator flex gap-1.5">
                <span></span>
                <span></span>
                <span></span>
              </div>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t border-dark-800/50 bg-dark-900/80 backdrop-blur-xl p-4 flex-shrink-0">
        <div className="flex items-center gap-3 max-w-4xl mx-auto">
          <div className="flex-1 relative">
            <input
              ref={inputRef}
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={connected ? 'Type a message...' : 'Connecting...'}
              disabled={!connected}
              className="w-full bg-dark-800/50 border border-dark-700 rounded-xl px-4 py-3 text-sm text-dark-100 placeholder-dark-500 focus:outline-none focus:ring-2 focus:ring-primary-500/50 focus:border-primary-500/50 disabled:opacity-50 transition-all"
            />
          </div>
          <button
            onClick={toggleVoiceMode}
            disabled={!connected}
            className={`flex-shrink-0 rounded-xl p-3 transition-all active:scale-95 ${
              voiceMode
                ? 'bg-red-500 hover:bg-red-600 text-white animate-pulse'
                : 'bg-dark-800 hover:bg-dark-700 text-dark-400 hover:text-white border border-dark-700'
            } disabled:opacity-50`}
            title={voiceMode ? 'Stop voice mode' : 'Voice mode (free STT/TTS)'}
          >
            {voiceMode || listening ? <MicOff className="h-5 w-5" /> : <Mic className="h-5 w-5" />}
          </button>
          <button
            data-voice-send
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="flex-shrink-0 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-xl p-3 transition-all active:scale-95"
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <div className="mt-2 flex items-center justify-center gap-3">
          <p className="text-[10px] text-dark-600">
            Shift+Enter for newline | Enter to send
          </p>
          {voiceMode && (
            <span className="text-[10px] text-red-400 animate-pulse">
              Voice mode ({chatLang.split('-')[0]?.toUpperCase()})
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
