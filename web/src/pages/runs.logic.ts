export type RunsStreamEffect = 'replace' | 'upsert' | 'disable' | 'resync' | 'ignore';

/** Classify stream frames so a lag can never be mistaken for a harmless event. */
export function runsStreamEffect(frameType: string): RunsStreamEffect {
  switch (frameType) {
    case 'snapshot':
      return 'replace';
    case 'run':
      return 'upsert';
    case 'disabled':
      return 'disable';
    case 'lagged':
      return 'resync';
    default:
      return 'ignore';
  }
}

export function confirmsCancellation(status: string, isTerminal: boolean): boolean {
  return status === 'cancel_requested' || isTerminal;
}
