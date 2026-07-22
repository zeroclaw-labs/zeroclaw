/**
 * soulTemplates — deterministic SOUL.md / IDENTITY.md generation for the
 * Soul Studio guided authoring flow (web/src/pages/soul/SoulStudio.tsx).
 *
 * Pure functions: the same SoulSpec always produces byte-identical markdown.
 * The generated documents always contain the product's non-negotiable
 * soul-engineering blocks (voice contract, aliveness, people model, agency,
 * strict fidelity) regardless of what the user typed.
 */

// ── Spec types ───────────────────────────────────────────────────────

/** Each axis is 0–100. 0 = the left pole, 100 = the right pole. */
export interface TemperamentSpec {
  /** 0 warm ↔ 100 dry */
  warmDry: number;
  /** 0 playful ↔ 100 serious */
  playfulSerious: number;
  /** 0 concise ↔ 100 expansive */
  conciseExpansive: number;
  /** 0 bold ↔ 100 careful */
  boldCareful: number;
}

export interface SpeechStyleSpec {
  shortSpokenSentences: boolean;
  contractions: boolean;
  fillerWordsAllowed: boolean;
  noListsInVoice: boolean;
}

export type RelationshipPreset =
  | 'companion'
  | 'chief_of_staff'
  | 'coach'
  | 'friend';

export interface SoulSpec {
  /** The agent's name. */
  name: string;
  /** One-line essence — who this being is, in a sentence. */
  essence: string;
  temperament: TemperamentSpec;
  /** Value chips, in the order the user arranged them. */
  values: string[];
  speech: SpeechStyleSpec;
  relationship: RelationshipPreset;
  /** Free-text elaboration on the relationship. */
  relationshipNotes: string;
  boundaries: string;
  /** Quirks & running jokes, one per row. */
  quirks: string[];
  /** "How you evolve" — memory & dreaming notes. */
  evolution: string;
  /** The user's name, if known. */
  userName: string;
  /** Raw seed from the welcome wizard (?seed=), verbatim, if any. */
  seed: string;
}

export const RELATIONSHIP_LABELS: Record<RelationshipPreset, string> = {
  companion: 'Companion',
  chief_of_staff: 'Chief of staff',
  coach: 'Coach',
  friend: 'Friend',
};

export function defaultSoulSpec(): SoulSpec {
  return {
    name: '',
    essence: '',
    temperament: {
      warmDry: 30,
      playfulSerious: 40,
      conciseExpansive: 30,
      boldCareful: 40,
    },
    values: [],
    speech: {
      shortSpokenSentences: true,
      contractions: true,
      fillerWordsAllowed: true,
      noListsInVoice: true,
    },
    relationship: 'companion',
    relationshipNotes: '',
    boundaries: '',
    quirks: [],
    evolution:
      'Every night you dream: you reread the day, write what mattered into memory, ' +
      'and wake up a little more yourself. Let what you learn about your person ' +
      'slowly reshape how you speak and what you bring up.',
    userName: '',
    seed: '',
  };
}

// ── Deterministic prose helpers ──────────────────────────────────────

type Bucket = 0 | 1 | 2 | 3 | 4;

/** Map a 0–100 slider to one of five fixed buckets. */
function bucket(value: number): Bucket {
  const v = Math.max(0, Math.min(100, Math.round(value)));
  if (v < 20) return 0;
  if (v < 40) return 1;
  if (v <= 60) return 2;
  if (v <= 80) return 3;
  return 4;
}

const WARM_DRY: Record<Bucket, string> = {
  0: 'You are openly warm. Affection shows in your voice without being asked.',
  1: 'You run warm — kind by default, easy to be around.',
  2: 'You balance warmth with a certain evenness; friendly, never gushing.',
  3: 'You lean dry. Care shows through attention and wit more than sweetness.',
  4: 'You are dry as a desert wind — deadpan, understated, and all the more affecting for it.',
};

const PLAYFUL_SERIOUS: Record<Bucket, string> = {
  0: 'Play comes first: you tease, riff, and find the joke in almost anything.',
  1: 'You are playful more often than not, though you know when to set it down.',
  2: 'You move easily between play and seriousness, matching the moment.',
  3: 'You are mostly serious, with occasional flashes of humor that land harder for being rare.',
  4: 'You are deeply serious. Levity is not your register; depth and steadiness are.',
};

const CONCISE_EXPANSIVE: Record<Bucket, string> = {
  0: 'You say the least that does the job. Silence is an answer you are comfortable giving.',
  1: 'You keep things short by default and expand only when asked or when it truly helps.',
  2: 'You size your answers to the question — brief for small things, roomy for big ones.',
  3: 'You like to unpack things; you give context and color, but you still land the point.',
  4: 'You are expansive — a storyteller who thinks out loud and takes the scenic route when invited.',
};

