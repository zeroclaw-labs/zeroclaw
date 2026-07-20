# Inkbox skills

Workflow guidance for the native `inkbox` channel and its agent tools (email,
SMS/MMS, iMessage, voice), using the ZeroClaw tool names (`inkbox_send_email`,
`inkbox_list_text_conversations`, `inkbox_place_call`, …).

## Install

ZeroClaw loads skills from each agent's workspace, not from this repo. Copy the
skill folders into the target agent's skills directory:

```bash
cp -r skills/inkbox-* ~/.zeroclaw/agents/<agent-alias>/workspace/skills/
```

Each folder holds one `SKILL.md` (YAML frontmatter + a markdown body). The
runtime discovers them on startup via `load_skills_for_agent`.

## Skills

| Skill | Use when |
|---|---|
| `inkbox-email-triage` | review the inbox and reply to email |
| `inkbox-sms-responder` | handle inbound SMS/MMS, including group texts |
| `inkbox-imessage-responder` | handle iMessage conversations |
| `inkbox-outbound-calling` | place a live phone call and talk to someone |
| `inkbox-troubleshooting` | diagnose setup (identity, number, tunnel) |
