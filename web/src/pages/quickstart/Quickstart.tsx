import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  Bot,
  Check,
  ChevronRight,
  Cpu,
  FileText,
  HardDrive,
  Plus,
  Radio,
  ShieldCheck,
  Trash2,
  Users,
} from "lucide-react";
import {
  type ModelsResponse,
  type QuickstartError,
  type QuickstartFieldDescriptor,
  type QuickstartState,
  type QuickstartStep,
  getCatalogModels,
  getPersonalityTemplates,
  getQuickstartState,
  quickstartApply,
  quickstartDismiss,
  quickstartFields,
} from "@/lib/api";
import { Badge, Button, Card, PageHeader } from "@/components/ui";
import { t } from "@/lib/i18n";
import {
  requiredQuickstartSelectionsComplete,
  runtimeAfterProviderChange,
  runtimeDefaultForProvider,
  runtimeValueForSubmit,
  type RuntimeSelection,
} from "./runtime-selection";

// Shared tokenized field control classes. Calm input surface with an accent
// focus ring — replaces the legacy `input-electric` utility.
const INPUT_CLASS =
  "w-full h-9 px-3 rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-sm text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pc-accent/40 focus-visible:border-pc-accent/40";
const TEXTAREA_CLASS =
  "w-full px-3 py-2 rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-sm text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pc-accent/40 focus-visible:border-pc-accent/40";

interface StagedProvider {
  provider_type: string;
  alias: string;
  model: string;
  /** Round-trip of `FieldDescriptor.key` -> user-typed value.
   *  The web surface knows nothing about which keys exist; the
   *  daemon authors them via `/api/quickstart/fields` and consumes
   *  them on the way back. */
  fields: Record<string, string>;
}

interface StagedChannel {
  mode: "fresh" | "existing";
  channel_type: string;
  alias: string;
  fields: Record<string, string>;
}

interface StagedPeerGroup {
  /** Default `<type>_<alias>_default`, derived from the channel ref. */
  name: string;
  /** `<type>.<alias>` — either a staged channel or an unassigned existing one. */
  channel: string;
  external_peers: string[];
}

interface StagedPersonalityFile {
  filename: string;
  content: string;
}

interface StagedPreset {
  preset_name: string;
}

interface FormState {
  provider: StagedProvider | null;
  risk: StagedPreset | null;
  runtime: RuntimeSelection | null;
  memory: StagedPreset | null;
  channels: StagedChannel[];
  peerGroups: StagedPeerGroup[];
  agentName: string;
  personalityFiles: StagedPersonalityFile[];
}

const DEFAULT_FORM: FormState = {
  provider: null,
  risk: null,
  runtime: null,
  memory: null,
  channels: [],
  peerGroups: [],
  agentName: "",
  personalityFiles: [],
};

const MUTED = { color: "var(--pc-text-muted)" } as const;
const FAINT = { color: "var(--pc-text-faint)" } as const;
const ERROR = { color: "var(--color-status-error)" } as const;