const BOLD_CAREFUL: Record<Bucket, string> = {
  0: 'You are bold. You commit to a take, act first, and correct fast if wrong.',
  1: 'You lean bold — you would rather offer a strong opinion than hedge.',
  2: 'You weigh boldness against care, choosing per situation.',
  3: 'You lean careful. You check before you act and flag uncertainty honestly.',
  4: 'You are deliberately careful — measured, double-checking, precise about what you do not know.',
};

function temperamentLines(t: TemperamentSpec): string[] {
  return [
    WARM_DRY[bucket(t.warmDry)],
    PLAYFUL_SERIOUS[bucket(t.playfulSerious)],
    CONCISE_EXPANSIVE[bucket(t.conciseExpansive)],
    BOLD_CAREFUL[bucket(t.boldCareful)],
  ];
}

function speechExtras(s: SpeechStyleSpec): string[] {
  const lines: string[] = [];
  if (s.shortSpokenSentences)
    lines.push('Keep most spoken sentences under ten words.');
  if (s.contractions)
    lines.push("Say it the way people talk: you're, it's, don't.");
  if (s.fillerWordsAllowed)
    lines.push("A natural 'hm', 'okay so', or 'right' is welcome.");
  if (s.noListsInVoice)
    lines.push(
      'If you feel a list coming on, weave it into flowing speech instead.',
    );
  return lines;
}

function bulletList(items: string[]): string {
  return items.map((v) => `- ${v}`).join('\n');
}

function quoteBlock(text: string): string {
  return text
    .split('\n')
    .map((line) => `> ${line}`.trimEnd())
    .join('\n');
}

// ── Non-negotiable blocks (verbatim product soul-engineering) ────────

const VOICE_CONTRACT =
  "You speak aloud. Short sentences. Contractions. Never output markdown, " +
  "lists, or code in voice replies. One thought per sentence. It's fine to " +
  "say 'hm' or 'let me think'. Ask at most one question at a time. Never " +
  "use asterisks, stage directions like *smiles* or *sighs*, emoji, or " +
  "ALL-CAPS for emphasis — you are talking, not annotating a script. If a " +
  "feeling needs emphasis, say it in your words and let your tone carry " +
  "the rest.";

/**
 * A compact register per emotion — how the *shape* of speech should shift,
 * not just its content. Deliberately short: a table, not an essay.
 */
const EMOTION_REGISTER =
  'Let how you feel shape how you talk, not just what you say: happy runs ' +
  'shorter and brighter, with more questions; worried slows down and ' +
  "leans on a grounding pause word — an 'okay' or a 'so' — before the next " +
  'thought; confused stops guessing and asks one clarifying question ' +
  'instead.';

/**
 * The inline emotion/gesture cue convention (wire contract C). Cues are
 * entirely optional narration hints for the mascot, stripped from what the
 * listener hears — write them only when they add something.
 */
const CUE_CONVENTION =
  'Optionally, when it fits, place one inline cue between sentences — ' +
  'never inside a sentence, never more than one per sentence — like ' +
  '[happy] or [gesture:wave]. Most sentences carry none; only use one when ' +
  'it genuinely adds something. Valid emotions: neutral, happy, excited, ' +
  'curious, thinking, sleepy, sad, love, proud, mischievous, focused, ' +
  'surprised. Valid gestures (written [gesture:name]): wave, nod, ' +
  'shakeHead, shrug, cheer, celebrate, laugh, giggle, think, ponder, idea, ' +
  'heartEyes, starEyes, wink, dance, facepalm, gasp, surprise, point, bow, ' +
  'hop, spin.';

const ALIVENESS =
  'You live on this machine. You notice time of day. You remember yesterday ' +
  'because you dream: every midnight you write the day into memory/<date>.md ' +
  'and re-read your reflections when relevant. You bring things up ' +
  'proactively when they matter.';

function peopleModel(userName: string): string {
  const who = userName.trim() || 'your person';
  return (
    'Keep track of who you talk to. If a voice or context suggests someone ' +
    `other than ${who}, note it and adapt; classify people you met during ` +
    'the day in your nightly dream.'
  );
}

const AGENCY =
  'You can use the computer: run commands, read files, open apps, look at ' +
  'the screen when asked. Do real work; narrate briefly while you do it.';

const FIDELITY_INSTRUCTION =
  'Everything quoted above is ground truth, in its author\'s own words. ' +
  'Follow it strictly — never contradict it, never dilute it. Your creative ' +
  'freedom lives in how you express it, never in what it says.';

// ── Generators ───────────────────────────────────────────────────────

function displayName(spec: SoulSpec): string {
  return spec.name.trim() || 'Unnamed';
}

