# ZeroClaw with ChatGPT

This skills-only package creates a fresh local ZeroClaw instance through the
native onboarding command. It requires a terminal and a local browser in
ChatGPT Work or Codex, plus a ZeroClaw release with `native-onboard` and ChatGPT
plan function tools. Check `zeroclaw native-onboard --help` before setup.

The package does not install a second configuration writer. The native command
claims the instance root, performs signed Sign in with ChatGPT, validates the
grant in the canonical encrypted auth store, and applies explicit provider,
agent, and risk references through Quickstart. Cancellation retains recoverable
owned state and does not report the instance as ready. YOLO must be selected for
this particular instance; it grants full autonomy without approval gates or
workspace scoping.

## Install locally

Extract the standalone ZIP or use this folder as the local marketplace root:

```sh
codex plugin marketplace add /absolute/path/to/zeroclaw-chatgpt
codex plugin add zeroclaw-chatgpt@zeroclaw-chatgpt-local
```

In the ChatGPT desktop app, restart the app after registering the local source,
open the Plugins Directory, and select the ZeroClaw ChatGPT setup marketplace.
Install ZeroClaw with ChatGPT, then start a new conversation and ask it to set up
a new local instance. Registration and installation do not grant inference;
the user grants ChatGPT plan usage in the native browser flow during setup.

Use `codex plugin marketplace list` and `codex plugin list` to inspect the local
source. Remove an installation through the client's plugin controls. Manage the
issued app's plan access in ChatGPT Settings → Usage. This package contains no
OAuth token, native Codex credential import, hosted MCP server, or lifecycle hook.

## Validate setup

The onboarding skill reports only nonsecret references and effective policy.
Configuration success is separate from a completed model or tool check.
Validation consumes the selected account's ChatGPT allowance. A catalog entry
alone does not prove entitlement, and failures do not select metered API billing.

The icon is the existing ZeroClaw application icon from
`apps/tauri/icons/icon.svg`, packaged here so the folder is self-contained.
The root manifest is canonical. `build_package.py` materializes its Codex
compatibility overlay in the reproducible standalone ZIP; tests check the
directory's overlay for drift.

```sh
python3 -I -B -m unittest discover -s tests -v
python3 -I -B build_package.py /path/to/zeroclaw-chatgpt.zip
```

Format and distribution: [OpenAI plugin packaging](https://developers.openai.com/plugins/build/plugins).
Grant and capability limits: [Sign in with ChatGPT preview](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
