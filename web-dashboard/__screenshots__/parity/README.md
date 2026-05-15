# Visual-parity screenshots — DEFERRED

The plan (`.omc/plans/multi-session-dashboard.md` §12, lines 764–769)
asks for a side-by-side screenshot comparison between OpenClaw's `/`
running against their gateway and our `/` running against ZeroClaw's
gateway, demonstrating:

- (a) Same layout topology: sidebar ≤ ±10% width, same column count,
      same primary-action placement
- (b) Same information architecture: slot sidebar at left, chat view
      centre, optional right-panel for side results
- (c) Theme switching producing visibly distinct color palettes
- (d) NO requirement for matching colors, typography, or branding

## Status

**Not captured yet.** Capturing this requires running OpenClaw's
gateway locally (cloning `openclaw/openclaw`, configuring it with a
provider, opening its UI) alongside ZeroClaw's gateway. That tooling
isn't set up here, and the comparison would be premature anyway:
ZeroClaw's persona system (M4a) and Board page (M4b) bring more
substantial visual surface than M3 alone.

The right time to capture parity screenshots is **after M4b** when
the dashboard has Chat + Board + Settings drawer wired — the three
surfaces OpenClaw is most recognisable for. At that point a single
30-minute session can produce all the deliverable images.

## How to capture (when ready)

1. Clone OpenClaw and bring up its gateway:
   ```bash
   git clone https://github.com/openclaw/openclaw && cd openclaw
   # follow OpenClaw's README — typically a `cargo run` + `npm run dev`
   ```
2. Open OpenClaw's `/` in one browser window; ZeroClaw's `/dashboard/`
   in another at the same viewport size (1440×900 is OpenClaw's
   default).
3. Capture three pairs (one per theme value).
4. Drop the PNGs in this directory with names like
   `default-light.openclaw.png` / `default-light.zeroclaw.png`.
5. Update this README with a short note flagging deliberate divergences
   (per §12: ZeroClaw owns colors/typography/branding).

Tracked in the post-M4b cleanup of the dashboard milestone.
