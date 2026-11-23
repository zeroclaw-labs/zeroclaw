# Remote Files tool contract

Design proposal for [RFC #12690](https://github.com/zeroclaw-labs/zeroclaw/issues/12690) and the credential grants in [RFC #12422](https://github.com/zeroclaw-labs/zeroclaw/issues/12422). The proposed tool is a native, opt-in ZeroClaw capability. The language model calls typed operations using configured resource aliases and relative paths.

## Operations

| Operation | Scope and behavior |
| --- | --- |
| List directory | List a bounded page of entries, with metadata and a scoped continuation token where supported. |
| Copy files | Copy one or more explicitly named files between an authorized remote root and the workspace, or between two authorized remote roots. Require read and destination create permissions. |
| Move files | Require source move and removal rights and destination create rights. A streamed move also requires source read. Verify the destination before attempting source removal. |
| Delete files | Delete one or more explicitly named regular files. |
| Rename files | Rename one or more files within their parent directories. Validate both names, reject duplicate destinations before starting and report outcomes per item. |
| Create directory | Create one directory. Creating missing parents requires explicit permission. |
| Rename directory | Rename within its parent. Require rights over the affected subtree for nonempty directories. |
| Delete directory | Remove an empty directory by default. Recursion requires a separate grant and a bounded traversal. |
| Copy or move directory | Support scoped directory transfers with separate recursive copy and move permissions. Check both trees and report partial results. |
| Get file info | Return available size, kind and timestamps; include owner, mode or checksum only where supported and authorized. Fetching a checksum may require reading the file. |
| Set file info | Grant metadata writes separately. Start with modification time where supported. Ownership, ACLs, executable bits and privilege-bearing metadata require a separate design. |
| Login | Establish or validate a task-scoped connection to an authorized resource. For a host-managed mount, verify the configured mount identity and access. Never mount or escalate privileges. |
| Logout | Close task-owned sessions and discard ephemeral handles. For host-managed mounts, release tool-owned handles without unmounting the operator's volume. |

Batches have bounded item counts and per-item results. No silent overwrite, wildcard mutation or implicit recursion. Use a backend-native atomic move only where its semantics are guaranteed inside the authorized resource. Otherwise stage and verify the destination, publish it, then recheck source removal authorization. If removal fails, report that both copies remain.

## Backend boundaries

- **SMB3 over TCP or QUIC** uses a verified mount, native platform API, or in-process host-client binding. QUIC is offered only when the host supports that transport. An explicit QUIC selection cannot fall back to TCP.
- **NFS** uses the host client through a verified mount. The host manages its own identity and authentication.
- **SFTP** uses Rust protocol code and can reuse reviewed SSH authentication and host-key verification primitives. An SSH command grant confers no SFTP file access.
- **FTPS** uses an in-process Rust client with peer verification and TLS on both control and data connections. No FTP plaintext fallback.
- **WebDAV** uses Rust HTTP and WebDAV handling over HTTPS, with exact-origin checks and no automatic cross-origin credential forwarding.

The Rust implementation uses platform APIs or narrowly scoped FFI where necessary. It needs no shell, subprocess, Python, or MCP server. Dependencies require license, maintenance and security review before selection. Logout ends a logical task session for stateless backends and does not imply a wire-level logout request to servers that lack one.

## Vault and WebUI rights

The operator registers a resource alias, protocol, verified remote identity, and allowed root. The resource grant controls list, copy, move, delete, rename, directory creation, metadata operations and session opening. In **Secrets → Usage permissions**, the operator binds compatible credential aliases to the Remote Files consumer and exact registered destination. Host-managed identities still require a resource grant. Grants are read-only by default if the operator selects a read-only preset; no remote root is granted implicitly.

The **Require human approval** checkbox can be selected on each relevant grant. The approval describes the exact operation, source and destination, affected tree, overwrite or delete effects, and credential alias. A checked grant on either endpoint requires approval before connection or credential use. Global approval rules also apply. Recheck the live grant revision before dispatch and destructive phases; revocation invalidates cached authorization. Always allow cleanup of handles owned by the requesting task.

Before a stat, canonicalization, DNS request or any network I/O, authorize both endpoint aliases, operation and lexical paths. Access beneath authorized roots uses handle-relative or equivalent containment. Reject ambiguous paths and escapes. Verify peer identity before releasing credentials. Log scope and outcome with path redaction; secrets and file contents remain outside model context and ordinary audit output.

## Implementation and acceptance

1. Register the opt-in Rust tool and connect its typed requests to the existing tool dispatch, approval, and vault boundaries. Use the authoritative grant state.
2. Implement verified SMB/NFS mounts and in-process SFTP. Verify bounded list/stat, copy, move, delete and cancellation; add the WebUI editor and test grant, denial and revocation against disposable servers.
3. Add WebDAV, FTPS and platform-native SMB options. Report unsupported operations or transports explicitly. Validate Linux, Windows and macOS capabilities separately.
4. Exercise task cleanup, same-resource rename, streamed cross-resource moves, rotation, stale approvals, and both endpoint grants. Include zero-I/O regressions for denied destinations, link escapes and unapproved UNC paths.

The draft Rust policy evaluates a resolved pair of resource grants and rejects ambiguous lexical paths without I/O. Runtime registration, live grant resolution, backend containment and the WebUI are separate implementation steps. The draft does not enable remote access.

## Transfer destinations and identities

For a remote source, the destination is one of three kinds. A path on the same registered remote resource uses the source connection and credential, with independent rights checks on both paths. The ZeroClaw workspace uses the service's operating-system identity; files created there belong to the service account and remain subject to workspace policy. A different remote server requires its own destination grant and separately authorized credential reference (or an explicitly configured destination OS identity for a host-managed mount). The same vault alias may be granted independently to both endpoints, including username/password and a separately granted TOTP factor. Authenticate each endpoint through its own verified grant. Never forward a secret or an OTP obtained for the source session to a different server. Any different registered resource on the same server requires its own scope check and may reuse a connection only when both grants resolve to the same peer, account and credential binding. Uploading from the workspace uses the destination's credential or configured identity. The tool takes aliases and relative paths, never raw credentials from the model.

## Who enforces file permissions

The source and destination servers independently enforce their filesystem permissions, ACLs and share/export policies for the identities presented to them. A transfer cannot grant access the authenticated account lacks. ZeroClaw does not reproduce or manage remote ACLs, ownership, or account privileges. It reports remote permission denials and partial transfers as returned by the endpoints.

The WebUI grants constrain what this agent is allowed to attempt, where it may use a vault alias, and whether an operator must approve. These are ZeroClaw tool and credential-use controls. They do not confer filesystem rights on either server. The registered root constrains the tool's requested paths. Workspace operations run under the ZeroClaw service identity and are subject to local workspace confinement and OS permissions. Remote metadata is not used to set local ownership or ACLs.

## Staged transfer executor boundary

The shared Rust transfer coordinator copies to an adapter-owned staging destination with a byte limit. The adapter must verify destination integrity and finalize the staging object. For a move, the coordinator requests source removal only after the adapter confirms publication. If publication is uncertain, keep the source. If the destination was published but source removal is denied or unconfirmed, report a partial move. The removal adapter must revalidate the grant and the source object's identity before deletion. Adapters own temporary cleanup and must distinguish a definite rejection from an ambiguous response.

## Linux mounted-resource read adapter

The draft includes a read-only adapter for an operator-provisioned SMB 3 or NFS mount on Linux. It accepts a directory handle acquired from an authorized resource and checks the handle's mount ID, filesystem, and source against kernel mount information. SMB requires a reported SMB 3 dialect. Descendant opens use Linux openat2 with no symlinks and no mount crossing. The adapter rechecks the recorded mount before each operation. Directory listing returns a bounded sample and signals truncation. Only regular files are opened for reading. Kernels without those primitives return an error.

Caller integration still needs to acquire or refresh the handle from the registered mount, apply live authorization, and schedule blocking filesystem work outside the async executor. This adapter does not mount shares, write remote data, expose a tool, or implement Windows/macOS mounting. Tests use synthetic mount information and a local filesystem fixture; real SMB/NFS coverage remains pending.

## Identical capabilities across host platforms

The public operation set for each backend is the intersection of what the shared contract verifies on Linux, Windows and macOS. A backend with missing host facilities remains unavailable on that host. An implementation present on one or two platforms is experimental until the same behavior is verified on all three. The same rule applies to operations, errors and approval semantics. The Linux mount adapter is not advertised by itself.

## WebDAV over HTTPS adapter

The draft uses one portable Reqwest transport for HEAD size lookup and bounded GET into a staging writer. It accepts an operator-managed HTTPS directory URL and addresses already checked against the deployment's egress policy. DNS addresses are pinned for the client, TLS authenticates the peer, redirects are disabled, and the request URL is built beneath that root from validated path components. The adapter receives the authorization header from its caller and marks it sensitive.

Runtime integration must resolve live grants and verify the permitted egress addresses before authorizing use of a vault reference. Tests cover URL and address validation plus a disposable HTTPS server for metadata, download, chunked size limits and redirect refusal. Windows/macOS and production WebDAV execution remain pending. The adapter also exposes MKCOL for one directory and same-parent MOVE for rename, with Overwrite: F. Both paths are built within the registered root. The WebDAV server must enforce the no-overwrite precondition. A transport failure during a mutation requires state verification before retry. The staging upload streams a PUT from an async reader with a 64 KiB read buffer and optional per-file limit. Its conditional If-None-Match header protects the staging name when honored by the server. The adapter checks a known size before sending when a limit is configured and enforces the budget for unknown lengths while reading. The staging object remains unpublished until the executor verifies and commits it. An interrupted or rejected upload may leave staging data and must not remove the source. Chunked HTTP transfer framing does not provide restartable upload sessions.

Prefer server-managed COPY or MOVE within one configured resource when the backend provides the needed semantics. For independent servers, use streaming through ZeroClaw by default. Direct cross-server transfer and resumable multipart sessions require explicit backend support and their own authorization checks. The WebDAV adapter now exposes shallow COPY within a registered resource and publication of a verified staging file through same-parent MOVE. Both requests forbid overwrites. Publication distinguishes confirmed, rejected and uncertain outcomes for the transfer coordinator. Automatic route selection remains pending.

The WebDAV relay streams a response from one approved endpoint into the destination's temporary PUT. It uses distinct authorization headers for both endpoints, even when the caller resolved both from the same vault alias. It compares the staged object's reported size to the number of bytes sent before handing off to publication. The low-level relay does not delete or publish anything. The copy operation can publish the staged object when a caller-provided live grant check succeeds and supplies refreshed destination authorization. A revoked grant leaves staging unpublished. Automatic selection, actual vault integration and source removal remain separate.

The adapter can query DAV resource type using Depth: 0 PROPFIND. It bounds the XML response and accepts only one matching resource with a successful DAV resourcetype property. A different href, unknown type, foreign namespace or ambiguous result fails closed. This is a prerequisite for file-only removal because DAV DELETE on a collection can affect all descendants. In the single-file move path, the source response must provide a strong ETag. After confirmed destination publication and a fresh source grant, the adapter checks the current DAV type and sends DELETE with If-Match for the copied version. A rejected precondition preserves the source. Uncertain results require reconciliation and are not retried automatically. General delete and directory moves remain separate.

WebDAV directory listing uses Depth: 1 PROPFIND. It validates the directory response and only returns direct children under the same approved origin, with bounded XML and result counts. It rejects off-root and ambiguous hrefs instead of treating server-supplied URLs as callable destinations. The result reports truncation without claiming a WebDAV paging extension.

## Typed agent request

The configuration crate defines the strict tagged request format for all proposed operations. Remote endpoints contain a registered resource alias and a relative path. Workspace endpoints carry only a relative path. The format accepts no passwords, URLs, connection options or approval overrides. Validation rejects empty or oversized batches, repeated destination arguments and renames that change parent directories. Canonical endpoint resolution, server-aware collision checks, grants, and tool registration remain executor responsibilities.

The draft WebDAV read dispatcher resolves the operator's resource alias and checks the requested right and shared platform capability before invoking network or vault services. A grant requiring human approval returns a hold. The runtime still needs to wire the live catalog, approval service and credential resolver into this boundary. Only list and metadata requests are routed by this dispatcher.

The WebDAV resource catalog accepts operator-supplied aliases, HTTPS roots, and credential aliases. It rejects aliases that collide across letter case, and rejects URLs carrying authentication, query parameters, or fragments. Endpoint lookup uses a case-insensitive alias. The read dispatcher looks up live rights separately using the canonical alias. A registered resource by itself grants no access. The WebUI and runtime dispatch still need to provide live grants.

An operator can provision endpoint metadata in the main configuration file:

```toml
[[remote_files.webdav]]
alias = "REPORTS"
root = "https://files.example.test/dav/reports/"
credential_alias = "REPORT_LOGIN"
```

Config validation compiles the catalog without network access. The default is an empty catalog. Requests must still match a live grant for the canonical alias, exact root, and credential binding. Editing an endpoint does not extend an existing grant to its new destination. The tool is not registered by this setting alone.

The catalog-backed dispatcher uses the shared ZeroClaw network guard. It denies private resolution by default and permits it only when the operator policy explicitly allows the resolved endpoint. Every resolved address is checked, including NAT64 translations and cloud metadata addresses. The HTTPS transport pins the vetted addresses. If deployment proxy policy would make that pinning ineffective, the operation is denied instead of taking a direct path. The managed vault callback remains separate so the feature can reuse the existing credential catalog when that upstream work is available.

## Integration with the managed credential catalog

The review branch depends on the scoped credential catalog and loader in #12596. The WebDAV dispatcher uses the same encrypted credential records. Its dedicated live resource grant binds the exact endpoint and credential alias to the requested operation. HTTP and Browser grants do not supply that authorization. The resolver rechecks the stored encrypted record and resource grant before returning a sensitive authorization header.

This slice supports local encrypted username/password, Bearer and Token credentials. It fails closed for other types or external provider references. The inherited external lookup can spawn a provider command; the Remote Files dispatcher does not invoke that path. A shell-free external vault integration and end-to-end runtime wiring remain pending.

Remote Files grants now live inside the existing encrypted credential entry. The operator-only credential mutation endpoint validates the registered resource, exact HTTPS root, and credential alias. Grant-list edits require the current revision. The ordinary credential create path clears supplied grants, while compatible secret rotation can preserve existing bindings. Before network use, the dispatcher requires a persisted grant as well as the runtime's live operation authorization. The WebUI editor and registered agent entry point remain pending.

In the dashboard, the existing Add authorization action offers Remote Files (WebDAV) for compatible credentials with a registered resource. Selecting it replaces the generic URL field with the matching resource list, operation checkboxes and an optional human approval checkbox. Edit and Remove use the same revision-checked credential endpoint; edits to a WebDAV grant preserve the other consumer grants.
