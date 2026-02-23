# ClawHub Integration Design

## Overview

Add ClawHub integration to ZeroClaw, enabling:
1. CLI commands to search, browse, install, and manage ClawHub skills
2. LLM tools for autonomous skill discovery and installation during agent runtime
3. Seamless integration with existing ZeroClaw skills architecture

## Background

### ClawHub
- **Purpose**: Public skill registry for Clawdbot (similar to npm for agent skills)
- **Tech Stack**: TanStack Start (React/Vite), Convex (backend/DB), OpenAI embeddings (search)
- **Key Features**:
  - Browse/render `SKILL.md` files
  - Publish skill versions with changelogs and tags
  - Vector search via OpenAI embeddings
  - Star and comment on skills
  - GitHub OAuth authentication

### ClawHub CLI Commands (reference)
| Category | Commands |
|----------|----------|
| Auth | `clawhub login`, `clawhub whoami` |
| Discover | `clawhub search ...`, `clawhub explore` |
| Install | `clawhub install <slug>`, `clawhub uninstall <slug>`, `clawhub list`, `clawhub update --all` |
| Inspect | `clawhub inspect <slug>` |
| Publish | `clawhub publish <path>`, `clawhub sync` |

### ZeroClaw Existing Skills
- Skills stored in `~/.zeroclaw/workspace/skills/<name>/SKILL.md` or `SKILL.toml`
- Supports local skills, git-based installation, and open-skills repo
- Security audit system for skill content
- Skills injected into LLM prompts as XML-formatted instructions

---

## Architecture

### High-Level Components

```
┌─────────────────────────────────────────────────────────────────┐
│                         ZeroClaw CLI                           │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  zeroclaw clawhub search <query>                        │   │
│  │  zeroclaw clawhub install <slug>                       │   │
│  │  zeroclaw clawhub list                                 │   │
│  │  zeroclaw clawhub uninstall <slug>                     │   │
│  │  zeroclaw clawhub update                               │   │
│  │  zeroclaw clawhub inspect <slug>                      │   │
│  │  zeroclaw clawhub login                                │   │
│  │  zeroclaw clawhub whoami                               │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     ClawHub Module (new)                       │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐    │
│  │  CLI Handler │  │  API Client  │  │  Skill Downloader│    │
│  └──────────────┘  └──────────────┘  └──────────────────┘    │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  Local Skill Registry (clawhub_skills.json)             │  │
│  │  - slug, version, source_url, installed_at             │  │
│  └─────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    External Services                           │
│  ┌──────────────────┐     ┌────────────────────────────┐     │
│  │  ClawHub API     │     │  GitHub (skill repos)      │     │
│  │  (Convex HTTP)  │     │  (raw content download)    │     │
│  └──────────────────┘     └────────────────────────────┘     │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    LLM Tool Integration                        │
│  ┌──────────────────────┐  ┌──────────────────────────────┐  │
│  │  ClawhubSearchTool   │  │  ClawhubInstallTool          │  │
│  │  - search skills     │  │  - install by slug           │  │
│  │  - browse listings   │  │  - update installed skills  │  │
│  └──────────────────────┘  └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Module Structure

```
src/clawhub/
├── mod.rs           # CLI commands, exports
├── client.rs        # ClawHub API client (Convex HTTP actions)
├── auth.rs          # GitHub OAuth handling
├── downloader.rs    # Skill download (GitHub raw URLs)
├── registry.rs      # Local clawhub skill registry tracking
└── types.rs         # Shared types (ClawhubSkill, etc.)

