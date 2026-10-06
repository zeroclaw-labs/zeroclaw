import type { Sop } from './sops';

/** Recognize a proposed definition, leaving schema validation to the gateway. */
export function isSopDraft(value: unknown): value is Sop {
  return (
    !!value &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    'name' in value &&
    typeof value.name === 'string' &&
    'steps' in value &&
    Array.isArray(value.steps) &&
    'triggers' in value &&
    Array.isArray(value.triggers)
  );
}

export function sopProposalSource(message: string): string | null {
  for (const match of message.matchAll(/```(?:json)?\s*\n([\s\S]*?)```/g)) {
    try {
      if (isSopDraft(JSON.parse(match[1]!))) return match[1]!;
    } catch {
      /* A partial streamed block is not an applicable proposal. */
    }
  }
  return null;
}
