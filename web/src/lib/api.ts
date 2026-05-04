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
} from '../types/api';
import { clearToken, getToken, setToken } from './auth';
import { apiOrigin, basePath } from './basePath';

// ---------------------------------------------------------------------------
// Base fetch wrapper
// ---------------------------------------------------------------------------

export class UnauthorizedError extends Error {
  constructor() {
    super('Unauthorized');
    this.name = 'UnauthorizedError';
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
    this.name = 'ApiError';
  }
}

export async function apiFetch<T = unknown>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const token = getToken();
  const headers = new Headers(options.headers);

  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }

  if (
    options.body &&
    typeof options.body === 'string' &&
    !headers.has('Content-Type')
  ) {
    headers.set('Content-Type', 'application/json');
  }

  const response = await fetch(`${apiOrigin}${basePath}${path}`, { ...options, headers });

  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event('zeroclaw-unauthorized'));
    throw new UnauthorizedError();
  }

  if (!response.ok) {
    // Try to parse a structured ConfigApiError envelope. Falls back to a
    // plain Error when the body is non-JSON or doesn't match the shape.
    // Centralises the parsing so callers (including the onboarding flow)
    // never have to regex-match `error.message` to recover the structured
    // code — they just `instanceof ApiError` and read `.envelope.code`.
    const text = await response.text().catch(() => '');
    if (text) {
      try {
        const parsed = JSON.parse(text);
        if (
          parsed &&
          typeof parsed === 'object' &&
          typeof parsed.code === 'string' &&
          typeof parsed.message === 'string'
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
  if (value !== null && typeof value === 'object' && !Array.isArray(value) && key in value) {
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
    method: 'POST',
    headers: { 'X-Pairing-Code': code },
  });

  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`Pairing failed (${response.status}): ${text || response.statusText}`);
  }

  const data = (await response.json()) as { token: string };
  setToken(data.token);
  return data;
}

export async function getAdminPairCode(): Promise<{ pairing_code: string | null; pairing_required: boolean }> {
  // Use the public /pair/code endpoint which works in Docker and remote environments
  // (no localhost restriction). Falls back to the admin endpoint for backward compat.
  const publicResp = await fetch(`${basePath}/pair/code`);
  if (publicResp.ok) {
    return publicResp.json() as Promise<{ pairing_code: string | null; pairing_required: boolean }>;
  }

  const response = await fetch('/admin/paircode');
  if (!response.ok) {
    throw new Error(`Failed to fetch pairing code (${response.status})`);
  }
  return response.json() as Promise<{ pairing_code: string | null; pairing_required: boolean }>;
}

// ---------------------------------------------------------------------------
// Public health (no auth required)
// ---------------------------------------------------------------------------

export async function getPublicHealth(): Promise<{ require_pairing: boolean; paired: boolean }> {
  const response = await fetch(`${basePath}/health`);
  if (!response.ok) {
    throw new Error(`Health check failed (${response.status})`);
  }
  return response.json() as Promise<{ require_pairing: boolean; paired: boolean }>;
}

// ---------------------------------------------------------------------------
// Status / Health
// ---------------------------------------------------------------------------

export function getStatus(): Promise<StatusResponse> {
  return apiFetch<StatusResponse>('/api/status');
}

export function getHealth(): Promise<HealthSnapshot> {
  return apiFetch<HealthSnapshot | { health: HealthSnapshot }>('/api/health').then((data) =>
    unwrapField(data, 'health'),
  );
}

// ---------------------------------------------------------------------------
// System / self-update
// ---------------------------------------------------------------------------

export interface SystemVersion {
  current: string;
  latest: string;
  update_available: boolean;
  latest_published_at: string | null;
  download_url: string | null;
}

export type UpdatePhase =
  | 'preflight'
  | 'download'
  | 'backup'
  | 'validate'
  | 'swap'
  | 'smoke_test'
  | 'done'
  | 'failed'
  | 'rolled_back';

export interface UpdateLogLine {
  task_id: string;
  phase: UpdatePhase;
  level: 'info' | 'warn' | 'error';
  message: string;
  timestamp: string;
}