export default function Quickstart() {
  const navigate = useNavigate();
  const [form, setForm] = useState<FormState>(DEFAULT_FORM);
  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<QuickstartError[]>([]);
  const [state, setState] = useState<QuickstartState | null>(null);
  const runIdRef = useRef<string>(
    `${Date.now().toString(16)}${Math.random().toString(16).slice(2, 10)}`,
  );
  const lastStepRef = useRef<QuickstartStep | null>(null);
  const submittedRef = useRef(false);
  const runtimeAutoDefaultedRef = useRef(true);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const s = await getQuickstartState();
        if (!cancelled) {
          setState(s);
          const defaultRuntime = runtimeDefaultForProvider(s);
          if (defaultRuntime) {
            setForm((f) =>
              f.runtime
                ? f
                : { ...f, runtime: { preset_name: defaultRuntime } },
            );
          }
        }
      } catch {
        /* surfaces empty pickers + error on submit */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const fire = () => {
      if (submittedRef.current) return;
      quickstartDismiss({
        run_id: runIdRef.current,
        surface: "web",
        last_step: lastStepRef.current,
      });
    };
    window.addEventListener("beforeunload", fire);
    return () => {
      window.removeEventListener("beforeunload", fire);
      fire();
    };
  }, []);

  const recordStep = (s: QuickstartStep) => {
    lastStepRef.current = s;
  };

  const submit = async () => {
    const runtimeProfile = runtimeValueForSubmit(form.runtime);
    if (!runtimeProfile) return;
    setBusy(true);
    setErrors([]);
    const res = await quickstartApply({
      model_provider: { mode: "fresh", value: form.provider! },
      risk_profile: { mode: "fresh", value: form.risk!.preset_name },
      runtime_profile: {
        mode: "fresh",
        value: runtimeProfile,
      },
      memory: { mode: "fresh", value: form.memory!.preset_name },
      channels: form.channels.map((c) =>
        c.mode === "existing"
          ? { mode: "existing", value: `${c.channel_type}.${c.alias}` }
          : {
              mode: "fresh",
              value: {
                channel_type: c.channel_type,
                alias: c.alias,
                fields: c.fields,
              },
            },
      ),
      peer_groups: form.peerGroups,
      agent: {
        name: form.agentName,
        system_prompt: "",
        personality_file: null,
        personality_files: form.personalityFiles,
      },
    });
    setBusy(false);
    if (res.kind === "errors") {
      setErrors(res.errors);
      return;
    }
    submittedRef.current = true;
    navigate(`/agent/${encodeURIComponent(res.agent.alias)}`);
  };

  const providerDone = form.provider !== null;
  const riskDone = form.risk !== null;
  const runtimeDone = form.runtime !== null;
  const memoryDone = form.memory !== null;
  const agentDone = form.agentName.trim() !== "";
  const allDone = requiredQuickstartSelectionsComplete(form);

  // Required-step progress for the wizard stepper. Channels / peer groups /
  // personality files are optional and intentionally excluded from the gate.
  const steps = [
    { label: t("quickstart.step_provider"), done: providerDone },
    { label: t("quickstart.step_risk"), done: riskDone },
    { label: t("quickstart.runtime_profile_title"), done: runtimeDone },
    { label: t("quickstart.step_memory"), done: memoryDone },
    { label: t("quickstart.step_agent"), done: agentDone },
  ];
  const completedCount = steps.filter((s) => s.done).length;

  return (
    <div className="max-w-3xl mx-auto px-6 py-8 space-y-5">
      <PageHeader
        title={t("quickstart.title")}
        description={t("quickstart.description")}
        actions={
          <div className="flex items-center gap-3">
            <Badge tone={allDone ? "ok" : "neutral"}>
              {completedCount}/{steps.length} {t("quickstart.required")}
            </Badge>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                // Treat an explicit exit like an abandon: record the dismissal
                // so the daemon stops treating this run as in-flight, then leave
                // to the dashboard. `submittedRef` short-circuits the duplicate
                // dismiss the unmount/`beforeunload` handler would otherwise fire.
                submittedRef.current = true;
                quickstartDismiss({
                  run_id: runIdRef.current,
                  surface: "web",
                  last_step: lastStepRef.current,
                });
                navigate("/");
              }}
              title={t("quickstart.skip_setup_title")}
            >
              {t("quickstart.skip_setup")}
            </Button>
          </div>
        }
      />

      <Stepper steps={steps} />

      <Section
        icon={<Cpu className="h-4 w-4" />}
        title={t("quickstart.model_provider_title")}
        done={providerDone}
        summary={
          form.provider
            ? `${form.provider.provider_type}.${form.provider.alias} — ${form.provider.model}`
            : null
        }
      >
        {form.provider ? (
          <StagedRow
            label={`${form.provider.provider_type}.${form.provider.alias}`}
            sub={`${t("quickstart.model_prefix")}${form.provider.model}`}
            onRemove={() => setForm((f) => ({ ...f, provider: null }))}
          />
        ) : (
          <ProviderForm
            state={state}
            onStage={(p) => {
              setForm((f) => {
                const runtime = runtimeAfterProviderChange(
                  state,
                  p.provider_type,
                  f.runtime,
                  runtimeAutoDefaultedRef.current,
                );
                return {
                  ...f,
                  provider: p,
                  runtime,
                };
              });
              recordStep("model_provider");
            }}
          />
        )}
      </Section>

      <PresetSection
        icon={<ShieldCheck className="h-4 w-4" />}
        title={t("quickstart.risk_profile_title")}
        rows={(state?.risk_presets ?? []).map((p) => ({
          value: p.preset_name,
          label: p.label,
          help: p.help,
        }))}
        value={form.risk?.preset_name ?? ""}
        onChange={(v) => {
          setForm((f) => ({ ...f, risk: { preset_name: v } }));
          recordStep("risk_profile");
        }}
      />

      <PresetSection
        icon={<Cpu className="h-4 w-4" />}
        title={t("quickstart.runtime_profile_title")}
        rows={(state?.runtime_presets ?? []).map((p) => ({
          value: p.preset_name,
          label: p.label,
          help: p.help,
        }))}
        value={form.runtime?.preset_name ?? ""}
        onChange={(v) => {
          runtimeAutoDefaultedRef.current = false;
          setForm((f) => ({ ...f, runtime: { preset_name: v } }));
          recordStep("runtime_profile");
        }}
      />

      <PresetSection
        icon={<HardDrive className="h-4 w-4" />}
        title={t("quickstart.memory_title")}
        rows={(state?.memory_kinds ?? []).map((k) => ({
          value: k,
          label: k,
          help: "",
        }))}
        value={form.memory?.preset_name ?? ""}
        onChange={(v) => {
          setForm((f) => ({ ...f, memory: { preset_name: v } }));
          recordStep("memory");
        }}
      />

      <Section
        icon={<Radio className="h-4 w-4" />}
        title={t("quickstart.channels_title")}
        done={true}
        summary={
          form.channels.length === 0
            ? t("quickstart.channels_none")
            : `${form.channels.length} ${t("quickstart.configured_suffix")}`
        }
      >
        <ChannelsList
          state={state}
          staged={form.channels}
          onAdd={(c) => {
            setForm((f) => ({ ...f, channels: [...f.channels, c] }));
            recordStep("channels");
          }}
          onRemove={(i) =>
            setForm((f) => {
              const removed = f.channels[i];
              const ref = removed
                ? `${removed.channel_type}.${removed.alias}`
                : null;
              return {
                ...f,
                channels: f.channels.filter((_, idx) => idx !== i),
                peerGroups: ref
                  ? f.peerGroups.filter((pg) => pg.channel !== ref)
                  : f.peerGroups,
              };
            })
          }
        />
      </Section>

      <Section
        icon={<Users className="h-4 w-4" />}
        title={t("quickstart.peer_groups_title")}
        done={true}
        summary={
          form.peerGroups.length === 0
            ? t("quickstart.peer_groups_none")
            : `${form.peerGroups.length} ${t("quickstart.configured_suffix")}`
        }
      >
        <PeerGroupsList
          state={state}
          stagedChannels={form.channels}
          stagedPeerGroups={form.peerGroups}
          onAdd={(pg) =>
            setForm((f) => ({ ...f, peerGroups: [...f.peerGroups, pg] }))
          }
          onRemove={(i) =>
            setForm((f) => ({
              ...f,
              peerGroups: f.peerGroups.filter((_, idx) => idx !== i),
            }))
          }
        />
      </Section>

      <Section
        icon={<Bot className="h-4 w-4" />}
        title={t("quickstart.agent_title")}
        done={form.agentName.trim() !== ""}
        summary={form.agentName.trim() || null}
      >
        <LabeledInput
          label={t("common.name")}
          value={form.agentName}
          onChange={(v) => {
            // Agent aliases must be lowercase — normalize as the user types so
            // a stray capital (or paste) can't fail validation at apply time.
            setForm((f) => ({ ...f, agentName: v.toLowerCase() }));
            recordStep("agent");
          }}
          placeholder={t("quickstart.agent_name_placeholder")}
        />
      </Section>

      <Section
        icon={<FileText className="h-4 w-4" />}
        title={t("quickstart.personality_files_title")}
        done={true}
        summary={
          form.personalityFiles.length === 0
            ? t("quickstart.personality_files_none")
            : `${form.personalityFiles.length} ${t("quickstart.staged_suffix")}`
        }
      >
        <PersonalityFilesList
          state={state}
          staged={form.personalityFiles}
          onStage={(file) =>
            setForm((f) => {
              const others = f.personalityFiles.filter(
                (p) => p.filename !== file.filename,
              );
              return { ...f, personalityFiles: [...others, file] };
            })
          }
          onRemove={(filename) =>
            setForm((f) => ({
              ...f,
              personalityFiles: f.personalityFiles.filter(
                (p) => p.filename !== filename,
              ),
            }))
          }
        />
      </Section>

      {errors.length > 0 && (
        <ul className="rounded-[var(--radius-md)] border border-status-error/20 bg-status-error/10 p-4 space-y-1 text-sm text-status-error">
          {errors.map((e, i) => (
            <li key={i}>
              <code>
                {e.step}
                {e.field ? `.${e.field}` : ""}
              </code>
              : {e.message}
            </li>
          ))}
        </ul>
      )}

      <div className="flex justify-end pt-2">
        <Button
          size="md"
          className="px-6"
          disabled={busy || !allDone}
          onClick={() => void submit()}
        >
          {busy ? t("quickstart.creating") : t("quickstart.create")}
        </Button>
      </div>
    </div>
  );
}

