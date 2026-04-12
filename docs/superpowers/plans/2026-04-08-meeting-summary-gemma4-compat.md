# Meeting Summary Skill — Gemma 4 Compatibility Rewrite

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract embedded Python scripts from Sam's meeting summary skill into mountable files and rewrite the skill markdown to be compact enough for Gemma 4.

**Architecture:** The current skill ConfigMap (`k8s/sam/21_meeting_summary_skill_configmap.yaml`) embeds a 120-line Python aggregation script and a 30-line transcript download script inline in the cron prompt. Gemma 4 can't reliably parse embedded code blocks in skill text. The fix: add the scripts as additional keys in the same ConfigMap, mount them as executable files at known paths, and replace the inline code with `python3 /path/to/script.py` calls. The skill markdown shrinks from 360 lines to ~60 lines.

**Tech Stack:** YAML (Kubernetes ConfigMap), Python, Markdown

---

### Task 1: Extract Python scripts into ConfigMap keys

**Files:**
- Modify: `k8s/sam/21_meeting_summary_skill_configmap.yaml`

Add two new data keys to the existing ConfigMap: `speakr-fetch.py` (the aggregation script from lines 81-197) and `speakr-transcript.py` (the transcript download script from lines 303-336). The scripts stay exactly as they are — no logic changes, just extraction.

- [ ] **Step 1: Add speakr-fetch.py key to the ConfigMap**

Add this as a new data key in the ConfigMap, after the existing `daily-meeting-summary.md` key. This is the exact script from lines 81-196 of the current file, unchanged:

```yaml
  speakr-fetch.py: |
    #!/usr/bin/env python3
    """Fetch today's Speakr recordings and write a meeting aggregation file.

    Usage: python3 speakr-fetch.py [YYYY-MM-DD]
    Defaults to today if no date argument given.
    Requires SPEAKR_API_TOKEN env var.
    """
    import json, os, sys, urllib.request, datetime, pathlib

    BASE = 'https://meetings.coffee-anon.com'
    TOKEN = os.environ['SPEAKR_API_TOKEN']
    DATE = sys.argv[1] if len(sys.argv) > 1 else datetime.date.today().isoformat()
    DATE_QUERY = f'date:{DATE}' if DATE == datetime.date.today().isoformat() else f'date:{DATE}'
    MEM = '/data/workspace/memory'
    OUT = f'{MEM}/meetings-{DATE}.md'
    DROP = {'transcription','transcription_text','summary_html','notes_html',
            'audio_available','audio_deleted_at','can_delete','can_edit',
            'deletion_exempt','file_size','mime_type','original_filename',
            'processing_time_seconds','transcription_duration_seconds',
            'is_highlighted','is_inbox','is_owner','is_shared','has_group_tags',
            'user_id','completed_at','created_at'}

    def api(path):
        r = urllib.request.Request(f'{BASE}{path}',
            headers={'Authorization': f'Bearer {TOKEN}'})
        return json.loads(urllib.request.urlopen(r).read())

    previous_titles = set()
    prev = pathlib.Path(OUT)
    if prev.exists():
        for line in prev.read_text().splitlines():
            if line.startswith('## '):
                previous_titles.add(line[3:].strip())

    data = api(f'/api/recordings?q={DATE_QUERY}&per_page=100')
    recs = [r for r in data.get('recordings', []) if r.get('status') == 'COMPLETED']

    if not recs:
        with open(OUT, 'w') as f:
            f.write(f'# Meetings — {DATE}\n\nNo completed meeting recordings for today.\n')
        print(f'No meetings today. Wrote {OUT}')
        raise SystemExit(0)

    ok = 0
    failed = 0
    pending_summaries = 0
    new_since_last = []

    with open(OUT, 'w') as f:
        f.write(f'# Meetings — {DATE}\n\n')
        for i, rec_stub in enumerate(recs):
            rid = rec_stub['id']
            title = rec_stub.get('title', 'Untitled')
            try:
                rec = api(f'/api/recordings/{rid}')
                for k in DROP:
                    rec.pop(k, None)
            except Exception as e:
                f.write(f'## {title}\n')
                f.write(f'*Failed to fetch recording {rid}: {e}*\n\n---\n\n')
                print(f'  [{i+1}/{len(recs)}] FAILED {title}: {e}')
                failed += 1
                continue

            title = rec.get('title', title)
            participants = rec.get('participants', 'Unknown')
            meeting_date = rec.get('meeting_date', '')
            time_str = meeting_date[11:16] if len(meeting_date) > 15 else meeting_date
            tags = ', '.join(t.get('name','') for t in rec.get('tags', []))
            summary = rec.get('summary', '')
            notes = rec.get('notes', '')
            events = rec.get('events', [])

            if title not in previous_titles and previous_titles:
                new_since_last.append(title)

            f.write(f'## {title}\n')
            f.write(f'**Participants:** {participants}\n')
            f.write(f'**Time:** {time_str}\n')
            if tags:
                f.write(f'**Tags:** {tags}\n')
            f.write(f'**Transcript:** {BASE}/recording/{rid}\n\n')

            if summary:
                f.write(f'### Summary\n{summary}\n\n')
            else:
                f.write('### Summary\n*Summary pending — recording may still be processing.*\n\n')
                pending_summaries += 1

            if events:
                f.write('### Action Items & Events\n')
                for ev in events:
                    ev_title = ev.get('title', 'Untitled')
                    ev_date = ev.get('start_datetime', '')
                    ev_attendees = ', '.join(ev.get('attendees', []))
                    ev_desc = ev.get('description', '')
                    f.write(f'- **{ev_title}**')
                    if ev_attendees:
                        f.write(f' ({ev_attendees})')
                    if ev_date:
                        f.write(f' — {ev_date}')
                    if ev_desc:
                        f.write(f': {ev_desc}')
                    f.write('\n')
                f.write('\n')

            if notes:
                f.write(f'### Notes\n{notes}\n\n')

            f.write('---\n\n')
            ok += 1
            print(f'  [{i+1}/{len(recs)}] {title}')

    parts = [f'Wrote {ok} meetings to {OUT}']
    if failed:
        parts.append(f'{failed} failed to fetch')
    if pending_summaries:
        parts.append(f'{pending_summaries} still processing (no summary yet)')
    if new_since_last:
        parts.append(f'New since last run: {", ".join(new_since_last)}')
    print('. '.join(parts))
```

