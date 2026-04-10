# Fix Browser Tool + Signal Receipt Noise — Schema, TOOLS.md Trim, Context Efficiency

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix browser tool parameter errors (50% wasted turns in benchmark) and eliminate Signal read receipt noise (thousands of wasted prompt tokens per turn) — both reducing LLM context waste and improving Sam's task efficiency.

**Architecture:** Three-pronged fix — (1) improve browser.rs schema descriptions so the model gets correct per-action param requirements from the tool definition itself, (2) trim the TOOLS.md browser section from a usage manual to instance-specific one-liner gotchas, (3) summarize Signal receipts in signal.rs instead of dumping raw timestamp lists. Plus a security policy fix for overflow file access.

**Tech Stack:** Rust (zeroclaw runtime), YAML (Kubernetes ConfigMaps)

**Repos:**
- Runtime: `~/github_projects/zeroclaw/`
- Config: `~/github_projects/scrapyard-applications/`

**Origin:** Benchmarking Sam on an HN browsing task revealed two categories of context waste:
1. **Browser tool errors** — 5 of 10 turns wasted on param errors from incorrect TOOLS.md docs and undocumented schema requirements
2. **Signal receipt noise** — read/delivery receipts inject thousands of tokens of raw timestamps into the user message (e.g., 100+ timestamps per receipt, multiple overlapping receipt blocks per message)

**Best practices alignment:** The claw-identity-best-practices doc says TOOLS.md should be a lean lookup table, NOT a tool usage manual. The morrohsu source material says tool definitions should be self-documenting. Both point toward fixing the schema and trimming the docs.

---

## Chunk 1: Runtime — Improve Browser Tool Schema (zeroclaw)

All file paths relative to `~/github_projects/zeroclaw/`.

### Task 1: Improve the `action` parameter description

**Files:**
- Modify: `src/tools/browser.rs` (lines ~1314-1319)

The current `action` description is:
```
"Browser action to perform (OS-level actions require backend=computer_use)"
```

This tells the model nothing about which actions exist or what params they need.

- [ ] **Step 1: Update the action enum description**

Replace the `action` description (line ~1319) with a concise per-action param guide:

```rust
"description": "Browser action. Common actions and their required params: open(url), get_text(selector — use 'body' for full page), click(selector), fill(selector, value), find(by, value, find_action), scroll(direction), snapshot(), screenshot(), press(key), hover(selector). OS-level actions (mouse_move, mouse_click, key_type, etc.) require backend=computer_use."
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`

---

### Task 2: Improve key parameter descriptions

**Files:**
- Modify: `src/tools/browser.rs` (lines ~1325-1410)

Several parameter descriptions are misleading or incomplete:

- [ ] **Step 1: Fix `selector` description**

Current: `"Element selector: @ref (e.g. @e1), CSS (#id, .class), or text=..."`
New: `"Element selector: @ref (e.g. @e1), CSS (#id, .class), text=..., or 'body' for full page. Required for get_text, click, fill, type, hover, is_visible."`

- [ ] **Step 2: Fix `value` description**

Current: `"Value to fill or type"`
New: `"Value to fill, type, or search for. Required for fill, find."`

- [ ] **Step 3: Fix `direction` description**