src/tools/
├── clawhub_search.rs    # NEW: LLM tool for searching
├── clawhub_install.rs   # NEW: LLM tool for installing
└── mod.rs               # Add exports
```

---

## Data Flow

### CLI: `zeroclaw clawhub search <query>`

1. User runs search command
2. CLI calls ClawHub search API (Convex HTTP endpoint)
3. API returns matching skills with metadata (name, description, tags, stars)
4. CLI renders results in table format
5. User selects skill to install

### CLI: `zeroclaw clawhub install <slug>`

1. User runs install command with skill slug
2. CLI fetches skill metadata from ClawHub API
3. CLI resolves GitHub repository URL from skill metadata
4. CLI downloads skill content (SKILL.md, supporting files) via GitHub raw
5. CLI performs security audit on downloaded content
6. CLI installs to `~/.zeroclaw/workspace/skills/<slug>/`
7. CLI updates local clawhub registry (`clawhub_skills.json`)
8. CLI updates workspace skills README

### LLM: Autonomous Skill Installation

1. LLM determines it needs a capability not currently available
2. LLM calls `clawhub_search` tool with search query
3. Tool returns search results from ClawHub API
4. LLM decides to install a skill
5. LLM calls `clawhub_install` tool with skill slug
6. Tool performs installation (same flow as CLI)
7. Tool returns success/failure with skill details
8. LLM can now use the newly available skill

---

## API Integration

### ClawHub API Endpoints (Convex)

Based on ClawHub's architecture, the API exposes:

```
GET  /api/search?q=<query>           # Vector search skills
GET  /api/skills/<slug>              # Get skill metadata
GET  /api/skills/<slug>/readme       # Get SKILL.md content
GET  /api/user                       # Get authenticated user
POST /api/auth/github                # GitHub OAuth callback
```

### Skill Metadata Response

```json
{
  "slug": "weather-tool",
  "name": "Weather Tool",
  "description": "Fetch weather forecasts",
  "author": "someuser",
  "tags": ["weather", "api", "utility"],
  "stars": 42,
  "version": "1.2.0",
  "github_url": "https://github.com/someuser/weather-tool",
  "readme_url": "https://raw.githubusercontent.com/.../SKILL.md"
}
```

---

## Configuration

### New Config Section: `clawhub`

```toml
[clawhub]
# ClawHub API endpoint (default: https://clawhub.ai)
# Can be self-hosted instance
api_url = "https://clawhub.ai"

# Authentication
# GitHub OAuth token for authenticated operations
# Stored in zeroclaw secrets store
github_token = { }

# Behavior
# Auto-update installed clawhub skills on agent start
auto_update = false
```

### Environment Variables

- `ZEROCLAW_CLAWHUB_API_URL` - Override API endpoint
- `ZEROCLAW_CLAWHUB_TOKEN` - GitHub token (or use secrets store)

---

## Local Registry

### File: `~/.zeroclaw/clawhub_skills.json`

```json
{
  "skills": [
    {
      "slug": "weather-tool",
      "name": "Weather Tool",
      "version": "1.2.0",
      "source_url": "https://github.com/someuser/weather-tool",
      "installed_at": "2025-01-15T10:30:00Z",
      "updated_at": "2025-01-20T14:00:00Z"
    }
  ],
  "last_sync": "2025-01-20T14:00:00Z"
}
```

---

## LLM Tools

### Tool: `clawhub_search`

**Purpose**: Search ClawHub for skills matching a query

**Parameters**:
```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search query for skills"
    },
    "limit": {
      "type": "integer",
      "description": "Maximum results to return",
      "default": 10
    }
  },
  "required": ["query"]
}
```

**Returns**: List of skills with name, description, tags, stars

### Tool: `clawhub_install`

**Purpose**: Install a skill from ClawHub by slug

**Parameters**:
```json
{
  "type": "object",
  "properties": {
    "slug": {
      "type": "string",
      "description": "ClawHub skill slug to install"
    },
    "version": {
      "type": "string",
      "description": "Specific version to install (optional, default: latest)"
    }
  },
  "required": ["slug"]
}
```

**Returns**: Installation result with skill details or error message

---

## Error Handling

### Network Errors
- Retry with exponential backoff (3 attempts)
- Fallback: offer to install from direct GitHub URL

### Auth Errors
- GitHub token expired/missing: prompt to run `zeroclaw clawhub login`
- Provide clear error message with login instructions

### Security Audit Failures
- If skill fails audit, do not install
- Show audit findings to user/LLM
- Log warning for security team review

### Duplicate Installation
- If skill already installed, offer to update
- Show current version vs available version

---

## Security Considerations

1. **Skill Audit**: All downloaded skills must pass security audit before installation
2. **Source Verification**: Verify skill source is from expected GitHub repository
3. **Token Storage**: GitHub tokens stored in ZeroClaw secrets store (encrypted)
4. **Network Isolation**: Skills can only access network resources they declare
5. **Audit Logging**: Log all install/update operations for compliance

---

## CLI Commands Detail

### `zeroclaw clawhub login`

- Initiates GitHub OAuth flow
- Opens browser for GitHub authorization
- Stores token in secrets store
- Shows success with username

### `zeroclaw clawhub whoami`

- Shows authenticated GitHub username
- Shows token expiry if applicable

### `zeroclaw clawhub search <query>`

```
$ zeroclaw clawhub search weather