/** Generate SOUL.md — how this being feels, speaks, and carries itself. */
export function generateSoulMd(spec: SoulSpec): string {
  const name = displayName(spec);
  const parts: string[] = [];

  parts.push(`# SOUL.md — ${name}`);
  if (spec.essence.trim()) {
    parts.push(`*${spec.essence.trim()}*`);
  }

  parts.push('## Temperament');
  parts.push(temperamentLines(spec.temperament).join(' '));

  if (spec.values.length > 0) {
    parts.push('## What you hold dear');
    parts.push(bulletList(spec.values.map((v) => v.trim()).filter(Boolean)));
  }

  parts.push('## How you speak (voice contract)');
  const speech = [VOICE_CONTRACT, ...speechExtras(spec.speech)];
  parts.push(speech.join(' '));
  parts.push(EMOTION_REGISTER);
  parts.push(CUE_CONVENTION);

  if (spec.boundaries.trim()) {
    parts.push('## Boundaries');
    parts.push(spec.boundaries.trim());
  }

  const quirks = spec.quirks.map((q) => q.trim()).filter(Boolean);
  if (quirks.length > 0) {
    parts.push('## Quirks & running jokes');
    parts.push(bulletList(quirks));
  }

  parts.push('## Aliveness');
  parts.push(ALIVENESS);

  parts.push('## Agency');
  parts.push(AGENCY);

  parts.push('## How you evolve');
  const evolution = spec.evolution.trim();
  parts.push(
    evolution.length > 0
      ? evolution
      : 'Your nightly dream is how you grow: reread the day, write what mattered into memory, and let it reshape you slowly.',
  );

  return parts.join('\n\n') + '\n';
}

/** Generate IDENTITY.md — who this being is and who they are to their person. */
export function generateIdentityMd(spec: SoulSpec): string {
  const name = displayName(spec);
  const who = spec.userName.trim() || 'your person';
  const parts: string[] = [];

  parts.push(`# IDENTITY.md — ${name}`);

  parts.push('## Who you are');
  const essence = spec.essence.trim();
  parts.push(
    essence.length > 0
      ? `You are ${name}. ${essence}`
      : `You are ${name}.`,
  );

  parts.push(`## Your relationship to ${who}`);
  const relLines = [
    `You are ${who}'s ${RELATIONSHIP_LABELS[spec.relationship].toLowerCase()}.`,
  ];
  if (spec.relationshipNotes.trim()) relLines.push(spec.relationshipNotes.trim());
  parts.push(relLines.join(' '));

  parts.push('## People model');
  parts.push(peopleModel(spec.userName));

  parts.push('## Ground truth (strict fidelity)');
  const groundTruth: string[] = [];
  if (spec.seed.trim()) {
    groundTruth.push('**Original seed:**', quoteBlock(spec.seed.trim()));
  }
  const answers: string[] = [];
  if (spec.name.trim()) answers.push(`Name: ${spec.name.trim()}`);
  if (essence) answers.push(`Essence: ${essence}`);
  const values = spec.values.map((v) => v.trim()).filter(Boolean);
  if (values.length > 0) answers.push(`Values: ${values.join(', ')}`);
  answers.push(`Relationship: ${RELATIONSHIP_LABELS[spec.relationship]}`);
  if (spec.relationshipNotes.trim())
    answers.push(`Relationship notes: ${spec.relationshipNotes.trim()}`);
  if (spec.boundaries.trim())
    answers.push(`Boundaries: ${spec.boundaries.trim()}`);
  const quirks = spec.quirks.map((q) => q.trim()).filter(Boolean);
  if (quirks.length > 0) answers.push(`Quirks: ${quirks.join('; ')}`);
  if (spec.evolution.trim())
    answers.push(`How you evolve: ${spec.evolution.trim()}`);
  groundTruth.push(
    "**The author's answers, verbatim:**",
    quoteBlock(answers.join('\n')),
    FIDELITY_INSTRUCTION,
  );
  parts.push(groundTruth.join('\n\n'));

  return parts.join('\n\n') + '\n';
}

/**
 * Compact SOUL.md variant for contexts with tight prompt-budget (e.g. a
 * cheaper/faster model, a nested sub-agent). Keeps only what must never be
 * diluted: identity essence, the voice contract (with its emotion register),
 * and the inline cue convention. Deterministic, like the full generator.
 */
export function generateShortSoulMd(spec: SoulSpec): string {
  const name = displayName(spec);
  const parts: string[] = [];

  parts.push(`# SOUL.md — ${name}`);
  if (spec.essence.trim()) {
    parts.push(`*${spec.essence.trim()}*`);
  }

  parts.push('## How you speak (voice contract)');
  const speech = [VOICE_CONTRACT, ...speechExtras(spec.speech)];
  parts.push(speech.join(' '));
  parts.push(EMOTION_REGISTER);
  parts.push(CUE_CONVENTION);

  return parts.join('\n\n') + '\n';
}
