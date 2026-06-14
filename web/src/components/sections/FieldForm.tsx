// Shared form renderer for a section's fields. Used by both /quickstart and
// /config. Walks the entries returned by GET /api/config/list?prefix=...,
// dispatches each input by `kind` (no value-sniffing), and submits all
// changed fields as one PATCH on save.
//
// Per-field behavior:
//  * bool       → <select> true/false
//  * enum       → <select> with enum_variants
//  * string-array → <textarea>, one value per line
//  * integer/float → <input type="number">
//  * secret     → <input type="password"> with populated indicator
//  * provider model field (path matches `model_providers.<name>.model`) →
//    fetches /api/config/catalog/models?provider=<name>, populates a
//    <datalist>; on fetch failure falls back to free-text with help text.
//  * everything else → <input type="text">
//
// Each field carries an optional comment input (per-PATCH-op `comment`).
//
// On error: structured ApiError envelope binds inline to the field by .path.

import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  useState,
} from "react";
import { Link } from "react-router-dom";
import {
  ExternalLink,
  FolderOpen,
  List as ListIcon,
  MessageSquarePlus,
  Plus,
  Save,
  Trash2,
  Type as TypeIcon,
  X,
} from "lucide-react";
import DirectoryPicker from "./DirectoryPicker";
import ToolPicker from "@/components/ToolPicker";
import { Badge, Button } from "@/components/ui";
import type { BadgeTone } from "@/components/ui";
import { t } from "@/lib/i18n";
import {
  ApiError,
  descriptionForPath,
  fetchConfigSchema,
  getAgentOptions,
  getCatalogModels,
  listProps,
  objectArrayElementProps,
  patchConfig,
  type AgentOptionsResponse,
  type ConfigApiError,
  type DriftEntry,
  type ListResponseEntry,
  type ObjectArrayPropMeta,
  type PatchOp,
} from "../../lib/api";
import { useConfigDraft } from "../../lib/draftStore";
import { fuzzyFilter } from "../../lib/fuzzy";
import { isLocalModelProviderName, primeModelProviderCatalog } from "../../lib/modelProviders";
import EntityEnabledToggle from "../EntityEnabledToggle";

function entryValue(entry: ListResponseEntry): unknown {
  return entry.populated ? entry.value : undefined;
}

/**
 * Inline switch for a `bool` field. Track + thumb pattern with an
 * adjacent `true` / `false` label so the form stays readable when
 * dense. The component is dumb — it takes the current `value` and
 * fires `onChange(next)` on click; the parent owns the draft state.
 */
function BoolSwitch({
  id,
  value,
  onChange,
}: {
  id?: string;
  value: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <button
      type="button"
      id={id}
      role="switch"
      aria-checked={value}
      onClick={() => onChange(!value)}
      className="inline-flex items-center gap-2 rounded-full px-1 py-1 select-none"
      style={{
        background: value
          ? "var(--color-status-success-alpha-08)"
          : "var(--pc-bg-elevated)",
        border: "1px solid",
        borderColor: value
          ? "var(--color-status-success-alpha-20)"
          : "var(--pc-border)",
      }}
    >
      <span
        className="relative inline-block h-4 w-7 rounded-full transition-colors"
        style={{
          background: value
            ? "var(--color-status-success)"
            : "var(--pc-border)",
        }}
      >
        <span
          className="absolute top-0.5 h-3 w-3 rounded-full bg-white transition-all"
          style={{ left: value ? "calc(100% - 14px)" : "2px" }}
        />
      </span>
      <span
        className="text-xs font-medium pr-2"
        style={{
          color: value ? "var(--color-status-success)" : "var(--pc-text-muted)",
        }}
      >
        {value ? "true" : "false"}
      </span>
    </button>
  );
}

interface FieldFormProps {
  /** Dotted prefix to fetch fields under, e.g. `model_providers.anthropic`. */
  prefix: string;
  /** Called after a successful save; parent typically advances or refreshes. */
  onSaved?: () => void;
  /** Hide the trash icon (per-prop reset) when the parent doesn't want it. */
  showDelete?: boolean;
  /** Optional title rendered above the form. */
  title?: string;
  /** Drift entries from the page-level fetch — passed through so each
   *  drifted field renders an inline `in-memory: [...] / on-disk: [...]`
   *  comparison next to its label. Empty / undefined when nothing drifted. */
  drift?: DriftEntry[];
  /** Filter for which entries this form renders. Returning false hides
   *  the entry. Used to partition a section's fields across tabs (e.g.
   *  Model providers: Connection / Model / Advanced). The form still
   *  fetches every entry under `prefix`; the predicate only gates
   *  rendering, so saves still validate against the full server-side
   *  config. */
  includePath?: (path: string) => boolean;
  /** Render the save bar as a normal inline element instead of
   *  `sticky bottom-0`. Set when the FieldForm is embedded inside a
   *  taller composite editor (e.g. an expandable rate-sheet row) where
   *  the sticky viewport-bottom behavior would conflict with sibling
   *  content rendered below the form. */
  inlineSaveBar?: boolean;
}

/** Imperative handle the parent uses to flush unsaved changes before
 *  advancing the wizard. Resolves `true` when the form was clean or the
 *  save succeeded; `false` if the save failed (so the parent can stop). */
export interface FieldFormHandle {
  flushSave: () => Promise<boolean>;
}

function rendererFor(
  entry: ListResponseEntry,
): "bool" | "array" | "object-array" | "secret" | "select" | "number" | "text" {
  if (entry.is_secret) return "secret";
  switch (entry.kind) {
    case "bool":
      return "bool";
    case "string-array":
      return "array";
    case "object-array":
      return "object-array";
    case "integer":
    case "float":
      return "number";
    case "enum":
      return entry.enum_variants && entry.enum_variants.length > 0
        ? "select"
        : "text";
    default:
      return "text";
  }
}

function fieldShortLabel(entry: ListResponseEntry): string {
  return entry.path.split(".").pop()!.replace(/[-_]/g, " ");
}

