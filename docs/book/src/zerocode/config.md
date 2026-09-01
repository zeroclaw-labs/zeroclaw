# Config pane

zerocode's **Config** pane is the way to configure a running ZeroClaw. Each
setting has a typed control, validation, and an inline explanation of what it
does, and most settings apply live without a daemon restart. Open it from any
zerocode session and edit settings there rather than hand editing the config
file.

Settings still persist to your config, and the docs
describe the relevant fields so you can see exactly what a given control writes. Read
those descriptions as the persisted result, not as an
instruction to open the file in an editor. Hand editing is a fallback for
headless hosts and scripted provisioning, where the docs call it out
explicitly.

## Why the pane over the file

- **Validation.** Controls reject malformed values before they reach the
  daemon, so a typo cannot leave the config in a state that fails to load.
- **Discoverability.** Every setting carries an inline description, so you do
  not have to cross-reference the config reference to know what a field does.
- **Live apply.** Most settings take effect on the next frame, with no restart.
- **Registry-backed lists.** Provider, channel, model, and theme choices come
  from the backend registry, so the options you see are exactly the ones this
  build supports.

## Local UI settings (`zerocode-config.toml`)

Some settings describe how *zerocode itself* draws its panes rather than how the
daemon behaves. Those live in zerocode's own file,
`<config-dir>/zerocode-config.toml`, and are edited from zerocode's **Config**
pane.

The TodoWrite tracker is one of them. It is a display-only concern: the daemon
just emits plan updates, and over ACP the client controls formatting entirely,
so it is owned by zerocode:

```toml
[todotracker]
enabled = true           # master switch; when false the tracker never renders
enabled_at_start = false # visible at launch, before the first plan arrives
location = "right"       # "bottom", "left", or "right"
width = 32               # side-panel target column width (left/right)
max_height = 5           # bottom-strip maximum height in rows
```

Values are re-read at every session boundary, so an edit made in the Config pane
applies to the next session you start, restart, or switch to, with no zerocode
restart needed.

### Environment overrides

Any field can be overridden for a single run with a `ZEROCODE_` variable. The
spelling is the prefix followed by the lowercase config path, with `.` written
as `__`:

```sh
ZEROCODE_todotracker__enabled=false zerocode
ZEROCODE_todotracker__location=bottom zerocode
```

These overrides are process-transient: they affect the running instance only and
are never written back to `zerocode-config.toml`. Saving an unrelated field in
the Config pane will not bake an env-injected value into the file.

### Upgrading from a daemon-owned `[todotracker]`

Before this setting moved, `[todotracker]` was a section of the *daemon's*
`config.toml`. If you set it there, copy the values across:

1. Open your daemon `config.toml` and note the `[todotracker]` values.
2. Put the same block into `<config-dir>/zerocode-config.toml` (shown above), or
   set them from **Config → Todo tracker**.
3. Delete the `[todotracker]` section from the daemon `config.toml`.

Existing `ZEROCLAW_todotracker__*` environment variables do **not** need to be
removed before upgrading: the five recognized fields (`enabled`,
`enabled_at_start`, `location`, `width`, `max_height`) are accepted and ignored
by the daemon so a previously working deployment still starts. They no longer
have any effect, so move them to the `ZEROCODE_` spelling above.