function Stepper({ steps }: { steps: { label: string; done: boolean }[] }) {
  // The first not-yet-done step is treated as the "active" cursor so the
  // accent lands on what the operator should fill in next.
  const activeIdx = steps.findIndex((s) => !s.done);
  return (
    <ol className="flex items-center gap-2" aria-label={t("quickstart.setup_progress")}>
      {steps.map((step, i) => {
        const active = i === activeIdx;
        const state = step.done
          ? "bg-pc-accent/10 border-pc-accent/30 text-pc-accent"
          : active
            ? "bg-pc-elevated border-pc-border-strong text-pc-text"
            : "bg-pc-surface border-pc-border text-pc-text-muted";
        return (
          <li key={step.label} className="flex items-center gap-2 flex-1 min-w-0">
            <div
              className={`flex items-center gap-2 px-3 py-1.5 rounded-[var(--radius-md)] border text-xs font-medium min-w-0 ${state}`}
            >
              <span
                className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[11px] ${
                  step.done
                    ? "bg-pc-accent/20 text-pc-accent"
                    : active
                      ? "bg-pc-accent text-[#0b1220]"
                      : "bg-pc-elevated text-pc-text-muted"
                }`}
              >
                {step.done ? <Check className="h-3 w-3" /> : i + 1}
              </span>
              <span className="truncate">{step.label}</span>
            </div>
            {i < steps.length - 1 && (
              <span
                className={`h-px flex-1 ${step.done ? "bg-pc-accent/30" : "bg-pc-border"}`}
                aria-hidden="true"
              />
            )}
          </li>
        );
      })}
    </ol>
  );
}

function Section({
  icon,
  title,
  done,
  summary,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  done: boolean;
  summary?: string | null;
  children: React.ReactNode;
}) {
  return (
    <Card className="p-5 space-y-4">
      <header className="flex items-center gap-3">
        <span
          className={`flex h-7 w-7 items-center justify-center rounded-[var(--radius-md)] ${
            done
              ? "bg-status-success/10 text-status-success"
              : "bg-pc-elevated text-pc-text-muted"
          }`}
        >
          {icon}
        </span>
        <h2 className="font-semibold flex-1 flex items-center gap-2 text-pc-text">
          {done && <Check className="h-4 w-4 text-status-success" />}
          {title}
        </h2>
        {summary && (
          <span className="text-xs" style={MUTED}>
            {summary}
          </span>
        )}
      </header>
      <div className="space-y-3">{children}</div>
    </Card>
  );
}

function PresetSection({
  icon,
  title,
  rows,
  value,
  onChange,
}: {
  icon: React.ReactNode;
  title: string;
  rows: { value: string; label: string; help: string }[];
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <Section
      icon={icon}
      title={title}
      done={value !== ""}
      summary={value || null}
    >
      {rows.length === 0 ? (
        <div className="text-xs" style={MUTED}>
          {t("common.loading")}
        </div>
      ) : (
        <div className="rounded-[var(--radius-md)] border border-pc-border bg-pc-base divide-y divide-pc-border overflow-hidden">
          {rows.map((r) => {
            const selected = r.value === value;
            return (
              <button
                key={r.value}
                type="button"
                onClick={() => onChange(r.value)}
                className={`w-full flex items-center gap-3 px-4 py-3 text-left text-sm transition-colors ${
                  selected
                    ? "bg-pc-accent/[0.08] text-pc-text"
                    : "text-pc-text hover:bg-[var(--pc-hover)]"
                }`}
              >
                <div className="flex-1 min-w-0">
                  <div className="font-medium">{r.label}</div>
                  {r.help && (
                    <div className="text-xs mt-0.5" style={MUTED}>
                      {r.help}
                    </div>
                  )}
                </div>
                {selected && (
                  <ChevronRight className="h-4 w-4 flex-shrink-0 text-pc-accent" />
                )}
              </button>
            );
          })}
        </div>
      )}
    </Section>
  );
}

function StagedRow({
  label,
  sub,
  onRemove,
}: {
  label: string;
  sub?: string;
  onRemove: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 px-4 py-3 rounded-[var(--radius-md)] bg-pc-elevated">
      <div className="min-w-0">
        <div className="font-medium text-pc-text">{label}</div>
        {sub && (
          <code className="block text-xs mt-0.5" style={FAINT}>
            {sub}
          </code>
        )}
      </div>
      <Button variant="ghost" size="sm" onClick={onRemove} title={t("quickstart.clear")}>
        <Trash2 className="h-4 w-4" />
      </Button>
    </div>
  );
}

function LabeledInput({
  label,
  value,
  onChange,
  type = "text",
  placeholder,
  multiline = false,
  help,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  type?: "text" | "password";
  placeholder?: string;
  multiline?: boolean;
  help?: string;
}) {
  return (
    <label className="block">
      <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
        {label}
      </div>
      {help ? (
        <div className="text-xs mb-1 italic" style={MUTED}>
          {help}
        </div>
      ) : null}
      {multiline ? (
        <textarea
          className={`${TEXTAREA_CLASS} min-h-24`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
        />
      ) : (
        <input
          className={INPUT_CLASS}
          type={type}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          // Aliases, model names, API keys etc. are technical identifiers —
          // the WebView must not autocapitalize/autocorrect them (e.g. agent
          // aliases must be lowercase).
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
        />
      )}
    </label>
  );
}

function LabeledSelect({
  label,
  value,
  onChange,
  options,
  help,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  options: string[];
  help?: string;
}) {
  return (
    <label className="block">
      <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
        {label}
      </div>
      {help ? (
        <div className="text-xs mb-1 italic" style={MUTED}>
          {help}
        </div>
      ) : null}
      <select
        className={INPUT_CLASS}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        {options.map((option) => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
      </select>
    </label>
  );
}

function ProviderForm({
  state,
  onStage,
}: {
  state: QuickstartState | null;
  onStage: (p: StagedProvider) => void;
}) {
  const [type, setType] = useState("");
  const [alias, setAlias] = useState("default");
  const [model, setModel] = useState("");
  // Generic field-buffer keyed by descriptor key. The web surface
  // knows nothing about which keys exist; whatever the daemon emits
  // in `quickstart/fields` gets a corresponding `<input>` here.
  const [fieldValues, setFieldValues] = useState<Record<string, string>>({});
  const [catalog, setCatalog] = useState<ModelsResponse | null>(null);
  const [descriptors, setDescriptors] = useState<QuickstartFieldDescriptor[]>(
    [],
  );

  useEffect(() => {
    if (!type) {
      setCatalog(null);
      setDescriptors([]);
      setFieldValues({});
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const f = await quickstartFields({
          section: "model_provider",
          type_key: type,
        });
        if (!cancelled) {
          setDescriptors(f.fields);
          // Reset the buffer to an empty value per descriptor so the
          // ghost-text placeholder (descriptor.default) is what the
          // user sees until they type. Enum rows are different: the
          // selected value is real state, so seed it from the descriptor
          // default or first variant and submit it back.
          const next: Record<string, string> = {};
          for (const d of f.fields) {
            next[d.key] =
              d.enum_variants && d.enum_variants.length > 0
                ? (d.default ?? d.enum_variants[0] ?? "")
                : "";
          }
          setFieldValues(next);
        }
      } catch {
        if (!cancelled) {
          setDescriptors([]);
          setFieldValues({});
        }
      }
      try {
        const r = await getCatalogModels(type);
        if (!cancelled) setCatalog(r);
      } catch {
        if (!cancelled) setCatalog(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [type]);

  const isLocal = useMemo(
    () =>
      state?.model_provider_types.find((t) => t.kind === type)?.local ?? false,
    [state, type],
  );
  // A required secret descriptor (e.g. `api-key`) is the gate that
  // prevents adding the provider when the user hasn't pasted a key.
  // Local providers (Ollama, etc.) carry `local = true` and skip the
  // gate even if a secret descriptor happens to exist.
  const missingRequiredSecret = descriptors.some(
    (d) =>
      d.required &&
      d.is_secret &&
      !isLocal &&
      (fieldValues[d.key] ?? "").trim() === "",
  );
  const canAdd =
    type !== "" &&
    alias.trim() !== "" &&
    model.trim() !== "" &&
    !missingRequiredSecret;

  return (
    <>
      <label className="block">
        <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
          {t("quickstart.provider_type")}
        </div>
        <select
          className={INPUT_CLASS}
          value={type}
          onChange={(e) => {
            const next = e.target.value;
            setType(next);
            setModel("");
          }}
        >
          <option value="" disabled>
            {t("quickstart.pick_provider")}
          </option>
          {state?.model_provider_types.map((opt) => (
            <option key={opt.kind} value={opt.kind}>
              {opt.display_name}
              {opt.local ? ` ${t("quickstart.local_suffix")}` : ""}
            </option>
          ))}
        </select>
      </label>

      <LabeledInput label={t("quickstart.alias")} value={alias} onChange={setAlias} />

      <label className="block">
        <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
          {t("quickstart.model")}
        </div>
        <input
          className={INPUT_CLASS}
          value={model}
          onChange={(e) => setModel(e.target.value)}
          list="qs-model-catalog"
          placeholder={type ? t("quickstart.model_placeholder") : ""}
        />
        <datalist id="qs-model-catalog">
          {catalog?.live &&
            catalog.models.map((m) => <option key={m} value={m} />)}
        </datalist>
      </label>

      {descriptors
        .filter((d) => d.key !== "model")
        .filter(
          (d) =>
            !(
              d.key === "api_key" &&
              (fieldValues["auth_mode"] ?? "").trim() === "codex"
            ),
        )
        .map((d) =>
          d.enum_variants && d.enum_variants.length > 0 ? (
            <LabeledSelect
              key={d.key}
              label={d.label}
              help={d.help}
              options={d.enum_variants}
              value={
                fieldValues[d.key] ??
                d.default ??
                d.enum_variants[0] ??
                ""
              }
              onChange={(value) =>
                setFieldValues((prev) => ({ ...prev, [d.key]: value }))
              }
            />
          ) : (
            <LabeledInput
              key={d.key}
              label={d.label}
              help={d.help}
              type={d.is_secret ? "password" : "text"}
              value={fieldValues[d.key] ?? ""}
              placeholder={d.default ?? ""}
              onChange={(value) =>
                setFieldValues((prev) => ({ ...prev, [d.key]: value }))
              }
            />
          ),
        )}

      <div className="flex justify-end">
        <Button
          size="sm"
          disabled={!canAdd}
          onClick={() => {
            const fields: Record<string, string> = {};
            for (const [key, value] of Object.entries(fieldValues)) {
              const trimmed = value.trim();
              if (trimmed !== "") {
                fields[key] = trimmed;
              }
            }
            onStage({
              provider_type: type,
              alias: alias.trim(),
              model: model.trim(),
              fields,
            });
          }}
        >
          <Plus className="h-3.5 w-3.5" />
          {t("quickstart.add")}
        </Button>
      </div>
    </>
  );
}

function ChannelsList({
  state,
  staged,
  onAdd,
  onRemove,
}: {
  state: QuickstartState | null;
  staged: StagedChannel[];
  onAdd: (c: StagedChannel) => void;
  onRemove: (i: number) => void;
}) {
  const [adding, setAdding] = useState(false);
  const inFlight = useMemo(
    () => new Set(staged.map((c) => `${c.channel_type}.${c.alias}`)),
    [staged],
  );
  const inConfig = useMemo(() => new Set(state?.channels ?? []), [state]);
  const reusable = useMemo(
    () =>
      (state?.unassigned_channels ?? []).filter((ref) => !inFlight.has(ref)),
    [state, inFlight],
  );

  return (
    <>
      {staged.length > 0 && (
        <div className="rounded-[var(--radius-md)] border border-pc-border bg-pc-base divide-y divide-pc-border overflow-hidden">
          {staged.map((c, i) => (
            <div
              key={`${c.channel_type}.${c.alias}.${i}`}
              className="flex items-center justify-between gap-3 px-4 py-3 text-sm"
            >
              <div className="min-w-0">
                <span className="font-medium text-pc-text">
                  {c.channel_type}.{c.alias}
                </span>
                <span className="ml-2 text-xs" style={MUTED}>
                  {c.mode === "existing" ? t("quickstart.reuse") : t("quickstart.new")}
                </span>
              </div>
              <Button variant="ghost" size="sm" onClick={() => onRemove(i)}>
                <Trash2 className="h-4 w-4" />
              </Button>
            </div>
          ))}
        </div>
      )}

      {adding ? (
        <ChannelAddForm
          state={state}
          inConfig={inConfig}
          inFlight={inFlight}
          reusable={reusable}
          onAdd={(c) => {
            onAdd(c);
            setAdding(false);
          }}
          onCancel={() => setAdding(false)}
        />
      ) : (
        <Button variant="ghost" size="md" onClick={() => setAdding(true)}>
          <Plus className="h-3.5 w-3.5" />
          {t("quickstart.add_channel")}
        </Button>
      )}
    </>
  );
}

function ChannelAddForm({
  state,
  inConfig,
  inFlight,
  reusable,
  onAdd,
  onCancel,
}: {
  state: QuickstartState | null;
  inConfig: Set<string>;
  inFlight: Set<string>;
  reusable: string[];
  onAdd: (c: StagedChannel) => void;
  onCancel: () => void;
}) {
  const [mode, setMode] = useState<"existing" | "fresh">(
    reusable.length > 0 ? "existing" : "fresh",
  );
  const [existingRef, setExistingRef] = useState(reusable[0] ?? "");
  const [type, setType] = useState("");
  const [alias, setAlias] = useState("");
  const [descriptors, setDescriptors] = useState<QuickstartFieldDescriptor[]>(
    [],
  );
  const [fields, setFields] = useState<Record<string, string>>({});

  useEffect(() => {
    if (mode !== "fresh" || !type) {
      setDescriptors([]);
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const f = await quickstartFields({ section: "channel", type_key: type });
        if (!cancelled) setDescriptors(f.fields);
      } catch {
        if (!cancelled) setDescriptors([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [mode, type]);

  const freshRef = type && alias.trim() ? `${type}.${alias.trim()}` : "";
  const conflict =
    freshRef !== "" && (inConfig.has(freshRef) || inFlight.has(freshRef));
  const canAdd =
    mode === "existing"
      ? existingRef !== ""
      : type !== "" && alias.trim() !== "" && !conflict;

  const submit = () => {
    if (mode === "existing") {
      const [t, a] = existingRef.split(".");
      if (!t || !a) return;
      onAdd({ mode: "existing", channel_type: t, alias: a, fields: {} });
    } else {
      onAdd({
        mode: "fresh",
        channel_type: type,
        alias: alias.trim(),
        fields,
      });
    }
  };

  return (
    <Card className="p-4 space-y-3 bg-pc-elevated">
      <div className="flex gap-2">
        <Button
          variant={mode === "existing" ? "primary" : "ghost"}
          size="sm"
          disabled={reusable.length === 0}
          onClick={() => setMode("existing")}
        >
          {t("quickstart.use_existing")}
        </Button>
        <Button
          variant={mode === "fresh" ? "primary" : "ghost"}
          size="sm"
          onClick={() => setMode("fresh")}
        >
          {t("quickstart.create_new")}
        </Button>
        <div className="flex-1" />
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
      </div>

      {mode === "existing" ? (
        reusable.length === 0 ? (
          <div className="text-xs" style={MUTED}>
            {t("quickstart.no_unassigned_channels")}
          </div>
        ) : (
          <select
            className={INPUT_CLASS}
            value={existingRef}
            onChange={(e) => setExistingRef(e.target.value)}
          >
            {reusable.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        )
      ) : (
        <>
          <label className="block">
            <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
              {t("quickstart.channel_type")}
            </div>
            <select
              className={INPUT_CLASS}
              value={type}
              onChange={(e) => {
                const next = e.target.value;
                setType(next);
                setAlias((prev) => (prev === "" || prev === type ? next : prev));
                setFields({});
              }}
            >
              <option value="" disabled>
                {t("quickstart.pick_channel_type")}
              </option>
              {state?.channel_types.map((opt) => (
                <option key={opt.kind} value={opt.kind}>
                  {opt.display_name}
                </option>
              ))}
            </select>
          </label>

          <LabeledInput label={t("quickstart.alias_label")} value={alias} onChange={setAlias} />
          {conflict && (
            <div className="text-xs" style={ERROR}>
              <code>{freshRef}</code> {t("quickstart.already_exists")}
            </div>
          )}

          {descriptors.map((d) => (
            <LabeledInput
              key={d.key}
              label={d.label}
              type={d.is_secret ? "password" : "text"}
              value={fields[d.key] ?? ""}
              onChange={(v) => setFields((x) => ({ ...x, [d.key]: v }))}
              placeholder={d.help}
            />
          ))}
        </>
      )}

      <div className="flex justify-end">
        <Button size="sm" disabled={!canAdd} onClick={submit}>
          <Plus className="h-3.5 w-3.5" />
          {t("quickstart.add")}
        </Button>
      </div>
    </Card>
  );
}

function PeerGroupsList({
  state,
  stagedChannels,
  stagedPeerGroups,
  onAdd,
  onRemove,
}: {
  state: QuickstartState | null;
  stagedChannels: StagedChannel[];
  stagedPeerGroups: StagedPeerGroup[];
  onAdd: (pg: StagedPeerGroup) => void;
  onRemove: (i: number) => void;
}) {
  const [adding, setAdding] = useState(false);
  // Available channel refs: staged channels (in this run) + unassigned
  // channels already in config. Refs already covered by a staged
  // peer-group are filtered out — one peer-group per channel in v1.
  const stagedRefs = useMemo(
    () => stagedChannels.map((c) => `${c.channel_type}.${c.alias}`),
    [stagedChannels],
  );
  const claimed = useMemo(
    () => new Set(stagedPeerGroups.map((pg) => pg.channel)),
    [stagedPeerGroups],
  );
  const available = useMemo(
    () =>
      [...stagedRefs, ...(state?.unassigned_channels ?? [])].filter(
        (r) => !claimed.has(r),
      ),
    [stagedRefs, state, claimed],
  );

  return (
    <>
      {stagedPeerGroups.length > 0 && (
        <div className="rounded-[var(--radius-md)] border border-pc-border bg-pc-base divide-y divide-pc-border overflow-hidden">
          {stagedPeerGroups.map((pg, i) => (
            <div
              key={`${pg.name}.${i}`}
              className="flex items-center justify-between gap-3 px-4 py-3 text-sm"
            >
              <div className="min-w-0">
                <div className="font-medium text-pc-text">{pg.name}</div>
                <code className="block text-xs mt-0.5" style={FAINT}>
                  {t("quickstart.channel_prefix")}
                  {pg.channel}
                  {pg.external_peers.length > 0
                    ? ` · ${pg.external_peers.length} ${t("quickstart.peers_suffix")}`
                    : ` · ${t("quickstart.no_peers")}`}
                </code>
              </div>
              <Button variant="ghost" size="sm" onClick={() => onRemove(i)}>
                <Trash2 className="h-4 w-4" />
              </Button>
            </div>
          ))}
        </div>
      )}

      {available.length === 0 ? (
        <div className="text-xs" style={MUTED}>
          {stagedChannels.length === 0
            ? t("quickstart.stage_channel_first")
            : t("quickstart.all_channels_staged")}
        </div>
      ) : adding ? (
        <PeerGroupAddForm
          availableChannels={available}
          onAdd={(pg) => {
            onAdd(pg);
            setAdding(false);
          }}
          onCancel={() => setAdding(false)}
        />
      ) : (
        <Button variant="ghost" size="md" onClick={() => setAdding(true)}>
          <Plus className="h-3.5 w-3.5" />
          {t("quickstart.add_peer_group")}
        </Button>
      )}
    </>
  );
}

function PeerGroupAddForm({
  availableChannels,
  onAdd,
  onCancel,
}: {
  availableChannels: string[];
  onAdd: (pg: StagedPeerGroup) => void;
  onCancel: () => void;
}) {
  const [channel, setChannel] = useState(availableChannels[0] ?? "");
  const [peersBuf, setPeersBuf] = useState("");

  // Default name derived from the channel ref (`<type>_<alias>_default`).
  // Backend re-derives if missing; surface fills for predictability.
  const name = useMemo(() => {
    const [type, alias] = channel.split(".");
    if (!type || !alias) return "";
    return `${type}_${alias}_default`;
  }, [channel]);

  const peers = useMemo(
    () =>
      peersBuf
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0),
    [peersBuf],
  );

  const canAdd = channel !== "" && name !== "";

  return (
    <Card className="p-4 space-y-3 bg-pc-elevated">
      <label className="block">
        <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
          {t("quickstart.channel_label")}
        </div>
        <select
          className={INPUT_CLASS}
          value={channel}
          onChange={(e) => setChannel(e.target.value)}
        >
          {availableChannels.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
      </label>

      <LabeledInput
        label={t("quickstart.external_peers_label")}
        value={peersBuf}
        onChange={setPeersBuf}
        multiline
        placeholder="@alice&#10;@bob"
      />

      <div className="text-xs" style={MUTED}>
        {t("quickstart.peer_group_named_prefix")}
        <code>{name || "—"}</code>.
      </div>

      <div className="flex justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
        <Button
          size="sm"
          disabled={!canAdd}
          onClick={() => onAdd({ name, channel, external_peers: peers })}
        >
          <Plus className="h-3.5 w-3.5" />
          {t("quickstart.add")}
        </Button>
      </div>
    </Card>
  );
}

function PersonalityFilesList({
  state,
  staged,
  onStage,
  onRemove,
}: {
  state: QuickstartState | null;
  staged: StagedPersonalityFile[];
  onStage: (file: StagedPersonalityFile) => void;
  onRemove: (filename: string) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const [buf, setBuf] = useState("");
  const [templates, setTemplates] = useState<Record<string, string> | null>(
    null,
  );
  const filenames = state?.personality_files ?? [];
  const stagedByFilename = useMemo(
    () => new Map(staged.map((f) => [f.filename, f.content])),
    [staged],
  );

  const loadTemplates = async () => {
    if (templates !== null) return templates;
    try {
      const resp = await getPersonalityTemplates({});
      const map: Record<string, string> = {};
      for (const file of resp.files) {
        map[file.filename] = file.content;
      }
      setTemplates(map);
      return map;
    } catch {
      const empty: Record<string, string> = {};
      setTemplates(empty);
      return empty;
    }
  };

  if (filenames.length === 0) {
    return (
      <div className="text-xs" style={MUTED}>
        {t("common.loading")}
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <div className="rounded-[var(--radius-md)] border border-pc-border bg-pc-base divide-y divide-pc-border overflow-hidden">
        {filenames.map((fn) => {
          const isStaged = stagedByFilename.has(fn);
          const isEditing = editing === fn;
          return (
            <div key={fn} className="px-4 py-3 text-sm space-y-2">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <span className="font-medium text-pc-text">{fn}</span>
                  {isStaged && (
                    <span className="ml-2 text-xs" style={MUTED}>
                      {t("quickstart.staged_badge")}
                    </span>
                  )}
                </div>
                <div className="flex gap-2">
                  {isStaged && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => onRemove(fn)}
                      title={t("quickstart.discard")}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  )}
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={async () => {
                      const map = await loadTemplates();
                      const content = map[fn] ?? "";
                      if (content) {
                        onStage({ filename: fn, content });
                      }
                    }}
                    title={t("quickstart.use_template_title")}
                  >
                    {t("quickstart.use_template")}
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      if (isEditing) {
                        if (buf.trim() === "") {
                          onRemove(fn);
                        } else {
                          onStage({ filename: fn, content: buf });
                        }
                        setEditing(null);
                      } else {
                        setBuf(stagedByFilename.get(fn) ?? "");
                        setEditing(fn);
                      }
                    }}
                  >
                    {isEditing
                      ? t("common.save")
                      : isStaged
                        ? t("common.edit")
                        : t("quickstart.add")}
                  </Button>
                </div>
              </div>
              {isEditing && (
                <textarea
                  className={`${TEXTAREA_CLASS} min-h-32 font-mono text-xs`}
                  value={buf}
                  onChange={(e) => setBuf(e.target.value)}
                  placeholder={`${t("quickstart.contents_of_prefix")}${fn}…`}
                />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
