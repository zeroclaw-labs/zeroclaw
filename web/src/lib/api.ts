import type {
  StatusResponse,
  ToolSpec,
  CronJob,
  CronRun,
  Integration,
  DiagResult,
  MemoryEntry,
  CostSummary,
  CliTool,
  HealthSnapshot,
  Session,
  ChannelDetail,
  SessionMessagesResponse,
  TuiEntry,
} from "../types/api";
import { clearToken, getToken, setToken } from "./auth";
import { apiOrigin, basePath } from "./basePath";

// ---------------------------------------------------------------------------
// Base fetch wrapper
// ---------------------------------------------------------------------------

export class UnauthorizedError extends Error {
  constructor() {
    super("Unauthorized");
    this.name = "UnauthorizedError";
  }
}

/**
 * Thrown when the gateway returns a structured `ConfigApiError` response body.
 * Carries the parsed envelope directly so callers can dispatch on `.code`
 * instead of regex-matching the message string. Also includes the HTTP
 * status for callers that care about it (typically just for Retry-After
 * 429 logic; the code is the authoritative dispatch key).
 */
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly envelope: {
      code: string;
      message: string;
      path?: string;
      op_index?: number;
    },
  ) {
    super(`[${envelope.code}] ${envelope.message}`);
    this.name = "ApiError";
  }
}

export async function apiFetch<T = unknown>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const token = getToken();
  const headers = new Headers(options.headers);

  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }

  if (
    options.body &&
    typeof options.body === "string" &&
    !headers.has("Content-Type")
  ) {
    headers.set("Content-Type", "application/json");
  }

  const response = await fetch(`${apiOrigin}${basePath}${path}`, {
    ...options,
    headers,
  });

  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event("zeroclaw-unauthorized"));
    throw new UnauthorizedError();
  }

  if (!response.ok) {
    // Try to parse a structured ConfigApiError envelope. Falls back to a
    // plain Error when the body is non-JSON or doesn't match the shape.
    // Centralises the parsing so callers (including the Quickstart flow)
    // never have to regex-match `error.message` to recover the structured
    // code — they just `instanceof ApiError` and read `.envelope.code`.
    const text = await response.text().catch(() => "");
    if (text) {
      try {
        const parsed = JSON.parse(text);
        if (
          parsed &&
          typeof parsed === "object" &&
          typeof parsed.code === "string" &&
          typeof parsed.message === "string"
        ) {
          throw new ApiError(response.status, parsed);
        }
      } catch (e) {
        if (e instanceof ApiError) throw e;
        // JSON.parse failure → fall through to the plain Error path.
      }
    }
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }

  // Some endpoints may return 204 No Content
  if (response.status === 204) {
    return undefined as unknown as T;
  }

  return response.json() as Promise<T>;
}

function unwrapField<T>(value: T | Record<string, T>, key: string): T {
  if (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    key in value
  ) {
    const unwrapped = (value as Record<string, T | undefined>)[key];
    if (unwrapped !== undefined) {
      return unwrapped;
    }
  }
  return value as T;
}

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

export async function pair(code: string): Promise<{ token: string }> {
  const response = await fetch(`${basePath}/pair`, {
    method: "POST",
    headers: { "X-Pairing-Code": code },
  });

  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw new Error(
      `Pairing failed (${response.status}): ${text || response.statusText}`,
    );
  }

  const data = (await response.json()) as { token: string };
  setToken(data.token);
  return data;
}

export async function getAdminPairCode(): Promise<{
  pairing_code: string | null;
  pairing_required: boolean;
}> {
  // Use the public /pair/code endpoint which works in Docker and remote environments
  // (no localhost restriction). Falls back to the admin endpoint for backward compat.
  const publicResp = await fetch(`${basePath}/pair/code`);
  if (publicResp.ok) {
    return publicResp.json() as Promise<{
      pairing_code: string | null;
      pairing_required: boolean;
    }>;
  }

  const response = await fetch("/admin/paircode");
  if (!response.ok) {
    throw new Error(`Failed to fetch pairing code (${response.status})`);
  }
  return response.json() as Promise<{
    pairing_code: string | null;
    pairing_required: boolean;
  }>;
}

// ---------------------------------------------------------------------------
// Public health (no auth required)
// ---------------------------------------------------------------------------

export async function getPublicHealth(): Promise<{
  require_pairing: boolean;
  paired: boolean;
}> {
  const response = await fetch(`${basePath}/health`);
  if (!response.ok) {
    throw new Error(`Health check failed (${response.status})`);
  }
  return response.json() as Promise<{
    require_pairing: boolean;
    paired: boolean;
  }>;
}

// ---------------------------------------------------------------------------
// Status / Health
// ---------------------------------------------------------------------------

export function getStatus(): Promise<StatusResponse> {
  return apiFetch<StatusResponse>("/api/status");
}

export function getHealth(): Promise<HealthSnapshot> {
  return apiFetch<HealthSnapshot | { health: HealthSnapshot }>(
    "/api/health",
  ).then((data) => unwrapField(data, "health"));
}

// ---------------------------------------------------------------------------
// TUIs
// ---------------------------------------------------------------------------

export function getTuis(): Promise<TuiEntry[]> {
  return apiFetch<TuiEntry[] | { tuis: TuiEntry[] }>("/api/tuis").then(
    (data) => {
      const result = unwrapField(data, "tuis");
      return Array.isArray(result) ? result : [];
    },
  );
}

// ---------------------------------------------------------------------------
// Config — per-property CRUD (issue #6175). Whole-file getConfig/putConfig
// removed; the gateway no longer exposes those endpoints.
// ---------------------------------------------------------------------------

/**
 * One non-fatal validation warning surfaced after a successful save —
 * config that loads and validates structurally but will fail at agent
 * runtime because of a logical inconsistency (e.g. `providers.fallback`
 * referencing a key not present in `providers.models`). Matches the
 * `tracing::warn!` signal the CLI shows on stderr; surfaced structured so
 * the dashboard can render it next to the offending field.
 */
export interface ValidationWarning {
  /** Stable machine-readable identifier (e.g. `'dangling_provider_fallback'`). */
  code: string;
  /** Human-readable description suitable for direct display. */
  message: string;
  /** Dotted property path the warning concerns (e.g. `'providers.fallback'`). */
  path: string;
}

