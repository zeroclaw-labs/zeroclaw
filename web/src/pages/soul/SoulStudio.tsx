/**
 * SoulStudio — soul & identity authoring for an agent.
 *
 * Two modes:
 *  - Guided: structured form → deterministic SOUL.md / IDENTITY.md via
 *    web/src/lib/soulTemplates.ts, with a live preview.
 *  - Direct: raw markdown editors for SOUL.md / IDENTITY.md / USER.md /
 *    HEARTBEAT.md with 409 conflict resolution.
 *
 * Route: /soul and /soul/:alias (inside the dashboard Layout). Reads
 * ?seed= (from the welcome wizard) to prefill the guided essence field.
 */

import { useEffect, useState } from 'react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom';
import { FileText, Flame, Wand2 } from 'lucide-react';
import { getAgentOptions } from '@/lib/api';
import DirectMode from './DirectMode';
import GuidedMode from './GuidedMode';
import { S } from './studioUi';

type Mode = 'guided' | 'direct';

export default function SoulStudio() {
  const { alias } = useParams<{ alias: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const seed = searchParams.get('seed');
  const seedName = searchParams.get('name');

  const [mode, setMode] = useState<Mode>('guided');
  const [agents, setAgents] = useState<string[]>([]);
  const [fallbackAgent, setFallbackAgent] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Resolve the agent list once; when no :alias, use the first agent.
  useEffect(() => {
    let cancelled = false;
    getAgentOptions()
      .then((resp) => {
        if (cancelled) return;
        setAgents(resp.agents);
        setFallbackAgent(resp.agents[0] ?? null);
        if (resp.agents.length === 0) setLoadError('no-agents');
      })
      .catch((e) => {
        if (cancelled) return;
        // The personality API requires a real agent alias — without the
        // list there is nothing valid to edit, so surface the failure.
        setLoadError(e instanceof Error ? e.message : String(e));
        setFallbackAgent(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const agent = alias ?? fallbackAgent;

  return (
    <div
      className="mx-auto flex w-full max-w-6xl flex-col gap-5 rounded-2xl p-4 sm:p-6"
      style={{ background: S.bg, color: S.text }}
    >
      {/* Header */}
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="flex items-center gap-2.5 text-xl font-semibold tracking-tight">
            <Flame size={20} style={{ color: S.accent }} />
            Soul Studio
          </h1>
          <p className="mt-1 text-sm" style={{ color: S.muted }}>
            Shape who they are — their temperament, voice, and ground truth.
          </p>
        </div>

        <div className="flex items-center gap-3">
          {agents.length > 0 && (
            <label className="flex items-center gap-2 text-xs" style={{ color: S.faint }}>
              Agent
              <select
                value={alias ?? agents[0] ?? ''}
                onChange={(e) =>
                  navigate(
                    `/soul/${encodeURIComponent(e.target.value)}${
                      seed ? `?seed=${encodeURIComponent(seed)}` : ''
                    }`,
                  )
                }
                className="rounded-lg border px-2.5 py-1.5 text-sm outline-none"
                style={{
                  background: S.surfaceRaised,
                  borderColor: S.border,
                  color: S.text,
                }}
              >
                {agents.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
                {alias && !agents.includes(alias) && (
                  <option value={alias}>{alias}</option>
                )}
              </select>
            </label>
          )}

          {/* Mode tabs */}
          <div
            className="inline-flex overflow-hidden rounded-lg border"
            style={{ borderColor: S.border }}
          >
            <ModeTab
              icon={<Wand2 size={13} />}
              label="Guided"
              active={mode === 'guided'}
              onClick={() => setMode('guided')}
            />
            <ModeTab
              icon={<FileText size={13} />}
              label="Direct"
              active={mode === 'direct'}
              onClick={() => setMode('direct')}
            />
          </div>
        </div>
      </header>

      {agent === null ? (
        <div
          className="flex min-h-[40vh] flex-col items-center justify-center gap-3 rounded-xl border text-sm"
          style={{ borderColor: S.border, background: S.surface, color: S.faint }}
        >
          {loadError === 'no-agents' ? (
            <>
              <span>No agents configured yet — a soul needs someone to live in.</span>
              <Link
                to="/welcome"
                className="rounded-lg px-4 py-2 text-sm font-medium"
                style={{ background: S.accent, color: '#000' }}
              >
                Run setup
              </Link>
            </>
          ) : loadError ? (
            <span>Could not load the agent list ({loadError}) — is the daemon running?</span>
          ) : (
            <span>Loading…</span>
          )}
        </div>
      ) : mode === 'guided' ? (
        <GuidedMode key={`g-${agent}`} agent={agent} seed={seed} seedName={seedName} />
      ) : (
        <DirectMode key={`d-${agent}`} agent={agent} />
      )}
    </div>
  );
}

function ModeTab({
  icon,
  label,
  active,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="inline-flex items-center gap-1.5 px-3.5 py-1.5 text-xs transition-colors"
      style={{
        background: active ? S.accentSoft : 'transparent',
        color: active ? S.accent : S.muted,
        fontWeight: active ? 600 : 400,
      }}
    >
      {icon}
      {label}
    </button>
  );
}
