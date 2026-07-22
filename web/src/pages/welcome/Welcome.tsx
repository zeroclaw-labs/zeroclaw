/**
 * Welcome — the enterprise setup experience.
 *
 * A fullscreen 8-step wizard (rendered outside the dashboard Layout):
 * Welcome → Brain (quickstart apply) → Voice (ElevenLabs) → Hearing (STT) →
 * Extras → Soul (name + seed) → Rituals (dreaming cron + heartbeat) → Done.
 *
 * Pure black, terracotta accents, keyboard-first: every panel is a <form>
 * so Enter continues; Escape steps back; the left rail tracks progress and
 * lets you revisit any step you've already passed.
 */
import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  Brain,
  Check,
  Ear,
  Heart,
  Mic,
  MoonStar,
  Sparkles,
  Wand2,
} from "lucide-react";
import { getAgentOptions } from "@/lib/api";
import { soulStudioLink } from "@/lib/companionSetup";
import StepBrain from "./StepBrain";
import StepVoice from "./StepVoice";
import StepHearing from "./StepHearing";
import StepExtras from "./StepExtras";
import StepSoul from "./StepSoul";
import StepRituals from "./StepRituals";
import StepAwaken from "./StepAwaken";
import {
  C,
  ErrorNote,
  GhostButton,
  LoadingNote,
  PrimaryButton,
  StepTitle,
} from "./ui";

const GLOBAL_CSS = `
@keyframes wlc-fade-up {
  from { opacity: 0; transform: translateY(14px); }
  to { opacity: 1; transform: translateY(0); }
}
@keyframes wlc-breathe {
  0%, 100% { transform: scale(1); opacity: 0.85; }
  50% { transform: scale(1.12); opacity: 1; }
}
@keyframes wlc-halo {
  0%, 100% { transform: scale(1); opacity: 0.35; }
  50% { transform: scale(1.35); opacity: 0.12; }
}
@keyframes wlc-spin { to { transform: rotate(360deg); } }
.wlc-fade-up { animation: wlc-fade-up 480ms cubic-bezier(0.22, 1, 0.36, 1) both; }
.wlc-fade-up-1 { animation: wlc-fade-up 480ms cubic-bezier(0.22, 1, 0.36, 1) 120ms both; }
.wlc-fade-up-2 { animation: wlc-fade-up 480ms cubic-bezier(0.22, 1, 0.36, 1) 240ms both; }
.wlc-fade-up-3 { animation: wlc-fade-up 480ms cubic-bezier(0.22, 1, 0.36, 1) 360ms both; }
.wlc-spinner {
  width: 14px; height: 14px; border-radius: 50%; display: inline-block;
  border: 2px solid rgba(217,119,87,0.25); border-top-color: #D97757;
  animation: wlc-spin 700ms linear infinite;
}
.wlc-spinner-dark { border-color: rgba(0,0,0,0.25); border-top-color: #0a0a0a; }
@media (prefers-reduced-motion: reduce) {
  .wlc-fade-up, .wlc-fade-up-1, .wlc-fade-up-2, .wlc-fade-up-3 { animation: none; }
}
`;

type StepId =
  | "welcome"
  | "brain"
  | "voice"
  | "hearing"
  | "extras"
  | "soul"
  | "rituals"
  | "awaken"
  | "done";

const STEPS: { id: StepId; label: string; optional?: boolean }[] = [
  { id: "welcome", label: "Welcome" },
  { id: "brain", label: "Brain" },
  { id: "voice", label: "Voice" },
  { id: "hearing", label: "Hearing" },
  { id: "extras", label: "Extras", optional: true },
  { id: "soul", label: "Soul" },
  { id: "rituals", label: "Rituals", optional: true },
  { id: "awaken", label: "Awakening" },
  { id: "done", label: "Meet them" },
];

