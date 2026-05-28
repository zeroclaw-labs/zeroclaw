# zeroclaw

100% Rust autonomous AI agent runtime — multi-provider, multi-channel,
deploys from Raspberry Pi to Kubernetes.

```sh
npx zeroclaw --help        # try without installing
npm install -g zeroclaw    # install globally
```

The `postinstall` script downloads the platform-specific native binary
from the matching GitHub Release into `./native/` (5 targets supported:
linux x64/arm64, macOS x64/arm64, windows x64).

## Environment overrides

| Variable                       | Purpose                                                     |
|--------------------------------|-------------------------------------------------------------|
| `ZEROCLAW_RELEASE_REPO`        | Override the GitHub repo (default `zeroclaw-labs/zeroclaw`) |
| `ZEROCLAW_DOWNLOAD_BASE`       | Override the release-asset base URL                         |
| `ZEROCLAW_SKIP_POSTINSTALL=1`  | Skip the binary download (CI / Docker)                      |

## Source

See https://github.com/zeroclaw-labs/zeroclaw for the full repo, docs,
and release notes.
