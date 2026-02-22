-- Users
CREATE TABLE IF NOT EXISTS users (
    id              TEXT PRIMARY KEY,
    email           TEXT NOT NULL UNIQUE,
    name            TEXT,
    is_super_admin  INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- OTP tokens
CREATE TABLE IF NOT EXISTS otp_tokens (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    hash        TEXT NOT NULL,
    salt        TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    used        INTEGER NOT NULL DEFAULT 0,
    attempts    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_otp_user ON otp_tokens(user_id);

-- Tenants
CREATE TABLE IF NOT EXISTS tenants (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    slug            TEXT NOT NULL UNIQUE,
    status          TEXT NOT NULL DEFAULT 'stopped',
    plan            TEXT NOT NULL DEFAULT 'free',
    port            INTEGER NOT NULL UNIQUE,
    uid             INTEGER NOT NULL UNIQUE,
    container_id    TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_tenants_slug ON tenants(slug);
CREATE INDEX IF NOT EXISTS idx_tenants_status ON tenants(status);

-- Tenant config (encrypted fields)
CREATE TABLE IF NOT EXISTS tenant_configs (
    tenant_id       TEXT PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    api_key_enc     TEXT NOT NULL,
    provider        TEXT NOT NULL DEFAULT 'openrouter',
    model           TEXT NOT NULL DEFAULT 'anthropic/claude-sonnet-4-20250514',
    temperature     REAL NOT NULL DEFAULT 0.7,
    system_prompt   TEXT,
    autonomy_level  TEXT NOT NULL DEFAULT 'supervised',
    extra_json      TEXT
);

-- Channels per tenant (encrypted secrets)
CREATE TABLE IF NOT EXISTS channels (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    config_enc  TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_channels_tenant ON channels(tenant_id);

-- Tenant members (RBAC)
CREATE TABLE IF NOT EXISTS members (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'viewer',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(user_id, tenant_id)
);
CREATE INDEX IF NOT EXISTS idx_members_tenant ON members(tenant_id);
CREATE INDEX IF NOT EXISTS idx_members_user ON members(user_id);

-- Audit log
CREATE TABLE IF NOT EXISTS audit_log (
    id          TEXT PRIMARY KEY,
    actor_id    TEXT,
    action      TEXT NOT NULL,
    resource    TEXT NOT NULL,
    resource_id TEXT,
    details     TEXT,
    ip_address  TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_log(actor_id);

-- Usage metrics
CREATE TABLE IF NOT EXISTS usage_metrics (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    period      TEXT NOT NULL,
    messages    INTEGER NOT NULL DEFAULT 0,
    tokens_in   INTEGER NOT NULL DEFAULT 0,
    tokens_out  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(tenant_id, period)
);
CREATE INDEX IF NOT EXISTS idx_usage_tenant ON usage_metrics(tenant_id);

-- Vault key metadata
CREATE TABLE IF NOT EXISTS vault_keys (
    version     INTEGER PRIMARY KEY,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    retired_at  TEXT
);