export default function Welcome() {
  const navigate = useNavigate();
  const [stepIndex, setStepIndex] = useState(0);
  const [maxVisited, setMaxVisited] = useState(0);

  // Wizard-wide state.
  const [agentAlias, setAgentAlias] = useState<string | null>(null);
  const [companionName, setCompanionName] = useState("");
  const [soulSeed, setSoulSeed] = useState("");
  const [rituals, setRituals] = useState({ dreaming: false, heartbeat: false });
  const [firstWords, setFirstWords] = useState("");

  // Existing agents (for Brain's adopt-vs-create choice).
  const [agentsLoad, setAgentsLoad] = useState<"loading" | "error" | "ready">(
    "loading",
  );
  const [agentsError, setAgentsError] = useState("");
  const [knownAgents, setKnownAgents] = useState<string[]>([]);

  const fetchAgents = useCallback(() => {
    setAgentsLoad("loading");
    setAgentsError("");
    getAgentOptions()
      .then((r) => {
        setKnownAgents(r.agents ?? []);
        setAgentsLoad("ready");
      })
      .catch((e) => {
        setAgentsError(e instanceof Error ? e.message : String(e));
        setAgentsLoad("error");
      });
  }, []);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  const goTo = useCallback((idx: number) => {
    setStepIndex(idx);
    setMaxVisited((m) => Math.max(m, idx));
    // Scroll the panel back to the top on step change.
    document.getElementById("wlc-panel")?.scrollTo({ top: 0 });
  }, []);

  const advance = useCallback(() => goTo(Math.min(stepIndex + 1, STEPS.length - 1)), [goTo, stepIndex]);
  const back = useCallback(() => {
    if (stepIndex > 0) goTo(stepIndex - 1);
  }, [goTo, stepIndex]);

  // Escape steps back — but never while typing in a field, where it would
  // silently discard the step's input. There, Escape just blurs the field;
  // a second Escape navigates.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const el = e.target as HTMLElement | null;
      const tag = el?.tagName ?? "";
      if (tag === "SELECT") return;
      if (tag === "INPUT" || tag === "TEXTAREA" || el?.isContentEditable) {
        el?.blur();
        return;
      }
      back();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [back]);

  const step = STEPS[stepIndex] ?? STEPS[0]!;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        display: "flex",
        background: C.bg,
        color: C.text,
        fontFamily:
          "'Inter', 'SF Pro Text', -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
      }}
    >
      <style>{GLOBAL_CSS}</style>

      {/* ── Left rail ── */}
      <nav
        aria-label="Setup progress"
        style={{
          width: 264,
          flexShrink: 0,
          borderRight: `1px solid ${C.border}`,
          background: C.surface,
          display: "flex",
          flexDirection: "column",
          padding: "28px 20px",
          overflowY: "auto",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 9,
            marginBottom: 34,
            paddingLeft: 4,
          }}
        >
          <span
            aria-hidden="true"
            style={{
              width: 10,
              height: 10,
              borderRadius: "50%",
              background: C.accent,
              boxShadow: `0 0 12px ${C.accent}`,
            }}
          />
          <span
            style={{
              fontSize: 13,
              fontWeight: 700,
              letterSpacing: "0.22em",
              textTransform: "uppercase",
              color: C.text,
            }}
          >
            ZeroClaw
          </span>
        </div>

        <ol style={{ listStyle: "none", margin: 0, padding: 0, flex: 1 }}>
          {STEPS.map((s, i) => {
            const isCurrent = i === stepIndex;
            const isDone = i < stepIndex || (i <= maxVisited && i !== stepIndex);
            const reachable = i <= maxVisited;
            return (
              <li key={s.id}>
                <button
                  type="button"
                  disabled={!reachable}
                  aria-current={isCurrent ? "step" : undefined}
                  onClick={() => reachable && goTo(i)}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 12,
                    width: "100%",
                    background: isCurrent ? C.accentSoft : "transparent",
                    border: "1px solid transparent",
                    borderColor: isCurrent ? C.accentBorder : "transparent",
                    borderRadius: 8,
                    padding: "9px 10px",
                    marginBottom: 4,
                    cursor: reachable ? "pointer" : "default",
                    textAlign: "left",
                  }}
                >
                  <span
                    aria-hidden="true"
                    style={{
                      width: 24,
                      height: 24,
                      borderRadius: "50%",
                      display: "inline-flex",
                      alignItems: "center",
                      justifyContent: "center",
                      flexShrink: 0,
                      fontSize: 11.5,
                      fontWeight: 700,
                      background: isCurrent
                        ? C.accent
                        : isDone
                          ? C.accentSoft
                          : C.raised,
                      color: isCurrent
                        ? "#0a0a0a"
                        : isDone
                          ? C.accent
                          : C.faint,
                      border: `1px solid ${
                        isCurrent || isDone ? C.accentBorder : C.border
                      }`,
                    }}
                  >
                    {isDone && !isCurrent ? <Check size={13} /> : i + 1}
                  </span>
                  <span style={{ minWidth: 0 }}>
                    <span
                      style={{
                        display: "block",
                        fontSize: 13.5,
                        fontWeight: isCurrent ? 600 : 500,
                        color: isCurrent ? C.text : reachable ? C.muted : C.faint,
                      }}
                    >
                      {s.label}
                    </span>
                    {s.optional ? (
                      <span style={{ fontSize: 10.5, color: C.faint }}>optional</span>
                    ) : null}
                  </span>
                </button>
              </li>
            );
          })}
        </ol>

        <div style={{ paddingTop: 18 }}>
          <div
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={STEPS.length - 1}
            aria-valuenow={stepIndex}
            style={{
              height: 3,
              borderRadius: 2,
              background: C.raised,
              overflow: "hidden",
              marginBottom: 10,
            }}
          >
            <div
              style={{
                height: "100%",
                width: `${(stepIndex / (STEPS.length - 1)) * 100}%`,
                background: C.accent,
                transition: "width 300ms ease",
              }}
            />
          </div>
          <div style={{ fontSize: 11.5, color: C.faint }}>
            Step {stepIndex + 1} of {STEPS.length} · Enter continues · Esc goes back
          </div>
        </div>
      </nav>

      {/* ── Main panel ── */}
      <main
        id="wlc-panel"
        style={{ flex: 1, overflowY: "auto", minWidth: 0 }}
      >
        <div
          key={step.id}
          className="wlc-fade-up"
          style={{ maxWidth: 720, margin: "0 auto", padding: "56px 48px 72px" }}
        >
          {step.id === "welcome" ? (
            <IntroStep onContinue={advance} />
          ) : null}

          {step.id === "brain" ? (
            agentsLoad === "loading" ? (
              <>
                <StepTitle kicker="Step 2 — Brain" title="Choose their mind" />
                <LoadingNote label="Checking for existing agents…" />
              </>
            ) : agentsLoad === "error" ? (
              <>
                <StepTitle kicker="Step 2 — Brain" title="Choose their mind" />
                <ErrorNote
                  message={`Could not list agents: ${agentsError}`}
                  onRetry={fetchAgents}
                />
                <GhostButton onClick={() => setAgentsLoad("ready")}>
                  Continue without existing agents
                </GhostButton>
              </>
            ) : (
              <StepBrain
                existingAgents={knownAgents}
                onBack={back}
                onDone={(alias) => {
                  setAgentAlias(alias);
                  setKnownAgents((prev) =>
                    prev.includes(alias) ? prev : [...prev, alias],
                  );
                  advance();
                }}
              />
            )
          ) : null}

          {step.id === "voice" ? (
            <StepVoice
              agentAlias={agentAlias ?? knownAgents[0] ?? "companion"}
              onBack={back}
              onDone={advance}
            />
          ) : null}

          {step.id === "hearing" ? (
            <StepHearing onBack={back} onDone={advance} />
          ) : null}

          {step.id === "extras" ? (
            <StepExtras onBack={back} onDone={advance} />
          ) : null}

          {step.id === "soul" ? (
            <StepSoul
              initialName={companionName}
              initialSeed={soulSeed}
              onBack={back}
              onDone={(name, seed) => {
                setCompanionName(name);
                setSoulSeed(seed);
                advance();
              }}
            />
          ) : null}

          {step.id === "rituals" ? (
            <StepRituals
              agentAlias={agentAlias ?? knownAgents[0] ?? "companion"}
              onBack={back}
              onDone={(r) => {
                setRituals(r);
                advance();
              }}
            />
          ) : null}

          {step.id === "awaken" ? (
            <StepAwaken
              agentAlias={agentAlias ?? knownAgents[0] ?? "companion"}
              name={companionName || agentAlias || "companion"}
              seed={soulSeed}
              onBack={back}
              onDone={(words) => {
                setFirstWords(words);
                advance();
              }}
            />
          ) : null}

          {step.id === "done" ? (
            <DoneStep
              name={companionName || "your companion"}
              agentAlias={agentAlias}
              rituals={rituals}
              firstWords={firstWords}
              onBack={back}
              onMeet={() =>
                navigate(
                  agentAlias
                    ? `/face/${encodeURIComponent(agentAlias)}`
                    : "/face",
                )
              }
              onSoul={() =>
                navigate(
                  soulStudioLink({
                    agentAlias,
                    name: companionName,
                    seed: soulSeed,
                  }),
                )
              }
            />
          ) : null}
        </div>
      </main>
    </div>
  );
}