// Humanize the schema-provided dotted path's LAST segment into a Title-ish
// label (de-kebab/de-snake, capitalize words). Purely a presentation transform
// of the schema's own path — no hardcoded field names — so the config renderer
// stays schema-driven (#6175). The dotted path itself remains visible as a
// secondary line for unambiguous identification.
function humanizeFieldLabel(path: string): string {
  const leaf = path.split(".").pop() ?? path;
  return leaf
    .replace(/[-_]/g, " ")
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

function setupFieldPriority(entry: ListResponseEntry): number {
  const leaf = entry.path.split(".").pop() ?? "";
  if (/^providers\.models\.[^.]+\.[^.]+\./.test(entry.path)) {
    const order = ["model", "api_key", "requires_openai_auth", "uri"];
    const idx = order.indexOf(leaf);
    if (idx >= 0) return idx;
  }
  if (/^agents\.[^.]+\./.test(entry.path)) {
    const order = [
      "enabled",
      "model_provider",
      "risk_profile",
      "runtime_profile",
      "channels",
    ];
    const idx = order.indexOf(leaf);
    if (idx >= 0) return idx;
  }
  if (entry.path === "memory.backend") return 0;
  if (/^risk_profiles\.[^.]+\./.test(entry.path)) {
    const idx = ["approval_mode", "allowed_commands", "sandbox_mode"].indexOf(
      leaf,
    );
    if (idx >= 0) return idx;
  }
  if (/^runtime_profiles\.[^.]+\./.test(entry.path)) {
    const idx = [
      "agentic",
      "max_iterations",
      "timeout_secs",
      "max_cost_usd",
    ].indexOf(leaf);
    if (idx >= 0) return idx;
  }
  return 100;
}

function setupRequirement(
  entry: ListResponseEntry,
): { label: string; tone: "required" | "choice" | "optional" } | null {
  const leaf = entry.path.split(".").pop() ?? "";
  if (/^providers\.models\.[^.]+\.[^.]+\./.test(entry.path)) {
    const localProvider = isLocalModelProviderPath(entry.path);
    if (leaf === "model") return { label: "Required", tone: "required" };
    if (leaf === "api_key") {
      return localProvider
        ? { label: "Optional for remote auth", tone: "optional" }
        : { label: "Required for API-key auth", tone: "required" };
    }
    if (leaf === "requires_openai_auth")
      return { label: "Auth option", tone: "choice" };
    if (leaf === "uri") return { label: "Endpoint option", tone: "choice" };
    return { label: "Optional", tone: "optional" };
  }
  const topLevelAgentField =
    entry.path.match(/^agents\.[^.]+\.([^.]+)$/)?.[1] ?? null;
  if (topLevelAgentField) {
    if (
      ["enabled", "model_provider", "risk_profile", "runtime_profile"].includes(
        topLevelAgentField,
      )
    ) {
      return { label: "Required", tone: "required" };
    }
    return { label: "Optional", tone: "optional" };
  }
  if (
    /^risk_profiles\.[^.]+\./.test(entry.path) ||
    /^runtime_profiles\.[^.]+\./.test(entry.path)
  ) {
    return { label: "Advanced", tone: "optional" };
  }
  if (entry.path === "memory.backend")
    return { label: "Recommended", tone: "choice" };
  return null;
}

// Map the schema-derived requirement tone to a calm Badge tone. The label
// text is whatever `setupRequirement` produced from the schema path — only
// the tint is chosen here, so nothing is hardcoded.
function requirementBadgeTone(
  tone: "required" | "choice" | "optional",
): BadgeTone {
  if (tone === "required") return "warn";
  if (tone === "choice") return "neutral";
  return "neutral";
}

function isLocalModelProviderPath(path: string): boolean {
  const provider = path.match(/^providers\.models\.([^.]+)\./)?.[1] ?? "";
  return isLocalModelProviderName(provider);
}

function modelFallbackExample(path: string): string {
  return isLocalModelProviderPath(path)
    ? "llama3.2"
    : "claude-sonnet-4-5-20251101";
}

function defaultInputValue(entry: ListResponseEntry): string {
  const v = entry.value;
  if (entry.kind === "string-array" || entry.kind === "object-array") {
    // API returns the TOML/JSON array form as a string. Keep it as the
    // canonical draft shape; the row editor parses on render.
    if (typeof v === "string") return v === "<unset>" ? "[]" : v;
    if (Array.isArray(v)) return JSON.stringify(v);
    return "[]";
  }
  if (typeof v === "string") return v === "<unset>" ? "" : v;
  if (typeof v === "boolean") return v ? "true" : "false";
  if (Array.isArray(v)) return v.join("\n");
  return "";
}

function parseInput(entry: ListResponseEntry, raw: string): unknown {
  switch (rendererFor(entry)) {
    case "bool":
      return raw === "true";
    case "array":
      return parseArrayDraft(raw);
    case "object-array": {
      const trimmed = raw.trim();
      if (!trimmed) return [];
      try {
        const parsed = JSON.parse(trimmed);
        return Array.isArray(parsed) ? parsed : [];
      } catch {
        return [];
      }
    }
    case "number": {
      const n = Number(raw);
      return Number.isNaN(n) ? raw : n;
    }
    default:
      return raw;
  }
}

// Parse the draft string for a Vec<String> field. Accepts the JSON-array
// form (the canonical shape both the chip editor and the textarea view
// emit), with comma- / newline-separated as a fallback for hand-typed
// freeform input. Trims whitespace and drops empty entries on save.
function parseArrayDraft(raw: string): string[] {
  const trimmed = raw.trim();
  if (!trimmed) return [];
  if (trimmed.startsWith("[")) {
    try {
      const parsed = JSON.parse(trimmed);
      if (Array.isArray(parsed)) {
        return parsed
          .map((v) => String(v))
          .map((s) => s.trim())
          .filter((s) => s.length > 0);
      }
    } catch {
      /* fall through to freeform split */
    }
  }
  return raw
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function parseArrayRows(value: string): string[] {
  if (!value) return [];
  try {
    const parsed = JSON.parse(value);
    if (Array.isArray(parsed)) return parsed.map((v) => String(v));
  } catch {
    // Fallback: comma- or newline-separated freeform.
    return value
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }
  return [];
}

// `Option<Vec<String>>` carries a three-state distinction: None / [] / ["a"].
// Detected via type_hint so the chip editor can offer a separate "Clear (set
// to none)" affordance and the save path can emit `null` for empty + optional.
function isOptionalArray(typeHint: string): boolean {
  const compact = typeHint.replace(/\s+/g, "");
  return (
    compact.startsWith("Option<Vec<") || compact.startsWith("Option<HashSet<")
  );
}

// A field is "required" (in the display-only validation sense) when its Rust
// type signature is NOT wrapped in `Option<…>`. Read purely from the schema's
// own `type_hint`, so nothing is hardcoded per field. Used only to surface an
// inline hint — the authoritative required/optional check still runs in
// `Config::validate()` on the server at save time.
function isRequiredField(typeHint: string): boolean {
  return !typeHint.replace(/\s+/g, "").startsWith("Option<");
}

// Display-only, pre-save validation derived entirely from the entry's own
// metadata (`kind` + `type_hint`). Returns a short message to show under the
// input (red), or null when the current draft value looks fine. This NEVER
// blocks typing and NEVER changes what gets serialized on save — the
// authoritative validation remains `Config::validate()` server-side, surfaced
// inline via the `.path`-bound `ConfigApiError`. Scope is deliberately narrow:
// only constraints we can read locally (a required scalar left empty; a
// non-numeric value in an integer/float field).
function validationHint(entry: ListResponseEntry, raw: string): string | null {
  // Secrets: an empty box means "keep the stored value", never "cleared" —
  // so emptiness is never an error here.
  if (entry.is_secret) return null;
  const renderer = rendererFor(entry);
  const trimmed = raw.trim();

  if (renderer === "number" && trimmed.length > 0) {
    const n = Number(trimmed);
    if (!Number.isFinite(n)) {
      return entry.kind === "integer"
        ? "Must be a whole number."
        : "Must be a number.";
    }
    if (entry.kind === "integer" && !Number.isInteger(n)) {
      return "Must be a whole number (no decimals).";
    }
  }

  // Required scalar left empty. Arrays/object-arrays carry their own three-state
  // semantics (empty list vs none) handled at save time, so we don't flag them.
  if (
    isRequiredField(entry.type_hint) &&
    renderer !== "array" &&
    renderer !== "object-array" &&
    renderer !== "bool" &&
    trimmed.length === 0
  ) {
    return "This field is required.";
  }

  return null;
}

// Per-provider+alias catalog cache. Cleared via clearFieldFormCatalogCaches() on
// nav so a new model alias added under (say) `anthropic` shows up the next
// time the user opens an agent form without a hard refresh.
let modelsCache: Record<
  string,
  { models: string[]; live: boolean; local: boolean }
> = {};

// In-flight `getAgentOptions()` promise so N FieldForm rows mounting at
// once share a single round-trip. Cleared when the request resolves;
// the response itself is NOT cached across mounts — each FieldForm mount
// triggers a fresh fetch so newly-created channels / agents / bundles
// surface immediately on the next form visit.
let agentOptionsPromise: Promise<AgentOptionsResponse> | null = null;
function loadAgentOptions(): Promise<AgentOptionsResponse> {
  if (agentOptionsPromise) return agentOptionsPromise;
  agentOptionsPromise = getAgentOptions().finally(() => {
    agentOptionsPromise = null;
  });
  return agentOptionsPromise;
}

/// Clear the per-provider model catalog cache. Called by Config.tsx when
/// the user navigates between sections so a model alias added under e.g.
/// `providers.models.anthropic` shows up the next time another agent's
/// `model_provider` dropdown is opened.
export function clearFieldFormCatalogCaches() {
  modelsCache = {};
}

// Single-select alias-ref fields on an agent: render as <select> with the
// matched options. Mandatory-vs-optional is enforced by `Config::validate()`
// at the backend on save — the frontend just submits whatever the user
// picks (including empty) and surfaces structured errors inline.
//
// Keys are snake_case to match `prop_fields()` emission (the macro at
// crates/zeroclaw-macros/src/lib.rs:1056 passes through snake_case Rust
// field name unchanged for the schema path).
const AGENT_SINGLE_ALIAS_FIELDS: Record<string, keyof AgentOptionsResponse> = {
  "model_provider": "model_providers",
  "risk_profile": "risk_profiles",
  "runtime_profile": "runtime_profiles",
  "memory_namespace": "memory_namespaces",
};

// Multi-select alias-ref fields on an agent: render as the chip editor with
// a `<datalist>` of the available aliases (no free text expected — the
// suggestions list is the canonical input source). Same snake_case
// convention as AGENT_SINGLE_ALIAS_FIELDS above.
const AGENT_MULTI_ALIAS_FIELDS: Record<string, keyof AgentOptionsResponse> = {
  channels: "channels",
  "skill_bundles": "skill_bundles",
  "knowledge_bundles": "knowledge_bundles",
  "mcp_bundles": "mcp_bundles",
};

// Peer-groups carry the same alias-ref shape as agents do: a single
// `channel` field (one-of the configured channels) plus an `agents`
// list (subset of configured agents). Mirror agent's picker UX so a
// peer-groups form doesn't fall back to free-text inputs.
const PEER_GROUP_SINGLE_ALIAS_FIELDS: Record<
  string,
  keyof AgentOptionsResponse
> = {
  channel: "channel_types",
};
const PEER_GROUP_MULTI_ALIAS_FIELDS: Record<
  string,
  keyof AgentOptionsResponse
> = {
  agents: "agents",
};

function agentFieldKey(path: string): string | null {
  const m = path.match(/^agents\.[^.]+\.(.+)$/);
  return m && m[1] ? m[1] : null;
}

function peerGroupFieldKey(path: string): string | null {
  const m = path.match(/^peer_groups\.[^.]+\.(.+)$/);
  return m && m[1] ? m[1] : null;
}

// Leaf-based single-alias reference map. The agent/peer-group maps above are
// gated on the `agents.*` / `peer_groups.*` prefix; this map matches the same
// clearly-referential fields by their dotted-path LEAF, in the same
// field-semantic spirit as the `…model` datalist and `allowed_tools` ToolPicker
// hooks (no hardcoded parent section). It is consulted only as a fallback when
// the prefixed maps don't apply, so existing behavior is unchanged. Each leaf's
// value is a single alias string of the target section — byte-identical to the
// free-text value it replaces — so the save/PATCH serialization is untouched.
// If the target section's aliases can't be fetched, the field FALLS BACK to the
// existing text input (the picker only renders when `agentOptions` is loaded
// and the option list is non-empty).
const LEAF_SINGLE_ALIAS_FIELDS: Record<string, keyof AgentOptionsResponse> = {
  model_provider: "model_providers",
  risk_profile: "risk_profiles",
  runtime_profile: "runtime_profiles",
  memory_namespace: "memory_namespaces",
};

// Resolve the single-alias reference section for a field by its path leaf.
// Returns null for anything not in the conservative reference map, leaving the
// field as free text.
function leafSingleAliasKind(path: string): keyof AgentOptionsResponse | null {
  const leaf = path.split(".").pop() ?? "";
  return LEAF_SINGLE_ALIAS_FIELDS[leaf] ?? null;
}

// Cross-section navigation map for agent alias-ref fields. Each entry
// answers: "where does this field's source live in /config/?"
// Used both by the empty-state hint and the per-item edit-jump links.
const AGENT_ALIAS_SOURCE_PATH: Record<keyof AgentOptionsResponse, string> = {
  channels: "/config/channels",
  channel_types: "/config/channels",
  model_providers: "/config/providers",
  risk_profiles: "/config/risk_profiles",
  runtime_profiles: "/config/runtime_profiles",
  skill_bundles: "/config/skill_bundles",
  knowledge_bundles: "/config/knowledge_bundles",
  mcp_bundles: "/config/mcp_bundles",
  agents: "/config/agents",
  memory_namespaces: "/config/memory_namespaces",
};

function AgentEmptyAliasFallback({
  fieldKind,
}: {
  fieldKind: keyof AgentOptionsResponse;
}) {
  const path = AGENT_ALIAS_SOURCE_PATH[fieldKind];
  const label = fieldKind.replace(/_/g, " ");
  return (
    <div
      className="text-xs px-3 py-2 rounded border"
      style={{
        color: "var(--pc-text-muted)",
        borderColor: "var(--pc-border)",
        background: "var(--pc-bg-surface-subtle)",
      }}
    >
      No {label} configured yet.{" "}
      <Link
        to={path}
        className="inline-flex items-center gap-1 underline"
        style={{ color: "var(--pc-text-link)" }}
      >
        Configure {label} <ExternalLink className="h-3 w-3" />
      </Link>
    </div>
  );
}

/// Path resolver for the per-item edit-jump beside picker entries.
/// `channels` → `/config/channels/<type>/<alias>`; bare-alias sections
/// like risk_profiles use `/config/<section>/<alias>`. Shape parallels the
/// AliasListView routes already configured in `Config.tsx`.
function agentAliasJumpPath(
  fieldKind: keyof AgentOptionsResponse,
  alias: string,
): string {
  const base = AGENT_ALIAS_SOURCE_PATH[fieldKind];
  // Channels and model_providers use dotted alias (`telegram.default`,
  // `anthropic.work`); split into the two URL segments. Single-tier
  // sections use the alias directly.
  if (fieldKind === "channels" || fieldKind === "model_providers") {
    const dot = alias.indexOf(".");
    if (dot > 0) {
      return `${base}/${encodeURIComponent(alias.slice(0, dot))}/${encodeURIComponent(alias.slice(dot + 1))}`;
    }
  }
  return `${base}/${encodeURIComponent(alias)}`;
}

const FieldForm = forwardRef<FieldFormHandle, FieldFormProps>(
  function FieldForm(
    {
      prefix,
      onSaved,
      showDelete = true,
      title,
      drift,
      includePath,
      inlineSaveBar = false,
    },
    ref,
  ) {
    const configDraft = useConfigDraft();
    const [entries, setEntries] = useState<ListResponseEntry[]>([]);
    const [draft, setDraft] = useState<Record<string, string>>({});
    const [comments, setComments] = useState<Record<string, string>>({});
    const [fieldErrors, setFieldErrors] = useState<
      Record<string, ConfigApiError>
    >({});
    const [topError, setTopError] = useState<string | null>(null);
    const [savedAt, setSavedAt] = useState<string | null>(null);
    const [loading, setLoading] = useState(true);
    const [saving, setSaving] = useState(false);
    const [schema, setSchema] = useState<Record<string, unknown> | undefined>(
      undefined,
    );
    const [filter, setFilter] = useState("");

    // Schema is whole-Config and ETag-cached server-side; fetch once per
    // session so every form row can resolve its `///` doc-comment helper
    // text via descriptionForPath without per-field round trips.
    useEffect(() => {
      let cancelled = false;
      void fetchConfigSchema().then((s) => {
        if (!cancelled) setSchema(s);
      });
      return () => {
        cancelled = true;
      };
    }, []);

    const reload = async () => {
      setLoading(true);
      setTopError(null);
      try {
        const resp = await listProps(prefix);
        setEntries(resp.entries);
        const seed: Record<string, string> = {};
        const commentSeed: Record<string, string> = {};
        for (const e of resp.entries) {
          const staged = configDraft.drafts[e.path];
          seed[e.path] = staged?.input ?? defaultInputValue(e);
          const stagedComment = configDraft.comments[e.path];
          if (stagedComment) commentSeed[e.path] = stagedComment;
        }
        setDraft(seed);
        setComments(commentSeed);
      } catch (e) {
        if (e instanceof ApiError) {
          setTopError(`[${e.envelope.code}] ${e.envelope.message}`);
        } else {
          setTopError(
            `Couldn't load fields for ${prefix}: ${e instanceof Error ? e.message : String(e)}`,
          );
        }
      } finally {
        setLoading(false);
      }
    };

    useEffect(() => {
      void reload();
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [prefix]);

    // Returns true when nothing was dirty or the save succeeded; false on
    // any error so callers (e.g. the wizard's Next button) can refuse to
    // advance past a broken state.
    const handleSave = async (): Promise<boolean> => {
      setSaving(true);
      setSavedAt(null);
      setTopError(null);
      setFieldErrors({});

      const ops: PatchOp[] = [];
      const parseStringArrayValue = (
        e: ListResponseEntry,
        raw: string,
      ): unknown => {
        let value: unknown = parseInput(e, raw);
        // For Option<Vec<String>>: empty rows = "no opinion" → send null
        // (clears the field). Mandatory Vec<String>: empty stays as [] (an
        // explicitly empty list, distinct from None).
        if (
          e.kind === "string-array" &&
          Array.isArray(value) &&
          value.length === 0 &&
          isOptionalArray(e.type_hint)
        ) {
          value = null;
        }
        return value;
      };
      for (const e of entries) {
        if (configDraft.tombstones.has(e.path)) {
          ops.push({ op: "remove", path: e.path });
          continue;
        }
        const raw = draft[e.path] ?? "";
        const original = defaultInputValue(e);
        const valueChanged =
          !(e.is_secret && raw.length === 0) && raw !== original;
        const comment = comments[e.path] ?? "";
        const commentChanged = comment.length > 0;
        if (!valueChanged && !commentChanged) continue;
        if (!valueChanged && commentChanged) {
          // Secret: route through the comment-only op so ciphertext is
          // preserved. Non-secret: round-trip via replace with the
          // existing value.
          if (e.is_secret) {
            ops.push({ op: "comment", path: e.path, comment });
          } else {
            ops.push({
              op: "replace",
              path: e.path,
              value: parseStringArrayValue(e, raw),
              comment,
            });
          }
          continue;
        }
        const op: PatchOp = {
          op: "replace",
          path: e.path,
          value: parseStringArrayValue(e, raw),
        };
        if (commentChanged) op.comment = comment;
        ops.push(op);
      }

      if (ops.length === 0) {
        setSaving(false);
        return true;
      }

      try {
        const resp = await patchConfig(ops);
        setSavedAt(`Saved ${resp.results.length} field(s).`);
        configDraft.discardSection(prefix);
        await reload();
        onSaved?.();
        return true;
      } catch (e) {
        if (e instanceof ApiError) {
          const env = e.envelope as ConfigApiError;
          if (env.path) {
            setFieldErrors({ [env.path]: env });
            setTopError(
              `Save failed: [${env.code}] ${env.message} (field: ${env.path})`,
            );
          } else {
            setTopError(`Save failed: [${env.code}] ${env.message}`);
          }
        } else {
          setTopError(
            `Save failed: ${e instanceof Error ? e.message : String(e)}`,
          );
        }
        return false;
      } finally {
        setSaving(false);
      }
    };

    useImperativeHandle(ref, () => ({
      flushSave: handleSave,
    }));

    // Stage a tombstone in the cross-section draft store rather than
    // POSTing DELETE immediately. The save bar (or the top banner's
    // Save-all) commits the removal as a JSON Patch `remove` op alongside
    // any other pending edits. Tombstoned rows render with a strikethrough
    // + Undo affordance via `tombstones` from the store.
    const handleDelete = (path: string) => {
      configDraft.stageTombstone(path);
    };

    const sortedEntries = useMemo(() => {
      // Stable order: `enabled` first (drives whether anything below it
      // matters), then first-run required fields, then secrets, then
      // alphabetical by short label. Curating these standard leaves keeps
      // surfaces from burying "model" or agent refs below advanced knobs.
      const isEnabledLeaf = (e: ListResponseEntry) =>
        e.path.endsWith(".enabled") || e.path === "enabled";
      return [...entries].sort((a, b) => {
        const ea = isEnabledLeaf(a);
        const eb = isEnabledLeaf(b);
        if (ea !== eb) return ea ? -1 : 1;
        const pa = setupFieldPriority(a);
        const pb = setupFieldPriority(b);
        if (pa !== pb) return pa - pb;
        if (a.is_secret !== b.is_secret) return a.is_secret ? -1 : 1;
        return fieldShortLabel(a).localeCompare(fieldShortLabel(b));
      });
    }, [entries]);

    // The entity-gate `enabled` bool gets hoisted into the title row as a
    // pill toggle. Hide it from the field list so it isn't editable in two
    // places at once.
    const enabledEntry = useMemo(
      () =>
        entries.find(
          (e) => e.path === `${prefix}.enabled` && e.kind === "bool",
        ) ?? null,
      [entries, prefix],
    );

    const visibleEntries = useMemo(() => {
      const base = enabledEntry
        ? sortedEntries.filter((e) => e.path !== enabledEntry.path)
        : sortedEntries;
      const filtered = includePath
        ? base.filter((e) => includePath(e.path))
        : base;
      if (!filter.trim()) return filtered;
      return fuzzyFilter(
        filtered,
        filter,
        (e) => `${fieldShortLabel(e)} ${e.path}`,
      );
    }, [sortedEntries, filter, includePath, enabledEntry]);

    // Count of fields whose draft value differs from the saved display value.
    // Drives the unsaved-changes counter in the sticky save bar. Must be
    // declared above the conditional render so hook count stays stable
    // across the loading / loaded transition (React error #310).
    const unsavedCount = useMemo(() => {
      let n = 0;
      for (const e of entries) {
        if (configDraft.tombstones.has(e.path)) {
          n += 1;
          continue;
        }
        const raw = draft[e.path] ?? "";
        const original = defaultInputValue(e);
        const valueChanged =
          !(e.is_secret && raw.length === 0) && raw !== original;
        const commentChanged = (comments[e.path] ?? "").length > 0;
        if (valueChanged || commentChanged) n += 1;
      }
      return n;
    }, [entries, draft, comments, configDraft.tombstones]);

    // Warn user before navigating away with unsaved changes.
    useEffect(() => {
      if (unsavedCount === 0) return;
      const handler = (e: BeforeUnloadEvent) => {
        e.preventDefault();
        e.returnValue = "";
      };
      window.addEventListener("beforeunload", handler);
      return () => window.removeEventListener("beforeunload", handler);
    }, [unsavedCount]);

    if (loading) {
      return (
        <div className="flex items-center justify-center py-12">
          <div
            className="h-8 w-8 border-2 rounded-full animate-spin"
            style={{
              borderColor: "var(--pc-border)",
              borderTopColor: "var(--pc-accent)",
            }}
          />
        </div>
      );
    }

    // When the parent's `includePath` predicate excludes every entry and the
    // user hasn't typed a filter, the section truly has nothing to configure
    // (e.g. `[tunnel]` with `tunnel_provider = "none"` has only the
    // discriminator field, which the parent excludes). Collapse the whole
    // form in that case so the operator doesn't see an empty "Foo settings"
    // header above a useless "No fields match." line.
    const trulyEmpty =
      !loading &&
      entries.length > 0 &&
      visibleEntries.length === 0 &&
      filter.trim().length === 0;

    if (trulyEmpty && !enabledEntry) {
      return null;
    }

    return (
      <div
        className={
          inlineSaveBar
            ? "flex flex-col"
            : "flex flex-col gap-4 pb-20 flex-1 min-h-full"
        }
      >
        {/* flex-1 + min-h-full stretches the form to fill the scroll area so
          the sticky save bar anchors to the viewport bottom even with a
          short field list. pb-20 reserves room so the last field isn't
          covered. inlineSaveBar drops both — the save bar is rendered
          tight against the last field as a footer of the embedding
          card. */}
        {(title || enabledEntry) && (
          <div className="flex items-center justify-between gap-3 flex-wrap">
            {title ? (
              <h2
                className="text-lg font-semibold"
                style={{ color: "var(--pc-text-primary)" }}
              >
                {title}
              </h2>
            ) : (
              <span />
            )}
            {enabledEntry && (
              <EntityEnabledToggle
                prefix={prefix}
                enabled={entryValue(enabledEntry) === "true"}
                onChange={(next) => {
                  setEntries((prev) =>
                    prev.map((e) =>
                      e.path === enabledEntry.path
                        ? { ...e, value: next, populated: true }
                        : e,
                    ),
                  );
                }}
              />
            )}
          </div>
        )}

        {visibleEntries.length > 1 && (
          <input
            type="text"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder={`Filter ${visibleEntries.length} fields — fuzzy match on name or path`}
            className="input-electric w-full px-3 py-2 text-sm"
            aria-label="Filter fields"
          />
        )}

        {entries.length === 0 ? (
          <div
            className="surface-panel p-6 text-center text-sm"
            style={{ color: "var(--pc-text-muted)" }}
          >
            No fields under{" "}
            <code style={{ color: "var(--pc-text-faint)" }}>{prefix}</code>.
          </div>
        ) : (
          <form
            className="surface-panel divide-y"
            style={{ borderColor: "var(--pc-border)" }}
            onSubmit={(e) => {
              e.preventDefault();
              void handleSave().catch(() => undefined);
            }}
          >
            {visibleEntries.length === 0 ? (
              <div
                className="px-4 py-6 text-sm text-center"
                style={{ color: "var(--pc-text-muted)" }}
              >
                {filter.trim().length === 0 ? (
                  <>No configurable settings for this selection.</>
                ) : (
                  <>
                    No fields match{" "}
                    <code style={{ color: "var(--pc-text-faint)" }}>
                      {filter}
                    </code>
                    .
                  </>
                )}
              </div>
            ) : null}
            {visibleEntries.map((f) => (
              <FieldRow
                key={f.path}
                entry={f}
                value={draft[f.path] ?? ""}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, [f.path]: v }));
                  const baseline = defaultInputValue(f);
                  if (v === baseline || (f.is_secret && v.length === 0)) {
                    configDraft.clearDraft(f.path);
                  } else {
                    let parsed: unknown;
                    try {
                      parsed = parseInput(f, v);
                    } catch {
                      parsed = v;
                    }
                    if (
                      f.kind === "string-array" &&
                      Array.isArray(parsed) &&
                      parsed.length === 0 &&
                      isOptionalArray(f.type_hint)
                    ) {
                      parsed = null;
                    }
                    configDraft.setDraft(f.path, v, parsed);
                  }
                }}
                comment={comments[f.path] ?? ""}
                onCommentChange={(v) => {
                  setComments((c) => ({ ...c, [f.path]: v }));
                  if (v.length > 0) {
                    configDraft.setComment(f.path, v);
                  } else {
                    configDraft.clearComment(f.path);
                  }
                }}
                tombstoned={configDraft.tombstones.has(f.path)}
                onUndoTombstone={() => configDraft.unstageTombstone(f.path)}
                error={fieldErrors[f.path]}
                onDelete={showDelete ? () => handleDelete(f.path) : undefined}
                description={descriptionForPath(schema, f.path)}
                elementProps={
                  f.kind === "object-array"
                    ? objectArrayElementProps(schema, f.path)
                    : null
                }
                drift={drift?.find((d) => d.path === f.path) ?? null}
              />
            ))}
          </form>
        )}

        {/* Sticky footer bar — pinned to the bottom of the scrolling form
          area so Save is always visible without scrolling. Status (unsaved
          count / save success / save error) renders inline next to the
          button so post-save feedback lands where the eye already is. */}
        {entries.length > 0 && (
          <div
            className={
              inlineSaveBar
                ? "px-3 py-2 mt-2 rounded-md"
                : "sticky bottom-0 left-0 right-0 -mx-6 px-6 py-3 border-t backdrop-blur z-10"
            }
            style={{
              borderColor: "var(--pc-border)",
              background: inlineSaveBar
                ? "var(--pc-bg-elevated)"
                : "color-mix(in srgb, var(--pc-bg-base) 88%, transparent)",
            }}
          >
            <div className="flex items-center justify-between gap-3">
              <div className="flex-1 min-w-0 text-sm">
                {topError ? (
                  <span style={{ color: "var(--color-status-error)" }}>
                    ⚠ {topError}
                  </span>
                ) : savedAt ? (
                  <span style={{ color: "var(--color-status-success)" }}>
                    ✓ {savedAt}
                  </span>
                ) : unsavedCount > 0 ? (
                  <span style={{ color: "var(--pc-text-secondary)" }}>
                    {unsavedCount} unsaved{" "}
                    {unsavedCount === 1 ? "change" : "changes"}
                  </span>
                ) : (
                  <span style={{ color: "var(--pc-text-faint)" }}>
                    No unsaved changes
                  </span>
                )}
              </div>
              <Button
                variant="primary"
                size="md"
                onClick={() => void handleSave()}
                disabled={saving || unsavedCount === 0}
                className="flex-shrink-0"
              >
                <Save className="h-4 w-4" />
                {saving ? "Saving…" : "Save"}
              </Button>
            </div>
          </div>
        )}
      </div>
    );
  },
);

