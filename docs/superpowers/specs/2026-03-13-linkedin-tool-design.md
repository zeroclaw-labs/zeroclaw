# LinkedIn Tool — Design Spec

**Date:** 2026-03-13
**Status:** Approved
**Risk tier:** Medium (new tool, external API, credential handling)

## Summary

Native LinkedIn integration tool for ZeroClaw. Enables the agent to create posts,
list its own posts, comment, react, delete posts, view post engagement, and retrieve
profile info — all through LinkedIn's official REST API with OAuth2 authentication.

## Motivation

Enable ZeroClaw to autonomously publish LinkedIn content on a schedule (via cron),
drawing from the user's memory, project history, and Medium feed. Removes dependency
on third-party platforms like Composio for social media posting.

## Required OAuth2 scopes

Users must grant these scopes when creating their LinkedIn Developer App:

| Scope | Required for |
|---|---|
| `w_member_social` | `create_post`, `comment`, `react`, `delete_post` |
| `r_liteprofile` | `get_profile` |
| `r_member_social` | `list_posts`, `get_engagement` |

The "Share on LinkedIn" and "Sign In with LinkedIn using OpenID Connect" products
must be requested in the LinkedIn Developer App dashboard (both auto-approve).

## Architecture

### File structure

| File | Role |
|---|---|
| `src/tools/linkedin.rs` | `Tool` trait impl, action dispatch, parameter validation |
| `src/tools/linkedin_client.rs` | OAuth2 token management, LinkedIn REST API wrappers |
| `src/tools/mod.rs` | Module declaration, pub use, registration in `all_tools_with_runtime` |
| `src/config/schema.rs` | `[linkedin]` config section (`LinkedInConfig`) |
| `src/config/mod.rs` | Add `LinkedInConfig` to pub use exports |

### No new dependencies

All required crates are already in `Cargo.toml`: `reqwest` (HTTP), `serde`/`serde_json`
(serialization), `chrono` (timestamps), `tokio` (async fs for .env reading).

## Config

### `config.toml`

```toml
[linkedin]
enabled = false
```

### `.env` credentials

```bash
LINKEDIN_CLIENT_ID=your_client_id
LINKEDIN_CLIENT_SECRET=your_client_secret
LINKEDIN_ACCESS_TOKEN=your_access_token
LINKEDIN_REFRESH_TOKEN=your_refresh_token
LINKEDIN_PERSON_ID=your_person_urn_id
```

Token format: `LINKEDIN_PERSON_ID` is the bare ID (e.g., `dXNlcjpA...`), not the
full URN. The client prefixes `urn:li:person:` internally.

## Tool design

### Single tool, action-dispatched

Tool name: `linkedin`

The LLM calls it with an `action` field and action-specific parameters:

```json
{ "action": "create_post", "text": "...", "visibility": "PUBLIC" }
```

### Actions

| Action | Params | API | Write? |
|---|---|---|---|
| `create_post` | `text`, `visibility?` (PUBLIC/CONNECTIONS, default PUBLIC), `article_url?`, `article_title?` | `POST /rest/posts` | Yes |
| `list_posts` | `count?` (default 10, max 50) | `GET /rest/posts?author={personUrn}&q=author` | No |
| `comment` | `post_id`, `text` | `POST /rest/socialActions/{id}/comments` | Yes |
| `react` | `post_id`, `reaction_type` (LIKE/CELEBRATE/SUPPORT/LOVE/INSIGHTFUL/FUNNY) | `POST /rest/reactions?actor={actorUrn}` | Yes |
| `delete_post` | `post_id` | `DELETE /rest/posts/{id}` | Yes |
| `get_engagement` | `post_id` | `GET /rest/socialActions/{id}` | No |
| `get_profile` | (none) | `GET /rest/me` | No |

Note: `list_posts` queries posts authored by the authenticated user (not a home feed —
LinkedIn does not expose a home feed API). `get_engagement` returns likes/comments/shares
counts for a specific post via the socialActions endpoint.

### Security enforcement

- Write actions (`create_post`, `comment`, `react`, `delete_post`): check `security.can_act()` + `security.record_action()`
- Read actions (`list_posts`, `get_engagement`, `get_profile`): still call `record_action()` for rate tracking

### Parameter validation

- `article_title` without `article_url` returns error: "article_title requires article_url"
- `react` requires both `post_id` and `reaction_type`
- `comment` requires both `post_id` and `text`
- `create_post` requires `text` (non-empty)

