import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  Bot,
  Check,
  ChevronRight,
  Cpu,
  FileText,
  Gauge,
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
  extras: Record<string, string>;
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

/** A preset selection — typed wrapper around a `preset_name` so the
 *  shape can't carry a raw user-typed string. The only way to construct
 *  one is via the `PresetSection` picker, which sources values from
 *  `state.risk_presets` / `state.runtime_presets` / `state.memory_kinds`. */
interface StagedPreset {
  preset_name: string;
}

interface FormState {
  provider: StagedProvider | null;
  risk: StagedPreset | null;
  runtime: StagedPreset | null;
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

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const s = await getQuickstartState();
        if (!cancelled) setState(s);
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
    setBusy(true);
    setErrors([]);
    const res = await quickstartApply({
      model_provider: { mode: "fresh", value: form.provider! },
      risk_profile: { mode: "fresh", value: form.risk!.preset_name },
      runtime_profile: { mode: "fresh", value: form.runtime!.preset_name },
      memory: { mode: "fresh", value: form.memory!.preset_name },
      channels: form.channels.map((c) =>
        c.mode === "existing"
          ? { mode: "existing", value: `${c.channel_type}.${c.alias}` }
          : {
              mode: "fresh",
              value: {
                channel_type: c.channel_type,
                alias: c.alias,
                token:
                  c.extras["bot_token"] ??
                  c.extras["token"] ??
                  c.extras["access_token"] ??
                  null,
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
  const allDone =
    providerDone &&
    form.risk !== null &&
    form.runtime !== null &&
    form.memory !== null &&
    form.agentName.trim() !== "";

  return (
    <div className="max-w-3xl mx-auto px-6 py-8 space-y-5">
      <header className="space-y-1">
        <h1 className="text-2xl font-semibold">Quickstart</h1>
        <p className="text-sm" style={MUTED}>
          Create one working agent end-to-end. Pick a provider, choose your
          profiles, and start chatting.
        </p>
      </header>

      <Section
        icon={<Cpu className="h-4 w-4" />}
        title="Model provider"
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
            sub={`model: ${form.provider.model}`}
            onRemove={() => setForm((f) => ({ ...f, provider: null }))}
          />
        ) : (
          <ProviderForm
            state={state}
            onStage={(p) => {
              setForm((f) => ({ ...f, provider: p }));
              recordStep("model_provider");
            }}
          />
        )}
      </Section>

      <PresetSection
        icon={<ShieldCheck className="h-4 w-4" />}
        title="Risk profile"
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
        icon={<Gauge className="h-4 w-4" />}
        title="Runtime profile"
        rows={(state?.runtime_presets ?? []).map((p) => ({
          value: p.preset_name,
          label: p.label,
          help: p.help,
        }))}
        value={form.runtime?.preset_name ?? ""}
        onChange={(v) => {
          setForm((f) => ({ ...f, runtime: { preset_name: v } }));
          recordStep("runtime_profile");
        }}
      />

      <PresetSection
        icon={<HardDrive className="h-4 w-4" />}
        title="Memory"
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
        title="Channels"
        done={true}
        summary={
          form.channels.length === 0
            ? "none — reachable via CLI"
            : `${form.channels.length} configured`
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
        title="Peer groups"
        done={true}
        summary={
          form.peerGroups.length === 0
            ? "none — channels accept no peers"
            : `${form.peerGroups.length} configured`
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
        title="Agent"
        done={form.agentName.trim() !== ""}
        summary={form.agentName.trim() || null}
      >
        <LabeledInput
          label="Name"
          value={form.agentName}
          onChange={(v) => {
            setForm((f) => ({ ...f, agentName: v }));
            recordStep("agent");
          }}
          placeholder="some_nickname"
        />
      </Section>

      <Section
        icon={<FileText className="h-4 w-4" />}
        title="Personality files"
        done={true}
        summary={
          form.personalityFiles.length === 0
            ? "none — agent uses bootstrap defaults"
            : `${form.personalityFiles.length} staged`
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
        <ul className="card p-4 space-y-1 text-sm" style={ERROR}>
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
        <button
          className="btn-primary px-6 py-2"
          disabled={busy || !allDone}
          onClick={() => void submit()}
        >
          {busy ? "Creating..." : "Create"}
        </button>
      </div>
    </div>
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
    <section className="card p-5 space-y-4">
      <header className="flex items-center gap-3">
        <span
          className="flex h-7 w-7 items-center justify-center rounded-lg"
          style={{
            background: done
              ? "rgba(0, 230, 138, 0.12)"
              : "var(--pc-bg-elevated)",
            color: done ? "var(--color-status-success)" : MUTED.color,
          }}
        >
          {icon}
        </span>
        <h2 className="font-semibold flex-1 flex items-center gap-2">
          {done && (
            <Check
              className="h-4 w-4"
              style={{ color: "var(--color-status-success)" }}
            />
          )}
          {title}
        </h2>
        {summary && (
          <span className="text-xs" style={MUTED}>
            {summary}
          </span>
        )}
      </header>
      <div className="space-y-3">{children}</div>
    </section>
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
          Loading…
        </div>
      ) : (
        <div
          className="surface-panel divide-y rounded-xl overflow-hidden"
          style={{ borderColor: "var(--pc-border)" }}
        >
          {rows.map((r) => (
            <button
              key={r.value}
              type="button"
              onClick={() => onChange(r.value)}
              className="w-full flex items-center gap-3 px-4 py-3 text-left text-sm hover:opacity-90"
              style={{
                background:
                  r.value === value ? "rgba(0,128,255,0.08)" : "transparent",
              }}
            >
              <div className="flex-1 min-w-0">
                <div style={{ fontWeight: 500 }}>{r.label}</div>
                {r.help && (
                  <div className="text-xs mt-0.5" style={MUTED}>
                    {r.help}
                  </div>
                )}
              </div>
              {r.value === value && (
                <ChevronRight
                  className="h-4 w-4 flex-shrink-0"
                  style={{ color: "var(--pc-accent)" }}
                />
              )}
            </button>
          ))}
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
    <div
      className="flex items-center justify-between gap-3 px-4 py-3 rounded-xl"
      style={{ background: "var(--pc-bg-elevated)" }}
    >
      <div className="min-w-0">
        <div style={{ fontWeight: 500 }}>{label}</div>
        {sub && (
          <code className="block text-xs mt-0.5" style={FAINT}>
            {sub}
          </code>
        )}
      </div>
      <button type="button" onClick={onRemove} className="btn-icon" title="Clear">
        <Trash2 className="h-4 w-4" />
      </button>
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
          className="input-electric w-full px-3 py-2 min-h-24"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
        />
      ) : (
        <input
          className="input-electric w-full px-3 py-2"
          type={type}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
        />
      )}
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
          // user sees until they type.
          const next: Record<string, string> = {};
          for (const d of f.fields) {
            next[d.key] = "";
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
          Provider type
        </div>
        <select
          className="input-electric w-full px-3 py-2"
          value={type}
          onChange={(e) => {
            const next = e.target.value;
            setType(next);
            setModel("");
          }}
        >
          <option value="" disabled>
            — pick a provider —
          </option>
          {state?.model_provider_types.map((opt) => (
            <option key={opt.kind} value={opt.kind}>
              {opt.display_name}
              {opt.local ? " (local)" : ""}
            </option>
          ))}
        </select>
      </label>

      <LabeledInput label="alias" value={alias} onChange={setAlias} />

      <label className="block">
        <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
          model
        </div>
        <input
          className="input-electric w-full px-3 py-2"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          list="qs-model-catalog"
          placeholder={type ? "pick or type a model id" : ""}
        />
        <datalist id="qs-model-catalog">
          {catalog?.live &&
            catalog.models.map((m) => <option key={m} value={m} />)}
        </datalist>
      </label>

      {descriptors
        .filter((d) => d.key !== "model")
        .map((d) => (
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
        ))}

      <div className="flex justify-end">
        <button
          type="button"
          className="btn-primary px-4 py-2 text-sm inline-flex items-center gap-2"
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
          Add
        </button>
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
        <div
          className="surface-panel divide-y rounded-xl overflow-hidden"
          style={{ borderColor: "var(--pc-border)" }}
        >
          {staged.map((c, i) => (
            <div
              key={`${c.channel_type}.${c.alias}.${i}`}
              className="flex items-center justify-between gap-3 px-4 py-3 text-sm"
            >
              <div className="min-w-0">
                <span style={{ fontWeight: 500 }}>
                  {c.channel_type}.{c.alias}
                </span>
                <span className="ml-2 text-xs" style={MUTED}>
                  {c.mode === "existing" ? "reuse" : "new"}
                </span>
              </div>
              <button
                type="button"
                className="btn-icon"
                onClick={() => onRemove(i)}
              >
                <Trash2 className="h-4 w-4" />
              </button>
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
        <button
          type="button"
          className="btn-secondary px-4 py-2 text-sm inline-flex items-center gap-2"
          onClick={() => setAdding(true)}
        >
          <Plus className="h-3.5 w-3.5" />
          Add channel
        </button>
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
  const [extras, setExtras] = useState<Record<string, string>>({});

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
      onAdd({ mode: "existing", channel_type: t, alias: a, extras: {} });
    } else {
      onAdd({
        mode: "fresh",
        channel_type: type,
        alias: alias.trim(),
        extras,
      });
    }
  };

  return (
    <div
      className="card p-4 space-y-3"
      style={{ background: "var(--pc-bg-elevated)" }}
    >
      <div className="flex gap-2">
        <button
          type="button"
          className={mode === "existing" ? "btn-primary" : "btn-secondary"}
          disabled={reusable.length === 0}
          onClick={() => setMode("existing")}
          style={{ padding: "0.4rem 0.8rem", fontSize: "0.85rem" }}
        >
          Use existing
        </button>
        <button
          type="button"
          className={mode === "fresh" ? "btn-primary" : "btn-secondary"}
          onClick={() => setMode("fresh")}
          style={{ padding: "0.4rem 0.8rem", fontSize: "0.85rem" }}
        >
          Create new
        </button>
        <div className="flex-1" />
        <button
          type="button"
          className="btn-secondary"
          onClick={onCancel}
          style={{ padding: "0.4rem 0.8rem", fontSize: "0.85rem" }}
        >
          Cancel
        </button>
      </div>

      {mode === "existing" ? (
        reusable.length === 0 ? (
          <div className="text-xs" style={MUTED}>
            No unassigned channels available.
          </div>
        ) : (
          <select
            className="input-electric w-full px-3 py-2"
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
              Channel type
            </div>
            <select
              className="input-electric w-full px-3 py-2"
              value={type}
              onChange={(e) => {
                const next = e.target.value;
                setType(next);
                setAlias((prev) => (prev === "" || prev === type ? next : prev));
                setExtras({});
              }}
            >
              <option value="" disabled>
                — pick a channel type —
              </option>
              {state?.channel_types.map((opt) => (
                <option key={opt.kind} value={opt.kind}>
                  {opt.display_name}
                </option>
              ))}
            </select>
          </label>

          <LabeledInput label="Alias" value={alias} onChange={setAlias} />
          {conflict && (
            <div className="text-xs" style={ERROR}>
              <code>{freshRef}</code> already exists.
            </div>
          )}

          {descriptors.map((d) => (
            <LabeledInput
              key={d.key}
              label={d.label}
              type={d.is_secret ? "password" : "text"}
              value={extras[d.key] ?? ""}
              onChange={(v) => setExtras((x) => ({ ...x, [d.key]: v }))}
              placeholder={d.help}
            />
          ))}
        </>
      )}

      <div className="flex justify-end">
        <button
          type="button"
          className="btn-primary px-4 py-2 text-sm inline-flex items-center gap-2"
          disabled={!canAdd}
          onClick={submit}
        >
          <Plus className="h-3.5 w-3.5" />
          Add
        </button>
      </div>
    </div>
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
        <div
          className="surface-panel divide-y rounded-xl overflow-hidden"
          style={{ borderColor: "var(--pc-border)" }}
        >
          {stagedPeerGroups.map((pg, i) => (
            <div
              key={`${pg.name}.${i}`}
              className="flex items-center justify-between gap-3 px-4 py-3 text-sm"
            >
              <div className="min-w-0">
                <div style={{ fontWeight: 500 }}>{pg.name}</div>
                <code className="block text-xs mt-0.5" style={FAINT}>
                  channel: {pg.channel}
                  {pg.external_peers.length > 0
                    ? ` · ${pg.external_peers.length} peers`
                    : " · no peers"}
                </code>
              </div>
              <button
                type="button"
                className="btn-icon"
                onClick={() => onRemove(i)}
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </div>
          ))}
        </div>
      )}

      {available.length === 0 ? (
        <div className="text-xs" style={MUTED}>
          {stagedChannels.length === 0
            ? "Stage at least one channel above to authorize peers."
            : "Every available channel has a peer-group staged."}
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
        <button
          type="button"
          className="btn-secondary px-4 py-2 text-sm inline-flex items-center gap-2"
          onClick={() => setAdding(true)}
        >
          <Plus className="h-3.5 w-3.5" />
          Add peer group
        </button>
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
    <div
      className="card p-4 space-y-3"
      style={{ background: "var(--pc-bg-elevated)" }}
    >
      <label className="block">
        <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
          Channel
        </div>
        <select
          className="input-electric w-full px-3 py-2"
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
        label="External peers (one per line or comma-separated)"
        value={peersBuf}
        onChange={setPeersBuf}
        multiline
        placeholder="@alice&#10;@bob"
      />

      <div className="text-xs" style={MUTED}>
        Peer group will be named <code>{name || "—"}</code>.
      </div>

      <div className="flex justify-end gap-2">
        <button
          type="button"
          className="btn-secondary"
          onClick={onCancel}
          style={{ padding: "0.4rem 0.8rem", fontSize: "0.85rem" }}
        >
          Cancel
        </button>
        <button
          type="button"
          className="btn-primary px-4 py-2 text-sm inline-flex items-center gap-2"
          disabled={!canAdd}
          onClick={() => onAdd({ name, channel, external_peers: peers })}
        >
          <Plus className="h-3.5 w-3.5" />
          Add
        </button>
      </div>
    </div>
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
        Loading…
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <div
        className="surface-panel divide-y rounded-xl overflow-hidden"
        style={{ borderColor: "var(--pc-border)" }}
      >
        {filenames.map((fn) => {
          const isStaged = stagedByFilename.has(fn);
          const isEditing = editing === fn;
          return (
            <div key={fn} className="px-4 py-3 text-sm space-y-2">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <span style={{ fontWeight: 500 }}>{fn}</span>
                  {isStaged && (
                    <span className="ml-2 text-xs" style={MUTED}>
                      staged
                    </span>
                  )}
                </div>
                <div className="flex gap-2">
                  {isStaged && (
                    <button
                      type="button"
                      className="btn-icon"
                      onClick={() => onRemove(fn)}
                      title="Discard"
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  )}
                  <button
                    type="button"
                    className="btn-secondary"
                    style={{ padding: "0.3rem 0.7rem", fontSize: "0.8rem" }}
                    onClick={async () => {
                      const map = await loadTemplates();
                      const content = map[fn] ?? "";
                      if (content) {
                        onStage({ filename: fn, content });
                      }
                    }}
                    title="Stage the default template content for this file"
                  >
                    Use template
                  </button>
                  <button
                    type="button"
                    className="btn-secondary"
                    style={{ padding: "0.3rem 0.7rem", fontSize: "0.8rem" }}
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
                    {isEditing ? "Save" : isStaged ? "Edit" : "Add"}
                  </button>
                </div>
              </div>
              {isEditing && (
                <textarea
                  className="input-electric w-full px-3 py-2 min-h-32 font-mono text-xs"
                  value={buf}
                  onChange={(e) => setBuf(e.target.value)}
                  placeholder={`Contents of ${fn}…`}
                />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
