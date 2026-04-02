# Plugin CLI Reference

All plugin management commands are under `zeroclaw plugin`.

## `zeroclaw plugin list`

List all installed plugins with their status.

```bash
zeroclaw plugin list
```

Output includes: name, version, status (loaded/disabled), tool count, and capabilities.

---

## `zeroclaw plugin info <name>`

Show detailed information about a specific plugin.

```bash
zeroclaw plugin info my-plugin
```

Displays: manifest fields, registered tools, allowed hosts/paths, host capabilities, configuration keys, and WASM hash.

---

## `zeroclaw plugin install <source>`

Install a plugin from a directory or URL.

```bash
zeroclaw plugin install ./path/to/plugin-dir
```

Copies the manifest and WASM binary into the configured `plugins_dir`, then loads the plugin.

---

## `zeroclaw plugin remove <name>`

Remove an installed plugin.

```bash
zeroclaw plugin remove my-plugin
```

Unloads the plugin from the runtime and removes it from the `plugins_dir`.

---

## `zeroclaw plugin reload`

Re-scan the plugins directory and reload all plugins.

```bash
zeroclaw plugin reload
```

Output is a `ReloadSummary`:

```
Reload complete:
  Total: 5
  Loaded: [new-plugin]
  Unloaded: [removed-plugin]
  Failed: []
```

Use this after adding, removing, or updating plugin files on disk without restarting the agent.

---

## `zeroclaw plugin audit <path>`

Audit a plugin manifest without installing it. Use this to review what a plugin requests before trusting it.

```bash
zeroclaw plugin audit ./manifest.toml
```

Example output:

```
Plugin: weather-plugin v1.2.0 by WeatherCorp
  "Real-time weather data for any city"

Network access:
  - api.openweathermap.org

Filesystem access:
  - cache → /tmp/weather-cache (read/write)

Host capabilities:
  memory: read
  context: session

Permissions:
  - http_client

Tools (2):
  get_forecast   [low]    — Get weather forecast for a city
  severe_alerts  [medium] — Check for severe weather alerts in a region
```

---

## `zeroclaw plugin doctor`

Run diagnostic checks on all installed plugins.

```bash
zeroclaw plugin doctor
```

Each plugin gets a set of checks with Pass/Warn/Fail status:

```
my-plugin:
  [PASS] Manifest valid
  [PASS] WASM binary exists
  [PASS] Hash verification
  [WARN] No integrity sidecar (.wasm.sha256)
  [PASS] Capabilities consistent

weather-plugin:
  [PASS] Manifest valid
  [FAIL] WASM binary missing
  [FAIL] Config key 'api_key' required but not set
```

Use this to diagnose plugin loading failures or configuration issues.

---

## `zeroclaw plugin enable <name>` / `zeroclaw plugin disable <name>`

Toggle a plugin's enabled state without removing it.

```bash
zeroclaw plugin disable untrusted-plugin
zeroclaw plugin enable untrusted-plugin
```

Disabled plugins remain on disk but are skipped during loading. State is persisted in `config.toml` under `[plugins].disabled_plugins`.