export interface PropResponse {
  path: string;
  value?: unknown;
  populated?: boolean;
  /**
   * Non-fatal validation warnings against the current config state. Empty
   * (or absent) when nothing is flagged.
   */
  warnings?: ValidationWarning[];
}

export interface ListResponseEntry {
  path: string;
  category: string;
  /**
   * Stable kind tag from the gateway: 'string' | 'bool' | 'integer' | 'float'
   * | 'enum' | 'string-array'. Use this — not value-sniffing — to choose the
   * right input renderer.
   */
  kind: string;
  /** Rust type signature for tooltips, e.g. 'Option<String>' or 'Vec<String>'. */
  type_hint: string;
  value?: unknown;
  populated: boolean;
  is_secret: boolean;
  /** Variants for `kind === 'enum'` fields (drives <select> options). */
  enum_variants?: string[];
  section?: string;
  /** Tab grouping from `ConfigTab` enum. Absent when `ConfigTab::None`. */
  tab?: string;
}

export interface DriftEntry {
  path: string;
  secret?: boolean;
  drifted: boolean;
  in_memory_value?: unknown;
  on_disk_value?: unknown;
}

export interface ListResponse {
  entries: ListResponseEntry[];
  drifted?: DriftEntry[];
}

export interface PatchOp {
  op: "add" | "replace" | "remove" | "test" | "comment";
  path: string;
  value?: unknown;
  comment?: string;
}

export interface PatchOpResult {
  op: string;
  path: string;
  value?: unknown;
  populated?: boolean;
  /** Echoed back from the request so clients can confirm the comment was written. */
  comment?: string;
}

export interface PatchResponse {
  saved: boolean;
  results: PatchOpResult[];
  /**
   * Non-fatal validation warnings against the post-save config state.
   * Empty (or absent) when nothing is flagged.
   */
  warnings?: ValidationWarning[];
}

export interface ConfigApiError {
  code: string;
  message: string;
  path?: string;
  op_index?: number;
}

export function getProp(path: string): Promise<PropResponse> {
  return apiFetch<PropResponse>(
    `/api/config/prop?path=${encodeURIComponent(path)}`,
  );
}

export function putProp(
  path: string,
  value: unknown,
  comment?: string,
): Promise<PropResponse> {
  return apiFetch<PropResponse>("/api/config/prop", {
    method: "PUT",
    body: JSON.stringify({ path, value, comment }),
  });
}

export function deleteProp(path: string): Promise<PropResponse> {
  return apiFetch<PropResponse>(
    `/api/config/prop?path=${encodeURIComponent(path)}`,
    {
      method: "DELETE",
    },
  );
}

export function listProps(prefix?: string): Promise<ListResponse> {
  const q = prefix ? `?prefix=${encodeURIComponent(prefix)}` : "";
  return apiFetch<ListResponse>(`/api/config/list${q}`);
}

export function patchConfig(ops: PatchOp[]): Promise<PatchResponse> {
  return apiFetch<PatchResponse>("/api/config", {
    method: "PATCH",
    body: JSON.stringify(ops),
  });
}

export function initSection(
  section?: string,
): Promise<{ initialized: string[] }> {
  const q = section ? `?section=${encodeURIComponent(section)}` : "";
  return apiFetch<{ initialized: string[] }>(`/api/config/init${q}`, {
    method: "POST",
  });
}

export function getDrift(): Promise<{ drifted: DriftEntry[] }> {
  return apiFetch<{ drifted: DriftEntry[] }>("/api/config/drift");
}

export function getReloadStatus(): Promise<{ pending_reload: boolean }> {
  return apiFetch<{ pending_reload: boolean }>("/api/config/reload-status");
}

export function getOpenApiSchema(): Promise<unknown> {
  return apiFetch<unknown>("/api/openapi.json");
}

// ── Personality files ────────────────────────────────────────────────

export interface PersonalityIndexEntry {
  filename: string;
  exists: boolean;
  size: number;
  mtime_ms: number | null;
}

export interface PersonalityIndex {
  files: PersonalityIndexEntry[];
  max_chars: number;
}

export interface PersonalityFile {
  filename: string;
  content: string;
  exists: boolean;
  truncated: boolean;
  mtime_ms: number | null;
}

export interface PersonalityPutResult {
  bytes_written: number;
  mtime_ms: number | null;
}

export interface PersonalityConflict {
  error: "personality_disk_drift";
  filename: string;
  current_content: string;
  current_mtime_ms: number | null;
}

function agentQuery(agent?: string): string {
  return agent ? `?agent=${encodeURIComponent(agent)}` : "";
}

export interface PersonalityTemplate {
  filename: string;
  content: string;
}

export interface PersonalityTemplatesResponse {
  preset: string;
  files: PersonalityTemplate[];
}

export interface PersonalityTemplateOverrides {
  agent_name?: string;
  user_name?: string;
  timezone?: string;
  communication_style?: string;
  include_memory?: boolean;
}

export function getPersonalityTemplates(
  overrides: PersonalityTemplateOverrides = {},
  preset = "default",
  agent?: string,
): Promise<PersonalityTemplatesResponse> {
  const params = new URLSearchParams();
  params.set("preset", preset);
  if (agent) params.set("agent", agent);
  if (overrides.agent_name) params.set("agent_name", overrides.agent_name);
  if (overrides.user_name) params.set("user_name", overrides.user_name);
  if (overrides.timezone) params.set("timezone", overrides.timezone);
  if (overrides.communication_style)
    params.set("communication_style", overrides.communication_style);
  if (overrides.include_memory !== undefined)
    params.set("include_memory", String(overrides.include_memory));
  return apiFetch<PersonalityTemplatesResponse>(
    `/api/personality/templates?${params}`,
  );
}

export function getPersonalityIndex(agent?: string): Promise<PersonalityIndex> {
  return apiFetch<PersonalityIndex>(`/api/personality${agentQuery(agent)}`);
}

export function getPersonalityFile(
  filename: string,
  agent?: string,
): Promise<PersonalityFile> {
  return apiFetch<PersonalityFile>(
    `/api/personality/${encodeURIComponent(filename)}${agentQuery(agent)}`,
  );
}

export class PersonalityConflictError extends Error {
  constructor(public conflict: PersonalityConflict) {
    super(`personality file changed on disk: ${conflict.filename}`);
    this.name = "PersonalityConflictError";
  }
}