- [ ] **Step 2: Add speakr-transcript.py key to the ConfigMap**

Add this as another data key:

```yaml
  speakr-transcript.py: |
    #!/usr/bin/env python3
    """Download a Speakr recording transcript to disk for chunked reading.

    Usage: python3 speakr-transcript.py RECORDING_ID
    Requires SPEAKR_API_TOKEN env var.
    Writes to /data/workspace/memory/transcript-{RECORDING_ID}.md
    """
    import json, os, sys, urllib.request

    REC_ID = sys.argv[1]
    BASE = 'https://meetings.coffee-anon.com'
    TOKEN = os.environ['SPEAKR_API_TOKEN']

    req = urllib.request.Request(f'{BASE}/api/recordings/{REC_ID}',
        headers={'Authorization': f'Bearer {TOKEN}'})
    r = json.loads(urllib.request.urlopen(req).read())
    t = r.get('transcription', [])

    out = f'# Transcript: {r.get("title", "Untitled")}\n'
    out += f'# Participants: {r.get("participants", "Unknown")}\n'
    out += f'# Date: {r.get("meeting_date", "Unknown")}\n'
    out += f'# Segments: {len(t) if isinstance(t, list) else "plain-text"}\n'
    if isinstance(t, list) and t:
        out += f'# Duration: {t[-1].get("end", "unknown")}s\n'
    out += '---\n'
    if isinstance(t, list):
        for seg in t:
            speaker = seg.get('speaker', '?')
            text = seg.get('text', seg.get('sentence', ''))
            start = seg.get('start', seg.get('start_time', 0))
            out += f'[{int(start//60):02d}:{int(start%60):02d}] {speaker}: {text}\n'
    else:
        out += str(t)

    path = f'/data/workspace/memory/transcript-{REC_ID}.md'
    with open(path, 'w') as f:
        f.write(out)
    print(f'Wrote {len(out):,} chars to {path}')
    print(f'Segments: {len(t) if isinstance(t, list) else "plain-text"}')
```

- [ ] **Step 3: Verify YAML is valid**

Run: `python3 -c "import yaml; data = yaml.safe_load(open('k8s/sam/21_meeting_summary_skill_configmap.yaml')); print('Keys:', sorted(data['data'].keys()))"`

Expected: `Keys: ['daily-meeting-summary.md', 'speakr-fetch.py', 'speakr-transcript.py']`

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/21_meeting_summary_skill_configmap.yaml
git commit -m "refactor(k8s/sam): extract meeting scripts into ConfigMap keys"
```

---

### Task 2: Mount scripts as executable files in the sandbox

**Files:**
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml`

Mount the two new ConfigMap keys as executable files in the pod. They go alongside the existing skill mount.

- [ ] **Step 1: Add volumeMounts for the scripts**

