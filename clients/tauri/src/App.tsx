import { useState, useCallback, useEffect, useRef } from "react";
import { Chat } from "./components/Chat";
import { Sidebar } from "./components/Sidebar";
import { Settings } from "./components/Settings";
import { Login } from "./components/Login";
import { SignUp } from "./components/SignUp";
import { DeviceSelect } from "./components/DeviceSelect";
import { Interpreter } from "./components/Interpreter";
import { SetupWizard } from "./components/SetupWizard";
import { GatewayStatus } from "./components/GatewayStatus";
import { DocumentEditor } from "./components/DocumentEditor";
import { apiClient, type DeviceInfo, type ToolInfo } from "./lib/api";
import { getStoredLocale, setStoredLocale, t, type Locale } from "./lib/i18n";
import { isTauri, onLifecycleEvent, isAuthenticated, onPythonEnvStatus, type PythonEnvStatus } from "./lib/tauri-bridge";
import {
  processAttachment,
  buildChatMessage,
  type AttachmentFile,
} from "./lib/chat-attachments";
import {
  loadChats,
  saveChats,
  getActiveChatId,
  setActiveChatId,
  createNewChat,
  createMessage,
  deriveChatTitle,
  type ChatSession,
  type ChatMessage,
} from "./lib/storage";

type Page = "setup" | "login" | "signup" | "device_select" | "chat" | "settings" | "interpreter" | "document";

// Lazy-load Tauri invoke for document pre-processing
let tauriInvoke: ((cmd: string, args?: Record<string, unknown>) => Promise<unknown>) | null = null;
try {
  const tauri = (window as Record<string, unknown>).__TAURI__;
  if (tauri && typeof tauri === "object") {
    tauriInvoke = (tauri as Record<string, (cmd: string, args?: Record<string, unknown>) => Promise<unknown>>).invoke;
  }
} catch { /* not in Tauri */ }