/** Resolves with the put result on success; throws `PersonalityConflictError` on 409. */
export async function putPersonalityFile(
  filename: string,
  content: string,
  expectedMtimeMs: number | null,
  agent?: string,
): Promise<PersonalityPutResult> {
  const url = `/api/personality/${encodeURIComponent(filename)}${agentQuery(agent)}`;
  const token = getToken();
  const headers = new Headers({ "Content-Type": "application/json" });
  if (token) headers.set("Authorization", `Bearer ${token}`);
  const response = await fetch(`${apiOrigin}${basePath}${url}`, {
    method: "PUT",
    headers,
    body: JSON.stringify({
      content,
      expected_mtime_ms: expectedMtimeMs ?? null,
    }),
  });
  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event("zeroclaw-unauthorized"));
    throw new UnauthorizedError();
  }
  if (response.status === 409) {
    const body = (await response
      .json()
      .catch(() => null)) as PersonalityConflict | null;
    if (body && body.error === "personality_disk_drift") {
      throw new PersonalityConflictError(body);
    }
    throw new Error("API 409: personality file changed on disk");
  }
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }
  return (await response.json()) as PersonalityPutResult;
}

// ── Skills (api_skills.rs) ───────────────────────────────────────────

export interface SkillFrontmatter {
  name: string;
  description: string;
  license?: string | null;
  author?: string | null;
  version?: string | null;
  category?: string | null;
}

export interface SkillBundleEntry {
  alias: string;
  directory: string;
  include: string[];
  exclude: string[];
}

export interface SkillEntry {
  bundle: string;
  name: string;
  directory: string;
  frontmatter: SkillFrontmatter;
}

export interface SkillDocument {
  bundle: string;
  name: string;
  frontmatter: SkillFrontmatter;
  body: string;
}

export interface SkillCreateRequest {
  name: string;
  frontmatter: SkillFrontmatter;
  body?: string;
  no_scaffold?: boolean;
}

export function listSkillBundles(): Promise<{ bundles: SkillBundleEntry[] }> {
  return apiFetch("/api/skills/bundles");
}

export function listSkillsInBundle(
  alias: string,
): Promise<{ skills: SkillEntry[] }> {
  return apiFetch(`/api/skills/bundles/${encodeURIComponent(alias)}/skills`);
}

export function readSkill(
  bundle: string,
  name: string,
): Promise<SkillDocument> {
  return apiFetch(
    `/api/skills/bundles/${encodeURIComponent(bundle)}/skills/${encodeURIComponent(name)}`,
  );
}

