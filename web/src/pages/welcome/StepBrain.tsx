/**
 * Step 2 — Brain. Picks a model provider + API key and creates the agent
 * through the existing quickstart backend (validate → apply). If agents
 * already exist the operator can adopt one instead.
 */
import { useCallback, useEffect, useState } from "react";
import ModelPicker from "./ModelPicker";
import {
  getCatalogModels,
  getQuickstartState,
  quickstartFields,
  type ModelsResponse,
  type QuickstartError,
  type QuickstartFieldDescriptor,
  type QuickstartState,
} from "@/lib/api";
import {
  BRAIN_PROVIDERS,
  applyBrain,
  freshProviderAlias,
  isTimeoutError,
} from "@/lib/companionSetup";
import {
  C,
  ErrorNote,
  Field,
  INPUT_STYLE,
  LoadingNote,
  OptionCard,
  StepFooter,
  StepTitle,
  TextInput,
  focusRing,
} from "./ui";

export default function StepBrain({
  existingAgents,
  onBack,
  onDone,
}: {
  existingAgents: string[];
  onBack: () => void;
  onDone: (agentAlias: string) => void;
}) {
  const [load, setLoad] = useState<"loading" | "error" | "ready">("loading");
  const [loadError, setLoadError] = useState("");
  const [state, setState] = useState<QuickstartState | null>(null);

  const [mode, setMode] = useState<"existing" | "fresh">(
    existingAgents.length > 0 ? "existing" : "fresh",
  );
  const [selectedAgent, setSelectedAgent] = useState(existingAgents[0] ?? "");

  const [providerType, setProviderType] = useState("");
  const [descriptors, setDescriptors] = useState<QuickstartFieldDescriptor[]>([]);
  const [fieldValues, setFieldValues] = useState<Record<string, string>>({});
  const [fieldsLoading, setFieldsLoading] = useState(false);
  const [fieldsError, setFieldsError] = useState("");
  const [catalog, setCatalog] = useState<ModelsResponse | null>(null);
  const [model, setModel] = useState("");
  const [agentName, setAgentName] = useState("companion");

  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<QuickstartError[]>([]);
  const [applyError, setApplyError] = useState("");

  const fetchState = useCallback(() => {
    setLoad("loading");
    setLoadError("");
    getQuickstartState()
      .then((s) => {
        setState(s);
        setLoad("ready");
      })
      .catch((e) => {
        setLoadError(e instanceof Error ? e.message : String(e));
        setLoad("error");
      });
  }, []);

  useEffect(() => {
    fetchState();
  }, [fetchState]);

  // Load daemon-authored field descriptors + model catalog per provider type.
  useEffect(() => {
    if (!providerType) {
      setDescriptors([]);
      setFieldValues({});
      setCatalog(null);
      return;
    }
    let cancelled = false;
    setFieldsLoading(true);
    setFieldsError("");
    void (async () => {
      try {
        const f = await quickstartFields({
          section: "model_provider",
          type_key: providerType,
        });
        if (cancelled) return;
        setDescriptors(f.fields);
        const next: Record<string, string> = {};
        for (const d of f.fields) {
          next[d.key] =
            d.enum_variants && d.enum_variants.length > 0
              ? (d.default ?? d.enum_variants[0] ?? "")
              : "";
        }
        setFieldValues(next);
      } catch (e) {
        if (!cancelled) {
          setDescriptors([]);
          setFieldValues({});
          setFieldsError(e instanceof Error ? e.message : String(e));
        }
      } finally {
        if (!cancelled) setFieldsLoading(false);
      }
      try {
        const r = await getCatalogModels(providerType);
        if (!cancelled) setCatalog(r);
      } catch {
        // Catalog is best-effort: fall back to free-text model input.
        if (!cancelled) setCatalog(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [providerType]);

  const pickProvider = (kind: string, defaultModel: string) => {
    setProviderType(kind);
    setModel(defaultModel);
    setErrors([]);
    setApplyError("");
  };

  const availableKinds = new Set(
    (state?.model_provider_types ?? []).map((t) => t.kind),
  );

  const missingRequiredSecret = descriptors.some(
    (d) => d.required && d.is_secret && (fieldValues[d.key] ?? "").trim() === "",
  );
  const freshReady =
    providerType !== "" &&
    model.trim() !== "" &&
    agentName.trim() !== "" &&
    !fieldsLoading &&
    !missingRequiredSecret;
  const canContinue =
    mode === "existing" ? selectedAgent !== "" : freshReady && state !== null;

  const submit = async () => {
    if (busy) return;
    if (mode === "existing") {
      if (selectedAgent) onDone(selectedAgent);
      return;
    }
    if (!state || !freshReady) return;
    setBusy(true);
    setErrors([]);
    setApplyError("");
    try {
      const fields: Record<string, string> = {};
      for (const [key, value] of Object.entries(fieldValues)) {
        const trimmed = value.trim();
        if (trimmed !== "") fields[key] = trimmed;
      }
      const res = await applyBrain({
        state,
        providerType,
        providerAlias: freshProviderAlias(state, providerType),
        model: model.trim(),
        fields,
        agentName: agentName.trim().toLowerCase(),
      });
      if (res.ok) {
        onDone(res.alias);
      } else {
        setErrors(res.errors);
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setApplyError(
        isTimeoutError(e)
          ? `${msg} — setup is taking longer than expected (slow network or provider). Try again.`
          : msg,
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <StepTitle
        kicker="Step 2 — Brain"
        title="Choose their mind"
        sub="Your companion thinks with a frontier model. Pick a provider and paste an API key — everything else is configured for you."
      />

      {load === "loading" ? <LoadingNote label="Contacting the daemon…" /> : null}
      {load === "error" ? (
        <ErrorNote
          message={`Could not load setup state: ${loadError}`}
          onRetry={fetchState}
        />
      ) : null}

      {load === "ready" && existingAgents.length > 0 ? (
        <div style={{ marginBottom: 24 }}>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
            <OptionCard
              selected={mode === "existing"}
              onSelect={() => setMode("existing")}
              title="Use an existing agent"
              blurb="An agent is already configured on this daemon — adopt it as your companion."
            />
            <OptionCard
              selected={mode === "fresh"}
              onSelect={() => setMode("fresh")}
              title="Set up a new brain"
              blurb="Create a fresh agent with its own provider and key."
            />
          </div>
        </div>
      ) : null}

      {load === "ready" && mode === "existing" ? (
        <Field label="Agent" hint="This agent will receive the voice, hearing and ritual configuration in the next steps.">
          <div style={{ display: "grid", gridTemplateColumns: "repeat(2, 1fr)", gap: 10 }}>
            {existingAgents.map((a) => (
              <OptionCard
                key={a}
                selected={selectedAgent === a}
                onSelect={() => setSelectedAgent(a)}
                title={a}
              />
            ))}
          </div>
        </Field>
      ) : null}

      {load === "ready" && mode === "fresh" ? (
        <>
          <Field label="Provider">
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))",
                gap: 10,
              }}
            >
              {BRAIN_PROVIDERS.map((p) => (
                <OptionCard
                  key={p.kind}
                  selected={providerType === p.kind}
                  onSelect={() => pickProvider(p.kind, p.defaultModel)}
                  title={p.title}
                  blurb={p.blurb}
                  disabled={!availableKinds.has(p.kind)}
                />
              ))}
            </div>
          </Field>

          {providerType ? (
            <>
              {fieldsLoading ? (
                <LoadingNote label="Loading provider fields…" />
              ) : null}
              {fieldsError ? (
                <ErrorNote
                  message={`Could not load provider fields: ${fieldsError}`}
                  onRetry={() => {
                    // Re-trigger the descriptor effect.
                    const t = providerType;
                    setProviderType("");
                    window.setTimeout(() => setProviderType(t), 0);
                  }}
                />
              ) : null}

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
                    <Field key={d.key} label={d.label} hint={d.help}>
                      <select
                        value={fieldValues[d.key] ?? d.default ?? d.enum_variants[0] ?? ""}
                        onChange={(e) =>
                          setFieldValues((prev) => ({ ...prev, [d.key]: e.target.value }))
                        }
                        {...focusRing()}
                        style={{ ...INPUT_STYLE, appearance: "auto" }}
                      >
                        {d.enum_variants.map((v) => (
                          <option key={v} value={v} style={{ background: C.raised }}>
                            {v}
                          </option>
                        ))}
                      </select>
                    </Field>
                  ) : (
                    <Field key={d.key} label={d.label} hint={d.help}>
                      <TextInput
                        type={d.is_secret ? "password" : "text"}
                        autoComplete="off"
                        value={fieldValues[d.key] ?? ""}
                        placeholder={d.default ?? ""}
                        onChange={(e) =>
                          setFieldValues((prev) => ({ ...prev, [d.key]: e.target.value }))
                        }
                      />
                    </Field>
                  ),
                )}

              <Field
                label="Model"
                hint={
                  catalog && catalog.models.length > 0
                    ? "Pick from the full catalog or type any model id."
                    : "Type the model id to use."
                }
              >
                <ModelPicker
                  models={catalog?.models ?? []}
                  live={catalog?.live ?? false}
                  value={model}
                  onChange={setModel}
                />
              </Field>

              <Field
                label="Agent alias"
                hint="Lowercase internal name for this agent (you'll name your companion later)."
              >
                <TextInput
                  value={agentName}
                  onChange={(e) => setAgentName(e.target.value.toLowerCase())}
                  placeholder="companion"
                />
              </Field>
            </>
          ) : null}
        </>
      ) : null}

      {errors.length > 0 ? (
        <ErrorNote
          message={errors
            .map((e) => `${e.step}${e.field ? `.${e.field}` : ""}: ${e.message}`)
            .join(" — ")}
        />
      ) : null}
      {applyError ? (
        <ErrorNote
          message={`Setup failed: ${applyError}`}
          onRetry={() => void submit()}
        />
      ) : null}

      <StepFooter
        onBack={onBack}
        busy={busy}
        continueDisabled={!canContinue}
        continueLabel={mode === "fresh" ? "Create brain" : "Continue"}
      />
    </form>
  );
}
