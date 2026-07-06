# Language & translations

ZeroClaw's interface strings (CLI messages, command help, and the `zerocode`
TUI) can be shown in languages other than English. English is always built in;
other languages are downloaded on demand.

## Set your language

ZeroClaw reads a top-level `locale` key from your config. Set it to a locale
code such as `ja`, `fr`, or `zh-CN`. If `locale` is unset, ZeroClaw uses your
operating system's language and falls back to English when no translation is
available.

## Fetch your language files

English ships inside the binary. For any other language you fetch the
translated files once:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw locales fetch ja
```

</div>

This downloads the Japanese translation files from the ZeroClaw project and
installs them under `~/.zeroclaw/data/ftl/ja/`, where ZeroClaw looks for them
at startup. Restart ZeroClaw (and `zerocode`) afterward to pick them up.

Fetch any locale the same way:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw locales fetch fr        # French
zeroclaw locales fetch zh-CN     # Simplified Chinese
```

</div>

### Fetching only part of a language

By default `fetch` downloads every catalogue for the locale. To download only
some, pass `--catalog` with a comma-separated list:

| Catalog | Covers |
|---|---|
| `cli` | CLI messages and command help |
| `tools` | Built-in tool descriptions |
| `zerocode` | The `zerocode` terminal UI |

<div class="os-tabs-src">

#### sh

```sh
zeroclaw locales fetch ja --catalog cli            # just CLI strings
zeroclaw locales fetch ja --catalog cli,zerocode   # CLI + the TUI
```

</div>

If a catalogue has not been translated for your language yet, `fetch` skips it
and tells you: the catalogues that do exist are still installed.

## Where the files live

| Path | What |
|---|---|
| `~/.zeroclaw/data/ftl/<locale>/cli.ftl` | CLI message translations |
| `~/.zeroclaw/data/ftl/<locale>/tools.ftl` | Tool description translations |
| `~/.zeroclaw/data/ftl/<locale>/zerocode.ftl` | `zerocode` TUI translations |

If you run ZeroClaw with a custom config directory (`--config-dir` or
`ZEROCLAW_CONFIG_DIR`), the files install under that directory's `data/ftl/`
instead.

## Troubleshooting

- **Still seeing English after fetching.** Confirm `locale` in your config
  matches the locale you fetched, and restart the process. ZeroClaw loads
  language files at startup.
- **`fetch` reports a catalogue was skipped.** That catalogue has not been
  translated for your locale yet. The available catalogues are still installed;
  untranslated strings fall back to English.
- **A specific string is in English even though the rest is translated.** That
  individual string has no translation yet and falls back to English by design.
