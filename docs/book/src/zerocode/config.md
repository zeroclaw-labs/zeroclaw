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

## Keybinding modifiers

Keybindings use canonical modifier names: `control` is literal Control, `primary` is Command on macOS and Control elsewhere, and `super` is literal Super/Command. For example, `control+c`, `primary+r`, and `alt+shift+up` are portable persisted values. Older `ctrl+...` values are migrated once when the config loads and rewritten to the corresponding canonical spelling.

## Why the pane over the file

- **Validation.** Controls reject malformed values before they reach the
  daemon, so a typo cannot leave the config in a state that fails to load.
- **Discoverability.** Every setting carries an inline description, so you do
  not have to cross-reference the config reference to know what a field does.
- **Live apply.** Most settings take effect on the next frame, with no restart.
- **Registry-backed lists.** Provider, channel, model, and theme choices come
  from the backend registry, so the options you see are exactly the ones this
  build supports.

## Plugins sub-tab

The Config pane has three sub-tabs: `zeroclaw` for the daemon's settings,
`zerocode` for zerocode's own settings, and `plugins`. `Tab` moves to the
next sub-tab by default, and clicking a sub-tab name selects it.

The `plugins` sub-tab is a read-only view of the connected daemon's plugin
catalog, the same package catalog the dashboard's Plugins page shows. It lists
installed packages and packages in the daemon's cached registry, one row per
package, in the order the daemon returns them. A filled dot marks a package
with an installed record and a hollow dot marks one that is only in the cached
registry. The filters on the left show all packages, only installed ones, or
only ones in the registry. When the daemon cannot read a catalog source, the
pane shows that source's facts as unknown rather than absent: a package with no
installed record gets a neutral `?` marker instead of the hollow dot, its
detail says the source could not be read, the filter for that source shows
`(?)` instead of a count, and the total reads as a lower bound, such as
`All (3+)`.

- **Installed and registry records stay separate.** When a package is
  installed at one version and listed in the registry at another, its row
  names both versions. Press `Enter` on a package to open its detail: the
  installed record and the registry record each get their own section with
  their own version, description, and capabilities, plus the requested
  permissions of the installed record and the `name@version` identity of the
  registry record. The two capability lists are never merged, and the pane
  never says which version is newer.
- **No runtime status.** Appearing in the catalog does not mean a plugin is
  loaded, running, or healthy, and the pane never claims any of those.
  `[plugins] enabled in config` reports the `plugins.enabled` setting, which
  is configuration intent.
- **Read-only.** The sub-tab never changes configuration and never installs,
  removes, enables, or disables a package. Change `plugins.*` settings in the
  `zeroclaw` sub-tab and manage packages with `zeroclaw plugin`.

The sub-tab needs a daemon that serves the `plugins/list` RPC method and was
built with WASM plugin support. A daemon without the method shows a message
saying it does not provide the catalog. A daemon built without WASM plugin
support shows that as its own state, not as an empty catalog. The connection
also needs the `plugins:read` grant; when the daemon refuses the request, the
pane shows the reason the daemon gave. When the daemon cannot read a catalog
source (the installed plugin directory or the cached registry), the left
column names the source and the details are in the daemon log.

The catalog loads the first time you open the sub-tab. Press `r` to refresh
it. The previous rows stay on screen, marked as refreshing, until the new
result arrives; if the refresh fails, the error replaces them. Refreshing
rereads what the daemon has on disk and does not download the registry:
`zeroclaw plugin search` updates the cached registry.

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

The shell-level agent sidebar is stored in the same file. The default section
is serialized explicitly so environment overrides have a schema node to
target:

```toml
[sidebar]
visible = true # show the agent/session sidebar at launch
width = 24     # target width in terminal columns
```

Press `Ctrl+B` to show or hide the sidebar. Quickstart remains reachable from
the keyboard mode bar and also appears as a sidebar launcher, including at
narrow terminal widths. Selecting an existing agent from the sidebar starts a
new Chat or Code session without replacing the other sessions already tracked
by that pane.

TodoWrite values are re-read at every session boundary, so an edit made in the
Config pane applies to the next session you start, restart, or switch to, with
no zerocode restart needed.

### Environment overrides

Any field can be overridden for a single run with a `ZEROCODE_` variable. The
spelling is the prefix followed by the lowercase config path, with `.` written
as `__`:

```sh
ZEROCODE_todotracker__enabled=false zerocode
ZEROCODE_todotracker__location=bottom zerocode
ZEROCODE_sidebar__visible=false zerocode
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
