/**
 * Pure decision logic for detached-turn recovery.
 *
 * A reconnecting dashboard cannot send a prompt until it knows whether the
 * previous connection left a turn running, so recovery deliberately holds the
 * composer closed (`typing=true`) while it resolves that question. The failure
 * mode this module exists to prevent is holding it closed *forever*: if
 * session-state polling gives up, the composer stays locked, and the only
 * affordance left is Stop — which merely aborts a turn on a socket that was
 * never attached to the detached turn, so it can never receive the frame that
 * would release the lock. The session becomes permanently unusable without a
 * page reload.
 *
 * The rule is that fail-closed applies to *sending*, not to the user's ability
 * to recover. Every terminal outcome must therefore name an explicit next
 * action. Keeping these outcomes in a pure module (rather than inline in
 * `AgentContext.tsx`) is deliberate: `web/` has no component-level test
 * harness, so logic embedded in the provider is effectively untestable, and
 * that absence is exactly what let this class of bug through before.
 */

/** Terminal disposition of a recovery attempt. */
export type RecoveryOutcome =
  /** Session state resolved; the composer can be released normally. */
  | { kind: 'resolved' }
  /**
   * Recovery could not determine the session's state. The composer stays
   * locked against sending, but the UI must offer an explicit retry so the
   * user is never stranded.
   */
  | UnrecoverableOutcome;

/** The failure half of {@link RecoveryOutcome}. */
export interface UnrecoverableOutcome {
  kind: 'unrecoverable';
  reason: RecoveryFailureReason;
  retryable: true;
}

/** Why recovery gave up, which decides the message the user sees. */
export type RecoveryFailureReason =
  /** A 4xx that will not improve on retry — typically auth or a missing session. */
  | 'rejected'
  /** Repeated transport/5xx failures exhausted the retry budget. */
  | 'exhausted'
  /**
   * The transcript could not be re-fetched after the turn completed. The
   * local copy is missing whatever the detached turn produced, so it is
   * stale rather than merely incomplete.
   */
  | 'hydration';

/**
 * Decide the disposition of a failed transcript hydration.
 *
 * Accepting this failure silently is what makes it dangerous: the turn has
 * already completed, so the local transcript is missing the answer, and the
 * user cannot tell. A follow-up prompt would then be composed against history
 * the operator never saw. Surface it as retryable instead of continuing.
 */
export function hydrationFailureOutcome(): UnrecoverableOutcome {
  return { kind: 'unrecoverable', reason: 'hydration', retryable: true };
}

/**
 * Classify a session-state error as terminal or worth retrying.
 *
 * 408 and 429 are excluded from the terminal 4xx range because both are
 * explicit invitations to try again.
 */
export function isTerminalRecoveryStatus(status: number | null): boolean {
  return (
    status !== null
    && status >= 400
    && status < 500
    && status !== 408
    && status !== 429
  );
}

/**
 * Decide whether a recovery attempt should stop, and with what disposition.
 *
 * Returning `null` means "keep polling".
 */
export function recoveryFailureOutcome(params: {
  status: number | null;
  failures: number;
  maxFailures: number;
}): UnrecoverableOutcome | null {
  const { status, failures, maxFailures } = params;
  if (isTerminalRecoveryStatus(status)) {
    return { kind: 'unrecoverable', reason: 'rejected', retryable: true };
  }
  if (failures >= maxFailures) {
    return { kind: 'unrecoverable', reason: 'exhausted', retryable: true };
  }
  return null;
}

/**
 * Whether the composer must remain locked against sending.
 *
 * Unresolved lifecycle state always locks: a prompt sent through a socket
 * whose Agent was seeded from pre-turn history would run against a stale
 * transcript.
 */
export function shouldBlockSending(outcome: RecoveryOutcome): boolean {
  return outcome.kind === 'unrecoverable';
}

/**
 * Whether the UI must present an explicit recovery affordance.
 *
 * This is the direct inverse of the lockout bug: any outcome that blocks
 * sending has to hand the user a way out, because no incoming socket frame
 * will arrive to clear the block on its own.
 */
export function shouldOfferRecoveryAction(outcome: RecoveryOutcome): boolean {
  return outcome.kind === 'unrecoverable' && outcome.retryable;
}

/** i18n key for the message shown for a terminal recovery outcome. */
export function recoveryMessageKey(reason: RecoveryFailureReason): string {
  if (reason === 'rejected') return 'agent.session_recovery_rejected';
  if (reason === 'hydration') return 'agent.session_recovery_hydration';
  return 'agent.session_recovery_error';
}