Current: `"Scroll direction"`
New: `"Scroll direction: up, down, left, right. Required for scroll."`

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`

- [ ] **Step 5: Commit**

```bash
git add src/tools/browser.rs
git commit -m "fix(browser): improve tool schema descriptions for LLM param discovery"
```

---

### Task 3: Build and tag new image

**Note:** Run this AFTER completing both Chunk 1 (browser schema) and Chunk 3 (signal receipts) to avoid building twice.

- [ ] **Step 1: Run full test suite**

Run: `cargo test`

- [ ] **Step 2: Build release**

Run: `cargo build --release`

- [ ] **Step 3: Build and push Docker image**

```bash
docker build -t citizendaniel/zeroclaw-sam:v1.4.11 .
docker push citizendaniel/zeroclaw-sam:v1.4.11
```

- [ ] **Step 4: Update image tag in sandbox YAML**

File: `~/github_projects/scrapyard-applications/k8s/sam/04_zeroclaw_sandbox.yaml`

Update the image tag from `v1.4.10` to `v1.4.11`.

- [ ] **Step 5: Commit the sandbox YAML change**

```bash
cd ~/github_projects/scrapyard-applications
git add k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "fix(zeroclaw): bump image to v1.4.11 — browser schema + signal receipt summary"
```

---

## Chunk 2: Config — Trim TOOLS.md + Fix Security Policy (scrapyard-applications)

All file paths relative to `~/github_projects/scrapyard-applications/`.

### Task 4: Trim TOOLS.md browser section — remove action table

**Files:**
- Modify: `k8s/sam/05_zeroclaw_identity_configmap.yaml`

The "Browser Automation" section (lines 471-498) is a usage manual with a WRONG action table (lists `select_option`, `evaluate`, `wait_for_selector` which don't exist; omits `get_text`, `find`, `scroll` which do). Per best practices, this belongs in a skill, not TOOLS.md.

- [ ] **Step 1: Remove the "Browser Automation" section**

Delete lines 471-500 (from `## Browser Automation` through the `---` separator after "When to use the browser"). This removes:
- The incorrect action table
- The `browser_open` tool description (redundant — it's in the tools quick reference table)
- The `screenshot` tool note (will be a one-liner in the gotchas)
- The "When to use the browser" list

---

### Task 5: Fix TOOLS.md browser gotchas section

**Files:**
- Modify: `k8s/sam/05_zeroclaw_identity_configmap.yaml`

Replace the current "Browser:" section (lines 393-398) with corrected, lean one-liners.

- [ ] **Step 1: Replace the browser gotchas**

Current (lines 393-398):
```
    **Browser:**
    - To extract page content, use `browser(action: get_text)`. This returns the text content of the page — searchable, parseable, and context-friendly.
    - `browser(action: screenshot)` is for visual verification only — you cannot grep, search, or parse screenshots with other tools. Do not use `content_search` or `image_info` on screenshot files.
    - `browser(action: open, url: ...)` navigates to a page. Follow with `get_text` to read the content.
    - `browser(action: find)` searches visible text on the current page. Use `get_text` if you need the full content to analyze.
    - For shell-based web requests, write a Python script to a file first (`file_write`), then execute it (`shell`). Do not attempt inline heredoc Python or base64-encoded scripts — use `file_write` + `shell` instead.
```

New:
```
    **Browser:**
    - Full page text: `browser(action: get_text, selector: body)` — `selector` is required, use `body` for the whole page.
    - Find + act: `browser(action: find, by: text, value: "search term", find_action: click)` — all three params required.
    - Scroll: `browser(action: scroll, direction: down)` — `direction` required (up/down/left/right), `pixels` optional.
    - `content_search` is a file search tool, not a browser action.
    - `screenshot` is visual-only — you cannot grep or parse screenshot files.
    - To search a site for specific content, use its search page/API (e.g., `site.com/search?q=...`) rather than scrolling through pages.
    - For shell-based HTTP: `file_write` a Python script, then `shell` to run it. No inline heredoc.
```

- [ ] **Step 2: Commit**

```bash
git add k8s/sam/05_zeroclaw_identity_configmap.yaml
git commit -m "fix(zeroclaw): trim browser docs in TOOLS.md — fix param errors, remove wrong action table"
```

---

### Task 6: Fix `/tmp/cmd-output` security policy

**Files:**
- Modify: `k8s/sam/03_zeroclaw_configmap.yaml`

The presentation layer saves overflow files to `/tmp/cmd-output/` (configured at line 203), but the security policy blocks `/tmp` in `forbidden_paths` (line 36). Sam tried `cat /tmp/cmd-output/cmd-3.txt` and got "Path blocked by security policy". The TOOLS.md "Output Size Discipline" section tells her to explore overflow files with shell — this is a direct contradiction.

- [ ] **Step 1: Add `allowed_roots` to the `[autonomy]` section**

After line 41 (after the `forbidden_paths` closing bracket), add:

```toml
      allowed_roots = ["/tmp/cmd-output"]
```

The `is_resolved_path_allowed` function in `policy.rs` checks `allowed_roots` before `forbidden_paths`, so this whitelists the overflow directory without opening all of `/tmp`.

- [ ] **Step 2: Commit**

```bash
git add k8s/sam/03_zeroclaw_configmap.yaml
git commit -m "fix(zeroclaw): allow shell access to /tmp/cmd-output overflow files"
```

---

## Chunk 3: Runtime — Strip Signal Receipts from LLM Context (zeroclaw)

All file paths relative to `~/github_projects/zeroclaw/`.

**Problem:** Signal read/delivery receipts are formatted with ALL original timestamps and injected verbatim into the user message context. A single receipt block can contain 100+ timestamps. Multiple receipt blocks arrive with overlapping timestamp lists. The LLM sees thousands of wasted tokens. From the benchmark: the user message was 5,676 chars; the actual request was one line at the end. The rest was receipt noise.

**Decision:** Strip receipts from LLM context entirely. The LLM has no use for read/delivery receipt data. The existing `strip_receipt_prefix()` function already strips receipts for command parsing — we just need to stop injecting them in the first place.

### Task 8: Stop injecting receipts into message content

**Files:**
- Modify: `src/channels/signal.rs` (lines ~479-575)

The buffering and injection logic in `listen()` collects receipts into `pending_receipts` (line ~482) and prepends them to the next user message at two injection points (lines ~520-526 and ~557-563):

```rust
if !pending_receipts.is_empty() {
    let receipt_block = format!(
        "[Message delivery status since your last message:\n{}]\n\n",
        pending_receipts.join("\n")
    );
    msg.content = format!("{receipt_block}{}", msg.content);
    pending_receipts.clear();
}
```

- [ ] **Step 1: Remove receipt injection at both injection points**

At both locations (lines ~520-526 and ~557-563), replace the injection block with just a clear:

```rust
if !pending_receipts.is_empty() {
    pending_receipts.clear();
}
```

Keep `pending_receipts` and `format_receipt()` alive for now — they may be useful for logging or future features. Just stop injecting into `msg.content`.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/channels/signal.rs
git commit -m "fix(signal): strip read/delivery receipts from LLM context

Receipts were injecting thousands of tokens of raw timestamp lists into
user messages. The LLM has no use for this data. Receipts are still
received and parsed but no longer prepended to message content."
```

---

## Chunk 4: Deploy

### Task 9: Apply ConfigMaps and restart

- [ ] **Step 1: Apply both ConfigMaps**

```bash
kubectl apply -f k8s/sam/03_zeroclaw_configmap.yaml
kubectl apply -f k8s/sam/05_zeroclaw_identity_configmap.yaml
```

- [ ] **Step 2: Apply sandbox (new image tag)**

```bash
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
```

- [ ] **Step 3: Verify pod restarts with new config**

```bash
kubectl get pods -n ai-agents -l app=zeroclaw-sam -w
```

---

## Verification

### 1. Re-run HN benchmark

Send Sam the same message: "Go to https://news.ycombinator.com and find any posts about AI agents. Give me the titles and comment counts."

**Expected browser improvements:**
- `get_text` works on first try (schema says `selector` required, gotcha shows `selector: body`)
- `find` uses `value` not `text` (schema says "Required for fill, find")
- `scroll` includes `direction` (schema says "Required for scroll")
- No `content_search` hallucination (disambiguation in gotchas)
- Overflow files accessible via `cat /tmp/cmd-output/...` (security policy fixed)

**Target:** Under 6 LLM turns (down from 10 in clean run, 21 in pre-change baseline).

### 2. Verify receipts stripped

Check llama-swap captures for the next Sam session. The user message should contain ONLY the actual message text — no `[SIGNAL:READ_RECEIPT]`, no `[Message delivery status...]` blocks.

**Expected token savings:** ~2000-4000 tokens per message eliminated entirely.

### 3. Verify TOOLS.md token savings

Browser section: ~30 lines (action table + usage docs) → ~7 one-liner gotchas (~60% reduction in browser-related tokens).