// ── Step 1: Welcome ──────────────────────────────────────────────────

function IntroStep({ onContinue }: { onContinue: () => void }) {
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        onContinue();
      }}
      style={{ textAlign: "center", paddingTop: 24 }}
    >
      {/* CSS-only breathing orb */}
      <div
        aria-hidden="true"
        style={{
          position: "relative",
          width: 128,
          height: 128,
          margin: "0 auto 40px",
        }}
      >
        <div
          style={{
            position: "absolute",
            inset: 0,
            borderRadius: "50%",
            background: `radial-gradient(circle at 35% 35%, ${C.accent}, #7a3a24 70%)`,
            animation: "wlc-breathe 4.5s ease-in-out infinite",
          }}
        />
        <div
          style={{
            position: "absolute",
            inset: -22,
            borderRadius: "50%",
            border: `1px solid ${C.accent}`,
            animation: "wlc-halo 4.5s ease-in-out infinite",
          }}
        />
      </div>

      <div className="wlc-fade-up-1">
        <div
          style={{
            color: C.accent,
            fontSize: 12,
            letterSpacing: "0.24em",
            textTransform: "uppercase",
            fontWeight: 600,
            marginBottom: 14,
          }}
        >
          ZeroClaw setup
        </div>
        <h1
          style={{
            fontSize: 44,
            fontWeight: 650,
            letterSpacing: "-0.03em",
            lineHeight: 1.08,
            color: C.text,
            margin: "0 0 16px",
          }}
        >
          Meet your companion
        </h1>
        <p
          style={{
            color: C.muted,
            fontSize: 16,
            lineHeight: 1.65,
            maxWidth: 480,
            margin: "0 auto 40px",
          }}
        >
          In the next few minutes you'll give them a mind, a voice, ears, a
          soul — and the rituals that let them dream. Then you'll meet, face
          to face.
        </p>
      </div>

      <div
        className="wlc-fade-up-2"
        style={{
          display: "flex",
          justifyContent: "center",
          gap: 26,
          marginBottom: 44,
          flexWrap: "wrap",
        }}
      >
        {[
          { icon: <Brain size={16} />, label: "A mind" },
          { icon: <Mic size={16} />, label: "A voice" },
          { icon: <Ear size={16} />, label: "Ears" },
          { icon: <Heart size={16} />, label: "A soul" },
          { icon: <MoonStar size={16} />, label: "Dreams" },
        ].map((f) => (
          <span
            key={f.label}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 8,
              color: C.muted,
              fontSize: 13.5,
            }}
          >
            <span style={{ color: C.accent, display: "inline-flex" }}>{f.icon}</span>
            {f.label}
          </span>
        ))}
      </div>

      <div className="wlc-fade-up-3">
        <PrimaryButton big>Begin</PrimaryButton>
        <div style={{ color: C.faint, fontSize: 12, marginTop: 16 }}>
          Takes about three minutes. You can revisit any step.
        </div>
      </div>
    </form>
  );
}