function App() {
  const [page, setPage] = useState<Page>("login");
  const [locale, setLocale] = useState<Locale>(getStoredLocale());
  const [chats, setChats] = useState<ChatSession[]>(() => loadChats());
  const [activeChatId, setActiveChatIdState] = useState<string | null>(() => getActiveChatId());
  const [sidebarOpen, setSidebarOpen] = useState(() => window.innerWidth > 768);
  const [isConnected, setIsConnected] = useState(false);
  const [pendingDevices, setPendingDevices] = useState<DeviceInfo[]>([]);
  const [sidebarDevices, setSidebarDevices] = useState<DeviceInfo[]>([]);
  const [sidebarChannels, setSidebarChannels] = useState<string[]>([]);
  const [sidebarTools, setSidebarTools] = useState<ToolInfo[]>([]);
  const lifecycleCleanup = useRef<(() => void) | null>(null);
  // In Tauri mode, wait for the MoA gateway to be ready before
  // allowing the user to interact with auth/chat screens.
  const [gatewayReady, setGatewayReady] = useState(!isTauri());
  // Python environment setup status (shown as a banner during first launch)
  const [pythonSetup, setPythonSetup] = useState<PythonEnvStatus | null>(null);

  const activeChat = chats.find((c) => c.id === activeChatId) ?? null;

  useEffect(() => {
    saveChats(chats);
  }, [chats]);

  useEffect(() => {
    setActiveChatId(activeChatId);
  }, [activeChatId]);

  // Listen for Python environment setup events (first-launch auto-install)
  useEffect(() => {
    if (!isTauri()) return;
    let cleanup: (() => void) | null = null;
    onPythonEnvStatus((status) => {
      setPythonSetup(status);
      // Auto-hide the banner after "ready" or "error" after a delay
      if (status.stage === "ready") {
        setTimeout(() => setPythonSetup(null), 3000);
      }
    }).then((unlisten) => { cleanup = unlisten; });
    return () => { cleanup?.(); };
  }, []);

  // Check auth on startup — show setup wizard for first-time users.
  // In Tauri mode, wait for the gateway to be ready before proceeding.
  useEffect(() => {
    if (!gatewayReady) return;

    const setupComplete = localStorage.getItem("zeroclaw_setup_complete");
    if (!setupComplete) {
      setPage("setup");
      return;
    }

    if (apiClient.isLoggedIn()) {
      // Already have a token → go to chat
      setIsConnected(true);
      setPage("chat");
      apiClient.startHeartbeat();

      // Auto-greet returning user if no active chat
      const existingChats = loadChats();
      const savedActiveId = getActiveChatId();
      const activeExists = existingChats.some((c) => c.id === savedActiveId);
      if (!activeExists) {
        sendAutoGreeting(existingChats.length === 0);
      }
    } else {
      setPage("login");
    }

    if (!isTauri()) return;

    onLifecycleEvent(async (event, data) => {
      if (event === "resume" && data) {
        if (data.has_token) {
          setIsConnected(true);
        }
      }
    }).then((cleanup) => {
      lifecycleCleanup.current = cleanup;
    });

    // In Tauri, also check backend auth state
    isAuthenticated().then((auth) => {
      if (auth === true && !apiClient.isLoggedIn()) {
        // Backend has token but frontend doesn't — sync
        setIsConnected(true);
        setPage("chat");
        apiClient.startHeartbeat();
      }
    });

    return () => {
      lifecycleCleanup.current?.();
    };
  }, [gatewayReady]);

  // Fetch sidebar data (devices, channels, tools) when connected
  useEffect(() => {
    if (!isConnected) {
      setSidebarDevices([]);
      setSidebarChannels([]);
      setSidebarTools([]);
      return;
    }

    const fetchSidebarData = async () => {
      const [devices, agentInfo] = await Promise.all([
        apiClient.getDevices().catch(() => [] as DeviceInfo[]),
        apiClient.getAgentInfo(),
      ]);
      setSidebarDevices(devices);
      setSidebarChannels(agentInfo.channels);
      setSidebarTools(agentInfo.tools);
    };

    fetchSidebarData();

    // Refresh devices periodically (every 60s, aligned with heartbeat)
    const interval = setInterval(() => {
      apiClient.getDevices().then(setSidebarDevices).catch(() => {});
    }, 60_000);

    return () => clearInterval(interval);
  }, [isConnected]);

  const handleLocaleChange = useCallback((newLocale: Locale) => {
    setLocale(newLocale);
    setStoredLocale(newLocale);
  }, []);

  const handleNewChat = useCallback(() => {
    const chat = createNewChat();
    setChats((prev) => [chat, ...prev]);
    setActiveChatIdState(chat.id);
    setPage("chat");
  }, []);

  const handleSelectChat = useCallback((id: string) => {
    setActiveChatIdState(id);
    setPage("chat");
  }, []);

  const handleDeleteChat = useCallback(
    (id: string) => {
      setChats((prev) => prev.filter((c) => c.id !== id));
      if (activeChatId === id) {
        setActiveChatIdState(null);
      }
    },
    [activeChatId],
  );

  const handleSendMessage = useCallback(
    async (content: string, attachments?: AttachmentFile[]) => {
      let chatId = activeChatId;
      let isNew = false;

      if (!chatId) {
        const chat = createNewChat();
        chatId = chat.id;
        isNew = true;
        setChats((prev) => [chat, ...prev]);
        setActiveChatIdState(chatId);
      }

      // Build display message (show filenames to user)
      const attachNames = attachments?.map((a) => a.name) ?? [];
      const displayContent = attachNames.length > 0
        ? `${content}\n\n[${locale === "ko" ? "첨부" : "Attached"}: ${attachNames.join(", ")}]`
        : content;

      const userMsg = createMessage("user", displayContent);

      setChats((prev) =>
        prev.map((c) => {
          if (c.id !== chatId) return c;
          const updated = {
            ...c,
            messages: [...(isNew ? [] : c.messages), userMsg],
            updatedAt: Date.now(),
          };
          if (updated.messages.length === 1) {
            updated.title = deriveChatTitle(updated.messages);
          }
          return updated;
        }),
      );

      try {
        // Process attachments if any
        let finalMessage = content;
        if (attachments && attachments.length > 0) {
          const processed = await Promise.all(
            attachments.map((a) =>
              processAttachment(a, { locale, tauriInvoke }),
            ),
          );
          const built = buildChatMessage(content, processed);
          finalMessage = built.message;
          // TODO: send built.images via multimodal API when supported
        }

        // Build conversation context from recent messages for the agent loop
        const currentChat = chats.find((c) => c.id === chatId);
        const recentContext = (currentChat?.messages ?? [])
          .slice(-10)
          .map((m) => `${m.role}: ${m.content}`);
        const response = await apiClient.chat(finalMessage, recentContext);
        const assistantMsg = createMessage("assistant", response.response, response.model);

        setChats((prev) =>
          prev.map((c) => {
            if (c.id !== chatId) return c;
            return {
              ...c,
              messages: [...c.messages, assistantMsg],
              updatedAt: Date.now(),
            };
          }),
        );
      } catch (err) {
        const errorMsg = createMessage(
          "error",
          err instanceof Error ? err.message : "An unknown error occurred",
        );

        setChats((prev) =>
          prev.map((c) => {
            if (c.id !== chatId) return c;
            return {
              ...c,
              messages: [...c.messages, errorMsg],
              updatedAt: Date.now(),
            };
          }),
        );

        if (err instanceof Error && err.message.includes("expired")) {
          setIsConnected(false);
          apiClient.stopHeartbeat();
          setPage("login");
        }
      }
    },
    [activeChatId, locale],
  );

  const handleRetry = useCallback(
    (messages: ChatMessage[]) => {
      const lastUserIdx = [...messages].reverse().findIndex((m) => m.role === "user");
      if (lastUserIdx === -1) return;

      const actualIdx = messages.length - 1 - lastUserIdx;
      const lastUserMsg = messages[actualIdx];

      setChats((prev) =>
        prev.map((c) => {
          if (c.id !== activeChatId) return c;
          return {
            ...c,
            messages: messages.slice(0, actualIdx),
            updatedAt: Date.now(),
          };
        }),
      );

      handleSendMessage(lastUserMsg.content);
    },
    [activeChatId, handleSendMessage],
  );

  // Send an automatic greeting from MoA when chat opens after login
  const sendAutoGreeting = useCallback(
    async (isFirstLogin: boolean) => {
      const chat = createNewChat();
      const chatId = chat.id;
      setChats((prev) => [chat, ...prev]);
      setActiveChatIdState(chatId);

      const user = apiClient.getUser();
      const username = user?.username ?? "User";

      // Choose prompt based on whether this is a first-time or returning user
      const promptKey = isFirstLogin ? "greeting_prompt" : "greeting_prompt_returning";
      const prompt = t(promptKey, locale).replace("{username}", username);

      try {
        const response = await apiClient.chat(prompt, []);
        const assistantMsg = createMessage("assistant", response.response, response.model);

        setChats((prev) =>
          prev.map((c) => {
            if (c.id !== chatId) return c;
            return {
              ...c,
              title: t("app_title", locale),
              messages: [assistantMsg],
              updatedAt: Date.now(),
            };
          }),
        );
      } catch (err) {
        // If greeting fails, show a placeholder welcome message
        const fallbackText = isFirstLogin
          ? (locale === "ko"
            ? "안녕하세요. 저는 MoA입니다. 현재 서버 연결을 확인 중입니다. 잠시 후 다시 시도해주세요."
            : "Hello. I'm MoA. The server connection is being established. Please try again shortly.")
          : (locale === "ko"
            ? "안녕하세요. 현재 서버 연결을 확인 중입니다."
            : "Hello. Checking server connection...");
        const fallbackMsg = createMessage("assistant", fallbackText);

        setChats((prev) =>
          prev.map((c) => {
            if (c.id !== chatId) return c;
            return {
              ...c,
              title: t("app_title", locale),
              messages: [fallbackMsg],
              updatedAt: Date.now(),
            };
          }),
        );
      }
    },
    [locale],
  );

  const handleLoginSuccess = useCallback((devices: DeviceInfo[]) => {
    const proceedToChat = () => {
      setIsConnected(true);
      setPage("chat");
      apiClient.startHeartbeat();

      // Determine if first login (no previous chats)
      const existingChats = loadChats();
      const isFirstLogin = existingChats.length === 0;
      sendAutoGreeting(isFirstLogin);
    };

    if (devices.length <= 1) {
      // 0 or 1 device → auto-connect
      if (devices.length === 0) {
        apiClient.registerDevice("MoA Device").catch(() => {});
      }
      proceedToChat();
    } else {
      // Multiple devices → show device selection
      const currentDeviceId = apiClient.getDeviceId();
      const currentInList = devices.find((d) => d.device_id === currentDeviceId);
      const onlineDevices = devices.filter((d) => d.is_online);

      if (currentInList) {
        proceedToChat();
      } else if (onlineDevices.length === 1 && !onlineDevices[0].has_pairing_code) {
        proceedToChat();
      } else {
        // Show device selection
        setPendingDevices(devices);
        setPage("device_select");
      }
    }
  }, [sendAutoGreeting]);

  const handleDeviceSelected = useCallback(() => {
    setIsConnected(true);
    setPage("chat");
    apiClient.startHeartbeat();

    const existingChats = loadChats();
    const isFirstLogin = existingChats.length === 0;
    sendAutoGreeting(isFirstLogin);
  }, [sendAutoGreeting]);

  const handleLogout = useCallback(async () => {
    await apiClient.logout();
    setIsConnected(false);
    setPage("login");
  }, []);

  // Stable callback for GatewayStatus
  const handleGatewayReady = useCallback(() => setGatewayReady(true), []);

  // ── Python setup banner ────────────────────────────────────────
  const pythonBanner = pythonSetup && pythonSetup.stage !== "ready" ? (
    <div style={{
      position: "fixed", bottom: 16, right: 16, zIndex: 9999,
      background: pythonSetup.stage === "error" ? "#dc2626" : "#2563eb",
      color: "#fff", padding: "10px 16px", borderRadius: 8,
      fontSize: 13, maxWidth: 360, boxShadow: "0 2px 8px rgba(0,0,0,0.2)",
      display: "flex", alignItems: "center", gap: 8,
    }}>
      {pythonSetup.stage !== "error" && (
        <span style={{ display: "inline-block", width: 14, height: 14, border: "2px solid #fff", borderTopColor: "transparent", borderRadius: "50%", animation: "spin 0.8s linear infinite" }} />
      )}
      <span>{pythonSetup.detail}</span>
    </div>
  ) : null;

  // ── Render ─────────────────────────────────────────────────────

  // Show gateway startup overlay while waiting for backend (Tauri only)
  if (!gatewayReady) {
    return (
      <div className="app">
        <GatewayStatus onReady={handleGatewayReady} />
      </div>
    );
  }

  // First-time setup wizard (no sidebar)
  if (page === "setup") {
    return (
      <div className="app">
        <SetupWizard
          locale={locale}
          onLocaleChange={handleLocaleChange}
          onComplete={() => setPage("login")}
        />
      </div>
    );
  }

  // Auth screens (no sidebar)
  if (page === "login") {
    return (
      <div className="app">
        <Login
          locale={locale}
          onLoginSuccess={handleLoginSuccess}
          onGoToSignUp={() => setPage("signup")}
          onGoToSettings={() => setPage("settings")}
        />
      </div>
    );
  }

  if (page === "signup") {
    return (
      <div className="app">
        <SignUp
          locale={locale}
          onSignUpSuccess={() => setPage("login")}
          onGoToLogin={() => setPage("login")}
        />
      </div>
    );
  }

  if (page === "device_select") {
    return (
      <div className="app">
        <DeviceSelect
          locale={locale}
          devices={pendingDevices}
          onDeviceSelected={handleDeviceSelected}
          onLogout={handleLogout}
        />
      </div>
    );
  }

  // Main app (with sidebar)
  return (
    <div className="app">
      {pythonBanner}
      <Sidebar
        chats={chats}
        activeChatId={activeChatId}
        isOpen={sidebarOpen}
        locale={locale}
        devices={sidebarDevices}
        channels={sidebarChannels}
        tools={sidebarTools}
        onNewChat={handleNewChat}
        onSelectChat={handleSelectChat}
        onDeleteChat={handleDeleteChat}
        onOpenSettings={() => setPage("settings")}
        onOpenInterpreter={() => {
          setPage("interpreter");
          if (window.innerWidth <= 768) setSidebarOpen(false);
        }}
        onOpenDocument={() => {
          setPage("document");
          if (window.innerWidth <= 768) setSidebarOpen(false);
        }}
        onToggle={() => setSidebarOpen((p) => !p)}
        currentPage={page}
      />
      <main className={`main-content ${sidebarOpen ? "" : "sidebar-collapsed"}`}>
        {page === "chat" ? (
          <Chat
            chat={activeChat}
            locale={locale}
            isConnected={isConnected}
            onSendMessage={handleSendMessage}
            onRetry={handleRetry}
            onOpenSettings={() => setPage("settings")}
            onToggleSidebar={() => setSidebarOpen((p) => !p)}
            sidebarOpen={sidebarOpen}
          />
        ) : page === "interpreter" ? (
          <Interpreter
            locale={locale}
            onBack={() => setPage("chat")}
            onToggleSidebar={() => setSidebarOpen((p) => !p)}
            sidebarOpen={sidebarOpen}
          />
        ) : page === "document" ? (
          <DocumentEditor
            locale={locale}
            onBack={() => setPage("chat")}
            onToggleSidebar={() => setSidebarOpen((p) => !p)}
            sidebarOpen={sidebarOpen}
            onDocumentUpdate={(markdown, _html) => {
              // Send document content to the active chat as context
              if (markdown) {
                handleSendMessage(`[Document updated]\n\n${markdown.substring(0, 2000)}`);
              }
            }}
          />
        ) : (
          <Settings
            locale={locale}
            isConnected={isConnected}
            onLocaleChange={handleLocaleChange}
            onBack={() => setPage("chat")}
            onLogout={handleLogout}
          />
        )}
      </main>
    </div>
  );
}

export default App;
