# YouTube Transcribe Skill + UV Migration — Design Spec

**Date:** 2026-03-16
**Status:** Draft
**Skills:** `yt-transcribe` (new), all existing skills migrated to uv

## Part 1: UV Migration

### Problem
All skills install Python dependencies globally (`pip3 install`). 4 skills with different deps polluting system Python.

### Solution
One shared virtual environment at `~/.zeroclaw/workspace/.venv/` managed by `uv`.

**Setup:**
- Install `uv` (single binary, no deps)
- Create `~/.zeroclaw/workspace/pyproject.toml` with all skill dependencies
- Create venv: `uv venv ~/.zeroclaw/workspace/.venv/`
- Install all: `uv pip install -r pyproject.toml`

**pyproject.toml dependencies (all skills combined):**
```toml
[project]
name = "zeroclaw-skills"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "requests>=2.28",
    "pyyaml>=6.0",
    "pymysql>=1.1",
    "telethon>=1.34",
    "yt-dlp>=2024.1",
]
```

**SKILL.toml command migration:**
All `python3 ~/.zeroclaw/workspace/skills/...` commands become:
```
~/.zeroclaw/workspace/.venv/bin/python3 ~/.zeroclaw/workspace/skills/...
```

**Affected SKILL.toml files:**
- `erp-analyst/SKILL.toml` — 7 tools
- `gmaps-places/SKILL.toml` — 3 tools
- `telegram-reader/SKILL.toml` — ~15 tools
- `yt-transcribe/SKILL.toml` — 1 tool (new)

**ffmpeg:** Required for audio chunking. Install via `apt` (system package, not pip).

## Part 2: yt-transcribe Skill

### Purpose
YouTube video → markdown transcript with minute-by-minute timestamps. User sends YouTube URL to Telegram bot, gets formatted .md back.

### Pipeline

```
YouTube URL
  → yt-dlp (download audio as mp3, lowest quality)
  → if >25MB: ffmpeg splits into 24MB chunks
  → Groq Whisper API (whisper-large-v3-turbo, verbose_json)
  → if chunked: merge segments, shift timestamps for chunks 2+
  → format as markdown with [MM:SS] timestamps
  → return to user
```

### File structure

```
~/.zeroclaw/workspace/skills/yt-transcribe/
├── SKILL.toml
└── scripts/
    └── yt_transcribe.py   # Single file — download + transcribe + format
```

### Tool

**`yt_transcribe(url, language)`**

Args:
- `url` — REQUIRED. YouTube URL
- `language` — Language hint: 'ru', 'en', 'auto' (default: 'auto')

### Output format

```markdown
# Video Title

**Channel:** Author Name
**Duration:** 15:30
**Language:** ru

---

**[00:00]** Привет всем, сегодня мы поговорим о...

**[01:15]** Первый важный момент — это...

**[03:42]** А теперь перейдём к...

**[05:10]** Подводя итог, мы рассмотрели...
```

### Groq Whisper API

- Endpoint: `https://api.groq.com/openai/v1/audio/transcriptions`
- Model: `whisper-large-v3-turbo`
- Auth: `Authorization: Bearer $GROQ_API_KEY`
- Input: multipart form upload, `file` + `model` + `response_format=verbose_json` + optional `language`
- Limit: 25MB per file
- Cost: free tier
- Response: `{"segments": [{"start": 0.0, "end": 2.5, "text": "..."}]}`

### Chunking (>25MB)

1. Get audio duration: `ffprobe -i file.mp3 -show_entries format=duration`
2. Calculate chunk count: `ceil(file_size / 24MB)`
3. Split: `ffmpeg -i file.mp3 -f segment -segment_time {chunk_seconds} -c copy chunk_%03d.mp3`
4. Transcribe each chunk
5. Merge: offset timestamps by cumulative duration of previous chunks

### Config

- `GROQ_API_KEY` added to `shell_env_passthrough` in config.toml
- `yt_transcribe` added to `auto_approve` in config.toml
- `yt_transcribe` added to erp_analyst `allowed_tools` (optional, for cross-skill use)

### Error handling

- Missing `GROQ_API_KEY` → `{"success": false, "error": "GROQ_API_KEY not set"}`
- Invalid/private YouTube URL → `{"success": false, "error": "..."}`
- Groq rate limit → `{"success": false, "error": "Rate limited, try again in 60s"}`
- Audio >25MB → auto-chunk (not an error)
- All exit 0 (soft failure pattern)

### Testing

- `tests/test_transcribe.py`: unit tests for markdown formatting, timestamp merging, chunk calculation
- E2E: real YouTube URL → transcript (requires GROQ_API_KEY)
- Target: 10+ unit tests, 2-3 E2E

### Temp files

Audio downloads stored in `/tmp/yt_transcribe_<hash>/`, cleaned up after transcription.

## Verification

1. `uv pip list` shows all deps in shared venv
2. All existing skill tests still pass with venv python
3. `yt_transcribe.py transcribe --url "https://youtube.com/watch?v=..." --language auto` returns markdown
4. Telegram bot test: send YouTube link, get transcript
