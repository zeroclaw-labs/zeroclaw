# 05 — Frontend Internals

## Tech Stack and Build Pipeline

| Layer            | Library / Tool                          |
|------------------|-----------------------------------------|
| Framework        | React 19 (concurrent mode)              |
| Language         | TypeScript (strict mode, `noUncheckedIndexedAccess`) |
| Build            | Vite 6 (esbuild + Rollup)               |
| Routing          | React Router v7                         |
| Server state     | TanStack Query v5 (React Query)         |
| Styling          | Tailwind CSS v4 + CSS custom properties |
| HTTP             | Fetch API via typed `ApiClient`         |

`vite.config.ts` proxies `/api/*` to `http://localhost:8080` in dev. Production: Caddy serves the SPA dist and reverse-proxies `/api`.

TypeScript `paths` alias: `@/` → `src/`. All imports use `@/` prefix.

---

## Page Inventory

| Route                  | Component       | Purpose                                              |
|------------------------|-----------------|------------------------------------------------------|
| `/login`               | Login           | OTP email entry + code verify, JWT storage           |
| `/dashboard`           | Dashboard       | Overview stats, tenant count, status summary         |
| `/tenants`             | TenantList      | Paginated tenant table, status badges, quick actions |
| `/tenants/new`         | SetupWizard     | 4-step create flow (name → provider → channels → deploy) |
| `/tenants/:id`         | TenantDetail    | Tabbed detail view (Overview, Config, Channels, Usage, Members) |
| `/users`               | UserList        | Super-admin only; list/manage platform users         |
| `/audit`               | AuditLog        | Filterable audit event table (super-admin)           |

`/`, `/dashboard`, `/tenants*`, `/users`, `/audit` all wrapped in `ProtectedRoute`. `/users` and `/audit` additionally wrapped in `AdminRoute`.

---

## Component Library

### Layout
Persistent shell: sidebar nav, top bar (user menu, logout). Renders `<Outlet />` for page content. Sidebar links conditionally rendered based on `user.role`.

### Modal
Controlled overlay with `isOpen`, `onClose`, `title`, `children`. Traps focus, closes on Escape. Used for channel edit, member invite.

### ConfirmModal
Extends Modal. Props: `message`, `confirmText` (typed input required), `onConfirm`, `onCancel`. Used for tenant delete — user must type tenant slug to enable "Confirm" button.

### FormField
```tsx
<FormField
  label="Token"
  name="token"
  type="password"   // text | password | number | boolean | select
  required
  options={[...]}   // for type="select"
  value={val}
  onChange={setVal}
  error={errors.token}
/>
```
Renders label + appropriate input + inline error. `boolean` type renders a toggle. Core building block for schema-driven forms.

### StatusBadge
```tsx
<StatusBadge status="running" />
// → colored pill: running=green, stopped=gray, error=red, deploying=yellow, draft=blue
```

### Pagination
Props: `page`, `total`, `pageSize`, `onPageChange`. Renders prev/next + page numbers. Used in TenantList, AuditLog, UserList.

### CopyButton
Wraps `navigator.clipboard.writeText`. Shows checkmark icon for 2s after copy. Props: `value: string`.

### Toast
See Toast System section below.

---

## API Client Architecture

`src/api/client.ts` — singleton `ApiClient` class:

```ts
class ApiClient {
  private base = '/api'

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const token = localStorage.getItem('jwt')
    const res = await fetch(`${this.base}${path}`, {
      ...init,
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        ...init?.headers,
      },
    })
    if (res.status === 401) {
      localStorage.removeItem('jwt')
      window.location.href = '/login'
    }
    if (!res.ok) throw new ApiError(res.status, await res.json())
    return res.json() as Promise<T>
  }

  get<T>(path: string) { return this.request<T>(path) }
  post<T>(path: string, body: unknown) { return this.request<T>(path, { method: 'POST', body: JSON.stringify(body) }) }
  put<T>(path: string, body: unknown) { return this.request<T>(path, { method: 'PUT', body: JSON.stringify(body) }) }
  delete<T>(path: string) { return this.request<T>(path, { method: 'DELETE' }) }
}

export const api = new ApiClient()
```

Domain modules (`auth.ts`, `tenants.ts`, `channels.ts`, `members.ts`, `users.ts`, `monitoring.ts`, `resources.ts`) import `api` and export typed functions:

```ts
// tenants.ts
export const getTenant = (id: string) => api.get<Tenant>(`/tenants/${id}`)
export const deployTenant = (id: string) => api.post<void>(`/tenants/${id}/deploy`, {})
```

