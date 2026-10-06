import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Bot,
  Crown,
  Plus,
  Save,
  X,
  Send,
  Pause,
  Play,
  Square,
  RefreshCw,
  ArrowRight,
  ChevronUp,
  ChevronDown,
} from "lucide-react";
import WorkspaceAttention from "@/components/layout/WorkspaceAttention";
import { useWorkspaceSettings } from "@/components/WorkspaceSettings";
import ColonyCanvas, {
  type ColonyCanvasNode,
  type ColonyCanvasWire,
} from "@/components/ColonyCanvas";
import { t } from "@/lib/i18n";
import { HttpError, putProp } from "@/lib/api";
import {
  activeGoal,
  blankColony,
  teamAliases,
  nodeKey,
  splitNodeKey,
  listColonies,
  getColony,
  createColony,
  saveColony,
  clarifyColony,
  createColonyGoal,
  controlColonyGoal,
  clarifyColonyGoal,
  colonyMessages,
  sendColonyMessage,
  getColonyContext,
  addColonyAgent,
  approveColonyCall,
  type ColonyConfig,
  type ColonyDetail,
  type ColonyAgent,
  type ColonySnapshot,
  type QueenProposal,
  type ColonyMessage,
  type ColonyGoal,
  type ColonyControlRequest,
} from "@/lib/colonies";

const input =
  "mt-1.5 w-full rounded-lg border border-pc-border bg-pc-elevated px-3 py-2 text-sm text-pc-text outline-none focus:border-pc-accent";
const button =
  "rounded-lg border border-pc-border px-3 py-2 text-xs hover:bg-pc-elevated disabled:opacity-40";
const memberId = (colony: string, key: string) => `${colony}::${key}`;
function unpack(id: string): [string | null, string] {
  const i = id.indexOf("::");
  return i < 0 ? [null, id] : [id.slice(0, i), id.slice(i + 2)];
}
/** Missing layout coordinates are a display projection, never a second policy
 * record. Explicitly saved positions always win, including deliberate overlap. */
