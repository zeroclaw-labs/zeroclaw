// Step-by-step field projection of a SOP graph, optionally tinted with a
// run overlay (state badges, captured calls). The one list renderer shared
// by the SOP fields layer and the run detail page.
import {
  overlayCallsByStep,
  overlayStateByStep,
  runStateBadge,
  type GraphPin,
  type RunOverlay,
  type SopGraph,
} from '@/lib/sops';
import { t } from '@/lib/i18n';
import { Badge } from '@/components/ui';
import { CapturedCallList } from '@/components/SopCalls';

function pinTypeLabel(pin: GraphPin): string {
  if (pin.class === 'flow') return 'flow';
  return pin.data_type ?? 'any';
}

export default function SopStepList({
  graph,
  overlay,
  showPins = true,
}: {
  graph: SopGraph;
  overlay?: RunOverlay | null;
  showPins?: boolean;
}) {
  const stateByStep = overlayStateByStep(overlay);
  const callsByStep = overlayCallsByStep(overlay);
  return (
    <div className="divide-y divide-pc-border rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface text-sm">
      {graph.nodes
        .filter((node) => node.kind === 'step')
        .map((node) => {
          const state = stateByStep.get(node.step);
          const calls = callsByStep.get(node.step);
          return (
            <div key={node.step} className="flex items-start gap-3 px-3 py-2">
              <span className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded bg-pc-accent text-xs font-semibold text-[#0b1220]">
                {node.step}
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="font-medium text-pc-text">{node.title}</span>
                  {state ? (
                    <Badge tone={runStateBadge(state)}>
                      {t(`sops.run_state.${state}`)}
                    </Badge>
                  ) : null}
                </div>
                {showPins ? (
                  <div className="mt-0.5 text-xs text-pc-text-muted">
                    {t('sops.inputs')}:{' '}
                    {node.inputs.length === 0
                      ? '-'
                      : node.inputs.map((p) => `${p.name}:${pinTypeLabel(p)}`).join(', ')}
                    {'  ·  '}
                    {t('sops.outputs')}:{' '}
                    {node.outputs.length === 0
                      ? '-'
                      : node.outputs.map((p) => `${p.name}:${pinTypeLabel(p)}`).join(', ')}
                  </div>
                ) : null}
                {calls ? (
                  <div className="mt-2">
                    <div className="mb-1 text-xs font-medium text-pc-text">
                      {t('sops.captured_calls')}
                    </div>
                    <CapturedCallList calls={calls} />
                  </div>
                ) : null}
              </div>
            </div>
          );
        })}
    </div>
  );
}
