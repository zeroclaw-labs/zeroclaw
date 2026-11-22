import { AcpWebSocketClient, type AcpRequest } from "@/lib/acp";
import { AUTH_EVENTS, isReauthPending } from "@/lib/auth";
import { t } from "@/lib/i18n";
import type { ApprovalDecision, PendingApproval, Session } from "@/types/api";

export interface CodeMessage {
  role: string;
  content: string;
  kind?: "message" | "tool_call" | "tool_result";
  tool_call_id?: string;
  tool_name?: string;
  tool_input?: unknown;
  tool_output?: string;
}

export interface CodeModel {
  model: string;
  model_provider: string;
}

export interface CodeSettings extends CodeModel {
  models: CodeModel[];
  reasoning_effort: string | null;
  reasoning_options: string[];
  can_change: boolean;
}

export type CodeSettingsPatch = CodeModel | { reasoning_effort: string };

interface SessionIdentity {
  session_id: string;
  agent_alias: string;
}

export type CodeHistory = Omit<Session, "agent_alias"> & SessionIdentity & {
  interaction_surface?: string;
  state?: "running" | "idle";
};

export interface CodePlanEntry {
  content: string;
  activeForm?: string;
  status: "pending" | "in_progress" | "completed";
}

export interface CodeChat {
  alias: string;
  attached: boolean;
  busy: boolean;
  observed: boolean;
  turn?: number;
  messages: CodeMessage[];
  stream: string;
  plan: CodePlanEntry[];
  error: string | null;
  approval: PendingApproval | null;
  question: AcpRequest | null;
  answering: boolean;
  refresh: number;
  historyCursor?: string;
  settings?: CodeSettings;
  settingsBusy?: boolean;
  nextSettings?: CodeSettingsPatch;
}

interface Snapshot {
  ready: boolean;
  loaded: boolean;
  error: string | null;
  history: CodeHistory[];
  chats: ReadonlyMap<string, CodeChat>;
}

interface HistoryPage {
  messages: CodeMessage[];
  next_cursor?: string;
  has_older?: boolean;
}

interface SessionState {
  state: "running" | "idle";
  plan?: CodePlanEntry[];
  code_settings?: CodeSettings;
}

type Update = { session_id: string } & (
  | { type: "agent_message_chunk"; text: string }
  | { type: "tool_call"; tool_call_id: string; name: string; raw_input: unknown }
  | { type: "tool_result"; tool_call_id: string; name: string; raw_output: string }
  | { type: "plan"; entries: CodePlanEntry[] }
  | { type: "approval_request"; request_id: string; tool_name: string; arguments_summary: string; timeout_secs: number }
  | { type: "turn_complete"; content: string; outcome?: "completed" | "cancelled" | "failed"; client_turn_generation?: number }
);

const newChat = (alias: string): CodeChat => ({
  alias, attached: false, busy: false, observed: false, messages: [], stream: "",
  plan: [], error: null, approval: null, question: null, answering: false, refresh: 0,
});

const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

function readIdentity(key: string): { tui_id?: string; tui_sig?: string } {
  try {
    const value = JSON.parse(sessionStorage.getItem(key) ?? "{}");
    if (typeof value?.tui_id === "string" && typeof value?.tui_sig === "string") return value;
  } catch { /* The daemon can issue a fresh identity when storage is unavailable. */ }
  return {};
}

/** The socket owns all turns started in this workspace. Each asynchronous
 * result is fenced by that socket and applied to the exact conversation. */
export class CodeConnection {
  private snapshot: Snapshot = { ready: false, loaded: false, error: null, history: [], chats: new Map() };
  private readonly listeners = new Set<() => void>();
  private readonly opening = new Map<string, Promise<void>>();
  private readonly recovering = new Set<string>();
  private client: AcpWebSocketClient | null = null;
  private catalogPending = false;
  private catalogRevision = 0;
  private turn = 0;
  private wanted = false;
  private reconnect?: ReturnType<typeof setTimeout>;
  private poll?: ReturnType<typeof setInterval>;