export function writeSkill(
  bundle: string,
  name: string,
  body: { frontmatter: SkillFrontmatter; body: string },
): Promise<void> {
  return apiFetch(
    `/api/skills/bundles/${encodeURIComponent(bundle)}/skills/${encodeURIComponent(name)}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  );
}

export function createSkill(
  bundle: string,
  body: SkillCreateRequest,
): Promise<{ bundle: string; name: string; directory: string }> {
  return apiFetch(`/api/skills/bundles/${encodeURIComponent(bundle)}/skills`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export function deleteSkill(
  bundle: string,
  name: string,
  purge = false,
): Promise<void> {
  const q = purge ? "?purge=true" : "";
  return apiFetch(
    `/api/skills/bundles/${encodeURIComponent(bundle)}/skills/${encodeURIComponent(name)}${q}`,
    { method: "DELETE" },
  );
}

// ── Config schema descriptions ───────────────────────────────────────
//
// `OPTIONS /api/config` returns the schemars-derived JSON Schema for the
// whole `Config` type, with every `///` doc comment surfaced as a
// `description` property. We fetch it once per session, walk it for any
// dotted path (kebab segments, snake-cased to match Rust field names),
// and surface the description as form helper text — no widening of the
// per-field list endpoint, no per-field round trips.

type JsonSchema = Record<string, unknown> | undefined;

let configSchemaCache: Promise<JsonSchema> | null = null;

export function fetchConfigSchema(): Promise<JsonSchema> {
  if (!configSchemaCache) {
    configSchemaCache = apiFetch<JsonSchema>("/api/config", {
      method: "OPTIONS",
    }).catch(() => undefined);
  }
  return configSchemaCache;
}

function resolveRef(node: unknown, root: unknown): unknown {
  if (!node || typeof node !== "object") return node;
  const ref = (node as { $ref?: unknown }).$ref;
  if (typeof ref !== "string" || !ref.startsWith("#/")) return node;
  let target: unknown = root;
  for (const seg of ref.slice(2).split("/")) {
    if (target && typeof target === "object")
      target = (target as Record<string, unknown>)[seg];
    else return node;
  }
  return target ?? node;
}

// `Option<T>` serializes as `{ anyOf: [<T schema>, { type: "null" }] }`.
// Take the non-null branch so traversal can dive into the inner type.
function unwrapOptional(node: unknown): unknown {
  if (!node || typeof node !== "object") return node;
  const anyOf = (node as { anyOf?: unknown[] }).anyOf;
  if (!Array.isArray(anyOf)) return node;
  const nonNull = anyOf.find((b) => {
    if (!b || typeof b !== "object") return false;
    const t = (b as { type?: unknown }).type;
    return (
      t !== "null" &&
      !(Array.isArray(t) && t.includes("null") && t.length === 1)
    );
  });
  return nonNull ?? node;
}

// Repeatedly resolve `$ref` and unwrap `Option<T>` until neither applies.
// Idempotent on plain object/leaf nodes. Bounded by a hop limit to guard
// against pathological self-refs in a hand-edited schema.
function resolveAndUnwrap(node: unknown, root: unknown): unknown {
  let cur = node;
  for (let i = 0; i < 8; i++) {
    const next = unwrapOptional(resolveRef(cur, root));
    if (next === cur) return cur;
    cur = next;
  }
  return cur;
}

/** One property on an `object-array` element type, derived from the
 *  JSON Schema. Used by the per-row editor to render each row as a
 *  small sub-form without hand-coding the element shape. */
export interface ObjectArrayPropMeta {
  /** snake_case key as it appears in the wire JSON. */
  key: string;
  /** Human-readable label (kebab-cased + spaced from `key`). */
  label: string;
  /** `string` | `bool` | `integer` | `float` | `string-array` | `object` | `enum` | `unknown`. */
  kind: string;
  /** Doc-comment description, when present. */
  description?: string;
  /** Enum variant names for `kind === 'enum'`. */
  enumVariants?: string[];
  /** True when the schema declares `Option<T>` (anyOf-with-null wrapper). */
  optional: boolean;
}

/** Walk the cached JSON Schema for `kebabPath` (a `Vec<T>` field) and
 *  return per-property metadata for the element type T. Returns `null`
 *  when the path doesn't resolve or the element isn't an object. */
export function objectArrayElementProps(
  schema: JsonSchema,
  kebabPath: string,
): ObjectArrayPropMeta[] | null {
  if (!schema) return null;
  let cur: unknown = schema;
  for (const seg of kebabPath.split(".")) {
    cur = unwrapOptional(resolveRef(cur, schema));
    if (!cur || typeof cur !== "object") return null;
    const snake = seg.replace(/-/g, "_");
    const props = (cur as { properties?: Record<string, unknown> }).properties;
    const additional = (cur as { additionalProperties?: unknown })
      .additionalProperties;
    if (props && Object.prototype.hasOwnProperty.call(props, snake)) {
      cur = props[snake];
    } else if (additional && typeof additional === "object") {
      cur = additional;
    } else {
      return null;
    }
  }
  // `cur` should now be an array schema; the element type is `items`.
  cur = unwrapOptional(resolveRef(cur, schema));
  if (!cur || typeof cur !== "object") return null;
  const items = (cur as { items?: unknown }).items;
  if (!items) return null;
  const elem = unwrapOptional(resolveRef(items, schema));
  if (!elem || typeof elem !== "object") return null;
  const elemProps = (elem as { properties?: Record<string, unknown> })
    .properties;
  if (!elemProps) return null;
  const out: ObjectArrayPropMeta[] = [];
  for (const [snakeKey, raw] of Object.entries(elemProps)) {
    const wrapped = raw as {
      description?: unknown;
      type?: unknown;
      anyOf?: unknown[];
      enum?: unknown[];
    } | null;
    const desc =
      typeof wrapped?.description === "string"
        ? wrapped.description
        : undefined;
    const isOptional =
      Array.isArray(wrapped?.anyOf) ||
      (Array.isArray(wrapped?.type) &&
        (wrapped!.type as string[]).includes("null"));
    const resolved = unwrapOptional(resolveRef(wrapped, schema)) as Record<
      string,
      unknown
    > | null;
    const t = resolved?.type;
    const enumVariants = Array.isArray(resolved?.enum)
      ? (resolved!.enum as unknown[]).filter(
          (v): v is string => typeof v === "string",
        )
      : undefined;
    let kind: string = "unknown";
    if (enumVariants && enumVariants.length > 0) kind = "enum";
    else if (t === "boolean" || (Array.isArray(t) && t.includes("boolean")))
      kind = "bool";
    else if (t === "integer" || (Array.isArray(t) && t.includes("integer")))
      kind = "integer";
    else if (t === "number" || (Array.isArray(t) && t.includes("number")))
      kind = "float";
    else if (t === "string" || (Array.isArray(t) && t.includes("string")))
      kind = "string";
    else if (t === "array") {
      const items = resolved?.items as { type?: unknown } | undefined;
      kind = items?.type === "string" ? "string-array" : "array";
    } else if (t === "object") kind = "object";
    out.push({
      key: snakeKey,
      label: snakeKey.replace(/_/g, " "),
      kind,
      description: desc,
      enumVariants,
      optional: isOptional,
    });
  }
  return out;
}

export function descriptionForPath(
  schema: JsonSchema,
  kebabPath: string,
): string | null {
  if (!schema) return null;
  let cur: unknown = schema;
  let last: unknown = null;
  for (const seg of kebabPath.split(".")) {
    cur = resolveAndUnwrap(cur, schema);
    if (!cur || typeof cur !== "object") return null;
    const snake = seg.replace(/-/g, "_");
    const props = (cur as { properties?: Record<string, unknown> }).properties;
    const additional = (cur as { additionalProperties?: unknown })
      .additionalProperties;
    if (props && Object.prototype.hasOwnProperty.call(props, snake)) {
      last = props[snake];
    } else if (additional && typeof additional === "object") {
      // `HashMap<String, T>` parent: current segment is a user-supplied
      // map key (e.g. provider name); dive into the value schema.
      last = additional;
    } else {
      return null;
    }
    cur = last;
  }
  // Wrapper carries the field's own `///` doc comment; the resolved
  // type's description is a fallback for fields that ref a typed config.
  const wrapDesc = (last as { description?: unknown } | null)?.description;
  if (typeof wrapDesc === "string" && wrapDesc.length > 0) return wrapDesc;
  const resolved = resolveAndUnwrap(last, schema) as {
    description?: unknown;
  } | null;
  const innerDesc = resolved?.description;
  return typeof innerDesc === "string" && innerDesc.length > 0
    ? innerDesc
    : null;
}

// ── Templates + map-key creation (issue #6175) ───────────────────────

/**
 * One addable shape — a HashMap<String, T> (Map) or Vec<T> (List) section
 * the dashboard can render a "+ Add" affordance for. Discovered from the
 * `Configurable` derive's `map_key_sections()`; never hand-listed.
 */
export interface TemplateEntry {
  path: string;
  /** 'map' for HashMap<String, T>; 'list' for Vec<T>. */
  kind: "map" | "list";
  /** Rust value type, for display only. */
  value_type: string;
  /** Doc comment from the schema field — describes what the user is adding. */
  description: string;
}

export interface TemplatesResponse {
  templates: TemplateEntry[];
}

export function getTemplates(): Promise<TemplatesResponse> {
  return apiFetch<TemplatesResponse>("/api/config/templates");
}

export interface MapKeyResponse {
  path: string;
  key: string;
  /** false for idempotent re-add on Map kinds; true on first creation. */
  created: boolean;
}

// ── Shared workspace browse ────────────────────────────────────────
// Hard-scoped to `<install>/shared/`. The gateway adapter at
// `crates/zeroclaw-gateway/src/api_browse.rs` defers all containment
// checks and walking to `zeroclaw_runtime::browse::list_directory`,
// so the path is interpreted relative to `shared/` here too.

export interface BrowseEntry {
  name: string;
  /** `"dir"` or `"file"`. */
  kind: "dir" | "file";
  /** Bytes; absent for directories. */
  size?: number;
  /** True for top-level entries the runtime owns (e.g. `sessions/`,
   *  `IDENTITY.md`). Server-side mutations on these are rejected; the
   *  dashboard hides delete/rename affordances when this is set. */
  protected?: boolean;
}

export interface BrowseResponse {
  /** Echoed cleaned path relative to `<install>/shared/`. */
  path: string;
  entries: BrowseEntry[];
}

export function browseShared(path = ""): Promise<BrowseResponse> {
  const q = path ? `?path=${encodeURIComponent(path)}` : "";
  return apiFetch<BrowseResponse>(`/api/browse${q}`);
}

/** Create a new directory under `<install>/shared/`. Idempotent on success. */
export function mkdirShared(path: string): Promise<{ created: string }> {
  return apiFetch<{ created: string }>(`/api/browse/mkdir`, {
    method: "POST",
    body: JSON.stringify({ path }),
  });
}

/** Recursively remove a directory under `<install>/shared/`. Backend refuses
 *  protected top-level entries (skills, skill-bundles, knowledge). */
export function rmdirShared(path: string): Promise<{ removed: string }> {
  return apiFetch<{ removed: string }>(`/api/browse/rmdir`, {
    method: "DELETE",
    body: JSON.stringify({ path }),
  });
}

// ── Agent workspace explorer ────────────────────────────────────────────
//
// All four endpoints scope to `<install>/agents/{alias}/workspace/`. The
// runtime enforces containment + protected-file refusal; the dashboard is
// a viewer/editor on top.

export interface AgentWorkspaceFileRead {
  path: string;
  size: number;
  is_text: boolean;
  /** UTF-8 text when `is_text` is true, base64 otherwise. */
  content: string;
  encoding: "utf8" | "base64";
}

export function listAgentWorkspace(
  alias: string,
  path = "",
): Promise<BrowseResponse> {
  const q = path ? `?path=${encodeURIComponent(path)}` : "";
  return apiFetch<BrowseResponse>(
    `/api/agents/${encodeURIComponent(alias)}/workspace/list${q}`,
  );
}

export function readAgentWorkspaceFile(
  alias: string,
  path: string,
): Promise<AgentWorkspaceFileRead> {
  return apiFetch<AgentWorkspaceFileRead>(
    `/api/agents/${encodeURIComponent(alias)}/workspace/read?path=${encodeURIComponent(path)}`,
  );
}

export function deleteAgentWorkspacePath(
  alias: string,
  path: string,
): Promise<{ removed: string }> {
  return apiFetch<{ removed: string }>(
    `/api/agents/${encodeURIComponent(alias)}/workspace/path`,
    { method: "DELETE", body: JSON.stringify({ path }) },
  );
}

export function moveAgentWorkspacePath(
  alias: string,
  from: string,
  to: string,
): Promise<{ from: string; to: string }> {
  return apiFetch<{ from: string; to: string }>(
    `/api/agents/${encodeURIComponent(alias)}/workspace/move`,
    { method: "POST", body: JSON.stringify({ from, to }) },
  );
}

export function createAgentWorkspaceDirectory(
  alias: string,
  path: string,
): Promise<{ created: string }> {
  return apiFetch<{ created: string }>(
    `/api/agents/${encodeURIComponent(alias)}/workspace/mkdir`,
    { method: "POST", body: JSON.stringify({ path }) },
  );
}

/**
 * Create a new entry under a map-keyed or list-shaped section. For Map
 * kinds the `key` is the new HashMap key; for List kinds it's the new
 * entry's natural identifier (e.g. `name` or `hint`).
 */
export function createMapKey(
  path: string,
  key: string,
): Promise<MapKeyResponse> {
  return apiFetch<MapKeyResponse>(
    `/api/config/map-key?path=${encodeURIComponent(path)}&key=${encodeURIComponent(key)}`,
    { method: "POST" },
  );
}

// ── Curated section catalog (provider + model picker source of truth) ────────

export interface CatalogProvider {
  name: string;
  display_name: string;
  local: boolean;
  aliases: string[];
}

export interface CatalogResponse {
  providers: CatalogProvider[];
}

export function getCatalog(): Promise<CatalogResponse> {
  return apiFetch<CatalogResponse>("/api/config/catalog");
}

export interface ModelPricing {
  prompt?: string;
  completion?: string;
  input_cache_read?: string;
  input_cache_write?: string;
}

export interface ModelsResponse {
  model_provider: string;
  models: string[];
  /** Optional pricing data keyed by model ID. */
  pricing?: Record<string, ModelPricing>;
  /** True when the provider family is local according to the gateway catalog. */
  local: boolean;
  /** false when the upstream catalog fetch failed; form should fall back to free-text. */
  live: boolean;
}

export function getCatalogModels(provider: string, alias?: string): Promise<ModelsResponse> {
  const params = new URLSearchParams({ provider });
  if (alias) params.set('alias', alias);
  return apiFetch<ModelsResponse>(
    `/api/config/catalog/models?${params.toString()}`,
  );
}

// ── Config sections + picker (mirrors the TUI flow) ─────────────────

export interface SectionInfo {
  /** Stable section key — matches `Section::as_path_prefix` in zeroclaw-runtime. */
  key: string;
  /** Human-readable section name. */
  label: string;
  /** Help text shown under the section header (verbatim from the TUI). */
  help: string;
  /** True when the section requires picking an item before fields render. */
  has_picker: boolean;
  /** True when the user has marked the section completed in the legacy
   *  per-section ledger (on-disk key `onboard_state.completed_sections`,
   *  retained for migration only). */
  completed: boolean;
  /** True when the section has enough usable config for first-run setup. */
  ready: boolean;
  /** Display group for the sidebar (`Quickstart`, `Agent`, `Tools`, ...). */
  group: string;
  /** True when this section is part of the canonical Quickstart list (driven
   *  by `zeroclaw_config::sections::QUICKSTART_SECTIONS`). */
  is_quickstart: boolean;
  /** Editor shape (`direct_form` / `one_tier_alias_map` / `typed_family_map`
   *  / `backend_picker`). Server-emitted from `WizardSection::shape()` so
   *  the dashboard explorer renders the same UI for the same section
   *  without hardcoded section keys.
   *  `null` / `undefined` for sections that aren't part of the canonical list. */
  shape?:
    | "direct_form"
    | "one_tier_alias_map"
    | "typed_family_map"
    | "backend_picker"
    | null;
}

export interface SectionsResponse {
  sections: SectionInfo[];
}

export function getSections(): Promise<SectionsResponse> {
  return apiFetch<SectionsResponse>("/api/config/sections");
}

export interface SectionStatusResponse {
  /** True when no enabled agent can reply yet. */
  needs_quickstart: boolean;
  /** Stable machine-readable reason: `fresh_install`, `incomplete_agent`, or
   * `has_dispatchable_agent`. */
  reason: string;
  /** True once the operator has entered any setup state. */
  has_partial_state: boolean;
  /** Human-readable readiness failures for the finish gate. */
  missing: string[];
  /** Structured repair targets for half-configured onboarding state. */
  repair_items: OnboardRepairItem[];
}

export interface OnboardRepairItem {
  code: string;
  message: string;
  section: string;
  focus?: string;
}

export function getSectionStatus(): Promise<SectionStatusResponse> {
  return apiFetch<SectionStatusResponse>("/api/config/status");
}

export interface AgentOptionsResponse {
  channels: string[];
  channel_types: string[];
  model_providers: string[];
  risk_profiles: string[];
  runtime_profiles: string[];
  skill_bundles: string[];
  knowledge_bundles: string[];
  mcp_bundles: string[];
  agents: string[];
  memory_namespaces?: string[];
}

export function getAgentOptions(): Promise<AgentOptionsResponse> {
  return apiFetch<AgentOptionsResponse>("/api/config/agent-options");
}

export interface PickerItem {
  key: string;
  label: string;
  description?: string;
  badge?: string;
}

export interface PickerResponse {
  section: string;
  items: PickerItem[];
  help: string;
}

export function getSectionPicker(section: string): Promise<PickerResponse> {
  return apiFetch<PickerResponse>(
    `/api/config/sections/${encodeURIComponent(section)}`,
  );
}

export interface SelectItemResponse {
  /** Dotted prefix to fetch fields under via listProps(prefix). */
  fields_prefix: string;
  created: boolean;
}

export function selectSectionItem(
  section: string,
  key: string,
  alias?: string,
): Promise<SelectItemResponse> {
  const body = alias ? JSON.stringify({ alias }) : undefined;
  return apiFetch<SelectItemResponse>(
    `/api/config/sections/${encodeURIComponent(section)}/items/${encodeURIComponent(key)}`,
    {
      method: "POST",
      headers: body ? { "Content-Type": "application/json" } : undefined,
      body,
    },
  );
}
// ── Quickstart ───────────────────────────────────────────────────────

export interface QuickstartTypeOption {
  /** Canonical kebab-case kind written into config (e.g. "anthropic", "telegram"). */
  kind: string;
  /** Picker label. */
  display_name: string;
  /** True for local providers that need no credential; always false for channels. */
  local: boolean;
}

export interface QuickstartState {
  quickstart_completed: boolean;
  agents: string[];
  risk_profiles: string[];
  runtime_profiles: string[];
  model_providers: string[];
  channels: string[];
  /**
   * Subset of `channels` not yet bound to any agent — safe to reuse
   * without breaking the one-channel-one-agent invariant.
   */
  unassigned_channels: string[];
  storage: string[];
  /**
   * Picker rows for "Create new model provider", supplied by the
   * daemon — sourced from `zeroclaw_providers::list_model_providers()`.
   * Surfaces render this list as-is and never keep their own copy.
   */
  model_provider_types: QuickstartTypeOption[];
  /**
   * Picker rows for "Create new channel", supplied by the daemon —
   * sourced from the schema-side `ChannelsConfig` inventory. Adding a
   * channel family in the schema lights up here automatically.
   */
  channel_types: QuickstartTypeOption[];
  /** Risk-profile presets from `RISK_PRESETS`. */
  risk_presets: QuickstartPreset[];
  /** Runtime-profile presets from `RUNTIME_PRESETS`. */
  runtime_presets: QuickstartPreset[];
  /** Memory backend snake-case keys from `MemoryBackendKind`. */
  memory_kinds: string[];
  /** Canonical personality filenames the Quickstart accepts. */
  personality_files: string[];
}

/** One row in a closed-set preset table (risk / runtime). */
export interface QuickstartPreset {
  preset_name: string;
  label: string;
  help: string;
}

export function getQuickstartState(): Promise<QuickstartState> {
  return apiFetch<QuickstartState>("/api/quickstart/state");
}

export interface QuickstartError {
  step: string;
  field: string;
  message: string;
}

export type QuickstartValidateResult =
  | { kind: "ok" }
  | { kind: "errors"; errors: QuickstartError[] };

export function quickstartValidate(submission: unknown): Promise<QuickstartValidateResult> {
  return apiFetch<QuickstartValidateResult>("/api/quickstart/validate", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(submission),
  });
}

export interface AppliedAgent {
  alias: string;
  model_provider: string;
  risk_profile: string;
  runtime_profile: string;
  channels: string[];
  memory_backend: string;
}

export type QuickstartApplyResult =
  | { kind: "applied"; agent: AppliedAgent; daemon_restarted: boolean }
  | { kind: "errors"; errors: QuickstartError[] };

export function quickstartApply(submission: unknown): Promise<QuickstartApplyResult> {
  return apiFetch<QuickstartApplyResult>("/api/quickstart/apply", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(submission),
  });
}