In the zeroclaw container's `volumeMounts` section (after the existing `skill-meeting-summary` mount at line ~213), add:

```yaml
            - name: skill-meeting-summary
              mountPath: /data/.zeroclaw/workspace/skills/daily-meeting-summary/speakr-fetch.py
              subPath: speakr-fetch.py
              readOnly: true
            - name: skill-meeting-summary
              mountPath: /data/.zeroclaw/workspace/skills/daily-meeting-summary/speakr-transcript.py
              subPath: speakr-transcript.py
              readOnly: true
```

- [ ] **Step 2: Update the volume to set executable permissions**

Find the existing volume definition for `skill-meeting-summary` (around line 304-306):

```yaml
        - name: skill-meeting-summary
          configMap:
            name: zeroclaw-skill-meeting-summary
```

Replace with:

```yaml
        - name: skill-meeting-summary
          configMap:
            name: zeroclaw-skill-meeting-summary
            items:
              - key: daily-meeting-summary.md
                path: daily-meeting-summary.md
              - key: speakr-fetch.py
                path: speakr-fetch.py
                mode: 0755
              - key: speakr-transcript.py
                path: speakr-transcript.py
                mode: 0755
```

- [ ] **Step 3: Verify YAML is valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('k8s/sam/04_zeroclaw_sandbox.yaml')); print('VALID')"`

Expected: `VALID`

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "refactor(k8s/sam): mount meeting scripts as executable files"
```

---

### Task 3: Rewrite the skill markdown for Gemma 4

**Files:**
- Modify: `k8s/sam/21_meeting_summary_skill_configmap.yaml` — the `daily-meeting-summary.md` data key

Replace the 360-line skill with a compact version that references the external scripts by path. Remove all embedded code blocks, multi-paragraph explanations, and markdown tables.

- [ ] **Step 1: Replace the daily-meeting-summary.md content**

Replace the entire `daily-meeting-summary.md` data key value with:

```yaml
  daily-meeting-summary.md: |
    ---
    name: daily-meeting-summary
    version: 3.0.0
    description: Fetches and summarizes daily meeting recordings from Speakr
    always: false
    ---

    # Daily Meeting Summary

    Speakr (meetings.coffee-anon.com) is Dan's self-hosted meeting transcription app.
    Tools: shell, file_read, file_write, memory_store, memory_recall, cron_list, cron_add, content_search.
    Do not use Serena tools for meeting tasks. No curl available, use Python urllib.

    ## Bootstrap

    Check cron_list for speakr-daily-summary. If missing, create with cron_add:
    name=speakr-daily-summary, schedule=0 12,17 * * 1-5, timezone=America/Vancouver, job_type=agent, session_target=isolated.

    Cron prompt (use verbatim):
    ```
    You are in an isolated cron session. Do not call cron_run.

    STEP 1: Run the aggregation script.
    shell: python3 /data/.zeroclaw/workspace/skills/daily-meeting-summary/speakr-fetch.py
    This writes meetings-YYYY-MM-DD.md. The status line tells you counts and new recordings.

    STEP 2: Build the executive summary.
    Read meetings-YYYY-MM-DD.md with file_read.
    Write meetings-exec-YYYY-MM-DD.md with file_write containing:
    - Action items for Dan (with meeting context and deadlines)
    - Action items others owe Dan (who, what, when)
    - Top 3-5 key decisions and issues
    Note any recordings still processing. Call out new recordings since last run.

    STEP 2.5: Create Vikunja tasks.
    For each concrete action item, run: vikunja task create 1 --title "action" --assignee dan --description "From [meeting]."
    Add --due YYYY-MM-DD if a deadline was mentioned. Skip vague items.
    Dedup: run vikunja tasks 1 first, skip if similar title exists.

    STEP 3: Store memory.
    memory_store key=meetings_digest_YYYY-MM-DD category=daily value="N meetings. Files: meetings-DATE.md, meetings-exec-DATE.md. Theme: [one sentence]."
    ```

    ## On-Demand Usage

    When Dan asks about meetings:
    1. memory_recall key meetings_digest_YYYY-MM-DD for quick check.
    2. file_read meetings-exec-YYYY-MM-DD.md for the summary.
    3. file_read meetings-YYYY-MM-DD.md for full per-meeting detail.
    4. If no files exist, run: shell python3 /data/.zeroclaw/workspace/skills/daily-meeting-summary/speakr-fetch.py
    5. For past dates, pass the date as argument: speakr-fetch.py YYYY-MM-DD

    ## Full Transcript Lookup

    When Dan asks about specific transcript content:
    1. shell: python3 /data/.zeroclaw/workspace/skills/daily-meeting-summary/speakr-transcript.py RECORDING_ID
    2. Read header (first 10 lines) for metadata.
    3. Use content_search to find relevant sections, then file_read with offset/limit.
    4. Delete transcript file when done (they can be large).
    Never load a full transcript into a single tool result.

    ## API Reference

    Base: meetings.coffee-anon.com. Auth: Bearer SPEAKR_API_TOKEN.
    Endpoints: /api/recordings?q=date:today, date:yesterday, date:YYYY-MM-DD, date:thisweek, date_from:X&date_to:Y. Add per_page=100.