export default FieldForm;

interface FieldRowProps {
  entry: ListResponseEntry;
  value: string;
  onChange: (v: string) => void;
  comment: string;
  onCommentChange: (v: string) => void;
  error: ConfigApiError | undefined;
  onDelete?: () => void;
  /** `///` doc comment resolved from the cached JSON Schema for this path. */
  description: string | null;
  /** Per-element property metadata for `kind === 'object-array'` fields. */
  elementProps?: ObjectArrayPropMeta[] | null;
  /** Drift entry for this path (in-memory ≠ on-disk). `null` when no drift. */
  drift: DriftEntry | null;
  /** `true` when the operator clicked the trash icon and the removal is
   *  staged (not yet committed). The row renders strikethrough with an
   *  Undo button replacing the input. */
  tombstoned?: boolean;
  /** Pulls the row out of tombstoned state. */
  onUndoTombstone?: () => void;
}

function FieldRow({
  entry,
  value,
  onChange,
  comment,
  onCommentChange,
  error,
  onDelete,
  description,
  elementProps,
  drift,
  tombstoned,
  onUndoTombstone,
}: FieldRowProps) {
  const renderer = rendererFor(entry);
  const requirement = setupRequirement(entry);
  // Display-only inline validation derived from this entry's schema metadata.
  // Pure read of `value` — it does not feed the save/PATCH path in any way.
  const validationMessage = validationHint(entry, value);
  // Suppress the local hint while a server-side error is already bound to this
  // field so the two don't stack; the authoritative server message wins.
  const showValidation = validationMessage !== null && !error;
  // The per-row "why?" comment is gated behind a reveal so it doesn't add a
  // permanent second input to every row. Open it automatically when a
  // comment value is already staged so the operator sees their own note.
  const [showComment, setShowComment] = useState(comment.length > 0);
  const [providerModels, setProviderModels] = useState<string[] | null>(null);
  const [modelsFetchFailed, setModelsFetchFailed] = useState(false);
  // Per-alias model field — `providers.models.<type>.<alias>.model`.
  const isProviderModelField = /^providers\.models\.[^.]+\.[^.]+\.model$/.test(
    entry.path,
  );
  // Skill-bundle directory field — `skill-bundles.<alias>.directory` (or
  // the legacy snake form `skill_bundles.<alias>.directory`). When unset
  // the runtime falls back to `<install>/shared/skills/<alias>/`; render
  // that resolved default as a placeholder so operators see the path
  // their bundle will actually use. Also gets a directory-picker button
  // wired to `GET /api/browse` (scoped to `<install>/shared/`).
  const skillBundleAlias = (() => {
    const m = entry.path.match(/^skill[-_]bundles\.([^.]+)\.directory$/);
    return m ? m[1] : null;
  })();
  // Any field that names a filesystem directory gets the shared/ picker.
  // Match on the dotted-path leaf: `directory`, `dir`, or `*_dir` / `*-dir`.
  // Secrets and `path` (overloaded for URL paths) deliberately excluded.
  const isDirectoryField = (() => {
    if (entry.is_secret) return false;
    const leaf = entry.path.split(".").pop() ?? "";
    return (
      leaf === "directory" ||
      leaf === "dir" ||
      leaf.endsWith("-dir") ||
      leaf.endsWith("_dir")
    );
  })();
  const showPicker = skillBundleAlias !== null || isDirectoryField;
  const [pickerOpen, setPickerOpen] = useState(false);

  // Tool-list enrichment — mirrors the per-alias `…model` field hook above.
  // Any `string-array` field whose dotted-path leaf is `allowed_tools`
  // (risk profiles, agents, …) renders the multi-select ToolPicker instead
  // of the one-value-per-line chip/text editor. Matched on the schema's own
  // path leaf, not a hardcoded section, so every `*.allowed_tools` list
  // gets the same picker. The draft value it reads/writes is the SAME
  // JSON-array string the ArrayFieldEditor uses, so parseInput() at save
  // time is untouched and the PATCH op is byte-identical.
  // Tool-list fields (allowed_tools / excluded_tools) get the catalog-backed
  // ToolPicker; detection is by leaf name, in the same field-semantic spirit
  // as the provider model-field datalist hook (no hardcoded section keys).
  const isAllowedToolsField =
    entry.kind === "string-array" &&
    (() => {
      const leaf = entry.path.split(".").pop();
      return leaf === "allowed_tools" || leaf === "excluded_tools";
    })();

  // Agent-form alias pickers. Each `agents.<alias>.<field>` row that
  // references another section's aliases (channels, model_provider, etc.)
  // renders as a picker over the live config rather than a free-text
  // input. The `system_prompt` field gets a textarea.
  const agentField = agentFieldKey(entry.path);
  const peerGroupField = peerGroupFieldKey(entry.path);
  // Schema path is kebab-case (matches prop_fields() emission).
  const isAgentSystemPrompt = agentField === "system-prompt";
  // Resolve which referenced section (if any) this single-value field points
  // at. Prefer the agent / peer-group field maps; otherwise fall back to a
  // leaf-name match (model_provider / risk_profile / runtime_profile /
  // memory_namespace). The leaf fallback runs LAST so it also covers those
  // reference fields when they appear UNDER `agents.*` (e.g.
  // agents.<alias>.risk_profile, which the agent field map doesn't list).
  // Falls back to free text if the target section's aliases can't be fetched.
  const agentSingleAliasKind: keyof AgentOptionsResponse | null =
    (agentField ? AGENT_SINGLE_ALIAS_FIELDS[agentField] : undefined) ??
    (peerGroupField
      ? PEER_GROUP_SINGLE_ALIAS_FIELDS[peerGroupField]
      : undefined) ??
    leafSingleAliasKind(entry.path) ??
    null;
  const agentMultiAliasKind: keyof AgentOptionsResponse | null = agentField
    ? (AGENT_MULTI_ALIAS_FIELDS[agentField] ?? null)
    : peerGroupField
      ? (PEER_GROUP_MULTI_ALIAS_FIELDS[peerGroupField] ?? null)
      : null;
  const agentNeedsOptions =
    agentSingleAliasKind !== null || agentMultiAliasKind !== null;
  const [agentOptions, setAgentOptions] = useState<AgentOptionsResponse | null>(
    null,
  );

  useEffect(() => {
    if (!isProviderModelField) return;
    void primeModelProviderCatalog();
    const [, , provider, alias] = entry.path.split('.');
    if (!provider || !alias) return;
    const cacheKey = `${provider}.${alias}`;
    const cached = modelsCache[cacheKey];
    if (cached) {
      setProviderModels(cached.models);
      setModelsFetchFailed(!cached.live && cached.models.length === 0);
      return;
    }
    void getCatalogModels(provider)
      .then((r) => {
        modelsCache[cacheKey] = { models: r.models, live: r.live, local: r.local };
        setProviderModels(r.models);
        setModelsFetchFailed(!r.live && r.models.length === 0);
      })
      .catch(() => {
        modelsCache[cacheKey] = {
          models: [],
          live: false,
          local: isLocalModelProviderName(provider),
        };
        setProviderModels([]);
        setModelsFetchFailed(true);
      });
  }, [isProviderModelField, entry.path]);

  // Refetch on every mount so newly-created channels / agents / bundles
  // (added in a different section) surface without a page reload.
  useEffect(() => {
    if (!agentNeedsOptions) return;
    let cancelled = false;
    void loadAgentOptions()
      .then((r) => {
        if (!cancelled) setAgentOptions(r);
      })
      .catch(() => {
        // Fail-open: leave options null so the field falls back to text.
      });
    return () => {
      cancelled = true;
    };
  }, [agentNeedsOptions]);

  if (tombstoned) {
    return (
      <div className="px-4 py-3 flex items-center justify-between gap-3 opacity-70">
        <div className="min-w-0 flex-1">
          <code
            className="text-xs font-mono line-through break-all"
            style={{ color: "var(--pc-text-muted)" }}
          >
            {entry.path}
          </code>
          <p
            className="text-xs mt-0.5"
            style={{ color: "var(--pc-text-muted)" }}
          >
            Staged for removal. Commits on Save.
          </p>
        </div>
        {onUndoTombstone && (
          <button
            type="button"
            onClick={onUndoTombstone}
            className="btn-secondary text-xs px-2 py-1 flex-shrink-0"
          >
            Undo
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="flex-1 min-w-0">
          <label
            className="block text-sm font-medium font-sans break-words"
            style={{ color: "var(--pc-text-primary)" }}
            htmlFor={entry.path}
            title={`${entry.path}${entry.type_hint ? ` — ${entry.type_hint}` : ""}`}
          >
            {humanizeFieldLabel(entry.path)}
            {requirement && (
              <Badge
                tone={requirementBadgeTone(requirement.tone)}
                className="ml-2 uppercase tracking-wide"
              >
                {requirement.label}
              </Badge>
            )}
            {entry.is_secret && (
              <span className="ml-2 text-xs font-sans text-pc-text-muted">
                🔒 {entry.populated ? "set" : "unset"}
              </span>
            )}
          </label>
          {/* Demoted: the raw schema path stays visible (faint, monospace) so
              the field is still unambiguously identifiable, but the humanized
              leaf above is now the primary label. */}
          <code
            className="block text-[11px] font-mono break-all mt-0.5"
            style={{ color: "var(--pc-text-faint)" }}
          >
            {entry.path}
          </code>
          {description && (
            <p
              className="text-xs mt-0.5"
              style={{ color: "var(--pc-text-secondary)" }}
            >
              {description}
            </p>
          )}
          {drift && <DriftDiff drift={drift} />}
        </div>
        {onDelete && (
          <button
            type="button"
            onClick={onDelete}
            title="Reset to default / unset"
            className="btn-icon flex-shrink-0"
          >
            <Trash2 className="h-4 w-4" />
          </button>
        )}
      </div>

      <div
        className={
          showValidation
            ? "mt-2 space-y-1.5 rounded-[var(--radius-md)] ring-1 ring-status-error p-1.5 -m-1.5"
            : "mt-2 space-y-1.5"
        }
        aria-invalid={showValidation || undefined}
      >
        {renderer === "bool" ? (
          <BoolSwitch
            id={entry.path}
            value={value === "true"}
            onChange={(next) => onChange(next ? "true" : "false")}
          />
        ) : renderer === "select" ? (
          <select
            id={entry.path}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm appearance-none cursor-pointer"
          >
            <option value="">—</option>
            {(entry.enum_variants ?? []).map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </select>
        ) : isProviderModelField &&
          providerModels !== null &&
          providerModels.length > 0 ? (
          <>
            <input
              id={entry.path}
              list={`models-${entry.path}`}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder="Pick from list or type a model name"
            />
            <datalist id={`models-${entry.path}`}>
              {providerModels.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
          </>
        ) : isProviderModelField && modelsFetchFailed ? (
          // Fetch failed — fall back to free text with explicit help.
          <>
            <input
              id={entry.path}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder="Type a model identifier (catalog unreachable)"
            />
            <p className="text-xs" style={{ color: "var(--pc-text-muted)" }}>
              Could not fetch model catalog for this provider. Type the
              identifier from your provider's docs (e.g.{" "}
              <code>{modelFallbackExample(entry.path)}</code>).
            </p>
          </>
        ) : isProviderModelField && providerModels === null ? (
          // Fetching catalog…
          <>
            <input
              id={entry.path}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder="Fetching models…"
              disabled
            />
            <p className="text-xs" style={{ color: "var(--pc-text-muted)" }}>
              Fetching available models from the provider's catalog…
            </p>
          </>
        ) : isAgentSystemPrompt ? (
          <textarea
            id={entry.path}
            rows={Math.max(4, Math.min(value.split("\n").length + 1, 14))}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
            placeholder="Optional. Prefer placing prose in agents/<alias>/AGENTS.md."
          />
        ) : agentSingleAliasKind && agentOptions ? (
          (agentOptions[agentSingleAliasKind] ?? []).length === 0 ? (
            <AgentEmptyAliasFallback fieldKind={agentSingleAliasKind} />
          ) : (
            <div className="flex items-center gap-2">
              <select
                id={entry.path}
                value={value}
                onChange={(e) => onChange(e.target.value)}
                className="input-electric flex-1 px-3 py-2 text-sm appearance-none cursor-pointer"
              >
                <option value="">— (none)</option>
                {(agentOptions[agentSingleAliasKind] ?? []).map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </select>
              {value && (
                <Link
                  to={agentAliasJumpPath(agentSingleAliasKind, value)}
                  title={`Edit ${value} in its source section`}
                  className="btn-icon flex-shrink-0"
                >
                  <ExternalLink className="h-4 w-4" />
                </Link>
              )}
            </div>
          )
        ) : agentMultiAliasKind && agentOptions ? (
          (agentOptions[agentMultiAliasKind] ?? []).length === 0 ? (
            <AgentEmptyAliasFallback fieldKind={agentMultiAliasKind} />
          ) : (
            <ArrayFieldEditor
              inputId={entry.path}
              value={value}
              onChange={onChange}
              isOptional={isOptionalArray(entry.type_hint)}
              suggestions={agentOptions[agentMultiAliasKind]}
            />
          )
        ) : isAllowedToolsField ? (
          // `allowed_tools` lists get the catalog-backed multi-select. We
          // bridge to the picker using the SAME draft representation every
          // other string-array field uses: the value is the JSON-array
          // string, so `parseArrayRows` decodes it to `string[]` for the
          // picker and `JSON.stringify` re-encodes the picker's output —
          // identical to ArrayFieldEditor.writeRows. The save path
          // (parseInput → parseStringArrayValue) never sees a difference.
          <ToolPicker
            id={entry.path}
            value={parseArrayRows(value)}
            onChange={(next) => onChange(JSON.stringify(next))}
          />
        ) : renderer === "array" ? (
          <ArrayFieldEditor
            inputId={entry.path}
            value={value}
            onChange={onChange}
            isOptional={isOptionalArray(entry.type_hint)}
          />
        ) : renderer === "object-array" ? (
          <ObjectArrayEditor
            inputId={entry.path}
            value={value}
            onChange={onChange}
            elementProps={elementProps ?? null}
          />
        ) : renderer === "number" ? (
          <input
            id={entry.path}
            type="number"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm"
          />
        ) : showPicker ? (
          <div className="relative">
            <div className="flex items-center gap-2">
              <input
                id={entry.path}
                type="text"
                value={value}
                onChange={(e) => onChange(e.target.value)}
                className="input-electric flex-1 px-3 py-2 text-sm"
                placeholder={
                  skillBundleAlias
                    ? `shared/skills/${skillBundleAlias}/ (default — leave empty)`
                    : "shared/… (leave empty to use the schema default)"
                }
              />
              <button
                type="button"
                data-dirpicker-trigger
                onClick={() => setPickerOpen((open) => !open)}
                className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2 flex-shrink-0"
                title="Browse shared/ for a directory"
                aria-expanded={pickerOpen}
              >
                <FolderOpen className="h-4 w-4" />
                Browse
              </button>
            </div>
            {pickerOpen && (
              <div className="absolute z-20 right-0 mt-2 w-[min(28rem,calc(100vw-3rem))]">
                <DirectoryPicker
                  value={value}
                  onSelect={(path) => {
                    onChange(path);
                    setPickerOpen(false);
                  }}
                  onClose={() => setPickerOpen(false)}
                />
              </div>
            )}
          </div>
        ) : (
          <input
            id={entry.path}
            type={renderer === "secret" ? "password" : "text"}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm"
            placeholder={
              renderer === "secret"
                ? entry.populated
                  ? "Leave blank to keep current value"
                  : "Enter value"
                : ""
            }
          />
        )}

        {showValidation && (
          <p className="text-xs text-status-error" role="alert">
            {validationMessage}
          </p>
        )}

        {showComment ? (
          <div className="flex items-center gap-1.5">
            <input
              type="text"
              value={comment}
              autoFocus={comment.length === 0}
              onChange={(e) => onCommentChange(e.target.value)}
              placeholder={t("cfg.field.commentPlaceholder")}
              className="input-electric flex-1 px-3 py-1.5 text-xs text-pc-text-secondary"
            />
            <button
              type="button"
              onClick={() => {
                onCommentChange("");
                setShowComment(false);
              }}
              title={t("cfg.field.commentHide")}
              aria-label={t("cfg.field.commentHide")}
              className="btn-icon flex-shrink-0"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => setShowComment(true)}
            className="inline-flex items-center gap-1 text-xs text-pc-text-faint hover:text-pc-text-secondary transition-colors"
          >
            <MessageSquarePlus className="h-3.5 w-3.5" />
            {t("cfg.field.commentAdd")}
          </button>
        )}

        {error && (
          <p
            className="mt-1 text-sm"
            style={{ color: "var(--color-status-error)" }}
          >
            <span className="font-mono text-xs">{error.code}</span>:{" "}
            {error.message}
          </p>
        )}
      </div>
    </div>
  );
}

interface ArrayFieldEditorProps {
  inputId: string;
  value: string;
  onChange: (next: string) => void;
  isOptional: boolean;
  /** When provided, each chip input gets an attached `<datalist>` so
   *  users can pick from a known list of valid values (e.g. channel
   *  aliases on an agent's `channels` field) instead of typing free text. */
  suggestions?: string[];
}

// Per-row chip editor for `Vec<String>` / `Option<Vec<String>>` fields with
// a "Rows / Text" toggle. Both views share the same underlying value (a
// JSON array string) so toggling preserves edits. Trim + drop-empty runs
// at save time in `parseArrayDraft`, not on every keystroke — typing a
// space inside a chip shouldn't truncate the entry.
function ArrayFieldEditor({
  inputId,
  value,
  onChange,
  isOptional,
  suggestions,
}: ArrayFieldEditorProps) {
  const [mode, setMode] = useState<"rows" | "text">("rows");
  const rows = useMemo(() => parseArrayRows(value), [value]);

  const writeRows = (next: string[]) => {
    onChange(JSON.stringify(next));
  };

  const setRow = (index: number, next: string) => {
    writeRows(rows.map((r, i) => (i === index ? next : r)));
  };

  const removeRow = (index: number) => {
    writeRows(rows.filter((_, i) => i !== index));
  };

  const addRow = () => {
    writeRows([...rows, ""]);
  };

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs" style={{ color: "var(--pc-text-faint)" }}>
          {rows.length} {rows.length === 1 ? "entry" : "entries"}
          {isOptional && rows.length === 0 ? " — saves as null" : null}
        </span>
        <div
          className="inline-flex rounded-md overflow-hidden border text-xs"
          style={{ borderColor: "var(--pc-border)" }}
        >
          <button
            type="button"
            onClick={() => setMode("rows")}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background:
                mode === "rows"
                  ? "var(--pc-bg-surface-elevated)"
                  : "transparent",
              color:
                mode === "rows"
                  ? "var(--pc-text-primary)"
                  : "var(--pc-text-muted)",
            }}
            aria-pressed={mode === "rows"}
          >
            <ListIcon className="h-3 w-3" /> Rows
          </button>
          <button
            type="button"
            onClick={() => setMode("text")}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background:
                mode === "text"
                  ? "var(--pc-bg-surface-elevated)"
                  : "transparent",
              color:
                mode === "text"
                  ? "var(--pc-text-primary)"
                  : "var(--pc-text-muted)",
            }}
            aria-pressed={mode === "text"}
          >
            <TypeIcon className="h-3 w-3" /> Text
          </button>
        </div>
      </div>

      {mode === "rows" ? (
        <>
          {rows.length === 0 ? (
            <p
              className="text-xs italic px-1 py-2"
              style={{ color: "var(--pc-text-faint)" }}
            >
              No entries. Click "+ Add" to add one.
            </p>
          ) : (
            <ul className="space-y-1.5" id={inputId}>
              {rows.map((row, i) => (
                <li key={i} className="flex items-center gap-2">
                  <input
                    type="text"
                    value={row}
                    onChange={(e) => setRow(i, e.target.value)}
                    className="input-electric flex-1 px-3 py-1.5 text-sm"
                    placeholder={
                      suggestions && suggestions.length > 0
                        ? "pick from list"
                        : "empty"
                    }
                    list={suggestions ? `${inputId}-suggestions` : undefined}
                  />
                  <button
                    type="button"
                    onClick={() => removeRow(i)}
                    title="Remove this entry"
                    className="btn-icon flex-shrink-0"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                </li>
              ))}
            </ul>
          )}
          {suggestions && (
            <datalist id={`${inputId}-suggestions`}>
              {suggestions.map((s) => (
                <option key={s} value={s} />
              ))}
            </datalist>
          )}
          <button
            type="button"
            onClick={addRow}
            className="btn-secondary text-xs px-3 py-1.5 inline-flex items-center gap-1"
          >
            <Plus className="h-3 w-3" /> Add
          </button>
        </>
      ) : (
        <textarea
          id={inputId}
          rows={Math.max(3, Math.min(rows.length + 1, 10))}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
          placeholder='["value1", "value2"]'
        />
      )}
    </div>
  );
}