/** Schema field-kind tag mirroring `zeroclaw_config::traits::PropKind`. */
export type QuickstartFieldKind =
  | "string"
  | "bool"
  | "integer"
  | "float"
  | "enum"
  | "string_array"
  | "object_array"
  | "object"
  | "duration"
  | "secret";

export interface QuickstartFieldDescriptor {
  key: string;
  label: string;
  help: string;
  kind: QuickstartFieldKind;
  is_secret: boolean;
  enum_variants: string[] | null;
  required: boolean;
  default: string | null;
}

export interface QuickstartFieldsRequest {
  section: "model_provider" | "channel";
  type_key: string;
}

export interface QuickstartFieldsResult {
  fields: QuickstartFieldDescriptor[];
}

export function quickstartFields(
  req: QuickstartFieldsRequest,
): Promise<QuickstartFieldsResult> {
  return apiFetch<QuickstartFieldsResult>("/api/quickstart/fields", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
}

export type QuickstartStep =
  | "model_provider"
  | "risk_profile"
  | "runtime_profile"
  | "memory"
  | "channels"
  | "agent";

export interface QuickstartDismissRequest {
  run_id: string;
  surface: "web" | "tui" | "cli";
  last_step?: QuickstartStep | null;
}

/// Beacon fired when the user closes the Quickstart page without
/// submitting a Create. The runtime records this as a `Note` event in
/// the same stream as the apply lifecycle so dashboard / SSE
/// consumers can see drop-off rates. Best-effort: failures are
/// swallowed.
export function quickstartDismiss(req: QuickstartDismissRequest): void {
  // `keepalive: true` lets the request survive the navigation that
  // typically triggers this — same trick `navigator.sendBeacon` uses,
  // but goes through the existing auth path.
  void apiFetch<void>("/api/quickstart/dismiss", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
    keepalive: true,
  } as RequestInit).catch(() => {});
}


