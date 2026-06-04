import { t } from '@/lib/i18n';

/**
 * Web chat slash commands (#7137).
 *
 * The gateway web chat input treats a leading `/` as a command rather than a
 * raw prompt. Commands drive existing frontend/session primitives (clear/reset,
 * model switch) instead of being sent to the model as text.
 *
 * This module is the single source of truth for the command list. The
 * autocomplete hint popover and the `/help` output are both derived from
 * {@link COMMANDS}; nothing duplicates the command names elsewhere.
 */

/** Canonical command name (without the leading slash). */
export type CommandName = 'help' | 'clear' | 'new' | 'model';

export interface CommandSpec {
  /** Command name without the leading slash, e.g. `help`. */
  name: CommandName;
  /** Display form including the slash and argument hint, e.g. `/model [name]`. */
  usage: string;
  /** i18n key for the one-line description shown in /help and the popover. */
  descriptionKey: string;
}

/**
 * Registry of supported commands. Order here is the order shown in the
 * autocomplete popover and `/help` output.
 */
export const COMMANDS: readonly CommandSpec[] = [
  { name: 'help', usage: '/help', descriptionKey: 'agent.cmd_help_help' },
  { name: 'clear', usage: '/clear', descriptionKey: 'agent.cmd_help_clear' },
  { name: 'new', usage: '/new', descriptionKey: 'agent.cmd_help_new' },
  { name: 'model', usage: '/model [name]', descriptionKey: 'agent.cmd_help_model' },
] as const;

export interface ParsedCommand {
  /** The command name without the leading slash, lower-cased. */
  command: string;
  /** Raw argument string (everything after the command token), trimmed. */
  args: string;
}

/**
 * Returns true when `input` should be treated as a slash command rather than a
 * prompt. A leading `/` (after trimming leading whitespace) triggers command
 * mode. A bare `/` or `//...` (escaped slash) is NOT a command.
 */
export function isSlashCommand(input: string): boolean {
  const trimmed = input.trimStart();
  return trimmed.startsWith('/') && !trimmed.startsWith('//') && trimmed.length > 1;
}

/**
 * Parse a slash-command input into its command token and argument string.
 * Assumes {@link isSlashCommand} returned true for `input`.
 */
export function parseCommand(input: string): ParsedCommand {
  const trimmed = input.trim().slice(1); // drop leading '/'
  const firstSpace = trimmed.search(/\s/);
  if (firstSpace === -1) {
    return { command: trimmed.toLowerCase(), args: '' };
  }
  return {
    command: trimmed.slice(0, firstSpace).toLowerCase(),
    args: trimmed.slice(firstSpace + 1).trim(),
  };
}

/**
 * Commands whose name starts with `prefix` (without leading slash). Used to
 * drive the autocomplete hint popover. An empty prefix returns all commands.
 */
export function matchCommands(prefix: string): CommandSpec[] {
  const p = prefix.toLowerCase();
  return COMMANDS.filter((c) => c.name.startsWith(p));
}

/** Markdown body for `/help`, derived from {@link COMMANDS}. */
export function helpText(): string {
  const lines = COMMANDS.map((c) => `- \`${c.usage}\` — ${t(c.descriptionKey)}`);
  return `**${t('agent.cmd_help_header')}**\n${lines.join('\n')}`;
}