export interface UpdateStatus {
  status: 'idle' | 'running' | 'succeeded' | 'failed';
  task_id: string | null;
  phase: UpdatePhase | null;
  started_at: string | null;
  log_tail: UpdateLogLine[];
}

export function getSystemVersion(): Promise<SystemVersion> {
  return apiFetch<SystemVersion>('/api/system/version');
}

export function postSystemUpdate(
  body: { version?: string; force?: boolean } = {},
): Promise<{ task_id: string }> {
  return apiFetch<{ task_id: string }>('/api/system/update', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

export function getSystemUpdateStatus(): Promise<UpdateStatus> {
  return apiFetch<UpdateStatus>('/api/system/update/status');
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
  onboard_section?: string;
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
  op: 'add' | 'replace' | 'remove' | 'test';
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
  return apiFetch<PropResponse>(`/api/config/prop?path=${encodeURIComponent(path)}`);
}

export function putProp(
  path: string,
  value: unknown,
  comment?: string,
): Promise<PropResponse> {
  return apiFetch<PropResponse>('/api/config/prop', {
    method: 'PUT',
    body: JSON.stringify({ path, value, comment }),
  });
}

export function deleteProp(path: string): Promise<PropResponse> {
  return apiFetch<PropResponse>(`/api/config/prop?path=${encodeURIComponent(path)}`, {
    method: 'DELETE',
  });
}

export function listProps(prefix?: string): Promise<ListResponse> {
  const q = prefix ? `?prefix=${encodeURIComponent(prefix)}` : '';
  return apiFetch<ListResponse>(`/api/config/list${q}`);
}

export function patchConfig(ops: PatchOp[]): Promise<PatchResponse> {
  return apiFetch<PatchResponse>('/api/config', {
    method: 'PATCH',
    body: JSON.stringify(ops),
  });
}

export function initSection(section?: string): Promise<{ initialized: string[] }> {
  const q = section ? `?section=${encodeURIComponent(section)}` : '';
  return apiFetch<{ initialized: string[] }>(`/api/config/init${q}`, { method: 'POST' });
}

export function getDrift(): Promise<{ drifted: DriftEntry[] }> {
  return apiFetch<{ drifted: DriftEntry[] }>('/api/config/drift');
}

export function getOpenApiSchema(): Promise<unknown> {
  return apiFetch<unknown>('/api/openapi.json');
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
  error: 'personality_disk_drift';
  filename: string;
  current_content: string;
  current_mtime_ms: number | null;
}

function agentQuery(agent?: string): string {
  return agent ? `?agent=${encodeURIComponent(agent)}` : '';
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
  preset = 'default',
  agent?: string,
): Promise<PersonalityTemplatesResponse> {
  const params = new URLSearchParams();
  params.set('preset', preset);
  if (agent) params.set('agent', agent);
  if (overrides.agent_name) params.set('agent_name', overrides.agent_name);
  if (overrides.user_name) params.set('user_name', overrides.user_name);
  if (overrides.timezone) params.set('timezone', overrides.timezone);
  if (overrides.communication_style)
    params.set('communication_style', overrides.communication_style);
  if (overrides.include_memory !== undefined)
    params.set('include_memory', String(overrides.include_memory));
  return apiFetch<PersonalityTemplatesResponse>(`/api/personality/templates?${params}`);
}

export function getPersonalityIndex(agent?: string): Promise<PersonalityIndex> {
  return apiFetch<PersonalityIndex>(`/api/personality${agentQuery(agent)}`);
}

export function getPersonalityFile(filename: string, agent?: string): Promise<PersonalityFile> {
  return apiFetch<PersonalityFile>(
    `/api/personality/${encodeURIComponent(filename)}${agentQuery(agent)}`,
  );
}

export class PersonalityConflictError extends Error {
  constructor(public conflict: PersonalityConflict) {
    super(`personality file changed on disk: ${conflict.filename}`);
    this.name = 'PersonalityConflictError';
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
  const headers = new Headers({ 'Content-Type': 'application/json' });
  if (token) headers.set('Authorization', `Bearer ${token}`);
  const response = await fetch(`${apiOrigin}${basePath}${url}`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ content, expected_mtime_ms: expectedMtimeMs ?? null }),
  });
  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event('zeroclaw-unauthorized'));
    throw new UnauthorizedError();
  }
  if (response.status === 409) {
    const body = (await response.json().catch(() => null)) as PersonalityConflict | null;
    if (body && body.error === 'personality_disk_drift') {
      throw new PersonalityConflictError(body);
    }
    throw new Error('API 409: personality file changed on disk');
  }
  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }
  return (await response.json()) as PersonalityPutResult;
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
    configSchemaCache = apiFetch<JsonSchema>('/api/config', { method: 'OPTIONS' })
      .catch(() => undefined);
  }
  return configSchemaCache;
}

