# 03 — User Flows

## 1. Platform Bootstrap

Runs once on first deployment. Idempotent if DB already exists.

```
zcplatform bootstrap
```

Steps:
1. Locate or create `zcplatform.db` (SQLite WAL mode).
2. Run migrations: create tables `users`, `otp_tokens`, `tenants`, `tenant_configs`, `channels`, `members`, `audit_log`, `usage_metrics`, `resource_snapshots`, `vault_keys`.
3. Generate vault key (XChaCha20-Poly1305), store in `vault_keys` with `version=1`.
4. Prompt for super-admin email. Create user row with `role=super_admin`, `is_active=true`.
5. Print confirmation. Server is ready to start with `zcplatform serve`.

---

## 2. Admin Login

**Frontend:** `/login` page (Login component)

```
User                    Frontend               Backend
 |-- enter email ------>|                        |
 |                      |-- POST /api/auth/otp -->|
 |                      |     { email }           | rate_limit check
 |                      |                         | generate 6-digit OTP
 |                      |                         | store otp_tokens (ttl 10min)
 |                      |                         | send email via SMTP
 |                      |<-- 200 OK -------------|
 |<- "check your email" |                        |
 |-- enter OTP code --->|                        |
 |                      |-- POST /api/auth/verify->|
 |                      |  { email, code }        | validate OTP
 |                      |                         | mark token used
 |                      |                         | sign JWT (24h, HS256)
 |                      |<-- { token, user } -----|
 |                      | store JWT in localStorage|
 |                      | redirect → /dashboard  |
```

Rate limit: 5 OTP requests per email per 15 minutes. Invalid code returns 401.

---

## 3. Create Tenant via SetupWizard

**Frontend:** `/tenants/new` → `SetupWizard` component (4-step form, step state in `useState`).

### Step 1 — Name + Plan

- User enters display name; slug auto-derived (`slugify(name)`).
- Selects plan tier (determines memory/CPU limits).
- Submit: `POST /api/tenants` with `{ name, slug, plan }`.
- Backend creates tenant row with `status=draft`, returns `{ id, slug }`.
- Frontend stores `tenantId` in wizard state, advances to step 2.

### Step 2 — Provider Setup

- Dropdown populated from `providerSchemas.ts` (14 providers, each with `ModelDef[]`).
- Select provider → select model from that provider's `ModelDef[]`.
- Enter API key in password field.
- "Test Connection" → `POST /api/tenants/{id}/config/test` with `{ provider, model, api_key }`.
  - Backend calls provider endpoint with a minimal prompt, returns `{ ok: true }` or error.
- On success: config saved to `tenant_configs` (API key vault-encrypted). Advance to step 3.

### Step 3 — Channel Setup

- Channel type picker (13 channel types from `channelSchemas.ts`).
- Selecting a type renders `FieldDef[]`-driven form (field types: `text`, `password`, `number`, `boolean`, `select`).
- "Add Channel" → `POST /api/tenants/{id}/channels` with channel fields.
  - Credential fields are vault-encrypted before DB insert.
- Multiple channels can be added. Each appears in a list below the form.
- "Skip" button is available; channel can be added later from TenantDetail → Channels tab.

### Step 4 — Deploy

- User clicks "Deploy". Frontend calls `POST /api/tenants/{id}/deploy`.
- Backend responds immediately with `202 Accepted`. Frontend begins polling `GET /api/tenants/{id}` every 2 seconds.
- Progress states displayed in UI as status transitions:

```
draft → deploying (provisioner starts)
  creating   — allocate slug/port/UID, create filesystem
  configuring — render config.toml, create Docker container
  starting   — docker start
  health check — poll container /health
running        — Caddy route registered, agent live
```

- On `status=running`: show success screen with agent URL. "Go to Tenant" navigates to `/tenants/{id}`.
- On `status=error`: show error message from `error_message` field. "Retry" or "Delete" options.

---

## 4. Tenant Management (TenantDetail Page)

**Route:** `/tenants/:id` — five tabs.

### Overview Tab
- Status badge, agent URL (with CopyButton), uptime, last health check timestamp.
- Restart / Stop / Delete action buttons (with ConfirmModal for destructive actions).
- Recent audit log entries (last 10).

### Config Tab
- Renders current `tenant_configs` values using `providerSchemas.ts` field definitions.
- Editable form; submit calls `PUT /api/tenants/{id}/config`.
- API key fields masked; "Reveal" button triggers re-entry.
- "Test Connection" available inline.

### Channels Tab
- Lists channels with type badge and enabled toggle.
- Add channel via same schema-driven form as wizard step 3.
- Delete channel with confirmation.
- Edit channel credentials: opens Modal with field form, `PUT /api/tenants/{id}/channels/{cid}`.

### Usage Tab
- Time-series charts (React Query polling every 60s from `GET /api/tenants/{id}/usage`).
- Metrics: requests/hour, tokens in/out, error rate.
- Selector for 1h / 24h / 7d window.

### Members Tab
- Table: email, role badge, joined date, actions.
- Invite via email input + role selector → `POST /api/tenants/{id}/members`.
- Change role: inline dropdown → `PUT /api/tenants/{id}/members/{uid}`.
- Remove member: `DELETE /api/tenants/{id}/members/{uid}` with confirmation.

---

## 5. Invite Member

Only users with `Manager` or `Owner` role on the tenant (or super-admin) can invite.

```
Manager                 Backend                 Invitee
 |-- POST /members  --->|                        |
 |   { email, role }    | create/find user row   |
 |                      | insert members row     |
 |                      | send OTP invite email  |
 |<-- 201 Created ------|                        |
 |                      |                        |-- open email link -->
 |                      |                        |-- /login?email=...  |
 |                      |                        | normal OTP login    |
 |                      |                        |<-- JWT + redirect   |
 |                      |                        |    to /tenants/{id} |
```

Roles: `Viewer` (read-only) < `Contributor` (can edit config/channels) < `Manager` (can invite/remove) < `Owner` (full control including delete).

---

## 6. Tenant Lifecycle Actions

All destructive actions require `ConfirmModal` with tenant name typed for confirmation.

| Action  | UI Trigger         | API Call                        | Result                         |
|---------|--------------------|---------------------------------|--------------------------------|
| Restart | Overview tab       | `POST /api/tenants/{id}/restart`| stop + start container         |
| Stop    | Overview tab       | `POST /api/tenants/{id}/stop`   | docker stop; status → stopped  |
| Start   | Overview tab       | `POST /api/tenants/{id}/start`  | docker start; status → running |
| Delete  | Overview tab       | `DELETE /api/tenants/{id}`      | stop + rm container, rm files, status → deleted |

State machine guards: Stop only if `running`. Start only if `stopped`. Delete from any non-`deleted` state. Backend returns `409 Conflict` if action is invalid for current state.

Audit log entry written for every lifecycle action with actor ID and IP.
