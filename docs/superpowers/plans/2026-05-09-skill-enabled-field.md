# Skill `enabled` Field Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `enabled: bool` field to SKILL.md/SKILL.toml so users can disable a skill without deleting it; disabled skills stay visible in `zeroclaw skills list`.

**Architecture:** Add `enabled` to `Skill`, `SkillMeta`, and `SkillMarkdownMeta`; parse the field in `parse_simple_frontmatter`; filter disabled skills out of `skills_to_prompt` and `skills_to_tools`; display `[disabled]` in the CLI list.

**Tech Stack:** Rust, serde, existing `console` crate for CLI styling.

---

## Files Changed

- Modify: `crates/zeroclaw-runtime/src/skills/mod.rs` — structs, parser, prompt/tool filters, inline tests
- Modify: `src/skills/mod.rs` — `skills list` display

---

### Task 1: Add failing tests for `enabled` field parsing and filtering

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/mod.rs` (append to the existing `#[cfg(test)]` block at line ~1748, inside `mod prompts_section_tests`)

- [ ] **Step 1: Add failing tests**

Append the following two test functions inside `mod prompts_section_tests` (the block at line ~1748 in `crates/zeroclaw-runtime/src/skills/mod.rs`):

```rust
#[test]
fn enabled_false_is_parsed_from_md_frontmatter() {
    let content = "---\nname: test-skill\ndescription: test\nenabled: false\n---\n\nBody";
    let parsed = parse_skill_markdown(content);
    assert_eq!(parsed.meta.enabled, Some(false));
}

#[test]
fn enabled_true_is_parsed_from_md_frontmatter() {
    let content = "---\nname: test-skill\ndescription: test\nenabled: true\n---\n\nBody";
    let parsed = parse_skill_markdown(content);
    assert_eq!(parsed.meta.enabled, Some(true));
}

#[test]
fn enabled_defaults_to_true_when_absent() {
    let content = "---\nname: test-skill\ndescription: test\n---\n\nBody";
    let parsed = parse_skill_markdown(content);
    assert_eq!(parsed.meta.enabled, None); // None → caller defaults to true
}

#[test]
fn disabled_skill_excluded_from_prompt() {
    let tmp = TempDir::new().unwrap();
    // Write a disabled skill
    let skill_dir = tmp.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: disabled\nenabled: false\n---\n\nDo stuff.",
    )
    .unwrap();

    let skills = load_skills_from_directory(tmp.path(), false);
    // skill is loaded
    assert_eq!(skills.len(), 1);
    assert!(!skills[0].enabled);

    // but excluded from prompt
    let prompt = skills_to_prompt(&skills, tmp.path());
    assert!(!prompt.contains("my-skill"), "disabled skill must not appear in prompt");
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo test -p zeroclaw-runtime enabled_ -- --nocapture 2>&1 | head -40
```

Expected: compile error — `enabled` field does not exist yet on `SkillMarkdownMeta`.

---

### Task 2: Extend data structures with `enabled` field

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/mod.rs`

- [ ] **Step 1: Add `default_true` helper after `default_version`**

After line 104 (`fn default_version() -> String { ... }`), add:

```rust
fn default_true() -> bool {
    true
}
```

- [ ] **Step 2: Add `enabled` to `Skill` struct**

In the `Skill` struct (starts at line 40), add after the `prompts` field:

```rust
    #[serde(default = "default_true")]
    pub enabled: bool,
```

- [ ] **Step 3: Add `enabled` to `SkillMeta` struct**

In the `SkillMeta` struct (starts at line 80), add after the `prompts` field:

```rust
    #[serde(default = "default_true")]
    enabled: bool,
```

- [ ] **Step 4: Add `enabled` to `SkillMarkdownMeta` struct**

In the `SkillMarkdownMeta` struct (starts at line 93), add:

```rust
    enabled: Option<bool>,
```

Full struct after change:
```rust
#[derive(Debug, Clone, Default)]
struct SkillMarkdownMeta {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
    author: Option<String>,
    tags: Vec<String>,
    enabled: Option<bool>,
}
```

- [ ] **Step 5: Run `cargo check` to confirm structs compile**

```
cargo check -p zeroclaw-runtime 2>&1 | head -30
```

Expected: errors about missing `enabled` in struct initialization — `load_skill_toml`, `load_skill_md`, `load_open_skill_md` not yet updated.

---

### Task 3: Wire `enabled` through loaders and parser

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/mod.rs`

- [ ] **Step 1: Parse `"enabled"` in `parse_simple_frontmatter`**

In the `match key { ... }` block (around line 766), add a new arm **before** the `_ => {}` catch-all:

```rust
"enabled" => {
    meta.enabled = match val.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    };
}
```

