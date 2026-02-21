# Community Presets

Community-contributed preset payloads and templates.

## File Layout

- `template.preset.json`: starter template for new submissions.

## Submit a New Preset

1. Copy the template:

```bash
cp presets/community/template.preset.json presets/community/my-team-automation.json
```

2. Edit values:

- `id`: lowercase, stable identifier (for example `my-team-automation`)
- `title`: short human-readable name
- `description`: clear workflow intent
- `packs`: list of known pack IDs from `src/onboard/feature_packs.rs`
- `metadata`: author, compatibility, and tags

3. Validate locally:

```bash
python3 scripts/validate_preset_payload.py presets/community/my-team-automation.json
```

4. Run Rust quality checks:

```bash
cargo fmt --all
cargo check --locked
cargo test --locked --lib presets::tests onboard::feature_packs::tests
```

5. Open PR and include:

- Use case and target audience
- Why current official presets are insufficient
- Security/risk impact (if any)

## Security and Privacy Rules

- Never include secrets, keys, tokens, cookies, or credentials.
- Keep `config_overrides` focused on non-secret behavior defaults.
- Mark risk-sensitive workflows clearly in PR description.