// ── Map-keyed alias CRUD ─────────────────────────────────────────────

export interface MapKeysResponse {
  path: string;
  keys: string[];
}

export function getMapKeys(path: string): Promise<MapKeysResponse> {
  return apiFetch<MapKeysResponse>(
    `/api/config/map-keys?path=${encodeURIComponent(path)}`,
  );
}

export interface MapKeyMutResponse {
  path: string;
  key: string;
  renamed?: boolean;
  created?: boolean;
}

export function deleteMapKey(
  path: string,
  key: string,
): Promise<MapKeyMutResponse> {
  return apiFetch<MapKeyMutResponse>(
    `/api/config/map-key?path=${encodeURIComponent(path)}&key=${encodeURIComponent(key)}`,
    { method: "DELETE" },
  );
}

export function renameMapKey(
  path: string,
  from: string,
  to: string,
): Promise<MapKeyMutResponse> {
  return apiFetch<MapKeyMutResponse>("/api/config/rename-map-key", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, from, to }),
  });
}

// ── Daemon admin (localhost-only on the gateway) ─────────────────────

export interface AdminResponse {
  success: boolean;
  message: string;
}

/**
 * Reload the daemon in place. Same PID — the daemon's main loop tears down
 * every subsystem (gateway/channels/heartbeat/scheduler/mqtt), re-reads
 * config from disk, and re-instantiates everything. Brief HTTP downtime
 * while the gateway listener rebinds; clients should poll `/health` to
 * detect when the new instance is ready.
 */
