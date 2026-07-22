/**
 * Step 7 — Rituals. One-click "dreaming" nightly cron (POST /api/cron via
 * addCronJob) and a heartbeat toggle (config prop writes). Both optional.
 */
import { useState } from "react";
import { Check, HeartPulse, MoonStar } from "lucide-react";
import {
  createDreamingCron,
  heartbeatWrites,
  writeProps,
} from "@/lib/companionSetup";
import { C, ErrorNote, GhostButton, StepFooter, StepTitle } from "./ui";

type ActionState = "idle" | "busy" | "done";

function RitualCard({
  icon,
  title,
  body,
  state,
  error,
  actionLabel,
  doneLabel,
  onAction,
}: {
  icon: React.ReactNode;
  title: string;
  body: React.ReactNode;
  state: ActionState;
  error: string;
  actionLabel: string;
  doneLabel: string;
  onAction: () => void;
}) {
  return (
    <div
      style={{
        border: `1px solid ${state === "done" ? C.accentBorder : C.border}`,
        borderRadius: 10,
        background: state === "done" ? C.accentSoft : C.surface,
        padding: "18px 20px",
        marginBottom: 16,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 8 }}>
        {icon}
        <span style={{ color: C.text, fontSize: 15, fontWeight: 600 }}>{title}</span>
      </div>
      <div style={{ color: C.muted, fontSize: 13, lineHeight: 1.6, marginBottom: 14 }}>
        {body}
      </div>
      {error ? <ErrorNote message={error} onRetry={onAction} /> : null}
      {state === "done" ? (
        <span
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 7,
            color: C.accent,
            fontSize: 13.5,
            fontWeight: 600,
          }}
        >
          <Check size={15} /> {doneLabel}
        </span>
      ) : (
        <GhostButton onClick={state === "busy" ? undefined : onAction}>
          {state === "busy" ? (
            <span className="wlc-spinner" aria-hidden="true" />
          ) : null}
          {actionLabel}
        </GhostButton>
      )}
    </div>
  );
}

export default function StepRituals({
  agentAlias,
  onBack,
  onDone,
}: {
  agentAlias: string;
  onBack: () => void;
  onDone: (rituals: { dreaming: boolean; heartbeat: boolean }) => void;
}) {
  const [dreamState, setDreamState] = useState<ActionState>("idle");
  const [dreamError, setDreamError] = useState("");
  const [beatState, setBeatState] = useState<ActionState>("idle");
  const [beatError, setBeatError] = useState("");

  const createDream = async () => {
    setDreamState("busy");
    setDreamError("");
    try {
      await createDreamingCron(agentAlias);
      setDreamState("done");
    } catch (e) {
      setDreamState("idle");
      setDreamError(e instanceof Error ? e.message : String(e));
    }
  };

  const enableHeartbeat = async () => {
    setBeatState("busy");
    setBeatError("");
    try {
      await writeProps(heartbeatWrites(agentAlias));
      setBeatState("done");
    } catch (e) {
      setBeatState("idle");
      setBeatError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        onDone({ dreaming: dreamState === "done", heartbeat: beatState === "done" });
      }}
    >
      <StepTitle
        kicker="Step 7 — Rituals"
        title="A life with rhythm"
        sub="Two optional rituals give your companion an inner life between conversations."
      />

      <RitualCard
        icon={<MoonStar size={16} color={C.accent} />}
        title="Dreaming — nightly reflection"
        body={
          <>
            Every night at midnight, your companion re-reads the day: what
            happened, who they spoke with and what those people mean to them.
            The reflection is written as a single markdown summary and stored
            as a Core memory titled with the date — so tomorrow, they remember
            not just facts but the shape of the day.
          </>
        }
        state={dreamState}
        error={dreamError ? `Creating the dreaming ritual failed — ${dreamError}` : ""}
        actionLabel="Create dreaming ritual"
        doneLabel="Dreaming ritual scheduled (nightly at 00:00)"
        onAction={() => void createDream()}
      />

      <RitualCard
        icon={<HeartPulse size={16} color={C.accent} />}
        title="Heartbeat — a pulse every 30 minutes"
        body={
          <>
            The heartbeat wakes your companion every 30 minutes to check
            whether anything needs doing — pending follow-ups, tasks in
            HEARTBEAT.md, things it promised to remember. A cheap two-phase
            check keeps quiet periods nearly free.
          </>
        }
        state={beatState}
        error={beatError ? `Enabling the heartbeat failed — ${beatError}` : ""}
        actionLabel="Enable heartbeat"
        doneLabel="Heartbeat enabled (every 30 minutes)"
        onAction={() => void enableHeartbeat()}
      />

      <StepFooter
        onBack={onBack}
        continueLabel="Continue"
        onSkip={
          dreamState === "done" || beatState === "done"
            ? undefined
            : () => onDone({ dreaming: false, heartbeat: false })
        }
        skipLabel="Skip rituals"
      />
    </form>
  );
}
