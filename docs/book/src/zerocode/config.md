# Config pane

zerocode's **Config** pane is the way to configure a running ZeroClaw. Each
setting has a typed control, validation, and an inline explanation of what it
does, and most settings apply live without a daemon restart. Open it from any
zerocode session and edit settings there rather than hand editing the config
file.

Settings still persist to `config.toml` in your config directory, and the docs
quote the relevant TOML so you can see exactly what a given control writes. Read
those TOML blocks as a description of the persisted result, not as an
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

## When to edit the file directly

Hand editing `config.toml` is the right tool only when a Config pane is not
reachable:

- Bootstrapping a fresh **headless host** before the first zerocode connection.
- **Scripted provisioning**, where the config is written by automation.

In those cases the docs show the TOML to write. Everywhere else, treat the TOML
blocks as the result of a Config pane change, not a call to open an editor.