  constructor(private readonly identityKey: string) {}

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  getSnapshot = () => this.snapshot;

  private publish(patch: Partial<Snapshot>) {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const listener of this.listeners) listener();
  }

  private edit(id: string, apply: (chat: CodeChat) => CodeChat) {
    const current = this.snapshot.chats.get(id);
    if (!current) return;
    const next = apply(current);
    if (current !== next) this.publish({ chats: new Map(this.snapshot.chats).set(id, next) });
  }

  start = () => {
    this.wanted = true;
    window.addEventListener(AUTH_EVENTS.resumed, this.retry);
    this.retry();
    this.poll = setInterval(() => void this.pollSessions(), 3000);
    return () => {
      this.wanted = false;
      window.removeEventListener(AUTH_EVENTS.resumed, this.retry);
      clearInterval(this.poll);
      clearTimeout(this.reconnect);
      const old = this.client;
      this.client = null;
      this.opening.clear();
      this.recovering.clear();
      this.catalogPending = false;
      this.publish({ ready: false });
      old?.disconnect();
    };
  };

  retry = () => {
    if (!this.wanted || this.client || isReauthPending()) return;
    clearTimeout(this.reconnect);
    const client = new AcpWebSocketClient({
      onOpen: () => void this.initialize(client),
      onClose: (event) => {
        if (client !== this.client) return;
        if (event.code === 4401 || event.code === 4403) {
          window.dispatchEvent(new Event(event.code === 4401 ? AUTH_EVENTS.sessionEnded : AUTH_EVENTS.accessLost));
        }
        this.disconnected(client);
      },
      onNotification: ({ method, params }) => {
        if (client === this.client && method === "session/update") this.update(params as Update);
      },
      onRequest: (request) => {
        if (client !== this.client) return;
        const params = request.params as { sessionId?: string } | undefined;
        const id = params?.sessionId;
        const chat = id ? this.snapshot.chats.get(id) : undefined;
        if (request.method === "elicitation/create" && id && chat && chat.busy && !chat.question) {
          this.edit(id, (current) => ({ ...current, question: request }));
        } else {
          client.respondError(request.id, { code: -32601, message: "Unsupported Code request" });
        }
      },
    }, "/ws/code");
    this.client = client;
    try { client.connect(); }
    catch { this.disconnected(client); }
  };

  private disconnected(client: AcpWebSocketClient, error = t("code.disconnected")) {
    if (this.client !== client) return;
    this.client = null;
    client.disconnect();
    this.opening.clear();
    this.recovering.clear();
    this.catalogPending = false;
    const chats = new Map(this.snapshot.chats);
    for (const [id, chat] of chats) {
      chats.set(id, { ...chat, attached: false, observed: chat.busy, approval: null, question: null,
        answering: false, settingsBusy: false, turn: undefined,
        error: chat.approval || chat.question ? t("code.request_interrupted") : chat.error });
    }
    this.publish({ ready: false, error, chats });
    if (this.wanted && !isReauthPending()) this.reconnect = setTimeout(this.retry, 1500);
  }

  private async initialize(client: AcpWebSocketClient) {
    try {
      const identity = await client.request<{ tui_id?: string; tui_sig?: string }>("initialize", {
        ...readIdentity(this.identityKey),
        protocol_version: 1,
        clientCapabilities: { elicitation: { form: {} } },
      });
      if (this.client !== client) return;
      if (identity.tui_id && identity.tui_sig) {
        try { sessionStorage.setItem(this.identityKey, JSON.stringify({ tui_id: identity.tui_id, tui_sig: identity.tui_sig })); }
        catch { /* The connection is still valid. */ }
      }
      await this.refreshCatalog(client);
      if (this.client !== client) return;
      this.publish({ ready: true, error: null });
      for (const row of this.snapshot.history) {
        if (this.snapshot.chats.get(row.session_id)?.observed) {
          void this.attach(row).catch(() => {});
        }
      }
    } catch (error) {
      this.disconnected(client, errorText(error));
    }
  }

  private async refreshCatalog(client: AcpWebSocketClient) {
    if (this.catalogPending) return;
    this.catalogPending = true;
    const revision = this.catalogRevision;
    try {
      const { sessions } = await client.request<{ sessions: CodeHistory[] }>("session/list-acp");
      if (this.client !== client || revision !== this.catalogRevision) return;
      this.publish({ history: sessions.filter((row) =>
        row.interaction_surface === "zerocode_code" && !!row.agent_alias), loaded: true });
    } finally {
      if (this.client === client) this.catalogPending = false;
    }
  }

  private async pollSessions() {
    const client = this.client;
    if (!this.snapshot.ready || !client?.connected) return;
    try {
      await this.refreshCatalog(client);
      if (client !== this.client) return;
      for (const [id, chat] of this.snapshot.chats) {
        if (chat.observed && chat.attached) void this.reconcile(client, id).catch(() => {});
        else this.applyQueuedSettings(id);
      }
    } catch { /* Keep the last catalog until the next live poll. */ }
  }

  private requireClient() {
    if (!this.snapshot.ready || !this.client?.connected) throw new Error(t("code.disconnected"));
    return this.client;
  }

  private assertClient(client: AcpWebSocketClient) {
    if (client !== this.client) throw new Error(t("code.disconnected"));
  }

  private async readState(client: AcpWebSocketClient, id: string) {
    return client.request<SessionState>("session/state", { session_id: id, include_code_settings: true });
  }

  private async reconcile(client: AcpWebSocketClient, id: string, attaching = false) {
    if (this.recovering.has(id)) return;
    this.recovering.add(id);
    const before = this.snapshot.chats.get(id);
    try {
      const state = await this.readState(client, id);
      if (!attaching && state.state === "running") return;
      const page = await client.request<HistoryPage>("session/messages", { session_id: id, cursor: null, limit: 120 });
      this.assertClient(client);
      this.edit(id, (chat) => chat.turn !== before?.turn ? chat : ({
        ...chat, attached: true, busy: state.state === "running", observed: state.state === "running",
        stream: "", messages: page.messages, plan: state.plan ?? [], settings: state.code_settings,
        historyCursor: page.has_older ? page.next_cursor : undefined,
        approval: state.state === "running" ? chat.approval : null,
        question: state.state === "running" ? chat.question : null,
      }));
      this.applyQueuedSettings(id);
    } finally {
      if (client === this.client) this.recovering.delete(id);
    }
  }

  attach = (row: SessionIdentity): Promise<void> => {
    const id = row.session_id;
    const current = this.snapshot.chats.get(id);
    if (this.snapshot.ready && current?.attached) return Promise.resolve();
    const existing = this.opening.get(id);
    if (existing) return existing;
    const task = this.openExisting(row);
    this.opening.set(id, task);
    const clear = () => { if (this.opening.get(id) === task) this.opening.delete(id); };
    void task.then(clear, clear);
    return task;
  };

  private async openExisting({ session_id: id, agent_alias: alias }: SessionIdentity) {
    const client = this.requireClient();
    const chat = this.snapshot.chats.get(id) ?? newChat(alias);
    this.publish({ chats: new Map(this.snapshot.chats).set(id, {
      ...chat, error: chat.observed ? chat.error : null,
    }) });
    try {
      const resumed = await client.request<SessionIdentity>("session/new", { session_id: id, agent_alias: alias });
      this.assertClient(client);
      if (resumed.session_id !== id || resumed.agent_alias !== alias) throw new Error(t("code.session_unavailable"));
      await this.reconcile(client, id, true);
    } catch (error) {
      if (client === this.client) this.edit(id, (current) => ({ ...current, error: errorText(error) }));
      throw error;
    }
  }

  create = async (alias: string): Promise<SessionIdentity> => {
    const client = this.requireClient();
    const row = await client.request<SessionIdentity>("session/new", { agent_alias: alias });
    this.assertClient(client);
    if (!row.session_id || row.agent_alias !== alias) throw new Error(t("code.session_unavailable"));
    const now = new Date().toISOString();
    this.catalogRevision += 1;
    this.publish({
      history: [{ ...row, session_key: row.session_id, created_at: now, last_activity: now,
        message_count: 0, channel_id: null, interaction_surface: "zerocode_code" },
        ...this.snapshot.history.filter((entry) => entry.session_id !== row.session_id)],
      chats: new Map(this.snapshot.chats).set(row.session_id, { ...newChat(alias), attached: true }),
    });
    try {
      const state = await this.readState(client, row.session_id);
      this.assertClient(client);
      this.edit(row.session_id, (chat) => ({ ...chat, settings: state.code_settings }));
    } catch {
      this.assertClient(client);
    }
    return row;
  };

  loadOlder = async (id: string) => {
    const client = this.requireClient();
    const cursor = this.snapshot.chats.get(id)?.historyCursor;
    if (!cursor) return;
    const page = await client.request<HistoryPage>("session/messages", { session_id: id, cursor, limit: 120 });
    this.assertClient(client);
    this.edit(id, (chat) => chat.historyCursor !== cursor ? chat : ({
      ...chat, messages: [...page.messages, ...chat.messages],
      historyCursor: page.has_older ? page.next_cursor : undefined,
    }));
  };

  submit = (id: string, text: string) => {
    const client = this.requireClient();
    const chat = this.snapshot.chats.get(id);
    if (!chat?.attached || chat.busy || chat.settingsBusy) throw new Error(t("code.session_unavailable"));
    const turn = ++this.turn;
    this.edit(id, (current) => ({
      ...current, busy: true, observed: false, turn, error: null, plan: [], stream: "",
      messages: [...current.messages, { role: "user", content: text }],
    }));
    void this.runPrompt(client, id, text, turn);
  };

  private async runPrompt(client: AcpWebSocketClient, id: string, text: string, turn: number) {
    try {
      // The terminal notification releases the turn. The RPC result alone
      // never releases a slot whose work might still be running.
      await client.request("session/prompt", { session_id: id, prompt: text, client_turn_generation: turn }, 24 * 60 * 60 * 1000);
      if (client === this.client && this.snapshot.chats.get(id)?.turn === turn) {
        this.edit(id, (current) => ({ ...current, observed: true }));
        await this.reconcile(client, id);
      }
    } catch (error) {
      if (client === this.client && this.snapshot.chats.get(id)?.turn === turn) {
        this.edit(id, (current) => ({ ...current, observed: true, error: errorText(error) }));
        try { await this.reconcile(client, id); } catch { /* The next poll will retry. */ }
      }
    }
  }

  private update(update: Update | null) {
    const client = this.client;
    if (!update?.session_id || !client) return;
    const id = update.session_id;
    const chat = this.snapshot.chats.get(id);
    if (!chat || !chat.busy) return;
    if (update.type === "turn_complete" && update.client_turn_generation !== undefined
        && update.client_turn_generation !== chat.turn) return;

    switch (update.type) {
      case "agent_message_chunk":
        this.edit(id, (current) => ({ ...current, stream: current.stream + update.text }));
        break;
      case "tool_call":
      case "tool_result":
        this.edit(id, (current) => ({
          ...current,
          refresh: current.refresh + Number(update.type === "tool_result"),
          messages: [...current.messages, {
            role: "tool", kind: update.type, content: "", tool_call_id: update.tool_call_id,
            tool_name: update.name,
            ...(update.type === "tool_call" ? { tool_input: update.raw_input } : { tool_output: update.raw_output }),
          }],
        }));
        break;
      case "plan":
        this.edit(id, (current) => ({ ...current, plan: update.entries }));
        break;
      case "approval_request":
        this.edit(id, (current) => ({
          ...current, approval: {
            requestId: update.request_id, toolName: update.tool_name, argumentsSummary: update.arguments_summary,
            timeoutSecs: update.timeout_secs, receivedAt: Date.now(),
          },
        }));
        break;
      case "turn_complete":
        this.edit(id, (current) => ({
          ...current, busy: false, turn: undefined, observed: false,
          approval: null, question: null, answering: false, stream: "", refresh: current.refresh + 1,
          error: update.outcome === "failed" ? (update.content || t("code.turn_failed"))
            : update.outcome === "cancelled" ? t("code.turn_cancelled") : null,
          messages: update.outcome === "failed" && !current.stream ? current.messages : [...current.messages, {
            role: "assistant", content: (update.outcome !== "failed" && update.content) || current.stream || t("agent.turn_no_output"),
          }],
        }));
        void this.refreshCatalog(client).catch(() => {});
        void this.readState(client, id).then((state) => {
          if (client === this.client) this.edit(id, (current) => current.settingsBusy ? current : ({ ...current, settings: state.code_settings }));
        }).catch(() => {});
        break;
    }
  }

  cancel = async (id: string) => {
    const client = this.requireClient();
    if (this.snapshot.chats.get(id)?.attached && this.snapshot.chats.get(id)?.busy) {
      await client.request("session/cancel", { session_id: id });
    }
  };

  respondApproval = async (id: string, pending: PendingApproval, decision: ApprovalDecision) => {
    const client = this.requireClient();
    const chat = this.snapshot.chats.get(id);
    if (!chat?.attached || chat.approval !== pending || chat.answering) return;
    this.edit(id, (current) => ({ ...current, answering: true }));
    try {
      await client.request("session/approve", {
        session_id: id, request_id: pending.requestId,
        decision: decision === "approve" ? "allow_once" : decision === "always" ? "allow_always" : "reject",
      });
      if (client === this.client) this.edit(id, (current) => current.approval === pending ? { ...current, approval: null } : current);
    } finally {
      if (client === this.client) this.edit(id, (current) => ({ ...current, answering: false }));
    }
  };

  respondQuestion = (id: string, pending: AcpRequest, result: unknown) => {
    const client = this.requireClient();
    const chat = this.snapshot.chats.get(id);
    if (!chat?.attached || chat.question !== pending) return;
    client.respond(pending.id, result);
    this.edit(id, (current) => ({ ...current, question: null }));
  };

  configure = async (id: string, patch: CodeSettingsPatch) => {
    const client = this.requireClient();
    const chat = this.snapshot.chats.get(id);
    if (!chat?.attached || !chat.settings?.can_change || chat.settingsBusy) return;
    if (chat.busy) {
      this.edit(id, (current) => ({ ...current, nextSettings: patch }));
      return;
    }
    this.edit(id, (current) => ({ ...current, settingsBusy: true, nextSettings: undefined, error: null }));
    try {
      await client.request("session/configure", { session_id: id, overrides: patch });
      const state = await this.readState(client, id);
      this.assertClient(client);
      this.edit(id, (current) => ({ ...current, settings: state.code_settings }));
    } catch (error) {
      if (client === this.client) this.edit(id, (current) => ({ ...current, error: errorText(error) }));
    } finally {
      if (client === this.client) this.edit(id, (current) => ({ ...current, settingsBusy: false }));
    }
  };

  private applyQueuedSettings(id: string) {
    const chat = this.snapshot.chats.get(id);
    if (chat?.nextSettings && chat.attached && !chat.busy) void this.configure(id, chat.nextSettings).catch(() => {});
  }
}
