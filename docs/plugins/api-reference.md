# Plugin REST API Reference

The ZeroClaw gateway exposes REST endpoints for managing plugins programmatically. All endpoints require authentication (same auth as the rest of the gateway API).

> **Base path:** `/api/plugins`

---

## List plugins

```
GET /api/plugins
```

Returns all installed plugins with their current status.

**Response:**

```json
{
  "plugins": [
    {
      "name": "my-plugin",
      "version": "0.1.0",
      "description": "A friendly greeter",
      "status": "loaded",
      "tools": [
        {
          "name": "greet",
          "description": "Greet a user",
          "risk_level": "low",
          "parameters_schema": { "type": "object", "properties": { "name": { "type": "string" } } }
        }
      ],
      "capabilities": ["tool"],
      "allowed_hosts": [],
      "allowed_paths": {},
      "config": { "base_url": "https://api.example.com" },
      "host_capabilities": {
        "memory": { "read": true, "write": true },
        "context": { "session": true }
      }
    }
  ]
}
```

---

## Get plugin details

```
GET /api/plugins/:name
```

Returns full details for a single plugin, including manifest metadata and config status.

**Response:**

```json
{
  "name": "my-plugin",
  "version": "0.1.0",
  "description": "A friendly greeter",
  "author": "Your Name",
  "status": "loaded",
  "enabled": true,
  "wasm_path": "/home/user/.zeroclaw/plugins/my-plugin/plugin.wasm",
  "tools": [ ... ],
  "capabilities": ["tool"],
  "permissions": [],
  "allowed_hosts": [],
  "allowed_paths": {},
  "config": { "base_url": "https://api.example.com" },
  "config_status": {
    "api_key": { "set": true, "sensitive": true },
    "base_url": { "set": true, "sensitive": false }
  },
  "host_capabilities": { ... }
}
```

**Errors:**
- `404` -- Plugin not found

---

## Enable plugin

```
POST /api/plugins/:name/enable
```

Enable a previously disabled plugin. Removes it from the `disabled_plugins` list and loads it.

**Response:**

```json
{ "status": "ok", "message": "Plugin 'my-plugin' enabled" }
```

**Errors:**
- `404` -- Plugin not found

---

## Disable plugin

```
POST /api/plugins/:name/disable
```

Disable a plugin without removing it. Adds it to `disabled_plugins` and unloads it from the runtime.

**Response:**

```json
{ "status": "ok", "message": "Plugin 'my-plugin' disabled" }
```

**Errors:**
- `404` -- Plugin not found

---

## Update plugin config

```
PATCH /api/plugins/:name/config
```

Update non-sensitive configuration values for a plugin. Sensitive keys (those declared with `sensitive = true`) cannot be changed via the API.

**Request body:**

```json
{
  "base_url": "https://new.endpoint.com",
  "timeout": "60"
}
```

**Response:**

```json
{ "status": "ok", "message": "Config updated for 'my-plugin'" }
```

**Errors:**
- `400` -- Attempted to update a sensitive key
- `404` -- Plugin not found

---

## Authentication

All plugin API endpoints require the same authentication as other gateway endpoints. Unauthenticated requests return `401`.

See the [gateway auth documentation](../reference/api/config-reference.md) for details on configuring API authentication.
