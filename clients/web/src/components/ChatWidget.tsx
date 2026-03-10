"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import {
  getClient,
  ZeroClawClient,
  renderMarkdown,
  type ChatMessage,
} from "@/lib/api";

const STORAGE_KEY_MODEL = "zeroclaw_selected_model";
const DEFAULT_MODEL = "google/gemini-3.1-pro-preview";

const LLM_MODELS = [
  { id: "google/gemini-3.1-pro-preview", label: "Gemini 3.1 Pro" },
  { id: "google/gemini-3.0-pro-preview", label: "Gemini 3.0 Pro" },
  { id: "google/gemini-3.0-flash-preview", label: "Gemini 3.0 Flash" },
  { id: "google/gemini-2.5-flash-preview", label: "Gemini 2.5 Flash" },
  { id: "openai/gpt-4o", label: "GPT-4o" },
  { id: "openai/gpt-4o-mini", label: "GPT-4o Mini" },
  { id: "anthropic/claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
  { id: "anthropic/claude-haiku-3.5", label: "Claude Haiku 3.5" },
] as const;

interface ChatWidgetProps {
  className?: string;
  compact?: boolean;
}

export default function ChatWidget({
  className = "",
  compact = false,
}: ChatWidgetProps) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isConnected, setIsConnected] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [serverUrl, setServerUrl] = useState("");
  const [pairingCode, setPairingCode] = useState("");
  const [pairUsername, setPairUsername] = useState("");
  const [pairPassword, setPairPassword] = useState("");
  const [isPairing, setIsPairing] = useState(false);
  const [selectedModel, setSelectedModel] = useState(DEFAULT_MODEL);
  const [showModelDropdown, setShowModelDropdown] = useState(false);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const clientRef = useRef<ZeroClawClient | null>(null);
  const modelDropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const client = getClient();
    clientRef.current = client;
    setServerUrl(client.getServerUrl());
    setIsConnected(client.isConnected());
    setMessages(ZeroClawClient.loadMessages());
    // Restore saved model selection
    const savedModel = localStorage.getItem(STORAGE_KEY_MODEL);
    if (savedModel && LLM_MODELS.some((m) => m.id === savedModel)) {
      setSelectedModel(savedModel);
    }
  }, []);

  // Close model dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      if (
        modelDropdownRef.current &&
        !modelDropdownRef.current.contains(e.target as Node)
      ) {
        setShowModelDropdown(false);
      }
    }
    if (showModelDropdown) {
      document.addEventListener("mousedown", handleClickOutside);
      return () => document.removeEventListener("mousedown", handleClickOutside);
    }
  }, [showModelDropdown]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const handlePair = useCallback(async () => {
    if (!pairingCode.trim() || !clientRef.current) return;
    setIsPairing(true);
    setError(null);

    try {
      clientRef.current.setServerUrl(serverUrl);
      const result = await clientRef.current.pair(
        pairingCode.trim(),
        pairUsername.trim() || undefined,
        pairPassword || undefined,
      );
      if (result.paired) {
        setIsConnected(true);
        setShowSettings(false);
        setPairingCode("");
        setPairUsername("");
        setPairPassword("");
        setError(null);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Pairing failed");
    } finally {
      setIsPairing(false);
    }
  }, [pairingCode, pairUsername, pairPassword, serverUrl]);

  const handleDisconnect = useCallback(() => {
    clientRef.current?.disconnect();
    setIsConnected(false);
    setShowSettings(false);
  }, []);

  const handleClearHistory = useCallback(() => {
    ZeroClawClient.clearMessages();
    setMessages([]);
  }, []);

  const handleModelSelect = useCallback((modelId: string) => {
    setSelectedModel(modelId);
    setShowModelDropdown(false);
    if (typeof window !== "undefined") {
      localStorage.setItem(STORAGE_KEY_MODEL, modelId);
    }
  }, []);

  const handleSend = useCallback(async () => {
    const trimmed = input.trim();
    if (!trimmed || isLoading || !clientRef.current) return;

    const userMsg = ZeroClawClient.createMessage("user", trimmed);
    const updatedMessages = [...messages, userMsg];
    setMessages(updatedMessages);
    ZeroClawClient.saveMessages(updatedMessages);
    setInput("");
    setIsLoading(true);
    setError(null);

    try {
      const response = await clientRef.current.chat(trimmed, selectedModel);
      const assistantMsg = ZeroClawClient.createMessage(
        "assistant",
        response.response,
        response.model,
      );
      const finalMessages = [...updatedMessages, assistantMsg];
      setMessages(finalMessages);
      ZeroClawClient.saveMessages(finalMessages);
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : "Failed to send message";
      setError(errMsg);
      if (errMsg.includes("re-pair")) {
        setIsConnected(false);
      }
    } finally {
      setIsLoading(false);
    }
  }, [input, isLoading, messages, selectedModel]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  const handleInputChange = useCallback(
    (e: React.ChangeEvent<HTMLTextAreaElement>) => {
      setInput(e.target.value);
      // Auto-resize textarea
      const target = e.target;
      target.style.height = "auto";
      target.style.height = Math.min(target.scrollHeight, 200) + "px";
    },
    [],
  );

  // Connection setup view
  if (!isConnected && !showSettings) {
    return (
      <div className={`flex flex-col items-center justify-center p-8 ${className}`}>
        <div className="w-full max-w-md space-y-6 text-center">
          <div className="flex h-16 w-16 mx-auto items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20">
            <svg
              className="h-8 w-8 text-primary-400"
              fill="none"
              viewBox="0 0 24 24"
              strokeWidth={1.5}
              stroke="currentColor"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M13.19 8.688a4.5 4.5 0 011.242 7.244l-4.5 4.5a4.5 4.5 0 01-6.364-6.364l1.757-1.757m9.86-2.556a4.5 4.5 0 00-1.242-7.244l4.5-4.5a4.5 4.5 0 016.364 6.364l-1.757 1.757"
              />
            </svg>
          </div>
          <div>
            <h2 className="text-xl font-semibold text-dark-50">
              {"\uC11C\uBC84 \uC5F0\uACB0"} Connect to Server
            </h2>
            <p className="mt-2 text-sm text-dark-400">
              {"\uCC44\uD305\uC744 \uC2DC\uC791\uD558\uB824\uBA74 ZeroClaw \uC11C\uBC84\uC5D0 \uC5F0\uACB0\uD558\uC138\uC694."}
            </p>
            <p className="mt-1 text-xs text-dark-500">
              Connect to your ZeroClaw server to start chatting.
            </p>
          </div>

          <div className="space-y-4 text-left">
            <div>
              <label className="block text-sm font-medium text-dark-300 mb-1.5">
                {"\uC11C\uBC84 URL"} Server URL
              </label>
              <input
                type="url"
                value={serverUrl}
                onChange={(e) => setServerUrl(e.target.value)}
                placeholder="https://your-server.railway.app"
                className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-dark-300 mb-1.5">
                {"\uC544\uC774\uB514"} Username
              </label>
              <input
                type="text"
                value={pairUsername}
                onChange={(e) => setPairUsername(e.target.value)}
                placeholder="Enter username"
                autoComplete="username"
                className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-dark-300 mb-1.5">
                {"\uBE44\uBC00\uBC88\uD638"} Password
              </label>
              <input
                type="password"
                value={pairPassword}
                onChange={(e) => setPairPassword(e.target.value)}
                placeholder="Enter password"
                autoComplete="current-password"
                className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-dark-300 mb-1.5">
                {"\uD398\uC5B4\uB9C1 \uCF54\uB4DC"} Pairing Code
              </label>
              <input
                type="text"
                value={pairingCode}
                onChange={(e) => setPairingCode(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handlePair()}
                placeholder="Enter pairing code from server"
                className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
              />
            </div>
          </div>

          {error && (
            <div className="rounded-lg bg-red-500/10 border border-red-500/20 px-4 py-3 text-sm text-red-400">
              {error}
            </div>
          )}

          <button
            onClick={handlePair}
            disabled={isPairing || !pairingCode.trim()}
            className="btn-primary w-full disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isPairing ? (
              <span className="flex items-center gap-2">
                <svg
                  className="h-4 w-4 animate-spin"
                  fill="none"
                  viewBox="0 0 24 24"
                >
                  <circle
                    className="opacity-25"
                    cx="12"
                    cy="12"
                    r="10"
                    stroke="currentColor"
                    strokeWidth="4"
                  />
                  <path
                    className="opacity-75"
                    fill="currentColor"
                    d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                  />
                </svg>
                {"\uC5F0\uACB0 \uC911..."} Connecting...
              </span>
            ) : (
              <>{"\uC5F0\uACB0\uD558\uAE30"} Connect</>
            )}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className={`flex flex-col ${className}`}>
      {/* Settings overlay */}
      {showSettings && (
        <div className="absolute inset-0 z-10 flex items-center justify-center bg-dark-950/80 backdrop-blur-sm animate-fade-in">
          <div className="w-full max-w-md rounded-2xl border border-dark-700 bg-dark-900 p-6 shadow-2xl mx-4">
            <div className="flex items-center justify-between mb-6">
              <h3 className="text-lg font-semibold text-dark-50">
                {"\uC124\uC815"} Settings
              </h3>
              <button
                onClick={() => setShowSettings(false)}
                className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all"
              >
                <svg
                  className="h-5 w-5"
                  fill="none"
                  viewBox="0 0 24 24"
                  strokeWidth={2}
                  stroke="currentColor"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    d="M6 18L18 6M6 6l12 12"
                  />
                </svg>
              </button>
            </div>

            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-dark-300 mb-1.5">
                  {"\uC11C\uBC84 URL"} Server URL
                </label>
                <input
                  type="url"
                  value={serverUrl}
                  onChange={(e) => setServerUrl(e.target.value)}
                  className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
                />
              </div>

              <div>
                <label className="block text-sm font-medium text-dark-300 mb-1.5">
                  {"\uD1A0\uD070"} Token
                </label>
                <div className="flex items-center gap-2">
                  <div className="flex-1 rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-400 font-mono">
                    {clientRef.current?.getMaskedToken() || "Not connected"}
                  </div>
                </div>
              </div>

              <div>
                <label className="block text-sm font-medium text-dark-300 mb-1.5">
                  {"\uC0C8 \uD398\uC5B4\uB9C1 \uCF54\uB4DC"} New Pairing Code
                </label>
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={pairingCode}
                    onChange={(e) => setPairingCode(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && handlePair()}
                    placeholder="Enter new pairing code"
                    className="flex-1 rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
                  />
                  <button
                    onClick={handlePair}
                    disabled={isPairing || !pairingCode.trim()}
                    className="btn-primary px-4 disabled:opacity-50"
                  >
                    {isPairing ? "..." : "\uC5F0\uACB0"}
                  </button>
                </div>
              </div>

              {error && (
                <div className="rounded-lg bg-red-500/10 border border-red-500/20 px-4 py-3 text-sm text-red-400">
                  {error}
                </div>
              )}

              <div className="flex gap-2 pt-2">
                <button
                  onClick={handleClearHistory}
                  className="btn-secondary flex-1 text-xs"
                >
                  {"\uB300\uD654 \uCD08\uAE30\uD654"} Clear History
                </button>
                <button
                  onClick={handleDisconnect}
                  className="flex-1 rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-2.5 text-xs font-semibold text-red-400 hover:bg-red-500/20 transition-all"
                >
                  {"\uC5F0\uACB0 \uD574\uC81C"} Disconnect
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Chat header */}
      <div className="flex items-center justify-between border-b border-dark-800/50 px-4 py-3">
        <div className="flex items-center gap-3">
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20">
            <span className="text-sm font-bold text-primary-400">Z</span>
          </div>
          <div>
            <h2 className="text-sm font-semibold text-dark-100">ZeroClaw Chat</h2>
            <div className="flex items-center gap-1.5">
              <div
                className={`h-1.5 w-1.5 rounded-full ${
                  isConnected ? "bg-green-400" : "bg-dark-500"
                }`}
              />
              <span className="text-xs text-dark-400">
                {isConnected ? "\uC5F0\uACB0\uB428 Connected" : "\uC5F0\uACB0 \uC548 \uB428 Disconnected"}
              </span>
            </div>
          </div>
        </div>

        <div className="flex items-center gap-2">
          {/* LLM Model selector */}
          <div className="relative" ref={modelDropdownRef}>
            <button
              onClick={() => setShowModelDropdown(!showModelDropdown)}
              className="flex items-center gap-1.5 rounded-lg border border-dark-700 bg-dark-800/50 px-3 py-1.5 text-xs text-dark-300 hover:border-primary-500/50 hover:text-dark-100 transition-all"
              aria-label="Select LLM model"
            >
              <svg
                className="h-3.5 w-3.5 text-primary-400"
                fill="none"
                viewBox="0 0 24 24"
                strokeWidth={1.5}
                stroke="currentColor"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M9.813 15.904L9 18.75l-.813-2.846a4.5 4.5 0 00-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 003.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 003.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 00-3.09 3.09zM18.259 8.715L18 9.75l-.259-1.035a3.375 3.375 0 00-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 002.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 002.455 2.456L21.75 6l-1.036.259a3.375 3.375 0 00-2.455 2.456z"
                />
              </svg>
              <span className="max-w-[120px] truncate">
                {LLM_MODELS.find((m) => m.id === selectedModel)?.label || selectedModel}
              </span>
              <svg
                className={`h-3 w-3 transition-transform ${showModelDropdown ? "rotate-180" : ""}`}
                fill="none"
                viewBox="0 0 24 24"
                strokeWidth={2}
                stroke="currentColor"
              >
                <path strokeLinecap="round" strokeLinejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" />
              </svg>
            </button>

            {showModelDropdown && (
              <div className="absolute right-0 top-full mt-1 z-20 w-64 rounded-xl border border-dark-700 bg-dark-900 shadow-2xl animate-fade-in overflow-hidden">
                <div className="px-3 py-2 border-b border-dark-800">
                  <p className="text-[10px] font-medium text-dark-500 uppercase tracking-wider">
                    LLM {"\uBAA8\uB378 \uC120\uD0DD"} Model Selection
                  </p>
                </div>
                <div className="py-1 max-h-64 overflow-y-auto custom-scrollbar">
                  {LLM_MODELS.map((model) => (
                    <button
                      key={model.id}
                      onClick={() => handleModelSelect(model.id)}
                      className={`w-full flex items-center gap-2 px-3 py-2 text-left text-sm transition-all ${
                        selectedModel === model.id
                          ? "bg-primary-500/10 text-primary-300"
                          : "text-dark-300 hover:bg-dark-800 hover:text-dark-100"
                      }`}
                    >
                      <div
                        className={`h-1.5 w-1.5 rounded-full flex-shrink-0 ${
                          selectedModel === model.id
                            ? "bg-primary-400"
                            : "bg-dark-600"
                        }`}
                      />
                      <div className="flex-1 min-w-0">
                        <span className="block truncate font-medium text-xs">
                          {model.label}
                        </span>
                        <span className="block truncate text-[10px] text-dark-500 font-mono">
                          {model.id}
                        </span>
                      </div>
                      {selectedModel === model.id && (
                        <svg
                          className="h-3.5 w-3.5 text-primary-400 flex-shrink-0"
                          fill="none"
                          viewBox="0 0 24 24"
                          strokeWidth={2.5}
                          stroke="currentColor"
                        >
                          <path strokeLinecap="round" strokeLinejoin="round" d="M4.5 12.75l6 6 9-13.5" />
                        </svg>
                      )}
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>

          {/* Settings button */}
          <button
            onClick={() => setShowSettings(!showSettings)}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all"
            aria-label="Settings"
          >
            <svg
              className="h-4.5 w-4.5"
              fill="none"
              viewBox="0 0 24 24"
              strokeWidth={1.5}
              stroke="currentColor"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.325.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.241-.438.613-.43.992a7.723 7.723 0 010 .255c-.008.378.137.75.43.991l1.004.827c.424.35.534.955.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.47 6.47 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.281c-.09.543-.56.94-1.11.94h-2.594c-.55 0-1.019-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.991a6.932 6.932 0 010-.255c.007-.38-.138-.751-.43-.992l-1.004-.827a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.086.22-.128.332-.183.582-.495.644-.869l.214-1.28z"
              />
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M15 12a3 3 0 11-6 0 3 3 0 016 0z"
              />
            </svg>
          </button>
        </div>
      </div>

      {/* Messages */}
      <div
        className={`flex-1 overflow-y-auto custom-scrollbar px-4 py-4 space-y-4 ${
          compact ? "max-h-96" : ""
        }`}
      >
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-center py-12">
            <div className="flex h-14 w-14 items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20 mb-4">
              <svg
                className="h-7 w-7 text-primary-400"
                fill="none"
                viewBox="0 0 24 24"
                strokeWidth={1.5}
                stroke="currentColor"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M8.625 12a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0H8.25m4.125 0a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0H12m4.125 0a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0h-.375M21 12c0 4.556-4.03 8.25-9 8.25a9.764 9.764 0 01-2.555-.337A5.972 5.972 0 015.41 20.97a5.969 5.969 0 01-.474-.065 4.48 4.48 0 00.978-2.025c.09-.457-.133-.901-.467-1.226C3.93 16.178 3 14.189 3 12c0-4.556 4.03-8.25 9-8.25s9 3.694 9 8.25z"
                />
              </svg>
            </div>
            <h3 className="text-base font-semibold text-dark-200 mb-1">
              {"\uB300\uD654\uB97C \uC2DC\uC791\uD558\uC138\uC694"}
            </h3>
            <p className="text-sm text-dark-400 max-w-xs">
              Start a conversation with your AI agent. Type a message below.
            </p>
          </div>
        )}

        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`flex chat-bubble-enter ${
              msg.role === "user" ? "justify-end" : "justify-start"
            }`}
          >
            <div
              className={`max-w-[85%] rounded-2xl px-4 py-3 ${
                msg.role === "user"
                  ? "bg-primary-500 text-white rounded-br-md"
                  : "bg-dark-800 border border-dark-700/50 text-dark-100 rounded-bl-md"
              }`}
            >
              {msg.role === "assistant" ? (
                <div
                  className="markdown-content text-sm leading-relaxed"
                  dangerouslySetInnerHTML={{
                    __html: renderMarkdown(msg.content),
                  }}
                />
              ) : (
                <p className="text-sm leading-relaxed whitespace-pre-wrap">
                  {msg.content}
                </p>
              )}
              <div
                className={`flex items-center gap-2 mt-1.5 ${
                  msg.role === "user" ? "justify-end" : "justify-start"
                }`}
              >
                {msg.model && (
                  <span className="text-[10px] text-dark-400 font-mono">
                    {msg.model}
                  </span>
                )}
                <span
                  className={`text-[10px] ${
                    msg.role === "user"
                      ? "text-primary-200/60"
                      : "text-dark-500"
                  }`}
                >
                  {new Date(msg.timestamp).toLocaleTimeString([], {
                    hour: "2-digit",
                    minute: "2-digit",
                  })}
                </span>
              </div>
            </div>
          </div>
        ))}

        {/* Loading indicator */}
        {isLoading && (
          <div className="flex justify-start chat-bubble-enter">
            <div className="rounded-2xl rounded-bl-md bg-dark-800 border border-dark-700/50 px-5 py-4">
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

      {/* Error banner */}
      {error && (
        <div className="mx-4 mb-2 rounded-lg bg-red-500/10 border border-red-500/20 px-4 py-2.5 text-sm text-red-400 flex items-center justify-between animate-fade-in">
          <span>{error}</span>
          <button
            onClick={() => setError(null)}
            className="ml-2 text-red-400/60 hover:text-red-400"
          >
            <svg
              className="h-4 w-4"
              fill="none"
              viewBox="0 0 24 24"
              strokeWidth={2}
              stroke="currentColor"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M6 18L18 6M6 6l12 12"
              />
            </svg>
          </button>
        </div>
      )}

      {/* Input area */}
      <div className="border-t border-dark-800/50 px-4 py-3">
        <div className="flex items-end gap-2">
          <div className="relative flex-1">
            <textarea
              ref={inputRef}
              value={input}
              onChange={handleInputChange}
              onKeyDown={handleKeyDown}
              placeholder={
                isConnected
                  ? "\uBA54\uC2DC\uC9C0\uB97C \uC785\uB825\uD558\uC138\uC694... Type a message..."
                  : "\uC11C\uBC84\uC5D0 \uC5F0\uACB0\uD574\uC8FC\uC138\uC694 Connect to server first"
              }
              disabled={!isConnected || isLoading}
              rows={1}
              className="w-full resize-none rounded-xl border border-dark-700 bg-dark-800/50 px-4 py-3 pr-12 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500/50 focus:ring-1 focus:ring-primary-500/30 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
              style={{ maxHeight: "200px" }}
            />
          </div>
          <button
            onClick={handleSend}
            disabled={!input.trim() || isLoading || !isConnected}
            className="flex h-[46px] w-[46px] flex-shrink-0 items-center justify-center rounded-xl bg-primary-500 text-white transition-all hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 disabled:cursor-not-allowed active:scale-95"
            aria-label="Send message"
          >
            {isLoading ? (
              <svg
                className="h-5 w-5 animate-spin"
                fill="none"
                viewBox="0 0 24 24"
              >
                <circle
                  className="opacity-25"
                  cx="12"
                  cy="12"
                  r="10"
                  stroke="currentColor"
                  strokeWidth="4"
                />
                <path
                  className="opacity-75"
                  fill="currentColor"
                  d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                />
              </svg>
            ) : (
              <svg
                className="h-5 w-5"
                fill="none"
                viewBox="0 0 24 24"
                strokeWidth={2}
                stroke="currentColor"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5"
                />
              </svg>
            )}
          </button>
        </div>
        <p className="mt-2 text-center text-[10px] text-dark-600">
          Shift+Enter {"\uC904\uBC14\uAFC8"} | Enter {"\uC804\uC1A1"}
        </p>
      </div>
    </div>
  );
}
