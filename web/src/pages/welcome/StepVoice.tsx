/**
 * Step 3 — Voice. ElevenLabs key + voice id, optional OpenAI TTS fallback.
 * Persists via config prop writes (see companionSetup.voiceWrites).
 *
 * Re-run friendly: when an ElevenLabs key is already stored, the step
 * offers a one-click "keep current voice" path instead of demanding the
 * key again (only the agent binding + tts.enabled are re-written).
 */
import { useEffect, useState } from "react";
import {
  VOICE_EFFECT_PRESETS,
  storeVoiceEffect,
  storedVoiceEffect,
  type VoiceEffectPreset,
} from "@/lib/voice/robotVoice";
import {
  DEFAULT_ELEVENLABS_VOICE,
  ELEVENLABS_PRESET_VOICES,
  checkExistingVoice,
  voiceWrites,
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

export default function StepVoice({
  agentAlias,
  onBack,
  onDone,
}: {
  agentAlias: string;
  onBack: () => void;
  onDone: () => void;
}) {
  const [elevenLabsKey, setElevenLabsKey] = useState("");
  const [voiceId, setVoiceId] = useState(DEFAULT_ELEVENLABS_VOICE);
  const [wantFallback, setWantFallback] = useState(false);
  const [character, setCharacter] = useState<VoiceEffectPreset>(() => storedVoiceEffect());
  const [openAiKey, setOpenAiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [existing, setExisting] = useState<{ configured: boolean; voice: string | null } | null>(
    null,
  );

  useEffect(() => {
    let cancelled = false;
    checkExistingVoice()
      .then((r) => {
        if (cancelled) return;
        setExisting(r);
        if (r.voice) setVoiceId(r.voice);
      })
      .catch(() => setExisting({ configured: false, voice: null }));
    return () => {
      cancelled = true;
    };
  }, []);

  /** Keep the stored key: only re-bind the agent + make sure TTS is on. */
  const keepExisting = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    storeVoiceEffect(character);
    try {
      await writeProps([
        { path: "tts.enabled", value: true },
        { path: `agents.${agentAlias}.tts_provider`, value: "elevenlabs.default" },
        ...(voiceId.trim()
          ? [{ path: "providers.tts.elevenlabs.default.voice", value: voiceId.trim() }]
          : []),
      ]);
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const ready =
    elevenLabsKey.trim() !== "" &&
    voiceId.trim() !== "" &&
    (!wantFallback || openAiKey.trim() !== "");

  const submit = async () => {
    if (busy || !ready) return;
    setBusy(true);
    setError("");
    storeVoiceEffect(character);
    try {
      await writeProps(
        voiceWrites({
          agentAlias,
          elevenLabsKey: elevenLabsKey.trim(),
          voiceId: voiceId.trim(),
          openAiKey: wantFallback ? openAiKey.trim() : undefined,
        }),
      );
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
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
        kicker="Step 3 — Voice"
        title="Give them a voice"
        sub="ElevenLabs powers the speaking half of the face-to-face experience. Sentences stream out as audio the moment they're written."
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
            An ElevenLabs key is already saved — no need to enter it again.
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
            Keep current voice →
          </button>
        </div>
      ) : null}

      <Field
        label={
          existing?.configured
            ? "ElevenLabs API key (only to replace the saved one)"
            : "ElevenLabs API key"
        }
        hint={
          existing?.configured
            ? "Leave blank to keep the stored key."
            : "Required for voice. Create one at elevenlabs.io → Profile → API keys."
        }
      >
        <TextInput
          type="password"
          autoComplete="off"
          value={elevenLabsKey}
          onChange={(e) => setElevenLabsKey(e.target.value)}
          placeholder="sk_…"
          autoFocus={!existing?.configured}
        />
      </Field>

      <Field
        label="Character"
        hint="Rau speaks with a synthetic character layered over the base voice — pick their sound. Cycle live later with R on the face."
      >
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
            gap: 10,
          }}
        >
          {VOICE_EFFECT_PRESETS.map((c) => (
            <OptionCard
              key={c.id}
              selected={character === c.id}
              onSelect={() => setCharacter(c.id)}
              title={c.name}
              blurb={c.blurb}
            />
          ))}
        </div>
      </Field>

      <Field
        label="Base voice"
        hint="Pick a preset or paste any ElevenLabs voice id from your Voice Lab."
      >
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
            gap: 10,
            marginBottom: 10,
          }}
        >
          {ELEVENLABS_PRESET_VOICES.map((v) => (
            <OptionCard
              key={v.id}
              selected={voiceId === v.id}
              onSelect={() => setVoiceId(v.id)}
              title={v.name}
              blurb={v.vibe}
            />
          ))}
        </div>
        <TextInput
          value={voiceId}
          onChange={(e) => setVoiceId(e.target.value)}
          placeholder="voice id"
          aria-label="ElevenLabs voice id"
        />
      </Field>

      <div
        style={{
          borderTop: `1px solid ${C.border}`,
          marginTop: 8,
          paddingTop: 18,
        }}
      >
        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            color: C.text,
            fontSize: 14,
            cursor: "pointer",
            marginBottom: 12,
          }}
        >
          <input
            type="checkbox"
            checked={wantFallback}
            onChange={(e) => setWantFallback(e.target.checked)}
            style={{ accentColor: C.accent, width: 16, height: 16 }}
          />
          Also configure an OpenAI TTS fallback
        </label>
        {wantFallback ? (
          <Field
            label="OpenAI API key (fallback TTS)"
            hint='Sets up providers.tts.openai.default with the "alloy" voice as a backup speaker.'
          >
            <TextInput
              type="password"
              autoComplete="off"
              value={openAiKey}
              onChange={(e) => setOpenAiKey(e.target.value)}
              placeholder="sk-…"
            />
          </Field>
        ) : (
          <p style={{ color: C.faint, fontSize: 12.5, lineHeight: 1.5, margin: 0 }}>
            Optional: if ElevenLabs is ever unavailable, an OpenAI voice can
            step in.
          </p>
        )}
      </div>

      {error ? <ErrorNote message={`Saving voice settings failed — ${error}`} /> : null}

      <StepFooter
        onBack={onBack}
        busy={busy}
        continueDisabled={!ready}
        continueLabel="Save voice"
      />
    </form>
  );
}