export function reloadDaemon(): Promise<AdminResponse> {
  return apiFetch<AdminResponse>("/admin/reload", { method: "POST" });
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

export function getTools(): Promise<ToolSpec[]> {
  return apiFetch<ToolSpec[] | { tools: ToolSpec[] }>("/api/tools").then(
    (data) => {
      const result = unwrapField(data, "tools");
      return Array.isArray(result) ? result : [];
    },
  );
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

export function getCronJobs(): Promise<CronJob[]> {
  return apiFetch<CronJob[] | { jobs: CronJob[] }>("/api/cron").then((data) => {
    const result = unwrapField(data, "jobs");
    return Array.isArray(result) ? result : [];
  });
}

export interface CronDelivery {
  mode: "none" | "announce";
  channel?: string;
  to?: string;
  best_effort?: boolean;
}

export function addCronJob(body: {
  agent: string;
  name?: string;
  schedule: string;
  tz?: string;
  command?: string;
  job_type?: string;
  prompt?: string;
  model?: string;
  session_target?: string;
  allowed_tools?: string[];
  enabled?: boolean;
  delivery?: CronDelivery;
}): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>("/api/cron", {
    method: "POST",
    body: JSON.stringify(body),
  }).then((data) =>
    typeof (data as { job?: CronJob }).job === "object"
      ? (data as { job: CronJob }).job
      : (data as CronJob),
  );
}

export function deleteCronJob(id: string): Promise<void> {
  return apiFetch<void>(`/api/cron/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

export interface CronTriggerResult {
  status: string;
  job_id: string;
  success: boolean;
  output: string;
  duration_ms: number;
  started_at: string;
  finished_at: string;
}

/** Manually trigger a cron job and wait for the result. */
export function triggerCronJob(id: string): Promise<CronTriggerResult> {
  return apiFetch<CronTriggerResult>(
    `/api/cron/${encodeURIComponent(id)}/run`,
    {
      method: "POST",
    },
  );
}

export function patchCronJob(
  id: string,
  patch: {
    agent?: string;
    name?: string;
    schedule?: string;
    tz?: string;
    clear_tz?: boolean;
    command?: string;
    prompt?: string;
    enabled?: boolean;
    delivery?: CronDelivery;
    model?: string;
    session_target?: string;
    allowed_tools?: string[];
    delete_after_run?: boolean;
  },
): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>(
    `/api/cron/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      body: JSON.stringify(patch),
    },
  ).then((data) =>
    typeof (data as { job?: CronJob }).job === "object"
      ? (data as { job: CronJob }).job
      : (data as CronJob),
  );
}

export function getCronRuns(
  jobId: string,
  limit: number = 20,
): Promise<CronRun[]> {
  const params = new URLSearchParams({ limit: String(limit) });
  return apiFetch<CronRun[] | { runs: CronRun[] }>(
    `/api/cron/${encodeURIComponent(jobId)}/runs?${params}`,
  ).then((data) => {
    const result = unwrapField(data, "runs");
    return Array.isArray(result) ? result : [];
  });
}

export interface CronSettings {
  enabled: boolean;
  catch_up_on_startup: boolean;
  max_run_history: number;
}

export function getCronSettings(): Promise<CronSettings> {
  return apiFetch<CronSettings>("/api/cron/settings");
}

export function patchCronSettings(
  patch: Partial<CronSettings>,
): Promise<CronSettings> {
  return apiFetch<CronSettings & { status: string }>("/api/cron/settings", {
    method: "PATCH",
    body: JSON.stringify(patch),
  });
}

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

export function getIntegrations(): Promise<Integration[]> {
  return apiFetch<Integration[] | { integrations: Integration[] }>(
    "/api/integrations",
  ).then((data) => {
    const result = unwrapField(data, "integrations");
    return Array.isArray(result) ? result : [];
  });
}

// ---------------------------------------------------------------------------
// Doctor / Diagnostics
// ---------------------------------------------------------------------------

