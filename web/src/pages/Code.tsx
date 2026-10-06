import { SopAssistantMessage, SopPromptMessage } from '@/components/SopProposal';
import { useCallback, useEffect, useRef, useState } from "react";
import {
  Link,
  useSearchParams,
  type SetURLSearchParams,
  createSearchParams,
} from "react-router-dom";
import {
  ArrowUp,
  PanelLeft,
  Plus,
  Search,
  FileText,
  Folder,
  RefreshCw,
  Send,
  Settings,
  Square,
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import CodeMirror from "@uiw/react-codemirror";
import { githubLight } from "@uiw/codemirror-theme-github";
import { oneDark } from "@codemirror/theme-one-dark";
import { AcpWebSocketClient, type AcpRequest } from "@/lib/acp";
import {
  getWorkspaceAvailability,
  listAgentWorkspace,
  readAgentWorkspaceFile,
  type BrowseEntry,
} from "@/lib/api";
import { useDraft } from "@/hooks/useDraft";
import {
  loadCodeSelection,
  resumeCodeSession,
  saveCodeSelection,
} from "@/lib/codeSelection";
import { useWorkspaceSettings } from "@/components/WorkspaceSettings";
import { t } from "@/lib/i18n";
import { useWorkspaceVisible } from "@/components/layout/WorkspaceOutlet";
import WorkspaceAttention from "@/components/layout/WorkspaceAttention";
import { useTheme } from "@/hooks/useTheme";
import ApprovalBanner from "@/components/ApprovalBanner";
import type { PendingApproval, ApprovalDecision, Session } from "@/types/api";

interface CodeMessage {
  role: string;
  content: string;
  kind?: string;
  tool_name?: string;
}
interface CodeSession {
  session_id: string;
  agent_alias: string;
  workspace_dir: string;
}
interface Update {
  type: string;
  session_id: string;
  client_turn_generation?: number;
  text?: string;
  content?: string;
  name?: string;
  raw_input?: unknown;
  raw_output?: string;
  request_id?: string;
  tool_name?: string;
  arguments_summary?: string;
  timeout_secs?: number;
}

export default function Code({
  embedded = false,
  contextText,
  sopAssistant,
  onBusyChange,
  attentionTarget,
  onAttentionOpen,
}: {
  embedded?: boolean;
  contextText?: string;
  sopAssistant?: { onApply: (source: string) => Promise<void> };
  attentionTarget?: string;
  onAttentionOpen?: () => void;
  onBusyChange?: (busy: boolean) => void;
}) {
  const visible = useWorkspaceVisible();
  const openSettings = useWorkspaceSettings();
  const [sessionQuery, setSessionQuery] = useState("");
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const [routeParams, setRouteParams] = useSearchParams();
  const [localParams, setLocalParams] = useState(
    () => new URLSearchParams({ new: "1" }),
  );
  const params = embedded ? localParams : routeParams;
  const setParams: SetURLSearchParams = useCallback(
    (next, options) => {
      if (embedded)
        setLocalParams((current) =>
          createSearchParams(typeof next === "function" ? next(current) : next),
        );
      else setRouteParams(next, options);
    },
    [embedded, setRouteParams],
  );
  const [agents, setAgents] = useState<string[] | null>(null);
  const [available, setAvailable] = useState<boolean | null>(null);
  const [alias, setAlias] = useState(
    params.get("agent") ?? (!embedded ? loadCodeSelection()?.agent : "") ?? "",
  );
  const [session, setSession] = useState<CodeSession | null>(null);
  const [history, setHistory] = useState<
    (Session & { interaction_surface?: string })[]
  >([]);
  const [messages, setMessages] = useState<CodeMessage[]>([]);
  const [stream, setStream] = useState("");
  const draftKey = `code.${attentionTarget ?? "workspace"}.${alias}.${session?.session_id ?? "new"}`;
  const { draft, saveDraft, clearDraft } = useDraft(draftKey);
  const [prompt, setPromptValue] = useState(draft);
  const setPrompt = (value: string) => {
    setPromptValue(value);
    saveDraft(value);
  };
  useEffect(() => {
    setPromptValue(draft);
  }, [draftKey, draft]);
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [observing, setObserving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [approval, setApproval] = useState<PendingApproval | null>(null);
  const [question, setQuestion] = useState<AcpRequest | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [connection, setConnection] = useState(0);
  const turnRef = useRef(0);
  const clientRef = useRef<AcpWebSocketClient | null>(null);
  const sessionRef = useRef<CodeSession | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    getWorkspaceAvailability()
      .then((value) => {
        if (cancelled) return;
        setAvailable(value.code);
        setAgents(value.agents);
        setAlias((current) =>
          value.agents.includes(current) ? current : (value.agents[0] ?? ""),
        );
      })
      .catch((error: Error) => {
        if (!cancelled) setError(error.message);
      });
    return () => {
      cancelled = true;
    };
  }, [connection]);

  useEffect(() => {
    if (!available || !alias) return;
    let cancelled = false;
    setReady(false);
    setError(null);
    setMessages([]);
    setStream("");
    setBusy(false);
    setObserving(false);
    setApproval(null);
    setQuestion(null);
    setSession(null);
    sessionRef.current = null;
    const client = new AcpWebSocketClient(
      {
        onOpen: () => {
          void client
            .request("initialize", {
              protocol_version: 1,
              clientCapabilities: { elicitation: { form: {} } },
            })
            .then(async () => {
              if (cancelled) return;
              const data = await client.request<{
                sessions: (Session & { interaction_surface?: string })[];
              }>("session/list-acp");
              if (!cancelled) {
                setHistory(data.sessions);
                setReady(true);
              }
            })
            .catch((error: Error) => {
              if (!cancelled) setError(error.message);
            });
        },
        onClose: () => {
          if (cancelled) return;
          setReady(false);
          setBusy(false);
          setApproval(null);
          setQuestion(null);
          setError(t("code.disconnected"));
        },
        onError: () => {
          if (!cancelled) setError(t("code.disconnected"));
        },
        onNotification: (notification) => {
          if (cancelled || notification.method !== "session/update") return;
          const update = notification.params as Update;
          if (!update || update.session_id !== sessionRef.current?.session_id)
            return;
          if (
            update.client_turn_generation !== undefined &&
            update.client_turn_generation !== turnRef.current
          )
            return;
          if (update.type === "agent_message_chunk")
            setStream((text) => text + (update.text ?? ""));
          if (update.type === "tool_call")
            setMessages((current) => [
              ...current,
              {
                role: "tool",
                content: JSON.stringify(update.raw_input, null, 2),
                tool_name: update.name,
              },
            ]);
          if (update.type === "tool_result") setRefresh((value) => value + 1);
          if (update.type === "approval_request" && update.request_id)
            setApproval({
              requestId: update.request_id,
              toolName: update.tool_name ?? "",
              argumentsSummary: update.arguments_summary ?? "",
              timeoutSecs: update.timeout_secs ?? 0,
              receivedAt: Date.now(),
            });
          if (update.type === "turn_complete") {
            setMessages((current) => [
              ...current,
              {
                role: "assistant",
                content: update.content || t("agent.turn_no_output"),
              },
            ]);
            setStream("");
            setBusy(false);
            setApproval(null);
            setQuestion(null);
            setRefresh((value) => value + 1);
            void client
              .request<{
                sessions: (Session & { interaction_surface?: string })[];
              }>("session/list-acp")
              .then((data) => {
                if (!cancelled) setHistory(data.sessions);
              })
              .catch((error: Error) => {
                if (!cancelled) setError(error.message);
              });
          }
        },
        onRequest: (request) => {
          if (cancelled) return;
          if (request.method === "elicitation/create") setQuestion(request);
          else
            client.respondError(request.id, {
              code: -32601,
              message: "Unsupported request",
            });
        },
      },
      "/ws/code",
    );
    clientRef.current = client;
    client.connect();
    return () => {
      cancelled = true;
      client.disconnect();
      if (clientRef.current === client) clientRef.current = null;
    };
  }, [alias, available, connection]);

  useEffect(() => {
    onBusyChange?.(busy);
  }, [busy, onBusyChange]);

  const requestedAgent = params.get("agent");
  useEffect(() => {
    if (
      !visible ||
      !requestedAgent ||
      !agents?.includes(requestedAgent) ||
      requestedAgent === alias
    )
      return;
    if (busy) {
      setError(t("code.finish_before_switch"));
      return;
    }
    setAlias(requestedAgent);
  }, [visible, requestedAgent, agents, alias, busy]);
  useEffect(() => {
    if (!busy) return;
    const protectWork = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", protectWork);
    return () => window.removeEventListener("beforeunload", protectWork);
  }, [busy]);

  // Keep the newest response visible only when the operator is already near
  // the bottom; reading earlier output never fights automatic scrolling.
  useEffect(() => {
    const end = endRef.current;
    const pane = end?.parentElement;
    if (pane && pane.scrollHeight - pane.scrollTop - pane.clientHeight < 180)
      end?.scrollIntoView({ block: "nearest" });
  }, [stream, messages]);

  const openSession = async (id?: string): Promise<CodeSession> => {
    const client = clientRef.current;
    if (!client?.connected) throw new Error(t("code.disconnected"));
    const next = await client.request<CodeSession>("session/new", {
      agent_alias: alias,
      ...(id ? { session_id: id } : {}),
    });
    if (client !== clientRef.current) throw new Error(t("code.disconnected"));
    sessionRef.current = next;
    setSession(next);
    if (!embedded) saveCodeSelection(next.agent_alias, next.session_id);
    if (visibleRef.current)
      setParams({ agent: alias, session: next.session_id }, { replace: true });
    if (id) {
      const result = await client.request<{ messages: CodeMessage[] }>(
        "session/messages",
        { session_id: id },
      );
      if (client === clientRef.current && sessionRef.current === next)
        setMessages(result.messages);
      const state = await client.request<{ state: string }>("session/state", {
        session_id: id,
      });
      if (client === clientRef.current && sessionRef.current === next) {
        setBusy(state.state === "running");
        setObserving(state.state === "running");
      }
    }
    return next;
  };

  // A resumed task can still be owned by another client. Its terminal event
  // goes to that connection, so reconcile from the canonical runtime state.
  useEffect(() => {
    if (!observing || !session || !ready) return;
    const client = clientRef.current;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const reconcile = async () => {
      try {
        const state = await client?.request<{ state: string }>(
          "session/state",
          { session_id: session.session_id },
        );
        const result = await client?.request<{ messages: CodeMessage[] }>(
          "session/messages",
          { session_id: session.session_id },
        );
        if (
          cancelled ||
          client !== clientRef.current ||
          sessionRef.current !== session
        )
          return;
        if (result) setMessages(result.messages);
        if (state?.state === "idle") {
          setObserving(false);
          setBusy(false);
          setRefresh((value) => value + 1);
          return;
        }
      } catch (error) {
        if (!cancelled)
          setError(error instanceof Error ? error.message : String(error));
      }
      if (!cancelled) timer = setTimeout(() => void reconcile(), 2000);
    };
    void reconcile();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [observing, session, ready]);

  const requestedSession = params.get("session");
  useEffect(() => {
    if (!visible || !ready || requestedSession || params.has("new")) return;
    if (sessionRef.current) {
      setParams(
        {
          agent: sessionRef.current.agent_alias,
          session: sessionRef.current.session_id,
        },
        { replace: true },
      );
      return;
    }
    const last = resumeCodeSession(history, agents ?? []);
    if (last?.agent_alias)
      setParams(
        { agent: last.agent_alias, session: last.session_id },
        { replace: true },
      );
    else if (alias) setParams({ agent: alias, new: "1" }, { replace: true });
  }, [
    visible,
    ready,
    requestedSession,
    params,
    history,
    agents,
    alias,
    setParams,
  ]);
  useEffect(() => {
    if (
      !visible ||
      !ready ||
      !requestedSession ||
      !clientRef.current?.connected ||
      sessionRef.current?.session_id === requestedSession
    )
      return;
    if (busy) {
      setError(t("code.finish_before_switch"));
      return;
    }
    if (
      !history.some(
        (item) =>
          item.session_id === requestedSession && item.agent_alias === alias,
      )
    )
      return;
    setBusy(true);
    void openSession(requestedSession).catch((error: Error) => {
      setError(error.message);
      setBusy(false);
    });
    // The URL is a selection request, not a second owner of session state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, requestedSession, visible, alias]);

  const send = async () => {
    if (!prompt.trim() || !ready || busy) return;
    const text = sopAssistant && contextText
      ? `${prompt.trim()}\n\n${t('sop_workspace.assistant_instructions')}\n\n\`\`\`json\n${contextText}\n\`\`\``
      : prompt.trim();
    const client = clientRef.current;
    const turn = ++turnRef.current;
    setBusy(true);
    setObserving(false);
    setError(null);
    setStream("");
    try {
      const current = sessionRef.current ?? (await openSession());
      setMessages((messages) => [...messages, { role: "user", content: text }]);
      setPrompt("");
      clearDraft();
      // The RPC terminal notification is authoritative. The request response
      // may arrive later and must not release the next turn prematurely.
      await client?.request(
        "session/prompt",
        {
          session_id: current.session_id,
          prompt: text,
          client_turn_generation: turn,
        },
        24 * 60 * 60 * 1000,
      );
    } catch (error) {
      if (client === clientRef.current && turn === turnRef.current) {
        setError(error instanceof Error ? error.message : String(error));
        setBusy(false);
      }
    }
  };
  const respondApproval = async (decision: ApprovalDecision) => {
    if (!approval || !session) return;
    try {
      await clientRef.current?.request("session/approve", {
        session_id: session.session_id,
        request_id: approval.requestId,
        decision:
          decision === "approve"
            ? "allow_once"
            : decision === "always"
              ? "allow_always"
              : "reject",
      });
      setApproval(null);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    }
  };

  return (
    <div className="relative flex h-full min-h-0">
      {!embedded && (
        <>
          <button
            type="button"
            onClick={() => setSidebarOpen(!sidebarOpen)}
            aria-label={t("code.history")}
            aria-expanded={sidebarOpen}
            className="absolute top-3 left-3 z-20 rounded-lg border border-pc-border bg-pc-surface p-2 md:hidden"
          >
            <PanelLeft className="h-4 w-4" />
          </button>
          {sidebarOpen && (
            <button
              type="button"
              className="absolute inset-0 z-20 bg-black/40 md:hidden"
              aria-label={t("common.close")}
              onClick={() => setSidebarOpen(false)}
            />
          )}
          <aside
            aria-label={t("code.history")}
            className={`${sidebarOpen ? "absolute inset-y-0 left-0 z-30 flex" : "hidden"} w-60 shrink-0 flex-col border-r border-pc-border bg-pc-surface md:static md:flex`}
          >
            <div className="flex items-center justify-between px-4 py-4">
              <h2 className="text-xs font-medium text-pc-text-muted">
                {t("code.history")}
              </h2>
              <button
                type="button"
                disabled={!ready || busy}
                onClick={() => {
                  setParams({ agent: alias, new: "1" }, { replace: true });
                  sessionRef.current = null;
                  setSession(null);
                  setMessages([]);
                  setStream("");
                  setSidebarOpen(false);
                }}
                aria-label={t("code.new_session")}
                className="rounded p-1 text-pc-text-muted hover:text-pc-text disabled:opacity-40"
              >
                <Plus className="h-4 w-4" />
              </button>
            </div>
            <label className="mx-3 mb-3 flex items-center gap-2 rounded-lg bg-pc-elevated px-3 py-2">
              <Search className="h-3.5 w-3.5 text-pc-text-muted" />
              <input
                aria-label={t("home.search_sessions")}
                placeholder={t("home.search_sessions")}
                value={sessionQuery}
                onChange={(e) => setSessionQuery(e.target.value)}
                className="min-w-0 w-full bg-transparent text-xs outline-none"
              />
            </label>
            <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
              {history
                .filter(
                  (item) =>
                    item.interaction_surface === "zerocode_code" &&
                    `${item.name} ${item.agent_alias} ${item.session_id}`
                      .toLowerCase()
                      .includes(sessionQuery.toLowerCase()),
                )
                .sort((a, b) => b.last_activity.localeCompare(a.last_activity))
                .map((item) => (
                  <button
                    key={item.session_id}
                    type="button"
                    aria-current={
                      session?.session_id === item.session_id
                        ? "page"
                        : undefined
                    }
                    disabled={busy || !ready}
                    onClick={() => {
                      setParams(
                        {
                          agent: item.agent_alias ?? alias,
                          session: item.session_id,
                        },
                        { replace: true },
                      );
                      setSidebarOpen(false);
                    }}
                    className={`mb-1 w-full rounded-lg px-3 py-3 text-left text-sm disabled:opacity-50 ${session?.session_id === item.session_id ? "bg-pc-elevated text-pc-text" : "text-pc-text-secondary hover:bg-pc-elevated/60"}`}
                  >
                    <span className="block truncate">
                      {item.name ||
                        `${item.agent_alias} · ${item.session_id.slice(0, 8)}`}
                    </span>
                    <span className="mt-1 block text-xs text-pc-text-faint">
                      {new Date(item.last_activity).toLocaleDateString()}
                    </span>
                  </button>
                ))}
              {!history.some(
                (item) => item.interaction_surface === "zerocode_code",
              ) && (
                <p className="p-3 text-xs text-pc-text-muted">
                  {t("workspace.no_code_sessions")}
                </p>
              )}
            </div>
            {busy && (
              <p className="p-3 text-xs text-pc-text-muted">
                {t("code.finish_before_switch")}
              </p>
            )}
          </aside>
        </>
      )}
      <div className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto p-3 sm:p-5">
        {(!visible || embedded) && (approval || question) && (
          <WorkspaceAttention
            to={
              attentionTarget ??
              `/code?${new URLSearchParams({ agent: alias, ...(session ? { session: session.session_id } : {}) })}`
            }
            label={t("nav.code")}
            onOpen={onAttentionOpen}
          />
        )}
        <div className={`flex flex-wrap items-center gap-3 ${embedded ? "" : "pl-10 md:pl-0"}`}>
          <select
            aria-label={t("code.agent")}
            value={alias}
            disabled={busy || !agents?.length}
            onChange={(event) => {
              setParams(
                { agent: event.target.value, new: "1" },
                { replace: true },
              );
              setAlias(event.target.value);
            }}
            className="input-electric p-2 text-sm max-w-48"
          >
            {(agents ?? []).map((agent) => (
              <option key={agent}>{agent}</option>
            ))}
          </select>
          <span role="status" className="text-xs text-pc-text-muted">
            {t(
              available === false
                ? "code.offline"
                : busy
                  ? "code.working"
                  : ready
                    ? "code.ready"
                    : "code.connecting",
            )}
          </span>
          <button
            type="button"
            onClick={() =>
              openSettings(`/config/agents/${encodeURIComponent(alias)}`)
            }
            aria-label={`${t("workspace.settings")}: ${alias}`}
            className="ml-auto rounded-lg p-2 text-pc-text-muted hover:bg-pc-elevated"
          >
            <Settings className="h-4 w-4" />
          </button>
        </div>
        {available === false && (
          <div className="rounded-lg border border-pc-border p-4 text-sm">
            {t("code.unavailable")}{" "}
            <Link to="/config/gateway" className="text-pc-accent">
              {t("nav.feature_settings")}
            </Link>
          </div>
        )}
        {error && (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/5 p-3 text-sm text-status-error"
          >
            {error}
            <button
              type="button"
              disabled={busy}
              onClick={() => setConnection((value) => value + 1)}
              className="underline"
            >
              {t("code.reconnect")}
            </button>
          </div>
        )}
        <div className="flex flex-1 min-h-0 flex-col gap-3">
          <section
            className="flex flex-1 flex-col min-h-[20rem] overflow-hidden"
            aria-label={t("code.conversation")}
          >
            <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-4">
              {messages.length === 0 && (
                <div className="py-8">
                  <h3 className="font-medium">{t(sopAssistant ? "sop_workspace.helper_title" : "code.start_title")}</h3>
                  <p className="mt-2 text-sm text-pc-text-muted">
                    {t(sopAssistant ? "sop_workspace.helper_hint" : "code.start_hint")}
                  </p>
                </div>
              )}
              {messages.map((message, index) =>
                message.role === "tool" ||
                message.kind === "tool_call" ||
                message.kind === "tool_result" ? (
                  <details
                    key={index}
                    className="rounded-md bg-pc-elevated p-2 text-xs"
                  >
                    <summary className="cursor-pointer font-mono">
                      {message.tool_name || t("acp.tool_call")}
                    </summary>
                    <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-words">
                      {message.content}
                    </pre>
                  </details>
                ) : (
                  <div
                    key={index}
                    className={
                      message.role === "user"
                        ? "rounded-lg bg-pc-elevated p-3 text-sm whitespace-pre-wrap"
                        : "chat-markdown text-sm break-words"
                    }
                  >
                    {sopAssistant && message.role === 'user' ? (
                      <SopPromptMessage message={message.content} />
                    ) : sopAssistant && message.role === 'assistant' ? (
                      <SopAssistantMessage message={message.content} onApply={sopAssistant.onApply} />
                    ) : (
                      <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown>
                    )}
                  </div>
                ),
              )}
              {stream && (
                <div className="chat-markdown text-sm break-words">
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>
                    {stream}
                  </ReactMarkdown>
                </div>
              )}
              <div ref={endRef} />
            </div>
            {approval && (
              <ApprovalBanner
                pending={approval}
                onRespond={(decision) => void respondApproval(decision)}
              />
            )}
            {question && (
              <CodeQuestion
                key={question.id}
                request={question}
                onRespond={(result) => {
                  clientRef.current?.respond(question.id, result);
                  setQuestion(null);
                }}
              />
            )}
            <form
              className="border-t border-pc-border p-3"
              onSubmit={(event) => {
                event.preventDefault();
                void send();
              }}
            >
              {sopAssistant && contextText && (
                <details className="mb-3 text-xs text-pc-text-muted">
                  <summary className="cursor-pointer">{t('sop_workspace.context_included')}</summary>
                  <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-pc-elevated p-2">{contextText}</pre>
                </details>
              )}
              {contextText && !sopAssistant && (
                <button
                  type="button"
                  className="mb-2 text-xs text-pc-accent"
                  onClick={() =>
                    setPrompt(
                      `${prompt}\n\n${t("workspace.sop_definition")}:\n\`\`\`json\n${contextText}\n\`\`\``.trim(),
                    )
                  }
                >
                  {t("workspace.insert_sop")}
                </button>
              )}
              <textarea
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
                rows={3}
                aria-label={t(sopAssistant ? "sop_workspace.helper_prompt" : "code.prompt")}
                placeholder={t(sopAssistant ? "sop_workspace.helper_prompt" : "code.prompt")}
                disabled={!ready}
                className="input-electric w-full resize-y p-3 text-sm"
                onKeyDown={(event) => {
                  if (
                    (event.metaKey || event.ctrlKey) &&
                    event.key === "Enter"
                  ) {
                    event.preventDefault();
                    void send();
                  }
                }}
              />
              <div className="mt-2 flex items-center justify-between gap-2">
                <p className="text-xs text-pc-text-muted">
                  {t("code.send_hint")}
                </p>
                {busy ? (
                  <button
                    type="button"
                    className="btn-secondary flex items-center gap-2 px-3 py-2 text-sm"
                    onClick={() => {
                      if (session)
                        void clientRef.current
                          ?.request("session/cancel", {
                            session_id: session.session_id,
                          })
                          .catch((error: Error) => setError(error.message));
                    }}
                  >
                    <Square className="h-4 w-4" />
                    {t("acp.cancel")}
                  </button>
                ) : (
                  <button
                    disabled={!ready || !prompt.trim()}
                    className="btn-primary flex items-center gap-2 px-3 py-2 text-sm disabled:opacity-40"
                  >
                    <Send className="h-4 w-4" />
                    {t("agent.send")}
                  </button>
                )}
              </div>
            </form>
          </section>
          {!embedded && alias && available && (
            <details
              className="shrink-0 rounded-xl border border-pc-border"
              open
            >
              <summary className="cursor-pointer px-4 py-2 text-xs text-pc-text-muted">
                {t("code.files")}
              </summary>
              <CodeFiles key={alias} alias={alias} refresh={refresh} />
            </details>
          )}
        </div>
      </div>
    </div>
  );
}

function CodeFiles({ alias, refresh }: { alias: string; refresh: number }) {
  const { theme } = useTheme();
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<BrowseEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let cancelled = false;
    listAgentWorkspace(alias, path)
      .then((result) => {
        if (!cancelled) {
          setEntries(result.entries);
          setError(null);
        }
      })
      .catch((error: Error) => {
        if (!cancelled) setError(error.message);
      });
    return () => {
      cancelled = true;
    };
  }, [alias, path, refresh, tick]);
  useEffect(() => {
    if (!selected) return;
    let cancelled = false;
    setText("");
    readAgentWorkspaceFile(alias, selected)
      .then((file) => {
        if (!cancelled) {
          setText(file.is_text ? file.content : t("code.binary"));
          setError(null);
        }
      })
      .catch((error: Error) => {
        if (!cancelled) setError(error.message);
      });
    return () => {
      cancelled = true;
    };
  }, [alias, selected, refresh, tick]);
  return (
    <section
      className="rounded-xl border border-pc-border bg-pc-surface overflow-hidden min-w-0"
      aria-label={t("code.files")}
    >
      <div className="flex items-center gap-2 border-b border-pc-border p-3">
        <span className="ml-auto text-xs text-pc-text-muted">
          {t("code.read_only")}
        </span>
        <button
          type="button"
          onClick={() => setTick((value) => value + 1)}
          aria-label={t("common.refresh")}
          className="p-2"
        >
          <RefreshCw className="h-4 w-4" />
        </button>
      </div>
      {error && (
        <p role="alert" className="p-3 text-sm text-status-error">
          {error}
        </p>
      )}
      <div className="grid sm:grid-cols-[10rem_minmax(0,1fr)] min-h-[10rem]">
        <nav
          aria-label={t("code.files")}
          className="border-b sm:border-b-0 sm:border-r border-pc-border p-2 max-h-60 overflow-auto"
        >
          {path && (
            <button
              type="button"
              className="flex items-center gap-2 p-2 text-xs"
              onClick={() => setPath(path.split("/").slice(0, -1).join("/"))}
            >
              <ArrowUp className="h-4 w-4" />
              {t("code.parent")}
            </button>
          )}
          {entries.map((entry) => {
            const full = path ? `${path}/${entry.name}` : entry.name;
            return (
              <button
                type="button"
                key={entry.name}
                onClick={() =>
                  entry.kind === "dir" ? setPath(full) : setSelected(full)
                }
                className={`flex w-full items-center gap-2 rounded p-2 text-left text-xs hover:bg-pc-elevated ${selected === full ? "bg-pc-elevated text-pc-accent" : ""}`}
                title={entry.name}
              >
                {entry.kind === "dir" ? (
                  <Folder className="h-4 w-4 shrink-0" />
                ) : (
                  <FileText className="h-4 w-4 shrink-0" />
                )}
                <span className="truncate">{entry.name}</span>
              </button>
            );
          })}
        </nav>
        <div className="min-w-0">
          {selected ? (
            <>
              <p
                className="truncate border-b border-pc-border p-3 text-xs font-mono"
                title={selected}
              >
                {selected}
              </p>
              <CodeMirror
                value={text}
                readOnly
                editable={false}
                theme={theme === "light" ? githubLight : oneDark}
                height="14rem"
                aria-label={selected}
                basicSetup={{
                  lineNumbers: true,
                  highlightActiveLine: false,
                  foldGutter: false,
                }}
              />
            </>
          ) : (
            <p className="p-6 text-sm text-pc-text-muted">
              {t("code.select_file")}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}

function CodeQuestion({
  request,
  onRespond,
}: {
  request: AcpRequest;
  onRespond: (result: unknown) => void;
}) {
  const params = request.params as {
    message?: string;
    requestedSchema?: {
      properties?: Record<
        string,
        {
          type?: string;
          minItems?: number;
          maxItems?: number;
          oneOf?: { const: string; title: string }[];
          items?: { anyOf?: { const: string; title: string }[] };
        }
      >;
    };
  };
  const [values, setValues] = useState<Record<string, string[]>>({});
  const properties = Object.entries(params.requestedSchema?.properties ?? {});
  const invalid =
    properties.length === 0 ||
    properties.some(([name, property]) => {
      const count = values[name]?.length ?? 0;
      return (
        !(property.oneOf ?? property.items?.anyOf)?.length ||
        count < (property.minItems ?? 1) ||
        count >
          (property.maxItems ?? (property.type === "array" ? Infinity : 1))
      );
    });
  return (
    <form
      className="border-t border-pc-border p-4 space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (invalid) return;
        onRespond({
          action: "accept",
          content: Object.fromEntries(
            properties.map(([name, property]) => [
              name,
              property.type === "array"
                ? (values[name] ?? [])
                : values[name]?.[0],
            ]),
          ),
        });
      }}
    >
      <p className="text-sm font-medium">{params.message}</p>
      {properties.map(([name, property]) => (
        <fieldset key={name} className="space-y-2">
          <legend className="sr-only">{name}</legend>
          {(property.oneOf ?? property.items?.anyOf ?? []).map((option) => (
            <label
              key={option.const}
              className="flex items-center gap-2 text-sm"
            >
              <input
                type={property.type === "array" ? "checkbox" : "radio"}
                name={name}
                value={option.const}
                checked={values[name]?.includes(option.const) ?? false}
                onChange={(event) =>
                  setValues((current) => ({
                    ...current,
                    [name]:
                      property.type === "array"
                        ? event.target.checked
                          ? [...(current[name] ?? []), option.const]
                          : (current[name] ?? []).filter(
                              (value) => value !== option.const,
                            )
                        : [option.const],
                  }))
                }
              />
              {option.title}
            </label>
          ))}
        </fieldset>
      ))}
      <div className="flex gap-3">
        <button
          type="button"
          onClick={() => onRespond({ action: "decline" })}
          className="btn-secondary px-3 py-2 text-sm"
        >
          {t("common.cancel")}
        </button>
        <button
          disabled={invalid}
          className="btn-primary px-3 py-2 text-sm disabled:opacity-40"
        >
          {t("common.confirm")}
        </button>
      </div>
    </form>
  );
}