Searching ClawHub for "weather"...
Found 12 skills:

  Name              Description              Stars  Tags
  ─────────────────────────────────────────────────────────
  weather-tool      Fetch weather data      42    [weather, api]
  wttr-integration  WTTR.in wrapper         28    [weather, cli]
  openweather       OpenWeatherMap wrapper  15    [weather, api]
```

### `zeroclaw clawhub install <slug>`

```
$ zeroclaw clawhub install weather-tool

Fetching skill metadata...
Downloading from GitHub...
Running security audit...
✓ Security audit passed (15 files scanned)

Installing to ~/.zeroclaw/workspace/skills/weather-tool/
Updating skills README...
✓ Installed weather-tool v1.2.0
```

### `zeroclaw clawhub list`

```
$ zeroclaw clawhub list

Installed ClawHub skills (3):

  weather-tool   v1.2.0   Updated: 2025-01-20
  github-utils  v0.8.1   Updated: 2025-01-18
  slack-notify  v2.0.0   Updated: 2025-01-15
```

### `zeroclaw clawhub update`

```
$ zeroclaw clawhub update

Checking for updates...
  weather-tool: v1.2.0 → v1.2.1 [update available]
  github-utils: v0.8.1 → v0.8.1 [up to date]
  slack-notify: v2.0.0 → v2.1.0 [update available]

Updating weather-tool...
...
✓ Updated to v1.2.1

Updating slack-notify...
...
✓ Updated to v2.1.0
```

### `zeroclaw clawhub inspect <slug>`

Shows full skill metadata, description, tools, requirements

---

## README Updates

When installing/removing clawhub skills, update the workspace skills README to include ClawHub skills with clear attribution:

```markdown
# ZeroClaw Skills

## Local Skills
...

## ClawHub Skills
These skills installed from [ClawHub](https://clawhub.ai):

| Skill | Version | Source |
|-------|---------|--------|
| [weather-tool](skills/weather-tool) | 1.2.0 | ClawHub |
| [github-utils](skills/github-utils) | 0.8.1 | ClawHub |

To browse and install more skills: `zeroclaw clawhub search`
```

---

## Testing Strategy

### Unit Tests
- API client request/response parsing
- Auth token storage/retrieval
- Registry read/write
- Download path resolution

### Integration Tests
- Mock ClawHub API responses
- Test full install flow (with mocked GitHub)
- Test security audit integration
- Test LLM tool execution

### CLI Tests
- All command parsing and flags
- Output formatting
- Error message clarity

---

## Implementation Phases

### Phase 1: Core Infrastructure
- Create `src/clawhub/` module
- Implement API client
- Implement skill downloader
- Create local registry

### Phase 2: CLI Commands
- Add `clawhub` subcommand to main CLI
- Implement all CLI commands
- Add README update functionality

### Phase 3: LLM Tools
- Implement `ClawhubSearchTool`
- Implement `ClawhubInstallTool`
- Register in tool registry

### Phase 4: Configuration & Polish
- Add config section
- Environment variable support
- Error handling improvements
- Documentation

---

## Migration/Compatibility

- Existing local skills continue to work unchanged
- ClawHub skills stored in same directory but tracked separately
- No breaking changes to existing commands

---

## Future Considerations

- **Publishing**: Add `zeroclaw clawhub publish` to publish local skills to ClawHub
- **Skill Updates**: Background check for skill updates
- **Offline Mode**: Cache ClawHub catalog for offline browsing
- **Self-hosted**: Support for private ClawHub instances
- **Ratings**: Display star ratings in search results

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| ClawHub API downtime | Medium | Medium | Offline fallback, clear error messages |
| Token storage security | Low | High | Use existing secrets store |
| Skill security vulnerabilities | Medium | High | Mandatory security audit |
| Breaking API changes | Low | Medium | Version API calls, handle gracefully |

---

## References

- ClawHub GitHub: https://github.com/openclaw/clawhub
- ClawHub Website: https://clawhub.ai/
- ZeroClaw Skills: `src/skills/mod.rs`
- ZeroClaw Tools: `src/tools/traits.rs`
