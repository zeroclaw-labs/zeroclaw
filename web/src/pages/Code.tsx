import { useContext, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { Link, useSearchParams, type NavigateOptions } from "react-router-dom";
import { Folder, PanelLeft, Plus, Search, Send, Settings, Square } from "lucide-react";
import ApprovalBanner from "@/components/ApprovalBanner";
import { useWorkspaceSettings } from "@/components/WorkspaceSettings";
import WorkspaceAttention from "@/components/layout/WorkspaceAttention";
import { useWorkspaceItems } from "@/components/layout/WorkspaceKeyboard";
import { useWorkspaceVisible } from "@/components/layout/WorkspaceOutlet";
import { useNavAccess, useOidcSession } from "@/hooks/useNavAccess";
import { DraftContext } from "@/hooks/useDraft";
import { getStatus, getWorkspaceAvailability } from "@/lib/api";
import { getLastPrincipalId } from "@/lib/auth";
import { loadCodeSelection, resumeCodeSession, saveCodeSelection } from "@/lib/codeSelection";
import { t } from "@/lib/i18n";
import { CodeConnection, type CodeHistory, type CodeModel, type CodeSettingsPatch } from "./code/CodeConnection";
import CodeFiles from "./code/CodeFiles";
import CodePanel, { useCodeLayout } from "./code/CodePanel";
import CodePicker from "./code/CodePicker";
import CodeQuestion from "./code/CodeQuestion";
import CodeTranscript, { type ReadingPosition } from "./code/CodeTranscript";

interface CodeProps {
  embedded?: boolean;
  contextText?: string;
  sopAssistant?: { onApply: (source: string) => Promise<void> };
  attentionTarget?: string;
  draftScope?: string;
  onAttentionOpen?: () => void;
  onBusyChange?: (busy: boolean) => void;
}

const modelKey = (model: CodeModel) => JSON.stringify([model.model_provider, model.model]);
const effortLabel = (effort: string) => t(`code.effort.${effort}`);

export default function Code({
  embedded = false, contextText, sopAssistant, onBusyChange, attentionTarget, draftScope, onAttentionOpen,
}: CodeProps) {
  const visible = useWorkspaceVisible();
  const access = useNavAccess();
  const account = useOidcSession();
  // The app keeps the same workspace behind its reauthentication screen and
  // reloads for a different principal. Preserve the draft namespace there.
  const owner = useRef(account ? JSON.stringify([account.provider, account.principal_id]) : getLastPrincipalId() ?? "paired").current;
  const draftStore = useContext(DraftContext);
  const openSettings = useWorkspaceSettings();
  const [connection] = useState(() => new CodeConnection(JSON.stringify(["code-transport", owner, draftScope ?? "workspace"])));
  const snapshot = useSyncExternalStore(connection.subscribe, connection.getSnapshot);
  function codeTitle(row: CodeHistory) {
    const first = snapshot.chats.get(row.session_id)?.messages.find((message) => message.role === "user");
    const preview = first?.content.trim().split("\n", 1)[0]?.slice(0, 80);
    return row.name || preview || `${row.agent_alias} · ${row.session_id.slice(0, 8)}`;
  }
  const [routeParams, setRouteParams] = useSearchParams();
  const [inlineParams, setInlineParams] = useState(() => new URLSearchParams({ new: "1" }));
  const lastParams = useRef(routeParams);
  if (visible) lastParams.current = routeParams;
  const params = embedded ? inlineParams : lastParams.current;
  const setParams = (next: Record<string, string>, options?: NavigateOptions) => {
    if (embedded) setInlineParams(new URLSearchParams(next));
    else setRouteParams(next, options ?? { replace: true });
  };

  const [availability, setAvailability] = useState<{ code: boolean; agents: string[] }>();
  const [availabilityError, setAvailabilityError] = useState<string>();
  const [availabilityRetry, setAvailabilityRetry] = useState(0);
  useEffect(() => {
    let cancelled = false;
    void getWorkspaceAvailability().then((result) => {
      if (!cancelled) { setAvailability(result); setAvailabilityError(undefined); }
    }).catch((error: Error) => { if (!cancelled) setAvailabilityError(error.message); });
    return () => { cancelled = true; };
  }, [availabilityRetry]);
  useEffect(() => {
    if (availability?.code) return connection.start();
  }, [availability?.code, connection]);

  const history = snapshot.history.filter((item) => availability?.agents.includes(item.agent_alias));
  const requestedId = params.get("session");
  const requestedAgent = params.get("agent");
  const selected = requestedId ? history.find((row) => row.session_id === requestedId &&
    (!requestedAgent || row.agent_alias === requestedAgent))
    : !params.has("new") && !embedded ? resumeCodeSession(history, availability?.agents ?? []) as CodeHistory | undefined : undefined;
  const inlineChat = embedded && requestedId ? snapshot.chats.get(requestedId) : undefined;
  const alias = selected?.agent_alias ?? inlineChat?.alias ?? (requestedAgent && availability?.agents.includes(requestedAgent) ? requestedAgent
    : availability?.agents.find((item) => item === loadCodeSelection()?.agent) ?? availability?.agents[0] ?? "");
  const id = selected?.session_id ?? (inlineChat ? requestedId ?? undefined : undefined);
  const [draftModel, setDraftModel] = useState<string>();
  useEffect(() => {
    setDraftModel(undefined);
    if (id || !visible || !alias || !access.operatorPages) return;
    let cancelled = false;
    void getStatus(alias).then((status) => {
      if (!cancelled) setDraftModel(status.model);
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [access.operatorPages, alias, id, visible]);
  const chat = id ? snapshot.chats.get(id) : undefined;
  const busy = Boolean(chat?.busy);
  const anyBusy = [...snapshot.chats.values()].some((current) => current.busy);
  useEffect(() => { onBusyChange?.(anyBusy); }, [anyBusy, onBusyChange]);
  useEffect(() => {
    if (!anyBusy) return;
    const protect = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", protect);
    return () => window.removeEventListener("beforeunload", protect);
  }, [anyBusy]);

  useEffect(() => {
    if (!visible || !snapshot.ready || !id || !alias) return;
    if (!embedded) {
      saveCodeSelection(alias, id);
      if (!requestedId) setRouteParams({ agent: alias, session: id }, { replace: true });
    }
    void connection.attach({ session_id: id, agent_alias: alias }).catch(() => {});
  }, [alias, connection, embedded, id, requestedId, setRouteParams, snapshot.ready, visible]);

  const { ref, narrow, layout, setPane, filesWidth, filesMax, sessionsWidth, sessionsMax } = useCodeLayout(owner);
  const showFiles = !embedded && access.operatorPages && layout.files.open && !!alias && !!availability?.code;
  const showSessions = !embedded && layout.sessions.open && !(narrow && showFiles);
  const narrowOverlay = narrow && (showSessions || showFiles);
  const sessionTrigger = useRef<HTMLButtonElement>(null);
  const filesTrigger = useRef<HTMLButtonElement>(null);
  const [query, setQuery] = useState("");
  const [picker, setPicker] = useState<"model" | "reasoning">();
  const [workingKey, setWorkingKey] = useState<string>();
  const [, redraw] = useState(0);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const operation = useRef(new Set<string>());
  const reading = useRef(new Map<string, ReadingPosition>());
  const carets = useRef(new Map<string, [number, number]>());
  const textarea = useRef<HTMLTextAreaElement>(null);
  const keyFor = (session?: string) => JSON.stringify(["code", owner, draftScope ?? attentionTarget ?? "workspace", alias, session ?? "new"]);
  const key = keyFor(id);
  const selection = useRef({ key, visible, setParams });
  selection.current = { key, visible, setParams };
  const position = reading.current.get(key) ?? { top: 0, follows: true };
  reading.current.set(key, position);
  const prompt = draftStore.getDraft(key);
  const settings = chat?.settings;
  const queuedModel = chat?.nextSettings?.model;
  const reasoningOptions = queuedModel
    ? settings?.models.find((model) => model.model === queuedModel && model.model_provider === chat?.nextSettings?.model_provider)?.reasoning_options ?? []
    : settings?.reasoning_options ?? [];
  // The order is fixed by creation, so new activity, approvals and refreshed
  // labels cannot reshuffle the keys under someone's hand.
  const rows = history.filter((item) =>
    `${codeTitle(item)} ${item.agent_alias} ${item.session_id}`.toLowerCase().includes(query.toLowerCase()))
    .sort((a, b) => b.created_at.localeCompare(a.created_at) || a.session_id.localeCompare(b.session_id));
  const missing = requestedId !== null && snapshot.loaded && !id;

  const select = (row: CodeHistory) => {
    setPicker(undefined);
    setParams({ agent: row.agent_alias, session: row.session_id });
    if (narrow && showSessions) setPane("sessions", { open: false });
  };
  useWorkspaceItems("code", visible && !embedded, {
    ids: rows.map((row) => row.session_id), selected: id,
    select: (next) => {
      const row = rows.find((item) => item.session_id === next);
      if (row) select(row);
    },
  });
  useLayoutEffect(() => {
    const range = carets.current.get(key);
    if (range) textarea.current?.setSelectionRange(...range);
  }, [key]);
  useEffect(() => {
    if (visible && showSessions) ref.current?.querySelector('#code-sessions [aria-current="page"]')?.scrollIntoView({ block: "nearest" });
  }, [id, ref, showSessions, visible]);

  // On desktop the drawer only handles Escape when focus is on its controls.
  useEffect(() => {
    if (!visible || !showFiles || narrow) return;
    const escape = (event: KeyboardEvent) => {
      const target = event.target;
      if (event.key === "Escape" && !event.defaultPrevented && !event.isComposing && target instanceof HTMLElement &&
          (target === filesTrigger.current || target.closest("#code-files")) &&
          !document.querySelector('dialog:modal, [role="dialog"][aria-modal="true"]')) {
        event.preventDefault();
        setPane("files", { open: false });
        filesTrigger.current?.focus({ preventScroll: true });
      }
    };
    document.addEventListener("keydown", escape);
    return () => document.removeEventListener("keydown", escape);
  }, [narrow, showFiles, visible, setPane]);

  const reportError = (target: string, error: unknown) =>
    setErrors((current) => ({ ...current, [target]: error instanceof Error ? error.message : String(error) }));
  const clearError = (target: string) =>
    setErrors((current) => {
      if (current[target] === undefined) return current;
      const next = { ...current };
      delete next[target];
      return next;
    });

  const ensureSession = async () => {
    if (id) return id;
    if (!alias || missing) throw new Error(t("code.session_unavailable"));
    const row = await connection.create(alias);
    const nextKey = keyFor(row.session_id);
    const text = draftStore.getDraft(key);
    if (text) {
      draftStore.setDraft(nextKey, text);
      draftStore.clearDraft(key);
    }
    const caret = carets.current.get(key);
    if (caret) carets.current.set(nextKey, caret);
    if (!embedded) saveCodeSelection(alias, row.session_id);
    if (selection.current.key === key) selection.current.setParams({ agent: alias, session: row.session_id });
    return row.session_id;
  };

  const send = async () => {
    if (!prompt.trim() || !snapshot.ready || busy || missing || operation.current.has(key) || chat?.settingsBusy || chat?.nextSettings) return;
    operation.current.add(key);
    setWorkingKey(key);
    clearError(key);
    let target = key;
    try {
      const currentId = await ensureSession();
      target = keyFor(currentId);
      const text = sopAssistant && contextText
        ? `${prompt.trim()}\n\n${t("sop_workspace.assistant_instructions")}\n\n\`\`\`json\n${contextText}\n\`\`\``
        : prompt.trim();
      position.follows = true;
      await connection.submit(currentId, text);
      if (draftStore.getDraft(target) === prompt) draftStore.clearDraft(target);
      redraw((value) => value + 1);
    } catch (error) { reportError(target, error); }
    finally {
      operation.current.delete(key);
      setWorkingKey((current) => current === key ? undefined : current);
    }
  };

  const openPicker = async (type: "model" | "reasoning") => {
    if (operation.current.has(key)) return;
    if (id) { setPicker(type); return; }
    operation.current.add(key);
    setWorkingKey(key);
    try {
      const createdId = await ensureSession();
      if (selection.current.visible && [key, keyFor(createdId)].includes(selection.current.key)) setPicker(type);
    } catch (error) { reportError(key, error); }
    finally {
      operation.current.delete(key);
      setWorkingKey((current) => current === key ? undefined : current);
    }
  };
  const applySettings = (patch: CodeSettingsPatch) => {
    setPicker(undefined);
    if (id) void connection.configure(id, patch).catch((error) => reportError(key, error));
  };

  const error = errors[key] ?? chat?.error ?? snapshot.error ?? availabilityError;
  const canCompose = snapshot.ready && !missing && !!alias && (!id || chat?.attached === true);
  const canPick = canCompose && (settings?.can_change ?? (!id && access.operatorPages)) && !chat?.settingsBusy && workingKey !== key;
  const status = (row: CodeHistory) => {
    const current = snapshot.chats.get(row.session_id);
    if (current?.approval || current?.question) return { text: t("code.needs_input"), color: "text-status-warning" };
    if (current?.busy || row.state === "running") return { text: t("code.working"), color: "text-pc-accent" };
    return { text: t("code.ready"), color: "text-pc-text-muted" };
  };

  return (
    <div ref={ref} className="relative flex h-full min-h-0 min-w-0 overflow-hidden bg-pc-base">
      {showSessions && (
        <CodePanel side="left" id="code-sessions" label={t("code.history")} narrow={narrow} visible={visible}
          width={sessionsWidth} minimum={154} maximum={sessionsMax} onResize={(width) => setPane("sessions", { width })}
          onClose={() => { setPane("sessions", { open: false }); requestAnimationFrame(() => sessionTrigger.current?.focus({ preventScroll: true })); }}
          actions={
            <button type="button" aria-label={t("code.new_session")} title={t("code.new_session")} disabled={!alias}
              onClick={() => {
                setPicker(undefined);
                setParams({ agent: alias, new: "1" });
                if (narrow) setPane("sessions", { open: false });
              }} className="rounded-md p-1.5 text-pc-accent hover:bg-pc-elevated disabled:opacity-40">
              <Plus className="h-4 w-4" aria-hidden="true" />
            </button>
          }>
          <label className="m-2 flex min-w-0 items-center gap-1.5 rounded-md border border-pc-border px-2 py-1.5 text-xs">
            <Search className="h-3.5 w-3.5 shrink-0 text-pc-text-muted" aria-hidden="true" />
            <input className="w-full min-w-0 bg-transparent" aria-label={t("home.search_sessions")}
              placeholder={t("home.search_sessions")} value={query} onChange={(event) => setQuery(event.target.value)} />
          </label>
          <nav aria-label={t("code.history")} className="min-h-0 flex-1 space-y-0.5 overflow-y-auto px-1.5 pb-3">
            {rows.map((row) => {
              const current = status(row);
              return (
                <button key={row.session_id} type="button" aria-current={id === row.session_id ? "page" : undefined}
                  onClick={() => select(row)} title={codeTitle(row)}
                  className={`w-full rounded-md border-l-2 px-2 py-2 text-left ${id === row.session_id
                    ? "border-pc-accent bg-pc-accent/10 text-pc-text" : "border-transparent text-pc-text-secondary hover:bg-pc-elevated"}`}>
                  <span className="block truncate text-xs font-semibold">{codeTitle(row)}</span>
                  <span className="mt-1 flex min-w-0 flex-wrap items-center gap-x-1.5 text-[0.6875rem]">
                    <span className="max-w-full truncate text-pc-text-muted">{row.agent_alias}</span>
                    <span className={current.color}>{current.text}</span>
                  </span>
                </button>
              );
            })}
            {!rows.length && <p className="p-2 text-xs text-pc-text-muted">{t(query ? "nav.cmdk.empty" : "workspace.no_code_sessions")}</p>}
          </nav>
        </CodePanel>
      )}
      <section data-workspace-conversation inert={narrowOverlay || undefined} aria-label={t("code.conversation")} className="flex min-h-0 min-w-0 flex-1 flex-col">
        {[...snapshot.chats.entries()].map(([sid, current]) => (
          (current.approval || current.question) && (!visible || embedded || id !== sid) ? (
            <WorkspaceAttention key={sid} to={attentionTarget ?? `/code?${new URLSearchParams({ agent: current.alias, session: sid })}`}
              label={`${t("nav.code")} · ${history.find((row) => row.session_id === sid)?.name || current.alias}`} onOpen={onAttentionOpen} />
          ) : null
        ))}
        <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-x-2 gap-y-1.5 border-b border-pc-border px-3 py-2">
          {!embedded && (
            <>
              <button type="button" ref={sessionTrigger} aria-controls="code-sessions" aria-expanded={showSessions}
                onClick={() => {
                  if (narrow && !showSessions) setPane("files", { open: false });
                  setPane("sessions", { open: !showSessions });
                }}
                title={t("code.history")} aria-label={t("code.history")}
                className="rounded-md p-1.5 text-pc-text-secondary hover:bg-pc-elevated">
                <PanelLeft className="h-4 w-4" aria-hidden="true" />
              </button>
              {!showSessions && <button type="button" aria-label={t("code.new_session")} title={t("code.new_session")}
                disabled={!alias} onClick={() => { setPicker(undefined); setParams({ agent: alias, new: "1" }); }}
                className="rounded-md p-1.5 text-pc-accent hover:bg-pc-elevated disabled:opacity-40">
                <Plus className="h-4 w-4" aria-hidden="true" />
              </button>}
            </>
          )}
          {selected && !embedded
            ? <span className="max-w-48 truncate text-sm font-semibold" title={alias}>{alias}</span>
            : <select aria-label={t("code.agent")} value={alias} disabled={!availability?.agents.length || (embedded && busy)}
                onChange={(event) => { setPicker(undefined); setParams({ agent: event.target.value, new: "1" }); }}
                className="input-electric max-w-48 px-2 py-1 text-sm">
                {(availability?.agents ?? []).map((agent) => <option key={agent}>{agent}</option>)}
              </select>}
          <span role="status" className={`text-xs ${chat?.approval || chat?.question ? "text-status-warning" : busy ? "text-pc-accent" : "text-pc-text-muted"}`}>
            {t(availability?.code === false ? "code.offline" : chat?.approval || chat?.question ? "code.needs_input"
              : busy ? "code.working" : canCompose ? "code.ready" : "code.connecting")}
          </span>
          <div className="ml-auto flex items-center gap-2">
            {!embedded && access.operatorPages && availability?.code && (
              <button type="button" ref={filesTrigger} data-workspace-hint="f" aria-keyshortcuts="Shift+F" aria-controls="code-files" aria-expanded={showFiles}
                onClick={() => {
                  if (narrow && !showFiles) setPane("sessions", { open: false });
                  setPane("files", { open: !showFiles });
                }}
                className="flex items-center gap-1.5 rounded-md border border-pc-border px-2 py-1.5 text-xs text-pc-text-secondary hover:bg-pc-elevated">
                <Folder className="h-3.5 w-3.5" aria-hidden="true" /> {t("code.files")}
              </button>
            )}
            {access.config && <button type="button" disabled={!alias} onClick={() => openSettings(`/config/agents/${encodeURIComponent(alias)}`)}
              aria-label={`${t("workspace.settings")} · ${alias}`} className="rounded-md p-1.5 text-pc-text-muted hover:bg-pc-elevated">
              <Settings className="h-4 w-4" aria-hidden="true" />
            </button>}
          </div>
        </div>
        {(availability?.code === false || missing) && (
          <p role="status" className="px-4 py-3 text-sm text-pc-text-muted">
            {t(missing ? "code.session_unavailable" : "code.unavailable")}{" "}
            {availability?.code === false && access.config && <Link to="/config/gateway" className="text-pc-accent">{t("nav.feature_settings")}</Link>}
          </p>
        )}
        {error && (
          <div role="alert" className="mx-3 mt-2 flex shrink-0 items-center gap-2 rounded-md border border-status-error/30 bg-status-error/5 px-3 py-2 text-sm text-status-error">
            <span className="min-w-0 break-words">{error}</span>
            {(!snapshot.ready || (id && !chat?.attached)) && (
              <button type="button" className="shrink-0 underline" onClick={() => {
                if (id && snapshot.ready) void connection.attach({ session_id: id, agent_alias: alias }).catch(() => {});
                else if (availability?.code) connection.retry();
                else setAvailabilityRetry((value) => value + 1);
              }}>{t("code.reconnect")}</button>
            )}
          </div>
        )}
        <CodeTranscript key={key} alias={alias} messages={chat?.messages ?? []} stream={chat?.stream ?? ""}
          plan={chat?.plan ?? []} busy={busy} loaded={canCompose} visible={visible} position={position}
          hasOlder={!!chat?.historyCursor} onOlder={() => { if (id) void connection.loadOlder(id).catch((error) => reportError(key, error)); }}
          sopAssistant={sopAssistant} />
        {(chat?.approval || chat?.question) && (
          <div className="max-h-[35%] shrink-0 overflow-auto border-t border-status-warning/40 bg-status-warning/5">
            <p className="px-4 pt-2 text-xs font-semibold text-status-warning">
              {t("code.needs_input")} · {selected ? codeTitle(selected) : alias} ({alias})
            </p>
            <fieldset disabled={!snapshot.ready || !chat.attached || chat.answering}>
              {id && chat.approval && <ApprovalBanner pending={chat.approval} onRespond={(decision) => {
                if (chat.approval) void connection.respondApproval(id, chat.approval, decision).catch((error) => reportError(key, error));
              }} />}
              {id && chat.question && <CodeQuestion key={`${id}:${chat.question.id}`} request={chat.question}
                onRespond={(result) => {
                  try { if (chat.question) connection.respondQuestion(id, chat.question, result); }
                  catch (error) { reportError(key, error); }
                }} />}
            </fieldset>
          </div>
        )}
        <form onSubmit={(event) => { event.preventDefault(); void send(); }} className="shrink-0 border-t border-pc-border px-3 py-3 sm:px-5">
          <div className="mx-auto max-w-3xl">
            {sopAssistant && contextText && (
              <details className="mb-2 text-xs text-pc-text-muted">
                <summary className="cursor-pointer">{t("sop_workspace.context_included")}</summary>
                <pre className="mt-2 max-h-32 overflow-auto whitespace-pre-wrap break-words rounded bg-pc-elevated p-2">{contextText}</pre>
              </details>
            )}
            {contextText && !sopAssistant && (
              <button type="button" className="mb-2 text-xs text-pc-accent"
                onClick={() => {
                  draftStore.setDraft(key, `${prompt}\n\n${t("workspace.sop_definition")}:\n\`\`\`json\n${contextText}\n\`\`\``.trim());
                  redraw((value) => value + 1);
                }}>{t("workspace.insert_sop")}</button>
            )}
            <textarea ref={textarea} data-workspace-composer value={prompt} rows={2} disabled={!canCompose}
              aria-label={t(sopAssistant ? "sop_workspace.helper_prompt" : "code.prompt")} placeholder={t(sopAssistant ? "sop_workspace.helper_prompt" : "code.prompt")}
              className="input-electric max-h-[28vh] min-h-12 w-full resize-y px-3 py-2.5 text-sm"
              onSelect={(event) => carets.current.set(key, [event.currentTarget.selectionStart, event.currentTarget.selectionEnd])}
              onChange={(event) => {
                draftStore.setDraft(key, event.target.value);
                carets.current.set(key, [event.target.selectionStart, event.target.selectionEnd]);
                redraw((value) => value + 1);
              }}
              onKeyDown={(event) => {
                if ((event.metaKey || event.ctrlKey) && event.key === "Enter" && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  void send();
                }
              }} />
            <div className="mt-1 flex flex-wrap items-center justify-between gap-2">
              {!embedded ? <div className="flex min-w-0 flex-wrap items-center gap-1.5 text-xs">
                <button type="button" data-workspace-hint="m" aria-keyshortcuts="Shift+M"
                  onClick={() => void openPicker("model")} disabled={!canPick || (!!id && !settings?.models.length)}
                  className="max-w-56 truncate rounded-md border border-pc-border px-2 py-1 text-pc-text-secondary hover:bg-pc-elevated disabled:opacity-60">
                  {t("code.model")} · {settings?.model ?? ((!id && draftModel) || t("code.agent_default"))}
                </button>
                <button type="button" data-workspace-hint="r" aria-keyshortcuts="Shift+R"
                  onClick={() => void openPicker("reasoning")}
                  disabled={!canPick || (!!id && !reasoningOptions.length)}
                  title={id && !reasoningOptions.length ? t("code.reasoning_unavailable") : undefined}
                  className="rounded-md border border-pc-border px-2 py-1 text-pc-text-secondary hover:bg-pc-elevated disabled:opacity-60">
                  {t("code.reasoning")} · {id && !reasoningOptions.length ? t("code.unavailable_short") : effortLabel(settings?.reasoning_effort ?? "default")}
                </button>
                {(chat?.nextSettings || chat?.settingsBusy) && <span role="status" className="text-pc-text-muted">{t(chat.nextSettings ? "code.settings_queued" : "code.settings_applying")}</span>}
              </div> : <p className="text-xs text-pc-text-muted">{t("code.send_hint")}</p>}
              {busy ? <button type="button" disabled={!canCompose} onClick={() => {
                if (id) void connection.cancel(id).catch((error) => reportError(key, error));
              }} className="btn-secondary flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm">
                <Square className="h-3.5 w-3.5" aria-hidden="true" /> {t("acp.cancel")}
              </button> : <button type="submit" title={t("code.send_hint")}
                disabled={!canCompose || !prompt.trim() || workingKey === key || chat?.settingsBusy || !!chat?.nextSettings}
                className="btn-primary flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm disabled:opacity-45">
                <Send className="h-3.5 w-3.5" aria-hidden="true" /> {t("acp.send")}
              </button>}
            </div>
          </div>
        </form>
      </section>
      {showFiles && (
        <CodePanel side="right" id="code-files" label={t("code.files")} narrow={narrow} visible={visible}
          width={filesWidth} minimum={276} maximum={filesMax} onResize={(width) => setPane("files", { width })}
          onClose={() => { setPane("files", { open: false }); requestAnimationFrame(() => filesTrigger.current?.focus({ preventScroll: true })); }}>
          <CodeFiles key={alias} alias={alias} refresh={chat?.refresh ?? 0} />
        </CodePanel>
      )}
      {visible && picker === "model" && (settings
        ? <CodePicker label={t("code.model")} selected={modelKey(settings)}
            options={settings.models.map((model) => ({ id: modelKey(model), label: model.model, description: model.model_provider }))}
            onClose={() => setPicker(undefined)} onSelect={(choice) => {
              const model = settings.models.find((value) => modelKey(value) === choice);
              if (model) applySettings({ model: model.model, model_provider: model.model_provider });
            }} />
        : id ? <CodePicker label={t("code.model")} options={[]} onClose={() => setPicker(undefined)} onSelect={() => {}} /> : null)}
      {visible && picker === "reasoning" && id && (
        <CodePicker label={t("code.reasoning")} selected={chat?.nextSettings?.reasoning_effort ?? (queuedModel ? "default" : settings?.reasoning_effort ?? "default")}
          options={reasoningOptions.length ? ["default", ...reasoningOptions].map((effort) => ({ id: effort, label: effortLabel(effort) })) : []}
          onClose={() => setPicker(undefined)} onSelect={(effort) => applySettings({ reasoning_effort: effort })} />
      )}
    </div>
  );
}