function layoutPositions(definition: ColonyConfig) {
  const keys = [
    ...teamAliases(definition).map((alias) => nodeKey("agent", alias)),
    ...definition.prompts.map((p) => nodeKey("prompt", p.id)),
    ...definition.rooms.map((r) => nodeKey("room", r.id)),
    ...definition.channels.map((c) => nodeKey("channel", c.id)),
  ];
  const positions: ColonyConfig["positions"] = Object.fromEntries(
    keys.flatMap((key) =>
      definition.positions[key] ? [[key, definition.positions[key]]] : [],
    ),
  );
  const queen = positions[nodeKey("agent", definition.queen)] ?? {
    x: 500,
    y: 120 + Math.max(0, definition.members.length - 1) * 85,
  };
  keys.forEach((key, index) => {
    if (positions[key]) return;
    const position =
      index === 0
        ? { ...queen }
        : index <= definition.members.length
          ? { x: queen.x + 330, y: 120 + (index - 1) * 170 }
          : {
              x: queen.x + 660,
              y: 120 + (index - definition.members.length - 1) * 170,
            };
    while (
      Object.values(positions).some(
        (other) =>
          Math.abs(other.x - position.x) < 232 &&
          Math.abs(other.y - position.y) < 144,
      )
    )
      position.y += 170;
    positions[key] = position;
  });
  return positions;
}
function errorText(error: unknown): string {
  if (error instanceof HttpError) {
    const body = error.message.match(/^API \d+: (.*)$/s)?.[1];
    if (body) {
      try {
        const value: unknown = JSON.parse(body);
        if (
          value &&
          typeof value === "object" &&
          "error" in value &&
          typeof value.error === "string"
        )
          return value.error;
      } catch {
        /* Plain gateway error. */
      }
    }
  }
  return error instanceof Error ? error.message : String(error);
}
function statusLabel(goal?: ColonyGoal): string {
  if (goal?.approvals.some((approval) => approval.decision == null))
    return t("colony.waiting_approval");
  if (
    goal?.task.status === "paused" &&
    goal.execution.active_child_id &&
    goal.turn_attached &&
    goal.goal.pause_reason === "operator_paused"
  )
    return t("colony.pausing");
  return goal ? t(`colony.status_${goal.task.status}`) : t("colony.ready");
}
function Choices({
  label,
  aliases,
  values,
  onChange,
}: {
  label: string;
  aliases: string[];
  values: string[];
  onChange: (values: string[]) => void;
}) {
  return (
    <fieldset className="space-y-2">
      <legend className="mb-2 text-xs text-pc-text-muted">{label}</legend>
      {aliases.map((alias) => (
        <label key={alias} className="flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            checked={values.includes(alias)}
            onChange={(e) =>
              onChange(
                e.target.checked
                  ? [...values, alias]
                  : values.filter((a) => a !== alias),
              )
            }
          />
          {alias}
        </label>
      ))}
    </fieldset>
  );
}
function SettingsFields({
  value,
  onChange,
}: {
  value: ColonyConfig;
  onChange: (patch: Partial<ColonyConfig>) => void;
}) {
  return (
    <div className="space-y-4">
      <label className="block text-xs text-pc-text-muted">
        {t("colony.autonomy")}
        <select
          className={input}
          value={value.autonomy}
          onChange={(e) =>
            onChange({ autonomy: e.target.value as ColonyConfig["autonomy"] })
          }
        >
          {(["plan_only", "supervised", "autonomous"] as const).map((v) => (
            <option key={v} value={v}>
              {t(`colony.autonomy_${v}`)}
            </option>
          ))}
        </select>
      </label>
      <p className="text-[11px] leading-relaxed text-pc-text-muted">
        {t("colony.autonomy_hint")}
      </p>
      <label className="block text-xs text-pc-text-muted">
        {t("colony.start_mode")}
        <select
          className={input}
          value={value.start_mode}
          onChange={(e) =>
            onChange({
              start_mode: e.target.value as ColonyConfig["start_mode"],
            })
          }
        >
          <option value="review">{t("colony.start_review")}</option>
          <option value="automatic">{t("colony.start_automatic")}</option>
        </select>
      </label>
      <p className="text-[11px] leading-relaxed text-pc-text-muted">
        {t("colony.start_hint")}
      </p>
    </div>
  );
}
function AccessSummary({ agents }: { agents: ColonyAgent[] }) {
  const settings = useWorkspaceSettings();
  return (
    <details className="rounded-lg border border-pc-border p-3">
      <summary className="cursor-pointer text-xs font-medium">
        {t("colony.access")}
      </summary>
      <p className="my-3 text-[11px] leading-relaxed text-pc-text-muted">
        {t("colony.access_hint")}
      </p>
      {agents.map((agent) => (
        <div
          key={agent.alias}
          className="mb-3 break-words rounded bg-pc-elevated p-3 text-[11px]"
        >
          <h4 className="font-semibold">{agent.alias}</h4>
          <dl className="mt-2 space-y-1 text-pc-text-muted">
            <div>
              <dt className="inline">{t("colony.provider")}: </dt>
              <dd className="inline">{agent.model_provider}</dd>
            </div>
            <div>
              <dt className="inline">{t("colony.risk_profile")}: </dt>
              <dd className="inline">{agent.risk_profile}</dd>
            </div>
            <div>
              <dt className="inline">{t("colony.runtime_profile")}: </dt>
              <dd className="inline">{agent.runtime_profile}</dd>
            </div>
            <div>
              <dt className="inline">{t("colony.tools")}: </dt>
              <dd className="inline">
                {agent.access.allowed_tools?.join(", ") ??
                  t("colony.inherited_tools")}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.files")}: </dt>
              <dd className="inline">
                {agent.access.allowed_roots.join(", ") ||
                  (agent.access.workspace_only
                    ? t("colony.workspace_only")
                    : t("colony.inherited_files"))}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.approval_request")}: </dt>
              <dd className="inline">
                {agent.access.always_ask.join(", ") ||
                  t("colony.inherited_approvals")}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.excluded_tools")}: </dt>
              <dd className="inline">
                {agent.access.excluded_tools?.join(", ") || t("colony.none")}
              </dd>
            </div>
            {Object.entries(agent.access.network_domains ?? {}).map(
              ([tool, domains]) => (
                <div key={tool}>
                  <dt className="inline">
                    {t("colony.network")} · {tool}:{" "}
                  </dt>
                  <dd className="inline">
                    {domains.join(", ") || t("colony.no_network_domains")}
                  </dd>
                </div>
              ),
            )}
            <div>
              <dt className="inline">{t("colony.actions_hour")}: </dt>
              <dd className="inline">
                {agent.access.max_actions_per_hour ?? t("common.no_data")}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.daily_cost_cents")}: </dt>
              <dd className="inline">
                {agent.access.max_cost_per_day_cents ?? t("common.no_data")}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.daily_limit")}: </dt>
              <dd className="inline">
                {agent.access.daily_limit_usd ?? t("common.no_data")}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.monthly_limit")}: </dt>
              <dd className="inline">
                {agent.access.monthly_limit_usd ?? t("common.no_data")}
              </dd>
            </div>
            <div>
              <dt className="inline">{t("colony.cost_tracking")}: </dt>
              <dd className="inline">
                {agent.access.cost_tracking_enabled
                  ? t("common.yes")
                  : t("common.no")}
              </dd>
            </div>
          </dl>
          <div className="mt-3 flex flex-wrap gap-2">
            <button
              type="button"
              className="text-pc-accent underline"
              onClick={() =>
                settings(
                  `/config/risk_profiles/${encodeURIComponent(agent.risk_profile)}`,
                )
              }
            >
              {t("colony.risk_profile")}
            </button>
            <button
              type="button"
              className="text-pc-accent underline"
              onClick={() =>
                settings(
                  `/config/runtime_profiles/${encodeURIComponent(agent.runtime_profile)}`,
                )
              }
            >
              {t("colony.runtime_profile")}
            </button>
          </div>
        </div>
      ))}
    </details>
  );
}
function ContextSummary({
  value,
  agents,
  onChange,
}: {
  value: ColonyConfig;
  agents: ColonyAgent[];
  onChange: (patch: Partial<ColonyConfig>) => void;
}) {
  const [open, setOpen] = useState(false);
  const [sources, setSources] = useState<
    Record<string, ColonyAgent["context_sources"]>
  >({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const aliases = agents.map((a) => a.alias).join("\n");
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setLoading(true);
    setError("");
    void Promise.all(
      aliases
        .split("\n")
        .filter(Boolean)
        .map(async (alias) => {
          const response = await getColonyContext(alias, controller.signal);
          return [alias, response.sources] as const;
        }),
    )
      .then((entries) => {
        if (!controller.signal.aborted) setSources(Object.fromEntries(entries));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(errorText(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [open, aliases]);
  return (
    <details
      onToggle={(e) => setOpen(e.currentTarget.open)}
      className="rounded-lg border border-pc-border p-3"
    >
      <summary className="cursor-pointer text-xs font-medium">
        {t("colony.context")} · {value.baseline_context.length}
      </summary>
      {loading && (
        <p role="status" className="my-3 text-xs text-pc-text-muted">
          {t("colony.context_loading")}
        </p>
      )}
      {error && (
        <p role="alert" className="my-3 text-xs text-status-error">
          {error}
        </p>
      )}
      <p className="my-3 text-[11px] text-pc-text-muted">
        {t("colony.context_hint")}
      </p>
      {agents.map((agent) => (
        <fieldset key={agent.alias} className="mb-3">
          <legend className="mb-2 text-xs font-medium">{agent.alias}</legend>
          {(sources[agent.alias] ?? agent.context_sources).length ? (
            (sources[agent.alias] ?? agent.context_sources).map((source) => {
              const checked = value.baseline_context.some(
                (c) => c.agent === agent.alias && c.key === source.key,
              );
              return (
                <label
                  key={source.key}
                  className="mb-2 flex items-start gap-2 rounded bg-pc-elevated p-2 text-[11px]"
                >
                  <input
                    type="checkbox"
                    className="mt-0.5 shrink-0"
                    checked={checked}
                    onChange={(e) =>
                      onChange({
                        baseline_context: e.target.checked
                          ? [
                              ...value.baseline_context,
                              { agent: agent.alias, key: source.key },
                            ]
                          : value.baseline_context.filter(
                              (c) =>
                                !(
                                  c.agent === agent.alias &&
                                  c.key === source.key
                                ),
                            ),
                      })
                    }
                  />
                  <span className="min-w-0 break-words">
                    <span className="block font-medium">{source.key}</span>
                    <span className="block text-pc-text-muted">
                      {source.preview}
                    </span>
                  </span>
                </label>
              );
            })
          ) : (
            <p className="text-[11px] text-pc-text-faint">
              {t("colony.no_context")}
            </p>
          )}
        </fieldset>
      ))}
    </details>
  );
}

export default function ColonyWorkspace({
  visible,
  onOpenAgent,
  onShow,
  attentionTarget,
}: {
  visible: boolean;
  onOpenAgent: (alias: string) => void;
  onShow: () => void;
  attentionTarget: string;
}) {
  const settings = useWorkspaceSettings();
  const [snapshot, setSnapshot] = useState<ColonySnapshot | null>(null);
  const [error, setError] = useState("");
  const [refreshError, setRefreshError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [selection, setSelection] = useState<string[]>([]);
  const [inspect, setInspect] = useState<string | null>(null);
  const [selectedColony, setSelectedColony] = useState<string | null>(null);
  const [panel, setPanel] = useState<"setup" | "node" | "goal">("node");
  const [panelOpen, setPanelOpen] = useState(false);
  const [draft, setDraft] = useState<ColonyConfig | null>(null);
  const baseline = useRef<ColonyConfig | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [newId, setNewId] = useState("");
  const [queenTemplate, setQueenTemplate] = useState("");
  const [agentProfiles, setAgentProfiles] = useState<
    Record<string, { risk_profile: string; runtime_profile: string }>
  >({});
  const [pendingControl, setPendingControl] = useState<string | null>(null);
  const [coreCommands, setCoreCommands] = useState<Record<string, string>>({});
  const [agentCommand, setAgentCommand] = useState("");
  const [newAgent, setNewAgent] = useState<{
    alias: string;
    core_command: string;
    template: string;
  } | null>(null);
  const [newAgentSend, setNewAgentSend] = useState(true);
  const [newAgentReturn, setNewAgentReturn] = useState(true);
  const [objective, setObjective] = useState("");
  const [success, setSuccess] = useState("");
  const [constraints, setConstraints] = useState("");
  const [proposal, setProposal] = useState<QueenProposal | null>(null);
  const [answers, setAnswers] = useState<
    { question: string; answer: string }[]
  >([]);
  const [currentAnswers, setCurrentAnswers] = useState<Record<string, string>>(
    {},
  );
  const [pendingPlanAnswers, setPendingPlanAnswers] = useState<
    Record<string, Record<string, string>>
  >({});
  const [goalMode, setGoalMode] = useState<"fresh" | "continue">("fresh");
  const [previousGoal, setPreviousGoal] = useState("");
  const [goalTokens, setGoalTokens] = useState("");
  const [goalCost, setGoalCost] = useState("");
  const [conversation, setConversation] = useState<ColonyMessage[]>([]);
  const [messageDrafts, setMessageDrafts] = useState<Record<string, string>>(
    {},
  );
  const [roomAddress, setRoomAddress] = useState("");
  const [promptTiming, setPromptTiming] = useState<"now" | "next_run">(
    "next_run",
  );
  const [edgeFrom, setEdgeFrom] = useState("");
  const [edgeTo, setEdgeTo] = useState("");
  const [unassignedPositions, setUnassignedPositions] = useState<
    Record<string, { x: number; y: number }>
  >({});
  const nodePalette = useRef<HTMLDetailsElement>(null);
  const generation = useRef(0);
  const snapshotGeneration = useRef(0);
  const selectedRef = useRef(selectedColony);
  selectedRef.current = selectedColony;
  const detail = snapshot?.colonies.find((c) => c.id === selectedColony);
  const dirty =
    !!draft &&
    !!baseline.current &&
    JSON.stringify(draft) !== JSON.stringify(baseline.current);
  const hasUnsavedTeam = dirty || isNew;
  const currentGoal = detail ? activeGoal(detail) : undefined;
  const pendingAnswers = currentGoal
    ? (pendingPlanAnswers[currentGoal.task.id] ?? {})
    : {};
  const latestResult = detail?.goals[0]?.messages.find(
    (message) => message.id === detail.goals[0]?.execution.summary_message_id,
  );
  const team = draft ? teamAliases(draft) : [];
  const teamAgents = team.flatMap((alias) => {
    const source =
      snapshot?.agents.find((a) => a.alias === alias) ??
      (isNew && alias === draft?.queen
        ? snapshot?.agents.find((a) => a.alias === queenTemplate)
        : undefined);
    if (!source) return [];
    const profiles = isNew ? agentProfiles[alias] : undefined;
    return [
      {
        ...source,
        alias,
        core_command: isNew
          ? (coreCommands[alias] ?? source.core_command)
          : source.core_command,
        risk_profile: profiles?.risk_profile ?? source.risk_profile,
        runtime_profile: profiles?.runtime_profile ?? source.runtime_profile,
        access: {
          ...source.access,
          ...(profiles
            ? snapshot?.risk_profile_access?.[profiles.risk_profile]
            : {}),
          ...(profiles
            ? snapshot?.runtime_profile_access?.[profiles.runtime_profile]
            : {}),
        },
      },
    ];
  });
  const contextAgents = teamAgents.filter((agent) =>
    snapshot?.agents.some((source) => source.alias === agent.alias),
  );
  const inspectParts = inspect ? unpack(inspect) : [null, ""];
  const [kind, inspectedId] = splitNodeKey(inspectParts[1] ?? "");
  const agent =
    kind === "agent"
      ? snapshot?.agents.find((a) => a.alias === inspectedId)
      : undefined;
  const recipient =
    kind === "agent"
      ? inspectedId
      : kind === "room"
        ? `room:${inspectedId}`
        : "";
  const messageKey = `${selectedColony ?? ""}:${recipient}`;
  const message = messageDrafts[messageKey] ?? "";
  const setMessage = (value: string) =>
    setMessageDrafts((drafts) => ({ ...drafts, [messageKey]: value }));
  const [conversationId, setConversationId] = useState("");

  const refresh = useCallback(async (signal?: AbortSignal) => {
    const request = ++snapshotGeneration.current;
    const result = await listColonies(signal);
    if (request === snapshotGeneration.current) {
      setSnapshot(result);
      setRefreshError(false);
    }
    return result;
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    void refresh(controller.signal).catch((e: unknown) => {
      if (!controller.signal.aborted) setError(errorText(e));
    });
    const timer = window.setInterval(
      () => {
        void refresh(controller.signal).catch(() => {
          if (!controller.signal.aborted) setRefreshError(true);
        });
      },
      visible ? 3500 : 10000,
    );
    return () => {
      controller.abort();
      window.clearInterval(timer);
    };
  }, [visible, refresh]);
  // Polls may update progress, but never discard a local graph edit.
  useEffect(() => {
    if (!detail || dirty || isNew) return;
    baseline.current = detail.definition;
    setDraft(detail.definition);
  }, [detail, dirty, isNew]);
  useEffect(() => {
    if (!visible || !selectedColony || !recipient || panel !== "node") return;
    const version = ++generation.current,
      controller = new AbortController();
    const id = `${selectedColony}:${recipient}`;
    setConversationId(id);
    setConversation([]);
    const load = () =>
      colonyMessages(selectedColony, recipient, controller.signal).then(
        (result) => {
          if (generation.current === version && !controller.signal.aborted)
            setConversation(result.messages);
        },
      );
    void load().catch((e: unknown) => {
      if (!controller.signal.aborted) setError(errorText(e));
    });
    const timer = window.setInterval(() => {
      void load().catch(() => {
        if (!controller.signal.aborted) setRefreshError(true);
      });
    }, 3000);
    return () => {
      controller.abort();
      window.clearInterval(timer);
    };
  }, [visible, selectedColony, recipient, panel]);

  const update = (patch: Partial<ColonyConfig>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));
  const acceptDetail = (result: ColonyDetail, replaceDraft = true) => {
    setSnapshot((s) =>
      s
        ? {
            ...s,
            colonies: [...s.colonies.filter((c) => c.id !== result.id), result],
          }
        : s,
    );
    if (replaceDraft) {
      baseline.current = result.definition;
      setDraft(result.definition);
      setSelectedColony(result.id);
      setIsNew(false);
    }
  };
  const acceptGoal = (colonyId: string, result: ColonyGoal) =>
    setSnapshot((snapshot) =>
      snapshot
        ? {
            ...snapshot,
            colonies: snapshot.colonies.map((colony) =>
              colony.id === colonyId
                ? {
                    ...colony,
                    goals: [
                      result,
                      ...colony.goals.filter(
                        (goal) => goal.task.id !== result.task.id,
                      ),
                    ],
                  }
                : colony,
            ),
          }
        : snapshot,
    );
  const run = async (work: () => Promise<void>) => {
    if (busy) return;
    ++snapshotGeneration.current;
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const selectInspect = (id: string) => {
    if (busy) return;
    const [colonyId, key] = unpack(id);
    const targetColony = key.startsWith("colony:") ? key.slice(7) : colonyId;
    if (hasUnsavedTeam && (isNew || targetColony !== selectedColony)) {
      setError(t("colony.save_or_reset"));
      return;
    }
    setNewAgent(null);
    if (key.startsWith("colony:")) {
      const selected = key.slice(7),
        item = snapshot?.colonies.find((c) => c.id === selected);
      if (item) {
        setSelectedColony(selected);
        setDraft(item.definition);
        baseline.current = item.definition;
        setInspect(null);
        setPanel("goal");
        setPanelOpen(true);
        setIsNew(false);
      }
      return;
    }
    if (colonyId && colonyId !== selectedColony) {
      const item = snapshot?.colonies.find((c) => c.id === colonyId);
      if (item) {
        setDraft(item.definition);
        baseline.current = item.definition;
      }
    }
    setSelectedColony(colonyId);
    setIsNew(false);
    setInspect(id);
    setPanel("node");
    setPanelOpen(true);
    setRoomAddress("");
    setAgentCommand(
      snapshot?.agents.find((a) => nodeKey("agent", a.alias) === key)
        ?.core_command ?? "",
    );
  };
  const beginSetup = () => {
    if (isNew) {
      setPanel("setup");
      setPanelOpen(true);
      return;
    }
    if (dirty) {
      setError(t("colony.save_or_reset"));
      return;
    }
    const aliases = selection
      .map((id) => splitNodeKey(id)[1])
      .filter((alias) =>
        snapshot?.agents.some(
          (a) => a.alias === alias && a.enabled && !a.colony_id,
        ),
      );
    if (!aliases.length) {
      setError(t("colony.invalid_selection"));
      return;
    }
    const id = `colony_${crypto.randomUUID().slice(0, 8)}`,
      queen = `queen_${id.slice(7)}`;
    const definition = blankColony("", queen, aliases);
    definition.connections = aliases.flatMap((alias) => [
      { from: queen, to: alias },
      { from: alias, to: queen },
    ]);
    setAgentProfiles(
      Object.fromEntries(
        [queen, ...aliases].map((alias) => {
          const source = snapshot?.agents.find(
            (a) => a.alias === (alias === queen ? aliases[0] : alias),
          );
          return [
            alias,
            {
              risk_profile: source?.risk_profile ?? "",
              runtime_profile: source?.runtime_profile ?? "",
            },
          ];
        }),
      ),
    );
    setCoreCommands(
      Object.fromEntries([
        [queen, t("colony.default_queen_command")],
        ...aliases.map((alias) => [
          alias,
          snapshot?.agents.find((a) => a.alias === alias)?.core_command ?? "",
        ]),
      ]),
    );
    setDraft(definition);
    baseline.current = null;
    setNewId(id);
    setQueenTemplate(aliases[0] ?? "");
    setSelectedColony(null);
    setIsNew(true);
    setPanel("setup");
    setPanelOpen(true);
    setProposal(null);
    setAnswers([]);
    setCurrentAnswers({});
    setGoalMode("fresh");
    setPreviousGoal("");
    setInspect(null);
  };
  const saveTeam = async (): Promise<ColonyDetail> => {
    if (!draft) throw new Error(t("colony.queen_required"));
    if (isNew) {
      if (!draft.queen.trim() || !queenTemplate || !draft.name.trim())
        throw new Error(t("colony.queen_required"));
      if (teamAliases(draft).some((alias) => !coreCommands[alias]?.trim()))
        throw new Error(t("colony.core_required"));
      const definition = { ...draft, positions: { ...draft.positions } };
      const left = Math.max(500, ...graph.nodes.map((node) => node.x + 308));
      definition.positions[nodeKey("agent", definition.queen)] ??= {
        x: left,
        y: 120 + Math.max(0, definition.members.length - 1) * 85,
      };
      definition.members.forEach((alias, index) => {
        definition.positions[nodeKey("agent", alias)] ??= {
          x: left + 330,
          y: 120 + index * 170,
        };
      });
      const result = await createColony({
        id: newId,
        definition,
        queen_template: queenTemplate,
        core_commands: Object.fromEntries(
          teamAliases(definition).map((alias) => [
            alias,
            coreCommands[alias] ?? t("colony.default_queen_command"),
          ]),
        ),
        agent_profiles: Object.fromEntries(
          teamAliases(definition).flatMap((alias) =>
            agentProfiles[alias] ? [[alias, agentProfiles[alias]]] : [],
          ),
        ),
      });
      acceptDetail(result);
      await refresh();
      return result;
    }
    if (!selectedColony || !baseline.current)
      throw new Error(t("colony.choose_colony"));
    const result = await saveColony(selectedColony, {
      definition: draft,
      expected_definition: baseline.current,
      prompt_activation: promptTiming,
    });
    acceptDetail(result);
    return result;
  };
  const goalObjective = () =>
    [
      objective,
      success ? `${t("colony.success_criteria")}: ${success}` : "",
      constraints ? `${t("colony.constraints")}: ${constraints}` : "",
    ]
      .filter(Boolean)
      .join("\n\n");
  const startGoal = async (
    id: string,
    plan: QueenProposal,
    approveNewAgents = false,
  ) => {
    const result = await createColonyGoal(id, {
      objective: goalObjective(),
      mode: goalMode,
      previous_goal_id: goalMode === "continue" ? previousGoal || null : null,
      proposal: plan,
      approve_new_agents: approveNewAgents,
      token_limit: goalTokens ? Number(goalTokens) : null,
      cost_limit_usd: goalCost ? Number(goalCost) : null,
    });
    acceptGoal(id, result);
    setPanel("goal");
    setProposal(null);
    setSelection([]);
    if (result.task.status === "paused" && draft?.autonomy !== "plan_only")
      acceptGoal(
        id,
        await controlColonyGoal(id, result.task.id, { action: "start" }),
      );
    const refreshed = await getColony(id);
    acceptDetail(refreshed);
  };
  const clarify = () =>
    void run(async () => {
      if (!objective.trim()) throw new Error(t("colony.goal_required"));
      const newAnswers = (proposal?.questions ?? []).map((q) => ({
        question: q,
        answer: currentAnswers[q]?.trim() ?? "",
      }));
      if (newAnswers.some((answer) => !answer.answer))
        throw new Error(t("colony.required_answer"));
      const answered = [...answers, ...newAnswers];
      const saved = isNew || dirty ? await saveTeam() : detail;
      if (!saved) throw new Error(t("colony.choose_colony"));
      const next = await clarifyColony(saved.id, {
        objective: goalObjective(),
        answers: answered,
      });
      setAnswers(answered);
      setCurrentAnswers({});
      setProposal(next);
      if (
        !next.questions.length &&
        saved.definition.start_mode === "automatic" &&
        (saved.definition.autonomy === "autonomous" ||
          next.new_agents.every((agent) =>
            teamAliases(saved.definition).includes(agent.alias),
          ))
      )
        await startGoal(saved.id, next);
    });
  const newGoal = () => {
    if (!detail) return;
    if (dirty) {
      setError(t("colony.save_or_reset"));
      return;
    }
    setDraft(detail.definition);
    baseline.current = detail.definition;
    setPanel("setup");
    setPanelOpen(true);
    setIsNew(false);
    setProposal(null);
    setAnswers([]);
    setCurrentAnswers({});
    setObjective("");
    setSuccess("");
    setConstraints("");
    setGoalMode(detail.goals.length ? "continue" : "fresh");
    setPreviousGoal(detail.goals[0]?.task.id ?? "");
  };
  const connect = (fromId: string, toId: string) => {
    const [fromColony, fromKey] = unpack(fromId),
      [toColony, toKey] = unpack(toId);
    if (hasUnsavedTeam && fromColony !== selectedColony) {
      setError(t("colony.save_or_reset"));
      return;
    }
    if (!fromColony || fromColony !== toColony) {
      setError(t("colony.connection_invalid"));
      return;
    }
    const owner = snapshot?.colonies.find((c) => c.id === fromColony);
    const current =
      fromColony === selectedColony && draft ? draft : owner?.definition;
    if (!current) return;
    const [fromKind, from] = splitNodeKey(fromKey),
      [toKind, to] = splitNodeKey(toKey);
    let next = current;
    if (fromKind === "agent" && toKind === "agent" && from !== to) {
      if (current.connections.some((e) => e.from === from && e.to === to)) {
        setError(t("colony.connection_exists"));
        return;
      }
      next = {
        ...current,
        connections: [...current.connections, { from, to }],
      };
    } else if (fromKind === "prompt" && toKind === "agent")
      next = {
        ...current,
        prompts: current.prompts.map((p) =>
          p.id === from ? { ...p, agents: [...new Set([...p.agents, to])] } : p,
        ),
      };
    else if (fromKind === "room" && toKind === "agent")
      next = {
        ...current,
        rooms: current.rooms.map((r) =>
          r.id === from
            ? { ...r, readers: [...new Set([...r.readers, to])] }
            : r,
        ),
      };
    else if (fromKind === "agent" && toKind === "room")
      next = {
        ...current,
        rooms: current.rooms.map((r) =>
          r.id === to
            ? { ...r, publishers: [...new Set([...r.publishers, from])] }
            : r,
        ),
      };
    else if (fromKind === "channel" && toKind === "agent")
      next = {
        ...current,
        channels: current.channels.map((c) =>
          c.id === from
            ? { ...c, inbound_agents: [...new Set([...c.inbound_agents, to])] }
            : c,
        ),
      };
    else if (fromKind === "agent" && toKind === "channel")
      next = {
        ...current,
        channels: current.channels.map((c) =>
          c.id === to
            ? {
                ...c,
                outbound_agents: [...new Set([...c.outbound_agents, from])],
              }
            : c,
        ),
      };
    else {
      setError(t("colony.connection_invalid"));
      return;
    }
    if (selectedColony !== fromColony) {
      baseline.current = owner?.definition ?? null;
      setSelectedColony(fromColony);
    }
    setDraft(next);
    setPanelOpen(true);
    setInspect(memberId(fromColony, fromKey));
    if (fromKind === "agent")
      setAgentCommand(
        snapshot?.agents.find((agent) => agent.alias === from)?.core_command ??
          "",
      );
    setPanel("node");
    setError("");
  };
  const addNode = (type: "prompt" | "room" | "channel") => {
    if (nodePalette.current) nodePalette.current.open = false;
    if (!draft || !selectedColony) return;
    const id = `${type}_${crypto.randomUUID().slice(0, 8)}`;
    const next = { ...draft };
    if (type === "prompt")
      next.prompts = [
        ...draft.prompts,
        { id, text: "", agents: [], revision: 1 },
      ];
    if (type === "room")
      next.rooms = [
        ...draft.rooms,
        {
          id,
          name: t("colony.node_room"),
          readers: [],
          publishers: [],
          responders: "addressed",
          max_turns: 8,
        },
      ];
    if (type === "channel") {
      const channel = snapshot?.channels.find((c) => c.enabled);
      if (!channel) {
        setError(t("colony.no_channels"));
        return;
      }
      next.channels = [
        ...draft.channels,
        {
          id,
          channel: channel.id,
          conversation: "",
          inbound_agents: [],
          outbound_agents: [],
        },
      ];
    }
    const positions = layoutPositions(next);
    const position = positions[nodeKey(type, id)];
    if (position)
      next.positions = { ...draft.positions, [nodeKey(type, id)]: position };
    setDraft(next);
    setInspect(memberId(selectedColony, nodeKey(type, id)));
    setPanel("node");
    setPanelOpen(true);
  };
  const moveNode = (id: string, x: number, y: number) => {
    const [colonyId, key] = unpack(id);
    if (!colonyId) {
      setUnassignedPositions((positions) => ({
        ...positions,
        [key]: { x, y },
      }));
      return;
    }
    if (hasUnsavedTeam && colonyId !== selectedColony) {
      setError(t("colony.save_or_reset"));
      return;
    }
    const owner = snapshot?.colonies.find((c) => c.id === colonyId);
    const definition =
      colonyId === selectedColony && draft ? draft : owner?.definition;
    if (!definition) return;
    if (colonyId !== selectedColony) {
      baseline.current = owner?.definition ?? null;
      setSelectedColony(colonyId);
    }
    setDraft({
      ...definition,
      positions: { ...definition.positions, [key]: { x, y } },
    });
  };

  const graph = useMemo(() => {
    const nodes: ColonyCanvasNode[] = [],
      wires: ColonyCanvasWire[] = [];
    const independents = snapshot?.agents.filter((a) => !a.colony_id) ?? [];
    independents.forEach((a, index) => {
      const key = nodeKey("agent", a.alias);
      nodes.push({
        id: key,
        kind: "agent",
        label: a.alias,
        purpose: a.core_command,
        selectable: a.enabled,
        x: unassignedPositions[key]?.x ?? 32 + (index % 2) * 245,
        y: unassignedPositions[key]?.y ?? 65 + Math.floor(index / 2) * 170,
        state: !a.enabled
          ? t("colony.disabled")
          : a.active_turns > 0
            ? t("colony.status_running")
            : undefined,
      });
    });
    const colonies = (snapshot?.colonies ?? []).map((colony) => {
      const value =
        colony.id === selectedColony && draft && !isNew
          ? draft
          : colony.definition;
      const definitions = [
        ...teamAliases(value).map((alias) => ({
          key: nodeKey("agent", alias),
          kind: alias === value.queen ? ("queen" as const) : ("agent" as const),
          label: alias,
          purpose:
            snapshot?.agents.find((a) => a.alias === alias)?.core_command ?? "",
        })),
        ...value.prompts.map((p) => ({
          key: nodeKey("prompt", p.id),
          kind: "text" as const,
          label: p.id,
          purpose: p.text,
        })),
        ...value.rooms.map((r) => ({
          key: nodeKey("room", r.id),
          kind: "room" as const,
          label: r.name,
          purpose: t(`colony.responders_${r.responders}`),
        })),
        ...value.channels.map((c) => ({
          key: nodeKey("channel", c.id),
          kind: "channel" as const,
          label: c.channel,
          purpose: c.conversation,
        })),
      ];
      const positions = layoutPositions(value);
      definitions.forEach((node) => {
        const p = positions[node.key];
        if (!p) return;
        const running = activeGoal(colony);
        nodes.push({
          id: memberId(colony.id, node.key),
          ...node,
          colonyId: colony.id,
          x: p.x,
          y: p.y,
          state: node.kind === "queen" ? statusLabel(running) : undefined,
        });
      });
      const wire = (from: string, to: string) =>
        wires.push({
          id: `${colony.id}:${from}>${to}`,
          from: memberId(colony.id, from),
          to: memberId(colony.id, to),
          label: `${from} → ${to}`,
        });
      value.connections.forEach((e) =>
        wire(nodeKey("agent", e.from), nodeKey("agent", e.to)),
      );
      value.prompts.forEach((p) =>
        p.agents.forEach((a) =>
          wire(nodeKey("prompt", p.id), nodeKey("agent", a)),
        ),
      );
      value.rooms.forEach((r) => {
        r.readers.forEach((a) =>
          wire(nodeKey("room", r.id), nodeKey("agent", a)),
        );
        r.publishers.forEach((a) =>
          wire(nodeKey("agent", a), nodeKey("room", r.id)),
        );
      });
      value.channels.forEach((c) => {
        c.inbound_agents.forEach((a) =>
          wire(nodeKey("channel", c.id), nodeKey("agent", a)),
        );
        c.outbound_agents.forEach((a) =>
          wire(nodeKey("agent", a), nodeKey("channel", c.id)),
        );
      });
      return {
        id: colony.id,
        name: value.name,
        status: statusLabel(activeGoal(colony)),
      };
    });
    return { nodes, wires, boxes: colonies };
  }, [snapshot, selectedColony, draft, isNew, unassignedPositions]);

  const prompt =
    kind === "prompt"
      ? draft?.prompts.find((p) => p.id === inspectedId)
      : undefined;
  const room =
    kind === "room"
      ? draft?.rooms.find((r) => r.id === inspectedId)
      : undefined;
  const channel =
    kind === "channel"
      ? draft?.channels.find((c) => c.id === inspectedId)
      : undefined;
  const setPrompt = (patch: Partial<ColonyConfig["prompts"][number]>) => {
    if (draft)
      update({
        prompts: draft.prompts.map((p) =>
          p.id === inspectedId ? { ...p, ...patch } : p,
        ),
      });
  };
  const setRoom = (patch: Partial<ColonyConfig["rooms"][number]>) => {
    if (draft)
      update({
        rooms: draft.rooms.map((r) =>
          r.id === inspectedId ? { ...r, ...patch } : r,
        ),
      });
  };
  const setChannel = (patch: Partial<ColonyConfig["channels"][number]>) => {
    if (draft)
      update({
        channels: draft.channels.map((c) =>
          c.id === inspectedId ? { ...c, ...patch } : c,
        ),
      });
  };
  const removeNode = () => {
    if (!draft) return;
    const positions = { ...draft.positions };
    delete positions[nodeKey(kind, inspectedId)];
    update({
      positions,
      prompts: draft.prompts.filter(
        (p) => !(kind === "prompt" && p.id === inspectedId),
      ),
      rooms: draft.rooms.filter(
        (r) => !(kind === "room" && r.id === inspectedId),
      ),
      channels: draft.channels.filter(
        (c) => !(kind === "channel" && c.id === inspectedId),
      ),
    });
    setInspect(null);
    setPanel("goal");
  };
  const control = (action: ColonyControlRequest["action"]) =>
    void run(async () => {
      if (!detail || !currentGoal) return;
      setPendingControl(action);
      try {
        acceptGoal(
          detail.id,
          await controlColonyGoal(detail.id, currentGoal.task.id, { action }),
        );
        acceptDetail(await getColony(detail.id), !dirty);
      } finally {
        setPendingControl(null);
      }
    });
  const send = () =>
    void run(async () => {
      if (!selectedColony || !recipient || !message.trim()) return;
      const id = `${selectedColony}:${recipient}`;
      const result = await sendColonyMessage(selectedColony, {
        recipient,
        content:
          roomAddress && recipient.startsWith("room:")
            ? `@${roomAddress} ${message}`
            : message,
      });
      if (conversationId === id) {
        setConversation(result.messages);
        setMessage("");
      }
      await refresh();
    });
  const addAgent = () =>
    void run(async () => {
      if (
        !newAgent ||
        !selectedColony ||
        !baseline.current ||
        !newAgent.alias.trim() ||
        !newAgent.core_command.trim() ||
        !newAgent.template
      )
        throw new Error(t("colony.alias_required"));
      if (dirty) await saveTeam();
      const expected = baseline.current;
      if (!expected) return;
      const connections = [
        ...(newAgentSend ? [{ from: expected.queen, to: newAgent.alias }] : []),
        ...(newAgentReturn
          ? [{ from: newAgent.alias, to: expected.queen }]
          : []),
      ];
      acceptDetail(
        await addColonyAgent(selectedColony, {
          ...newAgent,
          expected_definition: expected,
          connections,
        }),
      );
      setNewAgent(null);
      if (proposal) setPanel("setup");
      await refresh();
    });
  const removeMember = () => {
    if (!draft || !agent || draft.queen === agent.alias) return;
    const alias = agent.alias;
    const positions = { ...draft.positions };
    delete positions[nodeKey("agent", alias)];
    update({
      positions,
      members: draft.members.filter((a) => a !== alias),
      connections: draft.connections.filter(
        (e) => e.from !== alias && e.to !== alias,
      ),
      prompts: draft.prompts.map((p) => ({
        ...p,
        agents: p.agents.filter((a) => a !== alias),
      })),
      rooms: draft.rooms.map((r) => ({
        ...r,
        readers: r.readers.filter((a) => a !== alias),
        publishers: r.publishers.filter((a) => a !== alias),
      })),
      channels: draft.channels.map((c) => ({
        ...c,
        inbound_agents: c.inbound_agents.filter((a) => a !== alias),
        outbound_agents: c.outbound_agents.filter((a) => a !== alias),
      })),
      baseline_context: draft.baseline_context.filter((c) => c.agent !== alias),
    });
  };
  const showConversation = recipient && selectedColony;
  const attentionColonies =
    snapshot?.colonies.filter((colony) =>
      colony.goals.some(
        (goal) =>
          goal.approvals.some((approval) => approval.decision == null) ||
          (goal.task.status === "paused" && !!goal.execution.pending_plan),
      ),
    ) ?? [];

  return (
    <div
      className="relative flex h-full min-h-0 min-w-0 flex-1 flex-col"
      data-testid="colony-workspace"
    >
      {attentionColonies.map((colony) => (
        <WorkspaceAttention
          key={colony.id}
          to={attentionTarget}
          label={`${t("colony.title")} · ${colony.definition.name}`}
          onOpen={() => {
            onShow();
            selectInspect(`colony:${colony.id}`);
          }}
        />
      ))}
      <header className="flex shrink-0 flex-wrap items-center gap-2 border-b border-pc-border pl-14 pr-3 py-3 md:px-4">
        <Crown className="h-4 w-4 text-pc-accent" />
        <h1 className="mr-auto text-sm font-semibold">{t("colony.title")}</h1>
        <button
          type="button"
          disabled={busy || !selection.length}
          onClick={beginSetup}
          className="btn-primary flex items-center gap-1.5 px-3 py-2 text-xs disabled:opacity-40"
        >
          <Plus className="h-3.5 w-3.5" />
          {t("colony.create")}
          {selection.length ? ` · ${selection.length}` : ""}
        </button>
        <button
          type="button"
          aria-label={t("common.refresh")}
          disabled={busy}
          onClick={() =>
            void run(async () => {
              await refresh();
            })
          }
          className="rounded p-2 hover:bg-pc-elevated"
        >
          <RefreshCw className="h-4 w-4" />
        </button>
      </header>
      {attentionColonies.map((colony) => (
        <button
          key={colony.id}
          type="button"
          onClick={() => selectInspect(`colony:${colony.id}`)}
          className="mx-3 mt-2 rounded-lg border border-status-warning/30 bg-status-warning/5 px-3 py-2 text-left text-xs text-status-warning"
        >
          {t("colony.attention")} · {colony.definition.name}
        </button>
      ))}
      <div className="flex shrink-0 flex-wrap items-center gap-2 px-3 py-2 text-xs">
        <select
          aria-label={t("colony.choose_colony")}
          disabled={busy}
          className="max-w-56 rounded border border-pc-border bg-pc-surface px-2 py-2"
          value={selectedColony ?? ""}
          onChange={(e) => {
            if (e.target.value) selectInspect(`colony:${e.target.value}`);
          }}
        >
          <option value="">{t("colony.choose_colony")}</option>
          {snapshot?.colonies.map((c) => (
            <option key={c.id} value={c.id}>
              {c.definition.name}
            </option>
          ))}
        </select>
        {detail && (
          <>
            <button
              type="button"
              disabled={busy || !!currentGoal}
              onClick={newGoal}
              className={button}
            >
              {t("colony.new_goal")}
            </button>
            <details ref={nodePalette} inert={busy} className="relative z-20">
              <summary className={`${button} cursor-pointer list-none`}>
                {t("colony.add_node")}
              </summary>
              <div className="absolute left-0 top-full mt-1 w-52 rounded-lg border border-pc-border bg-pc-surface p-2 shadow-xl">
                {(["prompt", "room", "channel"] as const).map((type) => (
                  <button
                    key={type}
                    type="button"
                    onClick={() => addNode(type)}
                    className="block w-full rounded px-3 py-2 text-left hover:bg-pc-elevated"
                  >
                    {t(`colony.add_${type}`)}
                  </button>
                ))}
              </div>
            </details>
            <button
              type="button"
              onClick={() => {
                setPanel("goal");
                setPanelOpen(true);
              }}
              className="rounded px-2 py-2 text-pc-text-muted hover:bg-pc-elevated"
            >
              {t("colony.progress")}
            </button>
            <button
              type="button"
              disabled={busy || currentGoal?.task.status === "running"}
              onClick={() => {
                setNewAgent({
                  alias: "",
                  core_command: "",
                  template: draft?.queen ?? "",
                });
                setPanel("node");
                setPanelOpen(true);
                setInspect(null);
              }}
              className={button}
            >
              {t("colony.add_agent")}
            </button>
          </>
        )}
        {dirty && (
          <span className="text-status-warning">{t("colony.unsaved")}</span>
        )}
      </div>
      {(error || refreshError) && (
        <div
          role={error ? "alert" : "status"}
          className={`mx-3 mb-2 rounded-lg border border-pc-border px-3 py-2 text-xs ${error ? "text-status-error" : "text-status-warning"}`}
        >
          {error || t("colony.refresh_failed")}
        </div>
      )}
      <div className="relative flex min-h-0 min-w-0 flex-1">
        <section className="flex min-h-0 min-w-0 flex-1 flex-col p-3 pt-0">
          {snapshot && !snapshot.colonies.length && (
            <div className="mb-3 rounded-xl border border-pc-accent/20 bg-pc-accent/5 p-4">
              <h2 className="text-sm font-medium">{t("colony.empty_title")}</h2>
              <p className="mt-1 text-xs leading-relaxed text-pc-text-muted">
                {t(
                  snapshot.agents.length
                    ? "colony.empty_hint"
                    : "colony.no_agents",
                )}
              </p>
              {!snapshot.agents.length && (
                <button
                  type="button"
                  onClick={() => settings("/config/agents")}
                  className={`${button} mt-3`}
                >
                  {t("config.all_settings")}
                </button>
              )}
            </div>
          )}
          <ColonyCanvas
            nodes={graph.nodes}
            wires={graph.wires}
            boxes={graph.boxes}
            selection={new Set(selection)}
            onSelection={setSelection}
            onInspect={selectInspect}
            onMove={moveNode}
            onConnect={connect}
            disabled={busy}
            visible={visible}
          />
          {!snapshot && (
            <p className="absolute inset-0 m-auto h-fit w-fit rounded bg-pc-surface p-4 text-xs text-pc-text-muted">
              {t(error ? "colony.unavailable" : "common.loading")}
            </p>
          )}
        </section>
        {panelOpen && (
          <button
            type="button"
            aria-label={t("colony.close_panel")}
            className="absolute inset-0 z-20 bg-black/30 min-[1200px]:hidden"
            onClick={() => setPanelOpen(false)}
          />
        )}
        <aside
          hidden={!panelOpen}
          aria-label={t("colony.panel")}
          className="absolute inset-y-0 right-0 z-30 flex w-full max-w-96 shrink-0 flex-col border-l border-pc-border bg-pc-surface min-[1200px]:static min-[1200px]:z-auto min-[1200px]:w-80 2xl:w-96 [&[hidden]]:hidden"
        >
          <div className="flex shrink-0 items-center justify-between border-b border-pc-border px-4 py-3">
            <h2 className="text-sm font-medium">
              {t(
                panel === "setup"
                  ? "colony.setup"
                  : panel === "goal"
                    ? "colony.progress"
                    : "colony.inspect",
              )}
            </h2>
            <button
              type="button"
              onClick={() => setPanelOpen(false)}
              aria-label={t("colony.close_panel")}
              className="rounded p-2 hover:bg-pc-elevated"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto p-4" inert={busy}>
            {panel === "setup" && draft && (
              <div className="space-y-4">
                {!proposal && (
                  <>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.goal")}
                      <textarea
                        className={`${input} min-h-24`}
                        value={objective}
                        onChange={(e) => setObjective(e.target.value)}
                        placeholder={t("colony.goal_placeholder")}
                      />
                    </label>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.success_criteria")}
                      <textarea
                        className={input}
                        value={success}
                        onChange={(e) => setSuccess(e.target.value)}
                      />
                    </label>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.constraints")}
                      <textarea
                        className={input}
                        value={constraints}
                        onChange={(e) => setConstraints(e.target.value)}
                      />
                    </label>
                    {detail?.goals.length ? (
                      <fieldset>
                        <legend className="mb-2 text-xs text-pc-text-muted">
                          {t("colony.new_goal")}
                        </legend>
                        <div className="flex gap-2">
                          {(["continue", "fresh"] as const).map((mode) => (
                            <label
                              key={mode}
                              className="flex flex-1 items-center gap-2 rounded-lg border border-pc-border p-3 text-xs"
                            >
                              <input
                                type="radio"
                                name="colony-mode"
                                checked={goalMode === mode}
                                onChange={() => setGoalMode(mode)}
                              />
                              {t(`colony.${mode}`)}
                            </label>
                          ))}
                        </div>
                        <p className="mt-2 text-[11px] leading-relaxed text-pc-text-muted">
                          {t("colony.reuse_hint")}
                        </p>
                        {goalMode === "continue" && (
                          <select
                            aria-label={t("colony.history")}
                            className={input}
                            value={previousGoal}
                            onChange={(e) => setPreviousGoal(e.target.value)}
                          >
                            {detail.goals.map((g) => (
                              <option key={g.task.id} value={g.task.id}>
                                {g.goal.objective}
                              </option>
                            ))}
                          </select>
                        )}
                      </fieldset>
                    ) : null}
                    {isNew && (
                      <>
                        <label className="block text-xs text-pc-text-muted">
                          {t("colony.name")}
                          <input
                            className={input}
                            value={draft.name}
                            onChange={(e) => update({ name: e.target.value })}
                          />
                        </label>
                        <label className="block text-xs text-pc-text-muted">
                          {t("colony.queen_alias")}
                          <input
                            className={input}
                            value={draft.queen}
                            onChange={(e) => {
                              const old = draft.queen,
                                next = e.target.value;
                              setCoreCommands((commands) => ({
                                ...commands,
                                [next]:
                                  commands[old] ??
                                  t("colony.default_queen_command"),
                              }));
                              setAgentProfiles((profiles) => ({
                                ...profiles,
                                [next]: profiles[old] ?? {
                                  risk_profile: "",
                                  runtime_profile: "",
                                },
                              }));
                              update({
                                queen: next,
                                connections: draft.connections.map((edge) => ({
                                  from: edge.from === old ? next : edge.from,
                                  to: edge.to === old ? next : edge.to,
                                })),
                              });
                            }}
                          />
                        </label>
                        <label className="block text-xs text-pc-text-muted">
                          {t("colony.queen_template")}
                          <select
                            className={input}
                            value={queenTemplate}
                            onChange={(e) => {
                              const source = snapshot?.agents.find(
                                (a) => a.alias === e.target.value,
                              );
                              setQueenTemplate(e.target.value);
                              if (source)
                                setAgentProfiles((profiles) => ({
                                  ...profiles,
                                  [draft.queen]: {
                                    risk_profile: source.risk_profile,
                                    runtime_profile: source.runtime_profile,
                                  },
                                }));
                            }}
                          >
                            {snapshot?.agents
                              .filter((a) => a.enabled)
                              .map((a) => (
                                <option key={a.alias} value={a.alias}>
                                  {a.alias} · {a.model_provider}
                                </option>
                              ))}
                          </select>
                        </label>
                      </>
                    )}
                    {isNew && (
                      <details className="rounded-lg border border-pc-border p-3">
                        <summary className="cursor-pointer text-xs font-medium">
                          {t("colony.access_choices")}
                        </summary>
                        <p className="my-3 text-[11px] text-pc-text-muted">
                          {t("colony.access_choices_hint")}
                        </p>
                        {teamAgents.map((agent) => (
                          <fieldset key={agent.alias} className="mb-4">
                            <legend className="text-xs font-medium">
                              {agent.alias}
                            </legend>
                            {(["risk_profile", "runtime_profile"] as const).map(
                              (profile) => (
                                <label
                                  key={profile}
                                  className="mt-2 block text-xs text-pc-text-muted"
                                >
                                  {t(`colony.${profile}`)}
                                  <select
                                    aria-label={`${agent.alias} ${t(`colony.${profile}`)}`}
                                    className={input}
                                    value={
                                      agentProfiles[agent.alias]?.[profile] ??
                                      agent[profile]
                                    }
                                    onChange={(e) =>
                                      setAgentProfiles((profiles) => ({
                                        ...profiles,
                                        [agent.alias]: {
                                          risk_profile: agent.risk_profile,
                                          runtime_profile:
                                            agent.runtime_profile,
                                          ...profiles[agent.alias],
                                          [profile]: e.target.value,
                                        },
                                      }))
                                    }
                                  >
                                    {(profile === "risk_profile"
                                      ? (snapshot?.risk_profiles ?? [
                                          agent.risk_profile,
                                        ])
                                      : (snapshot?.runtime_profiles ?? [
                                          agent.runtime_profile,
                                        ])
                                    ).map((name) => (
                                      <option key={name} value={name}>
                                        {name}
                                      </option>
                                    ))}
                                  </select>
                                </label>
                              ),
                            )}
                          </fieldset>
                        ))}
                      </details>
                    )}
                    {isNew && (
                      <details className="rounded-lg border border-pc-border p-3">
                        <summary className="cursor-pointer text-xs font-medium">
                          {t("colony.core_command")}
                        </summary>
                        <p className="my-3 text-[11px] text-pc-text-muted">
                          {t("colony.core_hint")}
                        </p>
                        {[draft.queen, ...draft.members].map((alias) => (
                          <label key={alias} className="mb-3 block text-xs">
                            {alias}
                            <textarea
                              className={input}
                              value={coreCommands[alias] ?? ""}
                              placeholder={t("colony.core_not_set")}
                              onChange={(e) =>
                                setCoreCommands((commands) => ({
                                  ...commands,
                                  [alias]: e.target.value,
                                }))
                              }
                            />
                          </label>
                        ))}
                      </details>
                    )}
                    <SettingsFields value={draft} onChange={update} />
                    <ContextSummary
                      value={draft}
                      agents={contextAgents}
                      onChange={update}
                    />
                    <AccessSummary agents={teamAgents} />
                    <details className="rounded-lg border border-pc-border p-3">
                      <summary className="cursor-pointer text-xs font-medium">
                        {t("colony.connections")}
                      </summary>
                      <p className="my-3 text-[11px] text-pc-text-muted">
                        {t("colony.connections_hint")}
                      </p>
                      {draft.members.map((member) => (
                        <div key={member} className="mb-3 space-y-2">
                          {[
                            { from: draft.queen, to: member },
                            { from: member, to: draft.queen },
                          ].map((edge) => (
                            <label
                              key={`${edge.from}:${edge.to}`}
                              className="flex items-start gap-2 text-xs"
                            >
                              <input
                                type="checkbox"
                                checked={draft.connections.some(
                                  (e) =>
                                    e.from === edge.from && e.to === edge.to,
                                )}
                                onChange={(e) =>
                                  update({
                                    connections: e.target.checked
                                      ? [...draft.connections, edge]
                                      : draft.connections.filter(
                                          (v) =>
                                            !(
                                              v.from === edge.from &&
                                              v.to === edge.to
                                            ),
                                        ),
                                  })
                                }
                              />
                              <span className="min-w-0 break-words">
                                {edge.from} → {edge.to}
                              </span>
                            </label>
                          ))}
                        </div>
                      ))}
                    </details>
                    <details className="rounded-lg border border-pc-border p-3">
                      <summary className="cursor-pointer text-xs font-medium">
                        {t("colony.access")} · {t("colony.limits")}
                      </summary>
                      <p className="my-3 text-[11px] text-pc-text-muted">
                        {t("colony.limits_hint")}
                      </p>
                      <label className="block text-xs">
                        {t("colony.limit_tokens")}
                        <input
                          type="number"
                          min="1"
                          step="1"
                          className={input}
                          value={goalTokens}
                          onChange={(e) => setGoalTokens(e.target.value)}
                        />
                      </label>
                      <label className="mt-3 block text-xs">
                        {t("colony.limit_cost")}
                        <input
                          type="number"
                          min="0.001"
                          step="0.01"
                          className={input}
                          value={goalCost}
                          onChange={(e) => setGoalCost(e.target.value)}
                        />
                      </label>
                    </details>
                  </>
                )}
                {proposal && (
                  <>
                    <p className="whitespace-pre-wrap text-sm leading-relaxed">
                      {proposal.summary}
                    </p>
                    {proposal.questions.length ? (
                      <fieldset className="space-y-4">
                        <legend className="mb-3 text-xs font-medium text-pc-accent">
                          {t("colony.questions")}
                        </legend>
                        {proposal.questions.map((question) => (
                          <label
                            key={question}
                            className="block text-xs text-pc-text-muted"
                          >
                            {question}
                            <textarea
                              className={input}
                              value={currentAnswers[question] ?? ""}
                              onChange={(e) =>
                                setCurrentAnswers((a) => ({
                                  ...a,
                                  [question]: e.target.value,
                                }))
                              }
                            />
                          </label>
                        ))}
                      </fieldset>
                    ) : (
                      <>
                        <div className="rounded-lg border border-pc-accent/30 bg-pc-accent/5 p-3">
                          <h3 className="text-xs font-medium">
                            {t("colony.review")}
                          </h3>
                          <p className="mt-2 text-[11px] text-pc-text-muted">
                            {t("colony.review_hint")}
                          </p>
                          <p className="mt-3 break-words text-xs">
                            {draft.name} · {draft.queen}
                            <br />
                            {draft.members.join(", ")}
                            <br />
                            {t(`colony.autonomy_${draft.autonomy}`)} ·{" "}
                            {t(
                              draft.start_mode === "review"
                                ? "colony.start_review"
                                : "colony.start_automatic",
                            )}
                            <br />
                            {t("colony.context")}:{" "}
                            {draft.baseline_context.length}
                          </p>
                        </div>
                        {proposal.new_agents.some(
                          (agent) => !team.includes(agent.alias),
                        ) && (
                          <p className="text-xs leading-relaxed text-pc-text-muted">
                            {t("colony.new_agent_batch")}
                          </p>
                        )}
                        {(proposal.new_agents ?? [])
                          .filter((a) => !team.includes(a.alias))
                          .map((a) => (
                            <div
                              key={a.alias}
                              className="rounded-lg border border-pc-accent/30 p-3"
                            >
                              <p className="text-xs font-medium">
                                {t("colony.new_agent")}: {a.alias}
                              </p>
                              <p className="mt-1 text-xs text-pc-text-muted">
                                {a.core_command}
                              </p>
                              <p className="mt-2 text-xs">
                                {t("colony.agent_template")}: {a.template}
                              </p>
                              {a.connections.map((connection) => (
                                <p
                                  key={`${connection.from}:${connection.to}`}
                                  className="mt-1 break-words text-xs"
                                >
                                  {connection.from} → {connection.to}
                                </p>
                              ))}
                              <div className="mt-3">
                                <AccessSummary
                                  agents={teamAgents.filter(
                                    (template) => template.alias === a.template,
                                  )}
                                />
                              </div>
                              {draft.autonomy === "autonomous" && (
                                <p className="mt-2 text-xs text-pc-accent">
                                  {t("colony.new_agent_autonomous")}
                                </p>
                              )}
                            </div>
                          ))}
                        <h3 className="text-xs text-pc-text-muted">
                          {t("colony.assignments")}
                        </h3>
                        {proposal.assignments.map((a, index) => (
                          <div
                            key={`${a.agent}:${index}`}
                            className="rounded-lg bg-pc-elevated p-3"
                          >
                            <p className="text-xs font-medium">{a.agent}</p>
                            <p className="mt-1 whitespace-pre-wrap text-xs text-pc-text-muted">
                              {a.instruction}
                            </p>
                          </div>
                        ))}
                        <ContextSummary
                          value={draft}
                          agents={contextAgents}
                          onChange={update}
                        />
                        <AccessSummary agents={teamAgents} />
                      </>
                    )}
                  </>
                )}
                <div className="flex flex-wrap gap-2">
                  {isNew && (
                    <button
                      type="button"
                      disabled={busy}
                      className={button}
                      onClick={() => {
                        setDraft(null);
                        setIsNew(false);
                        setPanelOpen(false);
                        setProposal(null);
                        setSelection([]);
                        setError("");
                      }}
                    >
                      {t("colony.discard_setup")}
                    </button>
                  )}
                  {proposal && (
                    <button
                      type="button"
                      disabled={busy}
                      className={button}
                      onClick={() => setProposal(null)}
                    >
                      {t("colony.back_setup")}
                    </button>
                  )}
                  {proposal && !proposal.questions.length ? (
                    <button
                      type="button"
                      disabled={busy || !selectedColony || !!currentGoal}
                      className="btn-primary px-4 py-2 text-xs disabled:opacity-40"
                      onClick={() =>
                        void run(async () => {
                          const saved = dirty ? await saveTeam() : detail;
                          if (saved) await startGoal(saved.id, proposal, true);
                        })
                      }
                    >
                      {t(
                        busy
                          ? "common.loading"
                          : draft.autonomy === "plan_only"
                            ? "colony.save_plan"
                            : proposal.new_agents.some(
                                  (agent) => !team.includes(agent.alias),
                                )
                              ? "colony.approve_team_start"
                              : "colony.start",
                      )}
                    </button>
                  ) : (
                    <button
                      type="button"
                      disabled={busy}
                      className="btn-primary px-4 py-2 text-xs disabled:opacity-40"
                      onClick={clarify}
                    >
                      {t(busy ? "common.loading" : "colony.clarify")}
                    </button>
                  )}
                </div>
              </div>
            )}
            {panel === "goal" && detail && draft && (
              <div className="space-y-4">
                <h3 className="text-sm font-semibold">{draft.name}</h3>
                {currentGoal ? (
                  <>
                    <div className="rounded-xl border border-pc-border p-3">
                      <p className="text-xs font-medium text-pc-accent">
                        {pendingControl === "pause"
                          ? t("colony.pausing")
                          : pendingControl === "cancel"
                            ? t("colony.cancelling")
                            : statusLabel(currentGoal)}
                      </p>
                      <p className="mt-2 whitespace-pre-wrap text-xs">
                        {currentGoal.goal.objective}
                      </p>
                      <p className="mt-3 text-[11px] text-pc-text-muted">
                        {t("colony.assignments")}:{" "}
                        {currentGoal.execution.next_assignment} /{" "}
                        {currentGoal.execution.proposal.assignments.length}
                      </p>
                      {currentGoal.goal.pause_description && (
                        <p className="mt-2 text-xs text-status-warning">
                          {currentGoal.goal.pause_description}
                        </p>
                      )}
                      {currentGoal.error && (
                        <p
                          role="alert"
                          className="mt-2 text-xs text-status-error"
                        >
                          {currentGoal.error}
                        </p>
                      )}
                    </div>
                    {(currentGoal.approvals ?? [])
                      .filter((approval) => approval.decision == null)
                      .map((approval) => (
                        <article
                          key={approval.id}
                          className="rounded-lg border border-status-warning/40 p-3"
                        >
                          <h4 className="text-xs font-medium text-status-warning">
                            {t("colony.approval_request")} · {approval.agent}
                          </h4>
                          <p className="mt-2 text-xs">{approval.tool}</p>
                          <pre className="my-3 max-h-36 overflow-auto rounded bg-pc-elevated p-2 text-[11px]">
                            {JSON.stringify(approval.arguments, null, 2)}
                          </pre>
                          <div className="flex gap-2">
                            {[true, false].map((approved) => (
                              <button
                                key={String(approved)}
                                type="button"
                                disabled={busy}
                                className={button}
                                onClick={() =>
                                  void run(async () => {
                                    acceptGoal(
                                      detail.id,
                                      await approveColonyCall(
                                        detail.id,
                                        currentGoal.task.id,
                                        approval.id,
                                        { approved },
                                      ),
                                    );
                                    acceptDetail(
                                      await getColony(detail.id),
                                      !dirty,
                                    );
                                  })
                                }
                              >
                                {t(
                                  approved
                                    ? "colony.approve_once"
                                    : "colony.deny",
                                )}
                              </button>
                            ))}
                          </div>
                        </article>
                      ))}
                    {currentGoal.execution.pending_plan && (
                      <article className="space-y-3 rounded-xl border border-pc-accent/30 bg-pc-accent/5 p-3">
                        <h4 className="text-xs font-medium text-pc-accent">
                          {t("colony.next_plan")}
                        </h4>
                        <p className="whitespace-pre-wrap text-xs leading-relaxed">
                          {currentGoal.execution.pending_plan.summary}
                        </p>
                        {currentGoal.execution.pending_plan.questions.map(
                          (question) => (
                            <label
                              key={question}
                              className="block text-xs text-pc-text-muted"
                            >
                              {question}
                              <textarea
                                className={input}
                                value={pendingAnswers[question] ?? ""}
                                onChange={(event) =>
                                  setPendingPlanAnswers((answers) => ({
                                    ...answers,
                                    [currentGoal.task.id]: {
                                      ...answers[currentGoal.task.id],
                                      [question]: event.target.value,
                                    },
                                  }))
                                }
                              />
                            </label>
                          ),
                        )}
                        {currentGoal.execution.pending_plan.assignments.map(
                          (assignment, index) => (
                            <div
                              key={`${assignment.agent}:${index}`}
                              className="rounded bg-pc-elevated p-3 text-xs"
                            >
                              <p className="font-medium">{assignment.agent}</p>
                              <p className="mt-1 whitespace-pre-wrap text-pc-text-muted">
                                {assignment.instruction}
                              </p>
                            </div>
                          ),
                        )}
                        {currentGoal.execution.pending_plan.new_agents
                          .filter((agent) => !team.includes(agent.alias))
                          .map((agent) => (
                            <div
                              key={agent.alias}
                              className="rounded border border-pc-border p-3 text-xs"
                            >
                              <p className="font-medium">
                                {t("colony.new_agent")}: {agent.alias}
                              </p>
                              <p className="mt-1 text-pc-text-muted">
                                {agent.core_command}
                              </p>
                              <p className="mt-2">
                                {t("colony.agent_template")}: {agent.template}
                              </p>
                              {agent.connections.map((connection) => (
                                <p
                                  key={`${connection.from}:${connection.to}`}
                                  className="mt-1 break-words"
                                >
                                  {connection.from} → {connection.to}
                                </p>
                              ))}
                              <div className="mt-3">
                                <AccessSummary
                                  agents={teamAgents.filter(
                                    (template) =>
                                      template.alias === agent.template,
                                  )}
                                />
                              </div>
                            </div>
                          ))}
                        <p className="text-[11px] text-pc-text-muted">
                          {t("colony.next_plan_hint")}
                        </p>
                        {currentGoal.execution.pending_plan.questions.length ? (
                          <button
                            type="button"
                            className={`${button} w-full`}
                            disabled={
                              busy ||
                              currentGoal.execution.pending_plan.questions.some(
                                (question) => !pendingAnswers[question]?.trim(),
                              )
                            }
                            onClick={() =>
                              void run(async () => {
                                const answers =
                                  currentGoal.execution.pending_plan?.questions.map(
                                    (question) => ({
                                      question,
                                      answer:
                                        pendingAnswers[question]?.trim() ?? "",
                                    }),
                                  ) ?? [];
                                acceptGoal(
                                  detail.id,
                                  await clarifyColonyGoal(
                                    detail.id,
                                    currentGoal.task.id,
                                    { answers },
                                  ),
                                );
                                acceptDetail(
                                  await getColony(detail.id),
                                  !dirty,
                                );
                                setPendingPlanAnswers((answers) => ({
                                  ...answers,
                                  [currentGoal.task.id]: {},
                                }));
                              })
                            }
                          >
                            {t("colony.clarify")}
                          </button>
                        ) : (
                          <button
                            type="button"
                            className="btn-primary w-full px-3 py-2 text-xs disabled:opacity-40"
                            disabled={busy}
                            onClick={() => control("confirm_plan")}
                          >
                            {t("colony.confirm_plan")}
                          </button>
                        )}
                      </article>
                    )}
                    <div className="flex flex-wrap gap-2">
                      <button
                        type="button"
                        disabled={
                          busy ||
                          !!currentGoal.execution.pending_plan ||
                          (currentGoal.task.status === "paused" &&
                            draft.autonomy === "plan_only")
                        }
                        onClick={() =>
                          control(
                            currentGoal.task.status === "paused"
                              ? "resume"
                              : "pause",
                          )
                        }
                        className={button}
                      >
                        {currentGoal.task.status === "paused" ? (
                          <Play className="mr-1 inline h-3.5 w-3.5" />
                        ) : (
                          <Pause className="mr-1 inline h-3.5 w-3.5" />
                        )}
                        {t(
                          currentGoal.task.status === "paused"
                            ? "colony.resume"
                            : "colony.pause",
                        )}
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => control("cancel")}
                        className={`${button} text-status-error`}
                      >
                        <Square className="mr-1 inline h-3 w-3" />
                        {t("colony.cancel_goal")}
                      </button>
                    </div>
                    <p className="text-[11px] text-pc-text-muted">
                      {t("colony.cancel_hint")}
                    </p>
                    {currentGoal.task.status === "paused" &&
                      currentGoal.execution.active_child_id &&
                      !currentGoal.turn_attached && (
                        <div className="rounded-lg border border-status-warning/30 p-3">
                          <p className="text-[11px] text-status-warning">
                            {t("colony.uncertain_hint")}
                          </p>
                          <div className="mt-3 flex flex-wrap gap-2">
                            <button
                              type="button"
                              className={button}
                              disabled={busy}
                              onClick={() => control("retry_turn")}
                            >
                              {t("colony.retry_uncertain")}
                            </button>
                            <button
                              type="button"
                              className={button}
                              disabled={busy}
                              onClick={() => control("skip_turn")}
                            >
                              {t("colony.skip_uncertain")}
                            </button>
                          </div>
                        </div>
                      )}
                  </>
                ) : (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={newGoal}
                    className="btn-primary px-4 py-2 text-xs"
                  >
                    {t("colony.new_goal")}
                  </button>
                )}
                {!currentGoal && latestResult && (
                  <article className="rounded-xl border border-pc-accent/30 bg-pc-accent/5 p-3">
                    <h4 className="text-xs font-medium text-pc-accent">
                      {t("colony.result")}
                    </h4>
                    <p className="mt-2 whitespace-pre-wrap break-words text-xs leading-relaxed">
                      {latestResult.content}
                    </p>
                  </article>
                )}
                <details className="rounded-lg border border-pc-border p-3">
                  <summary className="cursor-pointer text-xs font-medium">
                    {t("colony.queen_settings")}
                  </summary>
                  <div className="mt-4">
                    <SettingsFields value={draft} onChange={update} />
                  </div>
                </details>
                <ContextSummary
                  value={draft}
                  agents={contextAgents}
                  onChange={update}
                />
                <AccessSummary agents={teamAgents} />
                <h3 className="text-xs font-medium">{t("colony.history")}</h3>
                {detail.goals.length ? (
                  detail.goals.map((goal) => (
                    <details
                      key={goal.task.id}
                      className="rounded-lg border border-pc-border p-3"
                    >
                      <summary className="cursor-pointer text-xs">
                        {statusLabel(goal)} ·{" "}
                        {goal.goal.objective.split("\n")[0]}
                      </summary>
                      <div className="mt-3 space-y-2">
                        {goal.messages.map((m) => (
                          <div
                            key={m.id}
                            className="rounded bg-pc-elevated p-2 text-xs"
                          >
                            <span className="font-medium">{m.sender}</span>
                            <p className="mt-1 whitespace-pre-wrap break-words text-pc-text-muted">
                              {m.content}
                            </p>
                          </div>
                        ))}
                      </div>
                    </details>
                  ))
                ) : (
                  <p className="text-xs text-pc-text-muted">
                    {t("colony.no_goals")}
                  </p>
                )}
              </div>
            )}
            {panel === "node" && (
              <div className="space-y-4">
                {newAgent && (
                  <div className="space-y-4">
                    <h3 className="text-sm font-medium">
                      {t("colony.new_agent")}
                    </h3>
                    <p className="text-xs text-pc-text-muted">
                      {t("colony.new_agent_hint")}
                    </p>
                    <label className="block text-xs">
                      {t("colony.agent_alias")}
                      <input
                        className={input}
                        value={newAgent.alias}
                        onChange={(e) =>
                          setNewAgent({ ...newAgent, alias: e.target.value })
                        }
                      />
                    </label>
                    <label className="block text-xs">
                      {t("colony.core_command")}
                      <textarea
                        className={input}
                        value={newAgent.core_command}
                        onChange={(e) =>
                          setNewAgent((a) =>
                            a ? { ...a, core_command: e.target.value } : a,
                          )
                        }
                      />
                    </label>
                    <label className="block text-xs">
                      {t("colony.agent_template")}
                      <select
                        className={input}
                        value={newAgent.template}
                        onChange={(e) =>
                          setNewAgent((a) =>
                            a ? { ...a, template: e.target.value } : a,
                          )
                        }
                      >
                        {snapshot?.agents
                          .filter((a) => a.enabled && team.includes(a.alias))
                          .map((a) => (
                            <option key={a.alias} value={a.alias}>
                              {a.alias}
                            </option>
                          ))}
                      </select>
                    </label>
                    <AccessSummary
                      agents={
                        snapshot?.agents.filter(
                          (a) => a.alias === newAgent.template,
                        ) ?? []
                      }
                    />
                    <fieldset className="space-y-2">
                      <legend className="mb-2 text-xs">
                        {t("colony.connections")}
                      </legend>
                      <>
                        <label className="flex items-center gap-2 text-xs">
                          <input
                            type="checkbox"
                            checked={newAgentSend}
                            onChange={(e) => setNewAgentSend(e.target.checked)}
                          />
                          {draft?.queen} →{" "}
                          {newAgent.alias || t("colony.new_agent")}
                        </label>
                        <label className="flex items-center gap-2 text-xs">
                          <input
                            type="checkbox"
                            checked={newAgentReturn}
                            onChange={(e) =>
                              setNewAgentReturn(e.target.checked)
                            }
                          />
                          {newAgent.alias || t("colony.new_agent")} →{" "}
                          {draft?.queen}
                        </label>
                      </>
                    </fieldset>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={addAgent}
                      className="btn-primary px-4 py-2 text-xs"
                    >
                      {t("colony.new_agent_approve")}
                    </button>
                    <button
                      type="button"
                      className={button}
                      onClick={() => setNewAgent(null)}
                    >
                      {t("common.cancel")}
                    </button>
                  </div>
                )}

                {agent && (
                  <>
                    <div className="flex items-center gap-2">
                      <Bot className="h-4 w-4 text-pc-accent" />
                      <h3 className="break-all text-sm font-semibold">
                        {agent.alias}
                      </h3>
                    </div>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.core_command")}
                      <textarea
                        className={input}
                        value={agentCommand}
                        onChange={(e) => setAgentCommand(e.target.value)}
                      />
                    </label>
                    <button
                      type="button"
                      disabled={
                        busy ||
                        !agentCommand.trim() ||
                        agentCommand === agent.core_command
                      }
                      className={button}
                      onClick={() =>
                        void run(async () => {
                          await putProp(
                            `agents.${agent.alias}.core-command`,
                            agentCommand,
                          );
                          await refresh();
                        })
                      }
                    >
                      {t("colony.save_core_command")}
                    </button>
                    <p className="text-[11px] leading-relaxed text-pc-text-muted">
                      {t("colony.core_hint")}
                    </p>
                    <AccessSummary agents={[agent]} />
                    {selectedColony && draft && draft.queen !== agent.alias && (
                      <>
                        <button
                          type="button"
                          disabled={
                            busy ||
                            currentGoal?.task.status === "running" ||
                            agent.active_turns > 0
                          }
                          onClick={removeMember}
                          className={`${button} text-status-error`}
                        >
                          {t("colony.remove_agent")}
                        </button>
                        <p className="text-[11px] text-pc-text-muted">
                          {t("colony.remove_hint")}
                        </p>
                      </>
                    )}
                    {!selectedColony && (
                      <button
                        type="button"
                        className={button}
                        onClick={() => onOpenAgent(agent.alias)}
                      >
                        {t("colony.open_chat")}
                      </button>
                    )}
                  </>
                )}
                {prompt && (
                  <>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.text")}
                      <textarea
                        className={`${input} min-h-40`}
                        value={prompt.text}
                        onChange={(e) => setPrompt({ text: e.target.value })}
                      />
                    </label>
                    <Choices
                      label={t("colony.prompt_targets")}
                      aliases={team}
                      values={prompt.agents}
                      onChange={(agents) => setPrompt({ agents })}
                    />
                    <fieldset>
                      <legend className="mb-2 text-xs text-pc-text-muted">
                        {t("colony.prompt_timing")}
                      </legend>
                      {(["now", "next_run"] as const).map((timing) => (
                        <label
                          key={timing}
                          className="mb-2 flex items-center gap-2 text-xs"
                        >
                          <input
                            type="radio"
                            name="colony-prompt-timing"
                            checked={promptTiming === timing}
                            onChange={() => setPromptTiming(timing)}
                          />
                          {t(
                            timing === "now"
                              ? "colony.prompt_now"
                              : "colony.prompt_next",
                          )}
                        </label>
                      ))}
                    </fieldset>
                    <div className="flex gap-2">
                      {([-1, 1] as const).map((direction) => (
                        <button
                          key={direction}
                          type="button"
                          className={button}
                          aria-label={t(
                            direction < 0
                              ? "colony.reorder_up"
                              : "colony.reorder_down",
                          )}
                          onClick={() => {
                            if (!draft) return;
                            const list = [...draft.prompts],
                              i = list.findIndex((p) => p.id === prompt.id),
                              j = i + direction;
                            if (j >= 0 && j < list.length) {
                              [list[i], list[j]] = [list[j]!, list[i]!];
                              update({ prompts: list });
                            }
                          }}
                        >
                          {direction < 0 ? (
                            <ChevronUp className="h-4 w-4" />
                          ) : (
                            <ChevronDown className="h-4 w-4" />
                          )}
                        </button>
                      ))}
                    </div>
                  </>
                )}
                {room && (
                  <>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.room_name")}
                      <input
                        className={input}
                        value={room.name}
                        onChange={(e) => setRoom({ name: e.target.value })}
                      />
                    </label>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.room_policy")}
                      <select
                        className={input}
                        value={room.responders}
                        onChange={(e) =>
                          setRoom({
                            responders: e.target
                              .value as typeof room.responders,
                          })
                        }
                      >
                        {(["addressed", "queen_selected", "open"] as const).map(
                          (r) => (
                            <option key={r} value={r}>
                              {t(`colony.responders_${r}`)}
                            </option>
                          ),
                        )}
                      </select>
                    </label>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.max_turns")}
                      <input
                        className={input}
                        type="number"
                        min="1"
                        max="100"
                        value={room.max_turns}
                        onChange={(e) =>
                          setRoom({ max_turns: Number(e.target.value) })
                        }
                      />
                    </label>
                    <p className="text-[11px] text-pc-text-muted">
                      {t("colony.room_hint")}
                    </p>
                    <Choices
                      label={t("colony.readers")}
                      aliases={team}
                      values={room.readers}
                      onChange={(readers) => setRoom({ readers })}
                    />
                    <Choices
                      label={t("colony.publishers")}
                      aliases={team}
                      values={room.publishers}
                      onChange={(publishers) => setRoom({ publishers })}
                    />
                  </>
                )}
                {channel && (
                  <>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.channel")}
                      <select
                        className={input}
                        value={channel.channel}
                        onChange={(e) =>
                          setChannel({ channel: e.target.value })
                        }
                      >
                        {snapshot?.channels.map((c) => (
                          <option key={c.id} value={c.id} disabled={!c.enabled}>
                            {c.label}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label className="block text-xs text-pc-text-muted">
                      {t("colony.conversation")}
                      <input
                        className={input}
                        value={channel.conversation}
                        onChange={(e) =>
                          setChannel({ conversation: e.target.value })
                        }
                      />
                    </label>
                    <p className="text-[11px] text-pc-text-muted">
                      {t("colony.channel_hint")}
                    </p>
                    <Choices
                      label={t("colony.inbound")}
                      aliases={team}
                      values={channel.inbound_agents}
                      onChange={(inbound_agents) =>
                        setChannel({ inbound_agents })
                      }
                    />
                    <Choices
                      label={t("colony.outbound")}
                      aliases={team}
                      values={channel.outbound_agents}
                      onChange={(outbound_agents) =>
                        setChannel({ outbound_agents })
                      }
                    />
                  </>
                )}
                {(prompt || room || channel) && (
                  <button
                    type="button"
                    onClick={removeNode}
                    className={`${button} text-status-error`}
                  >
                    {t("colony.remove")}
                  </button>
                )}
                {selectedColony && draft && (
                  <details className="rounded-lg border border-pc-border p-3">
                    <summary className="cursor-pointer text-xs font-medium">
                      {t("colony.connections")}
                    </summary>
                    <p className="my-3 text-[11px] text-pc-text-muted">
                      {t("colony.connections_hint")}
                    </p>
                    {draft.connections.map((e) => (
                      <div
                        key={`${e.from}:${e.to}`}
                        className="mb-2 flex items-center gap-1 break-all text-xs"
                      >
                        <span className="flex-1">
                          {e.from} → {e.to}
                        </span>
                        <button
                          type="button"
                          aria-label={`${t("colony.remove_connection")} ${e.from} → ${e.to}`}
                          onClick={() =>
                            update({
                              connections: draft.connections.filter(
                                (v) => !(v.from === e.from && v.to === e.to),
                              ),
                            })
                          }
                          className="rounded p-1 text-pc-text-muted hover:text-status-error"
                        >
                          <X className="h-3.5 w-3.5" />
                        </button>
                      </div>
                    ))}
                    <label className="block text-xs">
                      {t("colony.source")}
                      <select
                        className={input}
                        value={edgeFrom}
                        onChange={(e) => setEdgeFrom(e.target.value)}
                      >
                        <option value="">—</option>
                        {graph.nodes
                          .filter((n) => n.colonyId === selectedColony)
                          .map((n) => (
                            <option key={n.id} value={n.id}>
                              {n.label}
                            </option>
                          ))}
                      </select>
                    </label>
                    <label className="mt-2 block text-xs">
                      {t("colony.target")}
                      <select
                        className={input}
                        value={edgeTo}
                        onChange={(e) => setEdgeTo(e.target.value)}
                      >
                        <option value="">—</option>
                        {graph.nodes
                          .filter((n) => n.colonyId === selectedColony)
                          .map((n) => (
                            <option key={n.id} value={n.id}>
                              {n.label}
                            </option>
                          ))}
                      </select>
                    </label>
                    <button
                      type="button"
                      disabled={!edgeFrom || !edgeTo}
                      onClick={() => connect(edgeFrom, edgeTo)}
                      className={`${button} mt-3`}
                    >
                      <ArrowRight className="mr-1 inline h-3 w-3" />
                      {t("colony.grant")}
                    </button>
                  </details>
                )}
                {showConversation && (
                  <div className="border-t border-pc-border pt-4">
                    <h3 className="mb-3 text-xs font-medium">
                      {t("colony.chat")}
                    </h3>
                    <p className="mb-3 text-[11px] text-pc-text-muted">
                      {t("colony.busy_message")}
                    </p>
                    <div className="space-y-2">
                      {conversation.length ? (
                        conversation.map((m) => (
                          <article
                            key={m.id}
                            className="rounded-lg bg-pc-elevated p-3 text-xs"
                          >
                            <div className="text-[10px] font-medium text-pc-accent">
                              {m.sender}
                            </div>
                            <p className="mt-1 whitespace-pre-wrap break-words leading-relaxed">
                              {m.content}
                            </p>
                          </article>
                        ))
                      ) : (
                        <p className="text-xs text-pc-text-faint">
                          {t("colony.no_messages")}
                        </p>
                      )}
                    </div>
                    {room && (
                      <label className="mt-3 block text-xs text-pc-text-muted">
                        {t("colony.room_address")}
                        <select
                          className={input}
                          value={roomAddress}
                          onChange={(event) =>
                            setRoomAddress(event.target.value)
                          }
                        >
                          <option value="">{t("colony.room_everyone")}</option>
                          {room.publishers
                            .filter((alias) => room.readers.includes(alias))
                            .map((alias) => (
                              <option key={alias} value={alias}>
                                {alias}
                              </option>
                            ))}
                        </select>
                      </label>
                    )}
                    <textarea
                      aria-label={t("colony.message")}
                      className={`${input} mt-3 min-h-24`}
                      value={message}
                      onChange={(e) => setMessage(e.target.value)}
                      placeholder={t("colony.message")}
                      onKeyDown={(e) => {
                        if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                          e.preventDefault();
                          send();
                        }
                      }}
                    />
                    <button
                      type="button"
                      disabled={busy || !message.trim()}
                      onClick={send}
                      className="btn-primary mt-2 inline-flex items-center gap-2 px-4 py-2 text-xs disabled:opacity-40"
                    >
                      <Send className="h-3.5 w-3.5" />
                      {t("colony.send")}
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
          {draft && selectedColony && dirty && panel !== "setup" && (
            <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-pc-border p-3">
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await saveTeam();
                    await refresh();
                  })
                }
                className="btn-primary inline-flex items-center gap-1.5 px-3 py-2 text-xs disabled:opacity-40"
              >
                <Save className="h-3.5 w-3.5" />
                {t("common.save")}
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    const id = selectedRef.current;
                    if (id) acceptDetail(await getColony(id));
                  })
                }
                className={button}
              >
                {t("colony.reload")}
              </button>
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}