interface ObjectArrayEditorProps {
  inputId: string;
  /** JSON-array string of objects. Empty/`<unset>`/invalid JSON normalize to `[]`. */
  value: string;
  onChange: (next: string) => void;
  /** Per-property metadata for the element type, walked from the JSON Schema.
   *  `null` when the schema isn't loaded yet or the element shape can't be
   *  resolved — falls back to a raw JSON textarea. */
  elementProps: ObjectArrayPropMeta[] | null;
}

// Per-row form editor for `Vec<T>` of structs (e.g. `mcp.servers`).
// Parses the JSON-array value, renders one row per element with per-property
// inputs derived from the JSON Schema, and serializes back to JSON on save.
// Schema v3 / #5947 will migrate the load-bearing Vecs to `HashMap<String, T>`
// keyed tables; this editor is the bridge so the dashboard doesn't have to
// wait on that to surface MCP servers / peripheral boards / etc.
function ObjectArrayEditor({
  inputId,
  value,
  onChange,
  elementProps,
}: ObjectArrayEditorProps) {
  const rows = useMemo<Record<string, unknown>[]>(() => {
    try {
      const parsed = JSON.parse(value || "[]");
      if (Array.isArray(parsed)) {
        return parsed.filter(
          (r): r is Record<string, unknown> =>
            typeof r === "object" && r !== null,
        );
      }
    } catch {
      /* fall through */
    }
    return [];
  }, [value]);

  const writeRows = (next: Record<string, unknown>[]) => {
    onChange(JSON.stringify(next));
  };

  const setField = (rowIdx: number, key: string, raw: unknown) => {
    const next = rows.map((r, i) => (i === rowIdx ? { ...r, [key]: raw } : r));
    writeRows(next);
  };

  const removeRow = (rowIdx: number) => {
    writeRows(rows.filter((_, i) => i !== rowIdx));
  };

  const addRow = () => {
    // Seed required-string keys with empty strings so the row renders an
    // empty input rather than nothing.
    const seed: Record<string, unknown> = {};
    if (elementProps) {
      for (const p of elementProps) {
        if (p.kind === "string" && !p.optional) seed[p.key] = "";
      }
    }
    writeRows([...rows, seed]);
  };

  // Schema not loaded or unresolvable: degrade to a raw JSON textarea so
  // the field is still editable. Visually distinct so users see why.
  if (!elementProps || elementProps.length === 0) {
    return (
      <div className="space-y-1.5">
        <p className="text-xs" style={{ color: "var(--pc-text-muted)" }}>
          Element shape unavailable from schema; edit raw JSON below.
        </p>
        <textarea
          id={inputId}
          rows={Math.max(4, Math.min(rows.length * 4 + 2, 16))}
          value={value || "[]"}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
          placeholder='[{"key": "value"}]'
        />
      </div>
    );
  }

  return (
    <div className="space-y-2" id={inputId}>
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs" style={{ color: "var(--pc-text-faint)" }}>
          {rows.length} {rows.length === 1 ? "entry" : "entries"}
        </span>
        <button
          type="button"
          onClick={addRow}
          className="btn-secondary text-xs px-3 py-1.5 inline-flex items-center gap-1"
        >
          <Plus className="h-3 w-3" /> Add
        </button>
      </div>
      {rows.length === 0 ? (
        <p
          className="text-xs italic px-1 py-2"
          style={{ color: "var(--pc-text-faint)" }}
        >
          No entries. Click "+ Add" to create one.
        </p>
      ) : (
        <ul className="space-y-3">
          {rows.map((row, rowIdx) => (
            <li
              key={rowIdx}
              className="rounded-md border p-3 space-y-2"
              style={{
                borderColor: "var(--pc-border)",
                background: "var(--pc-bg-base)",
              }}
            >
              <div className="flex items-center justify-between">
                <span
                  className="text-xs font-mono"
                  style={{ color: "var(--pc-text-faint)" }}
                >
                  [{rowIdx}]
                  {typeof row.name === "string" && row.name.length > 0 && (
                    <span
                      className="ml-2"
                      style={{ color: "var(--pc-text-secondary)" }}
                    >
                      {row.name}
                    </span>
                  )}
                </span>
                <button
                  type="button"
                  onClick={() => removeRow(rowIdx)}
                  title="Remove this entry"
                  className="btn-icon"
                >
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>
              {elementProps.map((p) => (
                <ObjectArrayField
                  key={p.key}
                  meta={p}
                  rawValue={row[p.key]}
                  onChange={(v) => setField(rowIdx, p.key, v)}
                />
              ))}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ObjectArrayField({
  meta,
  rawValue,
  onChange,
}: {
  meta: ObjectArrayPropMeta;
  rawValue: unknown;
  onChange: (next: unknown) => void;
}) {
  const display = (() => {
    if (rawValue === null || rawValue === undefined) return "";
    if (typeof rawValue === "string") return rawValue;
    if (typeof rawValue === "number" || typeof rawValue === "boolean")
      return String(rawValue);
    return JSON.stringify(rawValue);
  })();
  return (
    <div>
      <label
        className="block text-xs font-mono"
        style={{ color: "var(--pc-text-secondary)" }}
      >
        {meta.key}
        {meta.optional && (
          <span
            className="ml-1.5 text-[10px]"
            style={{ color: "var(--pc-text-faint)" }}
          >
            optional
          </span>
        )}
      </label>
      {meta.description && (
        <p
          className="text-[11px] mt-0.5"
          style={{ color: "var(--pc-text-muted)" }}
        >
          {meta.description}
        </p>
      )}
      {meta.kind === "bool" ? (
        <div className="mt-1">
          <BoolSwitch
            value={display === "true"}
            onChange={(next) => onChange(next)}
          />
        </div>
      ) : meta.kind === "enum" && meta.enumVariants ? (
        <select
          value={display}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-2 py-1 mt-1 text-sm appearance-none cursor-pointer"
        >
          <option value="">—</option>
          {meta.enumVariants.map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
      ) : meta.kind === "integer" || meta.kind === "float" ? (
        <input
          type="number"
          value={display}
          onChange={(e) => {
            const n = Number(e.target.value);
            onChange(Number.isNaN(n) || e.target.value === "" ? null : n);
          }}
          className="input-electric w-full px-2 py-1 mt-1 text-sm"
        />
      ) : meta.kind === "string-array" ? (
        // Same chip + text-mode editor the top-level FieldForm uses for
        // Vec<String> fields. Bridges its JSON-string contract to/from
        // the row object's array-typed value: rows-mode edits emit valid
        // JSON arrays we can parse into the row property; mid-edit text
        // mode stores the in-progress string verbatim, deferring shape
        // validation to save time (same way the top-level path does).
        <ArrayFieldEditor
          inputId={`${meta.key}`}
          value={
            Array.isArray(rawValue)
              ? JSON.stringify(rawValue)
              : typeof rawValue === "string"
                ? rawValue
                : "[]"
          }
          onChange={(s) => {
            try {
              const parsed = JSON.parse(s);
              if (Array.isArray(parsed)) {
                onChange(parsed);
                return;
              }
            } catch {
              /* fall through */
            }
            onChange(s);
          }}
          isOptional={meta.optional}
        />
      ) : meta.kind === "object" ? (
        <KeyValueChipEditor
          pairs={
            typeof rawValue === "object" &&
            rawValue !== null &&
            !Array.isArray(rawValue)
              ? Object.entries(rawValue as Record<string, unknown>).map(
                  ([k, v]) =>
                    [k, typeof v === "string" ? v : JSON.stringify(v)] as [
                      string,
                      string,
                    ],
                )
              : []
          }
          onChange={(pairs) => onChange(Object.fromEntries(pairs))}
        />
      ) : (
        <input
          type="text"
          value={display}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-2 py-1 mt-1 text-sm"
        />
      )}
    </div>
  );
}

// Compact key-value chip editor for `HashMap<String, String>`
// properties inside an object-array row (e.g. `mcp.servers[i].env`,
// `headers`). Mirrors `ArrayFieldEditor`'s Rows / Text toggle so a
// power user can hand-edit the JSON object form when chips get
// unwieldy. Mid-edit invalid JSON is preserved in the textarea (no
// input fight); pairs only update when the buffer parses to an object.
function KeyValueChipEditor({
  pairs,
  onChange,
}: {
  pairs: [string, string][];
  onChange: (next: [string, string][]) => void;
}) {
  const [mode, setMode] = useState<"rows" | "text">("rows");
  // Local textarea buffer — only consulted in `text` mode. Reset when
  // the user re-enters text mode so the buffer reflects current pairs;
  // cleared when leaving text mode so re-entry shows fresh JSON.
  const [textDraft, setTextDraft] = useState<string | null>(null);

  const setKey = (i: number, k: string) => {
    onChange(pairs.map((p, idx) => (idx === i ? [k, p[1]] : p)));
  };
  const setValue = (i: number, v: string) => {
    onChange(pairs.map((p, idx) => (idx === i ? [p[0], v] : p)));
  };
  const removeAt = (i: number) => {
    onChange(pairs.filter((_, idx) => idx !== i));
  };

  const switchToRows = () => {
    setTextDraft(null);
    setMode("rows");
  };
  const switchToText = () => {
    setTextDraft(JSON.stringify(Object.fromEntries(pairs), null, 2));
    setMode("text");
  };

  return (
    <div className="space-y-1.5 mt-1">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs" style={{ color: "var(--pc-text-faint)" }}>
          {pairs.length} {pairs.length === 1 ? "entry" : "entries"}
        </span>
        <div
          className="inline-flex rounded-md overflow-hidden border text-xs"
          style={{ borderColor: "var(--pc-border)" }}
        >
          <button
            type="button"
            onClick={switchToRows}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background:
                mode === "rows"
                  ? "var(--pc-bg-surface-elevated)"
                  : "transparent",
              color:
                mode === "rows"
                  ? "var(--pc-text-primary)"
                  : "var(--pc-text-muted)",
            }}
            aria-pressed={mode === "rows"}
          >
            <ListIcon className="h-3 w-3" /> Rows
          </button>
          <button
            type="button"
            onClick={switchToText}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background:
                mode === "text"
                  ? "var(--pc-bg-surface-elevated)"
                  : "transparent",
              color:
                mode === "text"
                  ? "var(--pc-text-primary)"
                  : "var(--pc-text-muted)",
            }}
            aria-pressed={mode === "text"}
          >
            <TypeIcon className="h-3 w-3" /> Text
          </button>
        </div>
      </div>

      {mode === "text" ? (
        <textarea
          rows={Math.max(3, Math.min(pairs.length + 2, 10))}
          value={
            textDraft ?? JSON.stringify(Object.fromEntries(pairs), null, 2)
          }
          onChange={(e) => {
            const v = e.target.value;
            setTextDraft(v);
            try {
              const parsed = JSON.parse(v);
              if (
                parsed &&
                typeof parsed === "object" &&
                !Array.isArray(parsed)
              ) {
                onChange(
                  Object.entries(parsed as Record<string, unknown>).map(
                    ([k, val]) =>
                      [
                        k,
                        typeof val === "string" ? val : JSON.stringify(val),
                      ] as [string, string],
                  ),
                );
              }
            } catch {
              /* keep textDraft until valid JSON */
            }
          }}
          className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
          placeholder='{"key": "value"}'
        />
      ) : (
        <>
          {pairs.length === 0 ? (
            <p
              className="text-[11px] italic"
              style={{ color: "var(--pc-text-faint)" }}
            >
              No entries.
            </p>
          ) : (
            <ul className="space-y-1">
              {pairs.map(([k, v], i) => (
                <li key={i} className="flex items-center gap-2">
                  <input
                    type="text"
                    value={k}
                    onChange={(e) => setKey(i, e.target.value)}
                    className="input-electric flex-1 px-2 py-1 text-sm font-mono"
                    placeholder="key"
                  />
                  <span style={{ color: "var(--pc-text-faint)" }}>=</span>
                  <input
                    type="text"
                    value={v}
                    onChange={(e) => setValue(i, e.target.value)}
                    className="input-electric flex-1 px-2 py-1 text-sm"
                    placeholder="value"
                  />
                  <button
                    type="button"
                    onClick={() => removeAt(i)}
                    title="Remove this entry"
                    className="btn-icon flex-shrink-0"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={() => onChange([...pairs, ["", ""]])}
            className="btn-secondary text-xs px-2.5 py-1 inline-flex items-center gap-1"
          >
            <Plus className="h-3 w-3" /> Add
          </button>
        </>
      )}
    </div>
  );
}

// Per-field drift indicator: small inline pill showing in-memory vs
// on-disk values side by side. Secret-marked paths surface only the
// fact of drift — values never leave the server (server-side hash
// compare in `compute_drift`).
function DriftDiff({ drift }: { drift: DriftEntry }) {
  if (drift.secret) {
    return (
      <p
        className="text-xs mt-1 inline-flex items-center gap-1"
        style={{ color: "var(--color-status-warning, #f5b400)" }}
      >
        ⚠ secret value differs from on-disk
      </p>
    );
  }
  const inMem = formatDriftValue(drift.in_memory_value);
  const onDisk = formatDriftValue(drift.on_disk_value);
  return (
    <div
      className="text-xs mt-1 flex flex-wrap gap-x-3 gap-y-0.5"
      style={{ color: "var(--color-status-warning, #f5b400)" }}
    >
      <span>
        in-memory:{" "}
        <code style={{ color: "var(--pc-text-secondary)" }}>{inMem}</code>
      </span>
      <span>
        on-disk:{" "}
        <code style={{ color: "var(--pc-text-secondary)" }}>{onDisk}</code>
      </span>
    </div>
  );
}

function formatDriftValue(value: unknown): string {
  if (value === null || value === undefined) return "<unset>";
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}
