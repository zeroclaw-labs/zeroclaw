# YOLO Mode

**YOLO mode** disables every safety gate ZeroClaw ships with. No approval prompts, no workspace boundary, no shell policy, no command allow/denylist, no OTP, no sandbox. The agent can run any shell command, touch any file, hit any URL: immediately, without asking.

> **This is for dev boxes, home labs, and throwaway VMs.** Do not run YOLO mode on shared infrastructure. Do not run YOLO mode on a machine with production credentials in its environment. Do not run YOLO mode if you do not understand what an autonomous agent with `rm -rf` access can do.

## When YOLO is the right call

- A dev box where you're iterating fast and approval prompts slow you down
- A throwaway container/VM used for agent experiments
- A home-lab SBC where you own every byte on the machine
- CI/CD pipelines where the agent's actions are reviewed before merge

## When YOLO is the wrong call

- Your laptop with your email, your browser profile, and SSH keys to production
- A shared server
- A VPS with live customers on it
- Anywhere the agent might be reached by an untrusted user through a channel: a YOLO agent with a public Telegram bot is a Telegram-accessible root shell

## Enabling it

Name the YOLO posture explicitly on a dedicated risk profile (`yolo` is a good intent-naming choice) and point your agent at it:

```toml
[agents.devbox]
model_provider = "anthropic.home"
risk_profile   = "yolo"             # references the YOLO profile below
runtime_profile = "loose"           # high iteration cap; independent of risk_profile

[risk_profiles.yolo]
level                            = "full"
workspace_only                   = false
require_approval_for_medium_risk = false
block_high_risk_commands         = false
allowed_commands                 = []
forbidden_paths                  = []
sandbox_enabled                  = false
sandbox_backend                  = "none"

[runtime_profiles.loose]
max_tool_iterations  = 100
max_actions_per_hour = 1000

[security.otp]
enabled = false

[security.estop]
enabled = false

[gateway]
require_pairing = false
```

If multiple agents share the host, give the YOLO-bound one its own profile (the `yolo` block) and keep your other agents on a stricter profile (e.g. `hardened`): `[risk_profiles.<alias>]` is per-profile, so a YOLO agent and a hardened agent can coexist in the same config.

```toml
[agents.devbox]
risk_profile = "yolo"               # this one runs wide-open

[agents.publicbot]
risk_profile = "hardened"           # this one stays gated
channels     = ["telegram.home"]

[risk_profiles.hardened]
level                            = "supervised"
workspace_only                   = true
require_approval_for_medium_risk = true
block_high_risk_commands         = true
```

## What you lose

| Guard | Normal behaviour | YOLO behaviour |
|---|---|---|
| Autonomy | Medium-risk ops need operator approval | Agent runs everything unattended |
| Workspace boundary | Agent can only touch `~/.zeroclaw/workspace/` | Agent can touch any path its user can |
| Shell policy | Unknown commands blocked | Any command executes |
| Forbidden paths | `/etc`, `/sys`, `/boot`, `~/.ssh` etc. blocked | No path is off-limits |
| Sandbox | Docker / Firejail / Landlock / Seatbelt isolates tool execution | Tools run as the ZeroClaw process user |
| OTP gating | Gated actions require a code | No gate |
| Emergency stop | `zeroclaw estop` halts running ops | No halt semantics beyond `SIGTERM` |
| Gateway pairing | Clients must pair first | Anyone who reaches the port owns the agent |

## What you keep

YOLO mode doesn't lobotomise the agent:

- **[Tool receipts](../security/tool-receipts.md)** still get written. You can `tail -f` the receipts log and see exactly what ran.
- **[Audit logging](../ops/observability.md)** still works if enabled (`[security.audit] enabled = true`). Strongly recommended in YOLO.
- **Conversation memory** still persists: there's still a record of what happened.

You're not turning off the logs, you're turning off the approval gates and path enforcement.

## Reverting

Delete the YOLO settings from the risk profile, or flip `[risk_profiles.<alias>] level = "supervised"` back and restart the service. Nothing persists across config changes: each startup loads the current config fresh.

## See also

- [Security → Autonomy levels](../security/autonomy.md): the full gradient between YOLO and paranoid
- [Security → Tool receipts](../security/tool-receipts.md): the audit trail you should keep on even in YOLO
- [Philosophy](../philosophy.md): why this exists as an escape hatch rather than a default