```

Key changes:
- 360 lines → ~55 lines (85% reduction)
- Inline Python scripts replaced with `python3 /path/to/script.py` calls
- Multi-paragraph explanations replaced with single-line directives
- Markdown table replaced with inline endpoint list
- Removed "Deep Dive" section's multi-step example flow narrative
- Removed "Environment Notes" and "Tool Routing" as separate sections (merged into header)
- Preserved: all functional behavior (bootstrap, cron prompt, on-demand lookup, transcript chunking, API reference, dedup rules)

- [ ] **Step 2: Verify YAML is valid**

Run: `python3 -c "import yaml; data = yaml.safe_load(open('k8s/sam/21_meeting_summary_skill_configmap.yaml')); print('Keys:', sorted(data['data'].keys())); print('Skill chars:', len(data['data']['daily-meeting-summary.md']))"`

Expected: 3 keys, skill ~2,000-2,500 chars (down from ~8,000+).

- [ ] **Step 3: Verify no chevrons in skill content**

Run: `grep -n '[<>]' k8s/sam/21_meeting_summary_skill_configmap.yaml | grep -v 'apiVersion\|kind:\|metadata:\|namespace:'`

Expected: No matches.

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/21_meeting_summary_skill_configmap.yaml
git commit -m "refactor(k8s/sam): rewrite meeting summary skill for Gemma 4 — 360 to 55 lines"
```

---

### Task 4: Validate and deploy

**Files:**
- Verify: `k8s/sam/21_meeting_summary_skill_configmap.yaml`
- Verify: `k8s/sam/04_zeroclaw_sandbox.yaml`

- [ ] **Step 1: Validate both files are valid YAML**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/21_meeting_summary_skill_configmap.yaml')); print('configmap OK')"
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/04_zeroclaw_sandbox.yaml')); print('sandbox OK')"
```

Expected: Both print OK.

- [ ] **Step 2: Apply to cluster**

```bash
kubectl apply -f k8s/sam/21_meeting_summary_skill_configmap.yaml
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
```

- [ ] **Step 3: Restart zeroclaw pod**

```bash
kubectl delete pod zeroclaw -n ai-agents
sleep 30
kubectl get pods -n ai-agents -l app=zeroclaw
```

Expected: Pod comes back 3/3 Running.

- [ ] **Step 4: Verify scripts are mounted and executable**

```bash
kubectl exec zeroclaw -n ai-agents -c zeroclaw -- ls -la /data/.zeroclaw/workspace/skills/daily-meeting-summary/
```

Expected: Three files — `SKILL.md`, `speakr-fetch.py` (with execute permission), `speakr-transcript.py` (with execute permission).

- [ ] **Step 5: Verify script runs**

```bash
kubectl exec zeroclaw -n ai-agents -c zeroclaw -- python3 /data/.zeroclaw/workspace/skills/daily-meeting-summary/speakr-fetch.py 2>&1 | head -5
```

Expected: Either meeting data output or "No meetings today" — not a "file not found" or "permission denied" error.

- [ ] **Step 6: Commit (if any fixes needed)**

```bash
git add k8s/sam/21_meeting_summary_skill_configmap.yaml k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "fix(k8s/sam): meeting summary deploy validation"
```

---

## Reduction Summary

| Component | Before | After | Reduction |
|-----------|--------|-------|-----------|
| Skill markdown | 360 lines | ~55 lines | 85% |
| Inline Python | 120 + 30 lines embedded in markdown | 0 lines (external files) | 100% |
| Scripts | N/A | 120 + 35 lines (standalone files) | — |

The total ConfigMap is larger (scripts are still stored there), but what Gemma 4 sees (the skill markdown) shrinks by 85%. The scripts are never loaded into the LLM context — they run via `shell` tool as external processes.

## Verification

- YAML valid for both files
- ConfigMap has 3 keys (skill + 2 scripts)
- Scripts mounted with execute permission
- speakr-fetch.py runs successfully from the pod
- No chevrons in skill content
- Pod restarts 3/3
