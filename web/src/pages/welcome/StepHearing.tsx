/**
 * Step 4 — Hearing. Speech-to-text provider + key.
 * Persists via config prop writes (see companionSetup.hearingWrites).
 *
 * Re-run friendly: when an STT credential is already stored, a one-click
 * "keep current hearing" path skips re-entering the key.
 */
import { useEffect, useState } from "react";
import {
  STT_PROVIDERS,
  checkExistingHearing,
  hearingWrites,
  writeProps,
} from "@/lib/companionSetup";
import {
  C,
  ErrorNote,
  Field,
  OptionCard,
  StepFooter,
  StepTitle,
  TextInput,
} from "./ui";

export default function StepHearing({
  onBack,
  onDone,
}: {
  onBack: () => void;
  onDone: () => void;
}) {
  const [providerKind, setProviderKind] = useState("groq");
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [existing, setExisting] = useState<{
    configured: boolean;
    provider: string | null;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    checkExistingHearing()
      .then((r) => {
        if (!cancelled) setExisting(r);
      })
      .catch(() => setExisting({ configured: false, provider: null }));
    return () => {
      cancelled = true;
    };
  }, []);

  /** Keep the stored credential: just make sure transcription is enabled. */
  const keepExisting = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await writeProps([{ path: "transcription.enabled", value: true }]);
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const ready = providerKind !== "" && apiKey.trim() !== "";

  const submit = async () => {
    if (busy || !ready) return;
    setBusy(true);
    setError("");
    try {
      await writeProps(hearingWrites({ providerKind, apiKey: apiKey.trim() }));
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const selected = STT_PROVIDERS.find((p) => p.kind === providerKind);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <StepTitle
        kicker="Step 4 — Hearing"
        title="Let them hear you"
        sub="Speech-to-text turns your voice into words. Groq's Whisper is the recommended default — transcription lands in well under a second."
      />

      {existing?.configured ? (
        <div
          className="wlc-fade-up"
          style={{
            display: "flex",
            alignItems: "center",
            gap: 12,
            background: "rgba(217,119,87,0.08)",
            border: `1px solid ${C.accentBorder}`,
            borderRadius: 12,
            padding: "13px 16px",
            marginBottom: 18,
          }}
        >
          <span style={{ color: C.accent, fontWeight: 700 }}>✓</span>
          <span style={{ flex: 1, fontSize: 13.5, color: C.text }}>
            {existing.provider
              ? `A ${existing.provider} speech-to-text key is already saved.`
              : "A speech-to-text key is already saved."}
          </span>
          <button
            type="button"
            onClick={() => void keepExisting()}
            disabled={busy}
            style={{
              background: C.accent,
              color: "#0a0a0a",
              border: "none",
              borderRadius: 9,
              padding: "8px 14px",
              fontSize: 13,
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Keep current hearing →
          </button>
        </div>
      ) : null}

      <Field label="Transcription provider">
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))",
            gap: 10,
          }}
        >
          {STT_PROVIDERS.map((p) => (
            <OptionCard
              key={p.kind}
              selected={providerKind === p.kind}
              onSelect={() => {
                setProviderKind(p.kind);
                setError("");
              }}
              title={p.title}
              blurb={p.blurb}
              badge={p.recommended ? "Recommended" : undefined}
            />
          ))}
        </div>
      </Field>

      <Field
        label={`${selected?.title ?? "Provider"} API key`}
        hint={
          providerKind === "groq"
            ? "Free tier available at console.groq.com → API Keys."
            : undefined
        }
      >
        <TextInput
          type="password"
          autoComplete="off"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder="API key"
        />
      </Field>

      {error ? <ErrorNote message={`Saving hearing settings failed — ${error}`} /> : null}

      <StepFooter
        onBack={onBack}
        busy={busy}
        continueDisabled={!ready}
        continueLabel="Save hearing"
      />
    </form>
  );
}