function resolveRef(node: unknown, root: unknown): unknown {
  if (!node || typeof node !== 'object') return node;
  const ref = (node as { $ref?: unknown }).$ref;
  if (typeof ref !== 'string' || !ref.startsWith('#/')) return node;
  let target: unknown = root;
  for (const seg of ref.slice(2).split('/')) {
    if (target && typeof target === 'object') target = (target as Record<string, unknown>)[seg];
    else return node;
  }
  return target ?? node;
}

// `Option<T>` serializes as `{ anyOf: [<T schema>, { type: "null" }] }`.
// Take the non-null branch so traversal can dive into the inner type.
function unwrapOptional(node: unknown): unknown {
  if (!node || typeof node !== 'object') return node;
  const anyOf = (node as { anyOf?: unknown[] }).anyOf;
  if (!Array.isArray(anyOf)) return node;
  const nonNull = anyOf.find((b) => {
    if (!b || typeof b !== 'object') return false;
    const t = (b as { type?: unknown }).type;
    return t !== 'null' && !(Array.isArray(t) && t.includes('null') && t.length === 1);
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
  for (const seg of kebabPath.split('.')) {
    cur = unwrapOptional(resolveRef(cur, schema));
    if (!cur || typeof cur !== 'object') return null;
    const snake = seg.replace(/-/g, '_');
    const props = (cur as { properties?: Record<string, unknown> }).properties;
    const additional = (cur as { additionalProperties?: unknown }).additionalProperties;
    if (props && Object.prototype.hasOwnProperty.call(props, snake)) {
      cur = props[snake];
    } else if (additional && typeof additional === 'object') {
      cur = additional;
    } else {
      return null;
    }
  }
  // `cur` should now be an array schema; the element type is `items`.
  cur = unwrapOptional(resolveRef(cur, schema));
  if (!cur || typeof cur !== 'object') return null;
  const items = (cur as { items?: unknown }).items;
  if (!items) return null;
  const elem = unwrapOptional(resolveRef(items, schema));
  if (!elem || typeof elem !== 'object') return null;
  const elemProps = (elem as { properties?: Record<string, unknown> }).properties;
  if (!elemProps) return null;
  const out: ObjectArrayPropMeta[] = [];
  for (const [snakeKey, raw] of Object.entries(elemProps)) {
    const wrapped = raw as { description?: unknown; type?: unknown; anyOf?: unknown[]; enum?: unknown[] } | null;
    const desc = typeof wrapped?.description === 'string' ? wrapped.description : undefined;
    const isOptional = Array.isArray(wrapped?.anyOf)
      || (Array.isArray(wrapped?.type) && (wrapped!.type as string[]).includes('null'));
    const resolved = unwrapOptional(resolveRef(wrapped, schema)) as Record<string, unknown> | null;
    const t = resolved?.type;
    const enumVariants = Array.isArray(resolved?.enum)
      ? (resolved!.enum as unknown[]).filter((v): v is string => typeof v === 'string')
      : undefined;
    let kind: string = 'unknown';
    if (enumVariants && enumVariants.length > 0) kind = 'enum';
    else if (t === 'boolean' || (Array.isArray(t) && t.includes('boolean'))) kind = 'bool';
    else if (t === 'integer' || (Array.isArray(t) && t.includes('integer'))) kind = 'integer';
    else if (t === 'number' || (Array.isArray(t) && t.includes('number'))) kind = 'float';
    else if (t === 'string' || (Array.isArray(t) && t.includes('string'))) kind = 'string';
    else if (t === 'array') {
      const items = resolved?.items as { type?: unknown } | undefined;
      kind = items?.type === 'string' ? 'string-array' : 'array';
    } else if (t === 'object') kind = 'object';
    out.push({
      key: snakeKey,
      label: snakeKey.replace(/_/g, ' '),
      kind,
      description: desc,
      enumVariants,
      optional: isOptional,
    });
  }
  return out;
}

export function descriptionForPath(schema: JsonSchema, kebabPath: string): string | null {
  if (!schema) return null;
  let cur: unknown = schema;
  let last: unknown = null;
  for (const seg of kebabPath.split('.')) {
    cur = resolveAndUnwrap(cur, schema);
    if (!cur || typeof cur !== 'object') return null;
    const snake = seg.replace(/-/g, '_');
    const props = (cur as { properties?: Record<string, unknown> }).properties;
    const additional = (cur as { additionalProperties?: unknown }).additionalProperties;
    if (props && Object.prototype.hasOwnProperty.call(props, snake)) {
      last = props[snake];
    } else if (additional && typeof additional === 'object') {
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
  if (typeof wrapDesc === 'string' && wrapDesc.length > 0) return wrapDesc;
  const resolved = resolveAndUnwrap(last, schema) as { description?: unknown } | null;
  const innerDesc = resolved?.description;
  return typeof innerDesc === 'string' && innerDesc.length > 0 ? innerDesc : null;
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
  kind: 'map' | 'list';
  /** Rust value type, for display only. */
  value_type: string;
  /** Doc comment from the schema field — describes what the user is adding. */
  description: string;
}

export interface TemplatesResponse {
  templates: TemplateEntry[];
}

export function getTemplates(): Promise<TemplatesResponse> {
  return apiFetch<TemplatesResponse>('/api/config/templates');
}

export interface MapKeyResponse {
  path: string;
  key: string;
  /** false for idempotent re-add on Map kinds; true on first creation. */
  created: boolean;
}

/**
 * Create a new entry under a map-keyed or list-shaped section. For Map
 * kinds the `key` is the new HashMap key; for List kinds it's the new
 * entry's natural identifier (e.g. `name` or `hint`).
 */
export function createMapKey(path: string, key: string): Promise<MapKeyResponse> {
  return apiFetch<MapKeyResponse>(
    `/api/config/map-key?path=${encodeURIComponent(path)}&key=${encodeURIComponent(key)}`,
    { method: 'POST' },
  );
}

// ── Onboard catalog (provider + model picker source of truth) ────────

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
  return apiFetch<CatalogResponse>('/api/onboard/catalog');
}

export interface ModelsResponse {
  provider: string;
  models: string[];
  /** false when the upstream catalog fetch failed; form should fall back to free-text. */
  live: boolean;
}

export function getCatalogModels(provider: string): Promise<ModelsResponse> {
  return apiFetch<ModelsResponse>(
    `/api/onboard/catalog/models?provider=${encodeURIComponent(provider)}`,
  );
}

// ── Type parity with the generated OpenAPI client ──────────────────
//
// `api-generated.ts` is produced by `cargo web gen-api` (see
// `xtask/src/bin/web.rs`). The xtask renders the gateway's OpenAPI 3.1
// spec in-process from `zeroclaw_gateway::openapi::build_spec()` and
// pipes it through `openapi-typescript`. Neither the spec nor the
// generated TS is committed — both are regenerated on every
// `cargo web build` / `cargo web check`. tsc fails here if the
// hand-maintained shapes below stop matching.
export type { paths as ApiPaths, components as ApiComponents } from './api-generated';

// ── Onboard sections + picker (mirrors the TUI flow) ────────────────

export interface SectionInfo {
  /** Stable section key — matches `Section::as_path_prefix` in zeroclaw-runtime. */
  key: string;
  /** Human-readable section name. */
  label: string;
  /** Help text shown under the section header (verbatim from the TUI). */
  help: string;
  /** True when the section requires picking an item before fields render. */
  has_picker: boolean;
  /** True when the user has marked the section completed in onboard_state. */
  completed: boolean;
  /** Display group for the sidebar (`Onboarding`, `Agent`, `Tools`, ...). */
  group: string;
}

export interface SectionsResponse {
  sections: SectionInfo[];
}

export function getSections(): Promise<SectionsResponse> {
  return apiFetch<SectionsResponse>('/api/onboard/sections');
}

export interface OnboardStatusResponse {
  /** True when the user is on a fresh install (no completed sections AND
   * no provider configured). The Dashboard uses this to redirect first
   * visits to `/onboard`. */
  needs_onboarding: boolean;
  /** Stable machine-readable reason: `fresh_install`, `has_provider`, or
   * `has_completed_sections`. */
  reason: string;
}

export function getOnboardStatus(): Promise<OnboardStatusResponse> {
  return apiFetch<OnboardStatusResponse>('/api/onboard/status');
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
    `/api/onboard/sections/${encodeURIComponent(section)}`,
  );
}

export interface SelectItemResponse {
  /** Dotted prefix to fetch fields under via listProps(prefix). */
  fields_prefix: string;
  created: boolean;
}

export function selectSectionItem(section: string, key: string): Promise<SelectItemResponse> {
  return apiFetch<SelectItemResponse>(
    `/api/onboard/sections/${encodeURIComponent(section)}/items/${encodeURIComponent(key)}`,
    { method: 'POST' },
  );
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
  return apiFetch<AdminResponse>('/admin/reload', { method: 'POST' });
}


// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

export function getTools(): Promise<ToolSpec[]> {
  return apiFetch<ToolSpec[] | { tools: ToolSpec[] }>('/api/tools').then((data) => {
    const result = unwrapField(data, 'tools');
    return Array.isArray(result) ? result : [];
  });
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

export function getCronJobs(): Promise<CronJob[]> {
  return apiFetch<CronJob[] | { jobs: CronJob[] }>('/api/cron').then((data) => {
    const result = unwrapField(data, 'jobs');
    return Array.isArray(result) ? result : [];
  });
}

export function addCronJob(body: {
  name?: string;
  schedule: string;
  command?: string;
  job_type?: string;
  prompt?: string;
  model?: string;
  session_target?: string;
  allowed_tools?: string[];
  enabled?: boolean;
}): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>('/api/cron', {
    method: 'POST',
    body: JSON.stringify(body),
  }).then((data) => (typeof (data as { job?: CronJob }).job === 'object' ? (data as { job: CronJob }).job : (data as CronJob)));
}

export function deleteCronJob(id: string): Promise<void> {
  return apiFetch<void>(`/api/cron/${encodeURIComponent(id)}`, {
    method: 'DELETE',
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
  return apiFetch<CronTriggerResult>(`/api/cron/${encodeURIComponent(id)}/run`, {
    method: 'POST',
  });
}

export function patchCronJob(
  id: string,
  patch: { name?: string; schedule?: string; command?: string; prompt?: string },
): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>(
    `/api/cron/${encodeURIComponent(id)}`,
    {
      method: 'PATCH',
      body: JSON.stringify(patch),
    },
  ).then((data) => (typeof (data as { job?: CronJob }).job === 'object' ? (data as { job: CronJob }).job : (data as CronJob)));
}


export function getCronRuns(
  jobId: string,
  limit: number = 20,
): Promise<CronRun[]> {
  const params = new URLSearchParams({ limit: String(limit) });
  return apiFetch<CronRun[] | { runs: CronRun[] }>(
    `/api/cron/${encodeURIComponent(jobId)}/runs?${params}`,
  ).then((data) => {
    const result = unwrapField(data, 'runs');
    return Array.isArray(result) ? result : [];
  });
}

export interface CronSettings {
  enabled: boolean;
  catch_up_on_startup: boolean;
  max_run_history: number;
}

export function getCronSettings(): Promise<CronSettings> {
  return apiFetch<CronSettings>('/api/cron/settings');
}

export function patchCronSettings(
  patch: Partial<CronSettings>,
): Promise<CronSettings> {
  return apiFetch<CronSettings & { status: string }>('/api/cron/settings', {
    method: 'PATCH',
    body: JSON.stringify(patch),
  });
}

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

export function getIntegrations(): Promise<Integration[]> {
  return apiFetch<Integration[] | { integrations: Integration[] }>('/api/integrations').then(
    (data) => {
      const result = unwrapField(data, 'integrations');
      return Array.isArray(result) ? result : [];
    },
  );
}

// ---------------------------------------------------------------------------
// Doctor / Diagnostics
// ---------------------------------------------------------------------------

export function runDoctor(): Promise<DiagResult[]> {
  return apiFetch<DiagResult[] | { results: DiagResult[]; summary?: unknown }>('/api/doctor', {
    method: 'POST',
    body: JSON.stringify({}),
  }).then((data) => (Array.isArray(data) ? data : data.results));
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export function getMemory(
  query?: string,
  category?: string,
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams();
  if (query) params.set('query', query);
  if (category) params.set('category', category);
  const qs = params.toString();
  return apiFetch<MemoryEntry[] | { entries: MemoryEntry[] }>(`/api/memory${qs ? `?${qs}` : ''}`).then(
    (data) => {
      const result = unwrapField(data, 'entries');
      return Array.isArray(result) ? result : [];
    },
  );
}

export function storeMemory(
  key: string,
  content: string,
  category?: string,
): Promise<void> {
  return apiFetch<unknown>('/api/memory', {
    method: 'POST',
    body: JSON.stringify({ key, content, category }),
  }).then(() => undefined);
}

export function deleteMemory(key: string): Promise<void> {
  return apiFetch<void>(`/api/memory/${encodeURIComponent(key)}`, {
    method: 'DELETE',
  });
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

export function getCost(): Promise<CostSummary> {
  return apiFetch<CostSummary | { cost: CostSummary }>('/api/cost').then((data) =>
    unwrapField(data, 'cost'),
  );
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export function getSessions(): Promise<Session[]> {
  return apiFetch<Session[] | { sessions: Session[] }>('/api/sessions').then((data) => {
    const result = unwrapField(data, 'sessions');
    return Array.isArray(result) ? result : [];
  });
}

export function getSession(id: string): Promise<Session> {
  return apiFetch<Session>(`/api/sessions/${encodeURIComponent(id)}`);
}

/** Load persisted gateway WebSocket chat transcript for the dashboard Agent Chat. */
export function getSessionMessages(id: string): Promise<SessionMessagesResponse> {
  return apiFetch<SessionMessagesResponse>(
    `/api/sessions/${encodeURIComponent(id)}/messages`,
  );
}

/**
 * Cancel an in-flight agent turn for a session. Idempotent — returns
 * `{ status: "no_active_response" }` when the session is idle.
 */
export function abortSession(id: string): Promise<{ status: string }> {
  return apiFetch<{ status: string }>(
    `/api/sessions/${encodeURIComponent(id)}/abort`,
    { method: 'POST' },
  );
}

// ---------------------------------------------------------------------------
// Channels (detailed)
// ---------------------------------------------------------------------------

export function getChannels(): Promise<ChannelDetail[]> {
  return apiFetch<ChannelDetail[] | { channels: ChannelDetail[] }>('/api/channels').then((data) => {
    const result = unwrapField(data, 'channels');
    return Array.isArray(result) ? result : [];
  });
}

// ---------------------------------------------------------------------------
// CLI Tools
// ---------------------------------------------------------------------------

export function getCliTools(): Promise<CliTool[]> {
  return apiFetch<CliTool[] | { cli_tools: CliTool[] }>('/api/cli-tools').then((data) => {
    const result = unwrapField(data, 'cli_tools');
    return Array.isArray(result) ? result : [];
  });
}