export function runDoctor(): Promise<DiagResult[]> {
  return apiFetch<DiagResult[] | { results: DiagResult[]; summary?: unknown }>(
    "/api/doctor",
    {
      method: "POST",
      body: JSON.stringify({}),
    },
  ).then((data) => (Array.isArray(data) ? data : data.results));
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export function getMemory(
  query?: string,
  category?: string,
  agent?: string,
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams();
  if (query) params.set("query", query);
  if (category) params.set("category", category);
  if (agent) params.set("agent", agent);
  const qs = params.toString();
  return apiFetch<MemoryEntry[] | { entries: MemoryEntry[] }>(
    `/api/memory${qs ? `?${qs}` : ""}`,
  ).then((data) => {
    const result = unwrapField(data, "entries");
    return Array.isArray(result) ? result : [];
  });
}

export function storeMemory(
  key: string,
  content: string,
  category?: string,
  agent?: string,
): Promise<void> {
  return apiFetch<unknown>("/api/memory", {
    method: "POST",
    body: JSON.stringify({ key, content, category, agent }),
  }).then(() => undefined);
}

export function deleteMemory(key: string, agent?: string): Promise<void> {
  const qs = agent ? `?agent=${encodeURIComponent(agent)}` : "";
  return apiFetch<void>(`/api/memory/${encodeURIComponent(key)}${qs}`, {
    method: "DELETE",
  });
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

export function getCost(from?: Date, to?: Date): Promise<CostSummary> {
  const params = new URLSearchParams();
  if (from) params.set("from", from.toISOString());
  if (to) params.set("to", to.toISOString());
  const qs = params.toString();
  const url = qs ? `/api/cost?${qs}` : "/api/cost";
  return apiFetch<CostSummary | { cost: CostSummary }>(url).then((data) =>
    unwrapField(data, "cost"),
  );
}

/** Cost summary filtered to a single agent alias. Backed by the same
 * `/api/cost` endpoint as {@link getCost} via `?agent=<alias>`. */
export function getCostForAgent(alias: string): Promise<CostSummary> {
  const url = `/api/cost?agent=${encodeURIComponent(alias)}`;
  return apiFetch<CostSummary | { cost: CostSummary }>(url).then((data) =>
    unwrapField(data, "cost"),
  );
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export function getSessions(): Promise<Session[]> {
  return apiFetch<Session[] | { sessions: Session[] }>("/api/sessions").then(
    (data) => {
      const result = unwrapField(data, "sessions");
      return Array.isArray(result) ? result : [];
    },
  );
}

export function getSession(id: string): Promise<Session> {
  return apiFetch<Session>(`/api/sessions/${encodeURIComponent(id)}`);
}

/** Load persisted gateway WebSocket chat transcript for the dashboard Agent Chat. */
export function getSessionMessages(
  id: string,
): Promise<SessionMessagesResponse> {
  return apiFetch<SessionMessagesResponse>(
    `/api/sessions/${encodeURIComponent(id)}/messages`,
  );
}

/** Delete a persisted session by its full DB key. */
export function deleteSession(
  sessionKey: string,
): Promise<{ deleted: boolean }> {
  return apiFetch<{ deleted: boolean }>(
    `/api/sessions/${encodeURIComponent(sessionKey)}`,
    { method: "DELETE" },
  );
}

/**
 * Cancel an in-flight agent turn for a session. Idempotent — returns
 * `{ status: "no_active_response" }` when the session is idle.
 */
export function abortSession(id: string): Promise<{ status: string }> {
  return apiFetch<{ status: string }>(
    `/api/sessions/${encodeURIComponent(id)}/abort`,
    { method: "POST" },
  );
}

// ---------------------------------------------------------------------------
// Channels (detailed)
// ---------------------------------------------------------------------------

export function getChannels(): Promise<ChannelDetail[]> {
  return apiFetch<ChannelDetail[] | { channels: ChannelDetail[] }>(
    "/api/channels",
  ).then((data) => {
    const result = unwrapField(data, "channels");
    return Array.isArray(result) ? result : [];
  });
}

// ---------------------------------------------------------------------------
// Logs (persisted JSONL via zeroclaw-log)
// ---------------------------------------------------------------------------

/** Mirrors `zeroclaw_log::event::LogEvent` (Rust is the source of truth). */
export interface LogEvent {
  id: string;
  "@timestamp": string;
  severity_number: number;
  severity_text: string;
  event: { category: string; action: string; outcome?: string };
  service?: { name: string; version: string };
  trace_id?: string | null;
  span_id?: string | null;
  zeroclaw: Record<string, string> & { duration_ms?: number };
  message?: string;
  attributes?: Record<string, unknown>;
  schema_version?: number;
}

export interface LogsResponse {
  events: LogEvent[];
  /** `[timestamp, id]` to feed back as `until_ts` + `until_id` for older. */
  next_cursor: [string, string] | null;
  at_end: boolean;
  daemon_started_at: string;
  /** Canonical attribution-field names the daemon currently emits. Sourced
   *  from `ATTRIBUTION_FIELDS` + `COMPOSITE_PREFIXES` in zeroclaw-log so
   *  the UI never enumerates schema fields itself. */
  attribution_keys: string[];
}

/** Non-attribution top-level filters. Per-attribution exact matches live
 *  in `field_eq` — any `zeroclaw.*` key the daemon emits is valid there. */
export interface LogsQueryParams {
  since_ts?: string;
  until_ts?: string;
  until_id?: string;
  action?: string;
  category?: string;
  outcome?: string;
  severity_min?: number;
  trace_id?: string;
  q?: string;
  hide_internal?: boolean;
  limit?: number;
  field_eq?: Record<string, string>;
}

export function getLogs(params: LogsQueryParams = {}): Promise<LogsResponse> {
  const usp = new URLSearchParams();
  const { field_eq, ...rest } = params;
  for (const [key, value] of Object.entries(rest)) {
    if (value === undefined || value === null || value === "") continue;
    usp.set(key, String(value));
  }
  if (field_eq) {
    for (const [key, value] of Object.entries(field_eq)) {
      if (value === undefined || value === null || value === "") continue;
      usp.set(key, value);
    }
  }
  const qs = usp.toString();
  return apiFetch<LogsResponse>(`/api/logs${qs ? `?${qs}` : ""}`);
}

// ---------------------------------------------------------------------------
// CLI Tools
// ---------------------------------------------------------------------------

export function getCliTools(): Promise<CliTool[]> {
  return apiFetch<CliTool[] | { cli_tools: CliTool[] }>("/api/cli-tools").then(
    (data) => {
      const result = unwrapField(data, "cli_tools");
      return Array.isArray(result) ? result : [];
    },
  );
}