- [ ] **Step 2: Pass `enabled` in `load_skill_toml`**

In the `Ok(Skill { ... })` construction inside `load_skill_toml` (around line 615), add:

```rust
enabled: manifest.skill.enabled,
```

- [ ] **Step 3: Pass `enabled` in `load_skill_md`**

In the `Ok(Skill { ... })` construction inside `load_skill_md` (around line 637), add:

```rust
enabled: parsed.meta.enabled.unwrap_or(true),
```

- [ ] **Step 4: Pass `enabled` in `load_open_skill_md`**

In the `Ok(finalize_open_skill(Skill { ... }))` construction inside `load_open_skill_md` (around line 670), add:

```rust
enabled: true,
```

- [ ] **Step 5: Run `cargo check` to confirm no errors**

```
cargo check -p zeroclaw-runtime 2>&1 | head -30
```

Expected: clean compile (or only unrelated warnings).

- [ ] **Step 6: Run the failing tests again**

```
cargo test -p zeroclaw-runtime enabled_ -- --nocapture 2>&1 | head -40
```

Expected: parsing tests pass; `disabled_skill_excluded_from_prompt` still fails (filtering not yet done).

---

### Task 4: Filter disabled skills from prompt and tools

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/mod.rs`

- [ ] **Step 1: Skip disabled skills in `skills_to_prompt`**

In `skills_to_prompt_with_mode`, the `for skill in skills {` loop (around line 897). Change it to:

```rust
for skill in skills {
    if !skill.enabled {
        continue;
    }
    // ... rest of loop unchanged
```

- [ ] **Step 2: Skip disabled skills in `skills_to_tools`**

In `skills_to_tools`, the `for skill in skills {` loop (around line 991). Change it to:

```rust
for skill in skills {
    if !skill.enabled {
        continue;
    }
    // ... rest of loop unchanged
```

- [ ] **Step 3: Run all tests**

```
cargo test -p zeroclaw-runtime 2>&1 | tail -20
```

Expected: all tests pass including `disabled_skill_excluded_from_prompt`.

- [ ] **Step 4: Commit**

```
git add crates/zeroclaw-runtime/src/skills/mod.rs
git commit -m "feat(skills): add enabled field to skip disabled skills from prompt and tools"
```

---

### Task 5: Show `[disabled]` in `zeroclaw skills list`

**Files:**
- Modify: `src/skills/mod.rs`

- [ ] **Step 1: Update the list display**

In `handle_command`, inside the `SkillCommands::List` branch, find the `for skill in &skills {` loop (around line 41). Replace the `println!` for the skill name line:

**Before:**
```rust
println!(
    "  {} {} — {}",
    console::style(&skill.name).white().bold(),
    console::style(format!("v{}", skill.version)).dim(),
    skill.description
);
```

**After:**
```rust
let disabled_tag = if skill.enabled {
    String::new()
} else {
    format!(" {}", console::style("[disabled]").red().dim())
};
println!(
    "  {}{} {} — {}",
    console::style(&skill.name).white().bold(),
    disabled_tag,
    console::style(format!("v{}", skill.version)).dim(),
    skill.description
);
```

- [ ] **Step 2: Run `cargo check` on the binary**

```
cargo check -p zeroclaw 2>&1 | head -20
```

Expected: clean compile.

- [ ] **Step 3: Build and smoke-test**

```
cargo build --release 2>&1 | tail -5
./target/release/zeroclaw skills list
```

Expected: installed skills listed; any skill with `enabled: false` in its frontmatter shows `[disabled]` in red after the name.

- [ ] **Step 4: Commit**

```
git add src/skills/mod.rs
git commit -m "feat(skills): show [disabled] badge in skills list for disabled skills"
```

---

### Task 6: End-to-end manual verification

- [ ] **Step 1: Set a real skill to disabled**

Pick any installed skill (e.g. `~/.zeroclaw/workspace/skills/deep-web-research/SKILL.md`). Add `enabled: false` to its frontmatter:

```markdown
---
name: deep-web-research
description: ...
enabled: false
---
```

- [ ] **Step 2: Verify it shows as disabled in list**

```
./target/release/zeroclaw skills list
```

Expected: `deep-web-research [disabled] v... — ...`

- [ ] **Step 3: Verify it is absent from the agent system prompt**

```
./target/release/zeroclaw agent -m "list all your available skills"
```

Expected: `deep-web-research` is NOT mentioned.

- [ ] **Step 4: Re-enable and verify it returns**

Change `enabled: false` back to `enabled: true` and repeat Steps 2–3.
Expected: skill appears in both list and agent prompt again.

- [ ] **Step 5: Final commit if any touch-ups made**

```
git add -A
git commit -m "chore(skills): end-to-end verification of enabled field"
```