### Parameter schema

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": ["create_post", "list_posts", "comment", "react", "delete_post", "get_engagement", "get_profile"],
      "description": "The LinkedIn action to perform"
    },
    "text": {
      "type": "string",
      "description": "Post or comment text content"
    },
    "visibility": {
      "type": "string",
      "enum": ["PUBLIC", "CONNECTIONS"],
      "description": "Post visibility (default: PUBLIC)"
    },
    "article_url": {
      "type": "string",
      "description": "URL to attach as article/link preview"
    },
    "article_title": {
      "type": "string",
      "description": "Title for the attached article (requires article_url)"
    },
    "post_id": {
      "type": "string",
      "description": "LinkedIn post URN for comment/react/delete/engagement"
    },
    "reaction_type": {
      "type": "string",
      "enum": ["LIKE", "CELEBRATE", "SUPPORT", "LOVE", "INSIGHTFUL", "FUNNY"],
      "description": "Reaction type for the react action"
    },
    "count": {
      "type": "integer",
      "description": "Number of posts to retrieve (default 10, max 50)"
    }
  },
  "required": ["action"]
}
```

## LinkedIn client

### `LinkedInClient` struct

```rust
pub struct LinkedInClient {
    workspace_dir: PathBuf,
}
```

Uses `crate::config::build_runtime_proxy_client_with_timeouts("tool.linkedin", 30, 10)`
per request (same pattern as Pushover), respecting runtime proxy configuration.

### Credential loading

Same pattern as `PushoverTool`: reads `.env` from `workspace_dir`, parses key-value
pairs, supports `export` prefix and quoted values.

### Token refresh

1. All API calls use `LINKEDIN_ACCESS_TOKEN` in `Authorization: Bearer` header
2. On 401 response, attempt token refresh:
   - `POST https://www.linkedin.com/oauth/v2/accessToken`
   - Body: `grant_type=refresh_token&refresh_token=...&client_id=...&client_secret=...`
3. On successful refresh, update `LINKEDIN_ACCESS_TOKEN` in `.env` file via
   line-targeted replacement (read all lines, replace the matching key line, write back).
   Preserves `export` prefixes, quoting style, comments, and all other keys.
4. Retry the original request once
5. If refresh also fails, return error with clear message about re-authentication

### API versioning

All requests include:
- `LinkedIn-Version: 202402` header (stable version)
- `X-Restli-Protocol-Version: 2.0.0` header
- `Content-Type: application/json`

### React endpoint details

The `react` action sends:
- `POST /rest/reactions?actor=urn:li:person:{personId}`
- Body: `{"reactionType": "LIKE", "object": "urn:li:ugcPost:{postId}"}`

The actor URN is derived from `LINKEDIN_PERSON_ID` in `.env`.

### Response parsing

The client returns structured data types:

```rust
pub struct PostSummary {
    pub id: String,
    pub text: String,
    pub created_at: String,
    pub visibility: String,
}

pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub headline: String,
}

pub struct EngagementSummary {
    pub likes: u64,
    pub comments: u64,
    pub shares: u64,
}
```

## Registration

In `src/tools/mod.rs` (follows `security_ops` config-gated pattern):

```rust
// Module declarations
pub mod linkedin;
pub mod linkedin_client;

// Re-exports
pub use linkedin::LinkedInTool;

// In all_tools_with_runtime():
if root_config.linkedin.enabled {
    tool_arcs.push(Arc::new(LinkedInTool::new(
        security.clone(),
        workspace_dir.to_path_buf(),
    )));
}
```

## Config schema

In `src/config/schema.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinkedInConfig {
    pub enabled: bool,
}

impl Default for LinkedInConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}
```

Added as field `pub linkedin: LinkedInConfig` on the `Config` struct.
Added to `pub use` exports in `src/config/mod.rs`.

## Testing

### Unit tests (in `linkedin.rs`)

- Tool name, description, schema validation
- Action dispatch routes correctly
- Write actions blocked in read-only mode
- Write actions blocked by rate limiting
- Missing required params return clear errors
- Unknown action returns error
- `article_title` without `article_url` returns validation error

### Unit tests (in `linkedin_client.rs`)

- Credential parsing from `.env` (plain, quoted, export prefix, comments)
- Missing credential fields produce specific errors
- Token refresh writes updated token back to `.env` preserving other keys
- Post creation builds correct request body with URN formatting
- React builds correct query param with actor URN
- Visibility defaults to PUBLIC when omitted

### Registry tests (in `mod.rs`)

- `all_tools` excludes `linkedin` when `linkedin.enabled = false`
- `all_tools` includes `linkedin` when `linkedin.enabled = true`

### Integration tests

Not added in this PR — would require live LinkedIn API credentials.
A `#[cfg(feature = "test-linkedin-live")]` gate can be added later.

## Error handling

- Missing `.env` file: "LinkedIn credentials not found. Add LINKEDIN_* keys to .env"
- Missing specific key: "LINKEDIN_ACCESS_TOKEN not found in .env"
- Expired token + no refresh token: "LinkedIn token expired. Re-authenticate or add LINKEDIN_REFRESH_TOKEN to .env"
- `article_title` without `article_url`: "article_title requires article_url to be set"
- API errors: pass through LinkedIn's error message with status code
- Rate limited by LinkedIn: "LinkedIn API rate limit exceeded. Try again later."
- Missing scope: "LinkedIn API returned 403. Ensure your app has the required scopes: w_member_social, r_liteprofile, r_member_social"

## PR metadata

- **Branch:** `feature/linkedin-tool`
- **Title:** `feat(tools): add native LinkedIn integration tool`
- **Risk:** Medium — new tool, external API, no security boundary changes
- **Size target:** M (2 new files ~200-300 lines each, 3-4 modified files)
