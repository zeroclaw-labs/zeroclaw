import { useState, useCallback, useEffect, useRef } from "react";
import { Chat } from "./components/Chat";
import { Sidebar } from "./components/Sidebar";
import { Settings } from "./components/Settings";
import { Login } from "./components/Login";
import { SignUp } from "./components/SignUp";
import { DeviceSelect } from "./components/DeviceSelect";
import { Interpreter } from "./components/Interpreter";
import { SetupWizard } from "./components/SetupWizard";
import { apiClient, type DeviceInfo, type ToolInfo } from "./lib/api";
import { getStoredLocale, setStoredLocale, type Locale } from "./lib/i18n";
import { isTauri, onLifecycleEvent, isAuthenticated } from "./lib/tauri-bridge";
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

type Page = "setup" | "login" | "signup" | "device_select" | "chat" | "settings" | "interpreter";

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

  const activeChat = chats.find((c) => c.id === activeChatId) ?? null;

  useEffect(() => {
    saveChats(chats);
  }, [chats]);

  useEffect(() => {
    setActiveChatId(activeChatId);
  }, [activeChatId]);

  // Check auth on startup — show setup wizard for first-time users
  useEffect(() => {
    const setupComplete = localStorage.getItem("moa_setup_complete");
    if (!setupComplete) {
      setPage("setup");
      return;
    }

    if (apiClient.isLoggedIn()) {
      // Already have a token → go to chat
      setIsConnected(true);
      setPage("chat");
      apiClient.startHeartbeat();
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
  }, []);

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
    async (content: string) => {
      let chatId = activeChatId;
      let isNew = false;

      if (!chatId) {
        const chat = createNewChat();
        chatId = chat.id;
        isNew = true;
        setChats((prev) => [chat, ...prev]);
        setActiveChatIdState(chatId);
      }

      const userMsg = createMessage("user", content);

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
        // Build conversation context from recent messages for the agent loop
        const currentChat = chats.find((c) => c.id === chatId);
        const recentContext = (currentChat?.messages ?? [])
          .slice(-10)
          .map((m) => `${m.role}: ${m.content}`);
        const response = await apiClient.chat(content, recentContext);
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
    [activeChatId],
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

  const handleLoginSuccess = useCallback((devices: DeviceInfo[]) => {
    if (devices.length <= 1) {
      // 0 or 1 device → auto-connect
      setIsConnected(true);
      setPage("chat");
      apiClient.startHeartbeat();
      // Auto-register this device if no devices yet
      if (devices.length === 0) {
        apiClient.registerDevice("MoA Device").catch(() => {});
      }
    } else {
      // Multiple devices → show device selection
      const onlineDevices = devices.filter((d) => d.is_online);
      const currentDeviceId = apiClient.getDeviceId();
      const currentInList = devices.find((d) => d.device_id === currentDeviceId);

      if (currentInList) {
        // This device is in the list → auto-connect (local login)
        setIsConnected(true);
        setPage("chat");
        apiClient.startHeartbeat();
      } else if (onlineDevices.length === 1 && !onlineDevices[0].has_pairing_code) {
        // Only 1 online device without pairing code → auto-connect
        setIsConnected(true);
        setPage("chat");
        apiClient.startHeartbeat();
      } else {
        // Show device selection
        setPendingDevices(devices);
        setPage("device_select");
      }
    }
  }, []);

  const handleDeviceSelected = useCallback(() => {
    setIsConnected(true);
    setPage("chat");
  }, []);

  const handleLogout = useCallback(async () => {
    await apiClient.logout();
    setIsConnected(false);
    setPage("login");
  }, []);

  // ── Render ─────────────────────────────────────────────────────

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
        ) : (
          <Settings
            locale={locale}
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