React Query hooks wrap these in `useQuery` / `useMutation`.

---

## Schema-Driven Forms

### channelSchemas.ts

```ts
type FieldType = 'text' | 'password' | 'number' | 'boolean' | 'select'

interface FieldDef {
  name: string
  label: string
  type: FieldType
  required: boolean
  options?: string[]     // for select type
  placeholder?: string
}

interface ChannelSchema {
  type: string           // e.g. 'telegram', 'discord', 'slack'
  label: string
  fields: FieldDef[]
}

export const channelSchemas: ChannelSchema[] = [ /* 13 entries */ ]
```

`ChannelsTab` and `SetupWizard` step 3 do:

```ts
const schema = channelSchemas.find(s => s.type === selectedType)
// render schema.fields.map(field => <FormField key={field.name} {...field} />)
```

### providerSchemas.ts

```ts
interface ModelDef { id: string; label: string; contextWindow: number }
interface ProviderSchema {
  id: string             // e.g. 'openai', 'anthropic'
  label: string
  models: ModelDef[]
  fields: FieldDef[]     // e.g. api_key, base_url (optional)
}

export const providerSchemas: ProviderSchema[] = [ /* 14 entries */ ]
```

`ConfigTab` renders provider selector → model selector from `schema.models` → `schema.fields` via `FormField`.

---

## Dark Theme

Tailwind CSS v4 with `@theme` block in `src/index.css`:

```css
@theme {
  --color-bg-primary:    #0f1117;
  --color-bg-card:       #1a1d2e;
  --color-bg-elevated:   #222538;
  --color-accent-blue:   #4f8ef7;
  --color-accent-green:  #34d399;
  --color-accent-red:    #f87171;
  --color-accent-yellow: #fbbf24;
  --color-text-primary:  #e2e8f0;
  --color-text-muted:    #64748b;
  --color-border:        #2d3148;
}
```

Usage in components: `bg-bg-primary`, `bg-bg-card`, `text-text-primary`, `text-accent-blue`, `border-border`. No separate light-mode variants — platform is dark-only.

---

## State Management

**Server state (React Query v5):**

```ts
// query
const { data: tenant, isLoading } = useQuery({
  queryKey: ['tenant', id],
  queryFn: () => getTenant(id),
  refetchInterval: status === 'deploying' ? 2000 : false,
})

// mutation with cache invalidation
const deploy = useMutation({
  mutationFn: () => deployTenant(id),
  onSuccess: () => queryClient.invalidateQueries({ queryKey: ['tenant', id] }),
})
```

**Local state:** `useState` for form values, selected tab, wizard step, modal open flags. No global client-state library; all shared server data flows through Query cache.

Cache keys convention: `['tenants']` (list), `['tenant', id]` (single), `['channels', tenantId]`, `['members', tenantId]`, `['usage', tenantId, window]`.

---

## Routing

```tsx
<Routes>
  <Route path="/login" element={<Login />} />
  <Route element={<ProtectedRoute />}>          {/* redirect to /login if no JWT */}
    <Route element={<Layout />}>
      <Route path="/dashboard" element={<Dashboard />} />
      <Route path="/tenants" element={<TenantList />} />
      <Route path="/tenants/new" element={<SetupWizard />} />
      <Route path="/tenants/:id" element={<TenantDetail />} />
      <Route element={<AdminRoute />}>          {/* redirect to /dashboard if not super_admin */}
        <Route path="/users" element={<UserList />} />
        <Route path="/audit" element={<AuditLog />} />
      </Route>
    </Route>
  </Route>
  <Route path="*" element={<Navigate to="/dashboard" />} />
</Routes>
```

`ProtectedRoute`: checks `localStorage.getItem('jwt')` + decodes payload to verify `exp`. `AdminRoute`: additionally checks `user.role === 'super_admin'` from JWT payload or user context.

---

## Toast System

`ToastContext` provides `showToast(message, type)`. `type`: `'success' | 'error' | 'info'`.

```tsx
// Provider wraps app root
<ToastProvider>
  <App />
</ToastProvider>

// Usage in any component
const { showToast } = useToast()
deploy.mutate(undefined, {
  onSuccess: () => showToast('Deployment started', 'success'),
  onError: (e) => showToast(e.message, 'error'),
})
```

Implementation: `useState<Toast[]>` in provider. Each toast has auto-dismiss timeout (4s success, 6s error). Rendered as fixed bottom-right stack via portal. Stacks up to 5; oldest dismissed first when limit reached.