// ── Step 8: Done ─────────────────────────────────────────────────────

function DoneStep({
  name,
  agentAlias,
  rituals,
  firstWords,
  onBack,
  onMeet,
  onSoul,
}: {
  name: string;
  agentAlias: string | null;
  rituals: { dreaming: boolean; heartbeat: boolean };
  firstWords: string;
  onBack: () => void;
  onMeet: () => void;
  onSoul: () => void;
}) {
  const facts = [
    agentAlias ? `Agent “${agentAlias}” is ready to talk` : null,
    firstWords ? `They know who they are — their first words: “${firstWords}”` : null,
    "Voice and hearing are wired for real-time conversation",
    rituals.dreaming ? "They'll dream each night at midnight" : null,
    rituals.heartbeat ? "Their heart beats every 30 minutes" : null,
  ].filter((f): f is string => f !== null);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        onMeet();
      }}
      style={{ textAlign: "center", paddingTop: 40 }}
    >
      <div className="wlc-fade-up">
        <Sparkles
          size={36}
          color={C.accent}
          style={{ marginBottom: 24 }}
          aria-hidden="true"
        />
        <h1
          style={{
            fontSize: 40,
            fontWeight: 650,
            letterSpacing: "-0.03em",
            color: C.text,
            margin: "0 0 14px",
          }}
        >
          {name.charAt(0).toUpperCase() + name.slice(1)} is awake.
        </h1>
        <p
          style={{
            color: C.muted,
            fontSize: 15.5,
            lineHeight: 1.65,
            maxWidth: 460,
            margin: "0 auto 32px",
          }}
        >
          Everything is in place.
        </p>
      </div>

      {facts.length > 0 ? (
        <ul
          className="wlc-fade-up-1"
          style={{
            listStyle: "none",
            margin: "0 auto 40px",
            padding: 0,
            maxWidth: 420,
            textAlign: "left",
          }}
        >
          {facts.map((f) => (
            <li
              key={f}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 10,
                color: C.muted,
                fontSize: 14,
                padding: "7px 0",
              }}
            >
              <Check size={15} color={C.accent} style={{ flexShrink: 0 }} />
              {f}
            </li>
          ))}
        </ul>
      ) : null}

      <div
        className="wlc-fade-up-2"
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 16,
        }}
      >
        <PrimaryButton big>Meet {name}</PrimaryButton>
        <GhostButton onClick={onSoul}>
          <Wand2 size={15} />
          Shape their soul first — open Soul Studio pre-seeded
        </GhostButton>
        <button
          type="button"
          onClick={onBack}
          style={{
            background: "none",
            border: "none",
            color: C.faint,
            fontSize: 12.5,
            cursor: "pointer",
            marginTop: 8,
            textDecoration: "underline",
            textUnderlineOffset: 3,
          }}
        >
          Go back
        </button>
      </div>
    </form>
  );
}
