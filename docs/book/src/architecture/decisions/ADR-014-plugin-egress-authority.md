---
id: ADR-014
title: Plugin instances reach the network only through one host-owned egress authority
date: 2026-08-20
status: proposed
relates-to:
  - ADR-006
  - ADR-009
  - https://github.com/zeroclaw-labs/zeroclaw/issues/9395
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8850
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8398
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9580
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9137
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9582
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9584
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9126
  - crates/zeroclaw-infra/src/net_guard.rs
  - crates/zeroclaw-plugins/src/egress.rs
  - crates/zeroclaw-plugins/src/component.rs
  - crates/zeroclaw-config/src/schema.rs
  - crates/zeroclaw-tools/src/helpers/domain_guard.rs
---

# ADR-014: Plugin Instances Reach the Network Only Through One Host-Owned Egress Authority

## Context

ZeroClaw links the `wasi:http` import into a plugin store when the plugin's manifest requests the `http_client` permission. That store installs wasmtime's default request hooks (`crates/zeroclaw-plugins/src/component.rs`), so nothing on the ZeroClaw side decides where the guest may connect: no destination allowlist, no address-class guard, no operator-facing configuration. Manifest permissions are requests, and the host today grants every request verbatim (`crates/zeroclaw-plugins/src/instance.rs`), while `plugins.security.signature_mode` defaults to `disabled`. An unsigned component can therefore declare `http_client`, receive it, and become an unfiltered pivot to loopback services, the local gateway, or cloud metadata addresses. Bug [#9395](https://github.com/zeroclaw-labs/zeroclaw/issues/9395) records that consequence.

The host-side sibling already fails closed. The `http_request` tool refuses every call when its `allowed_domains` list is empty, and validates the destination and every resolved address against metadata, loopback, and private ranges through `crates/zeroclaw-tools/src/helpers/domain_guard.rs` and `crates/zeroclaw-infra/src/net_guard.rs`. Two outbound paths in one process carried opposite guarantees.

Pressure on this boundary grows with ADR-006, which makes runtime plugins the target for optional channels, because every messaging channel is an egress consumer. Today `wit/v0/inbound.wit` states plainly that a channel plugin runs with no network and no sockets, and the official plugin repository drafts socket and WebSocket interfaces for transports the host does not expose. Whichever transport arrives next needs an existing answer to "may this instance reach that address", not a third one.

Part of this direction has shipped. [#9580](https://github.com/zeroclaw-labs/zeroclaw/pull/9580) moved the built-in HTTP egress onto a shared network guard, and [#9137](https://github.com/zeroclaw-labs/zeroclaw/pull/9137) added the plugin-side egress policy foundation. Three changes remain in review and are not part of the running system: [#9582](https://github.com/zeroclaw-labs/zeroclaw/pull/9582) for enforcement at the `wasi:http` boundary, [#9584](https://github.com/zeroclaw-labs/zeroclaw/pull/9584) for the operator grant ceremony, and [#9126](https://github.com/zeroclaw-labs/zeroclaw/pull/9126) for typed instance configuration. This record states the decision the merged foundation implements and the in-review changes complete, and it settles the network slice of RFC [#8398](https://github.com/zeroclaw-labs/zeroclaw/issues/8398): Q1 for network permissions, Q4 for user-extended destination grants.

The alternatives are to treat the manifest declaration itself as the grant, to keep one global allowlist for every installed plugin, or to give each transport its own destination policy. Manifest-as-grant preserves the #9395 self-grant path for unsigned packages, violates the default-closed doctrine, and makes the manifest a second source of truth for live authority. A global allowlist denies per-instance isolation, because any installed plugin could then reach every host any other plugin needs. Per-transport policies put three knobs on one question and invite drift, when the destination decision is transport-independent.

## Decision

### Use one egress authority with shared policy machinery and default deny

All plugin outbound network access, `wasi:http` today and any future socket, WebSocket, or TLS-profile import, is mediated by one host-owned egress authority. Destination matching, address classification, NAT64 translation, and the post-resolution verdict live in `zeroclaw-infra::net_guard`, which the built-in tools already use. `crates/zeroclaw-plugins/src/egress.rs` holds the instance-scoped service that consumes those primitives and re-implements none of them, and `zeroclaw-plugins` takes no dependency on `zeroclaw-tools` for this. A plugin and a built-in tool must not be able to disagree about whether a destination is reachable.

A plugin instance with no granted destination has no network reach. There is no compatibility mode in which `http_client` alone confers unrestricted HTTP. The permission grants the surface; the operator's list grants the destinations.

The service re-checks the transport's required permission at the operation boundary even though the linker exposes only the imports an admitted instance was granted. The duplicate check is deliberate defense in depth: a transport adapter must not be able to turn a linked-but-ungranted import into network reach.

### Enforce at the `wasi:http` boundary with a pinned send path

Enforcement lives at the transport boundary. A ZeroClaw hooks implementation stored per plugin store replaces wasmtime's default hooks, and its request hook evaluates policy on every guest-issued request. The send path is host-owned and pinned: resolve the destination once, validate the resolved addresses through the shared guard, then connect only to those exact addresses, using the hostname for SNI and certificate verification. A DNS answer cannot change class between the check and the connection because there is no second resolution. `ResolvedDestination` exists to keep that property structural: it retains the validated addresses and offers no route back to a fresh lookup.

The host never follows redirects on a guest's behalf. A guest that chooses to chase a redirect issues a new request, and every hop passes the full policy independently.

A denial returns a masked error to the guest that names the policy rather than host internals, and emits a structured host-side log event attributing the attempt to the exact instance.

This seam is the subject of #9582 and is in review. The store built on master still installs the default hooks.

### Let the manifest declare and the operator's configuration grant

A plugin manifest may declare the destinations the package needs in an `[egress]` section: exact hosts or explicit suffix patterns. That declaration is a signature-covered request, a statement of intent that travels with the package. It is never consulted as a grant at request time.

The grant lives in the operator's canonical configuration, as plaintext fields on the instance's `[[plugins.entries]]` row: `egress_hosts` for destinations and `egress_allow_private` for the address-class carveout. Both are deliberate siblings of the `#[secret]` `config` map and never inside it, because the allowlist is the thing an operator audits and it has to stay readable in the file they audit.

The effective reach of an instance is the intersection of what its manifest requests and what the operator granted. A declaration confers nothing on its own, and an operator grant cannot reach past what the package declared it needs. Two grant paths are first-class:

1. Seeded from the declaration during the grant ceremony below.
2. Authored directly by the operator, which is the only possible path for a plugin whose destination is itself instance configuration, such as a self-hosted Gitea, a Matrix homeserver, or a LAN Nextcloud. The author cannot know the host, so the operator who configures it grants it.

Policy is read from canonical configuration on each request rather than snapshotted into instance state, so an operator's edit applies to the next connection instead of the next restart. ADR-012, still proposed, describes the related generation-scoped apply mode; it is context here, not authority.

The operator fields and the per-request read are merged. The manifest `[egress]` declaration, and therefore the intersection, are not yet implemented; gate G1 covers them.

### Keep the destination grammar free of an allow-all form

Entries are exact hosts or explicit `*.suffix` patterns. A bare domain never implies its subdomains, a suffix pattern never matches the apex, and no entry means "anywhere": validation refuses a bare `*`, and an unvalidated one would only ever match a host literally named `*`, so even a list that escaped validation fails closed. An IP-literal host is matched only by an exactly equal entry and never by a suffix pattern, because addresses have no subdomain structure and treating one as a dotted name would let `*.0.0.1` match `127.0.0.1`. An empty list matches nothing. Ports are not part of an entry: granting a host grants every port on it.

### Keep address classes blocked above the allowlist

Cloud metadata addresses are refused unconditionally, in both access modes, so an operator opt-in for private destinations never re-opens them. Loopback, link-local, and private-range destinations are refused even when a pattern matches, unless the entry lists that host in `egress_allow_private`. The carveout relaxes the address class only: a host listed there but not in `egress_hosts` is still denied, and it mirrors the `allowed_private_hosts` semantics the built-in tools already use so both consumers keep one vocabulary.

An answer set that spans both trust zones is rejected rather than collapsed to one of them, even when private access is authorized. Otherwise resolver ordering or connection fallback silently decides which zone the connection lands in.

### Make operators declare NAT64 prefixes, and fail a malformed list closed

A NAT64 translator delivers any IPv6 destination inside its configured prefix to the IPv4 address embedded in that destination. The prefix is a deployment choice and nothing in the address reveals it, so an attacker who controls a hostname's DNS answer can return an apparently global IPv6 address that the local translator delivers to `10.0.0.1` or `169.254.169.254`. `security.nat64_prefixes` is where operators declare the translations their network actually runs. Entries are `<ipv6>/<length>` using RFC 6052 section 2.2 lengths, all six of them, with no bits set beyond the prefix.

Two rules follow, and both are stricter than the obvious implementation. First, one bad entry rejects the whole list instead of reducing it to its well-formed subset, because a list that parsed to "no prefixes" looks exactly like a deployment that runs no translator and would disable network-specific classification with no signal. Second, declared prefixes may overlap, and one address then decodes to a different IPv4 destination under each; every one of those destinations is reachable, so an address is accepted only when every declared translation it matches lands somewhere acceptable, rather than on the first that happens to be acceptable.

### Make the grant an explicit ceremony

`zeroclaw plugin install` prints a package's declared destinations and seeds them into the entries it already creates. An instance whose binding is created later, such as a channel alias, receives its egress entry when that binding is created, from the same declaration and with the same printout. Installation and binding creation are explicit operator acts, and the printed, persisted allowlist is their record. `zeroclaw plugin list` shows each instance's granted destinations so reach can be audited without reading the configuration file.

A package upgrade whose declaration adds destinations does not extend an existing entry. The CLI prints the difference and the operator applies it deliberately. Absent an entry, egress is denied.

The ceremony is the subject of #9584 and is in review.

### Bind future transports to the same policy object

Socket, WebSocket, and TLS-profile imports, when the host adopts them, consume this same per-instance policy. A socket connect or a WebSocket upgrade to a host and port passes the same allowlist and the same address-class rules, and a TLS profile selects trust material but never bypasses destination policy. A transport import that cannot route its destination decision through this service does not land.

Raw-TCP channels are out of scope for this record until a WASI sockets capability exists on the host. Until then, a channel whose protocol is not HTTP has no plugin-side path and remains native under ADR-006's capability-exception rule. Matrix is such an exception and this record does not retire it: end-to-end encryption is the named missing host capability, and the native implementation stays until the plugin boundary can carry it. That is an ADR-006 question about capability parity, and nothing here changes it.

A grant is protocol-independent. An operator who grants a host grants it for whatever transport that instance's permissions already allow, so there is no separate plaintext axis and no `egress_allow_plaintext` field. Confidentiality is the transport's contract with its peer, not a second destination list. Where a protocol negotiates TLS in band, the host owns the phase transition and never permits a plaintext fallback once the upgrade has begun.

Per-instance connection budgets belong to the foundation rather than to each transport. `plugins.limits.max_connections_per_instance` is shared across every transport and every store belonging to one logical instance, and an authorized request holds a lease for its connection's lifetime.

### Bound untrusted manifest input at the same boundary

A manifest is attacker-controlled input at the same trust boundary as an egress destination, so cheap manifest bytes must not buy expensive host work. The typed instance-configuration work in review (#9126) bounds pattern validation in manifest-supplied schemas accordingly: patterns compile on a linear-time engine under an explicit 256 KiB compiled-program limit and a 1 MiB DFA limit, and a pattern that exceeds either is rejected as an invalid manifest rather than compiled. Rejecting the package is the safe direction, because a plugin author who wants a pattern that expensive can write a smaller one, while a host that has already started compiling it cannot get its scheduler back. This is recorded here because it hardens the boundary this ADR owns, not because it sits on the egress path.

### Acceptance gates

This ADR remains proposed until all of these conditions are met:

- the shared guard primitives live in `zeroclaw-infra::net_guard` with both consumers on them, the per-store hooks and the pinned send path ship for plugin stores, the manifest `[egress]` declaration exists with parsing, validation, and signature coverage, the effective grant is the intersection of that declaration with the operator's entry, and install-time and binding-time seeding and the upgrade-diff ceremony work (G1);
- required CI proves the boundary with a real component fixture: denied by default with no entry, allowed through a seeded entry, metadata and private-address refusal over a matching allowlist, and a component that chases a redirect from an allowed host toward a blocked class has its second request denied (G2);
- the first channel plugin selected by [#8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850) runs under a seeded entry for its API host (G3); and
- the rollout for the existing fleet is complete: official registry `http_client` packages carry `[egress]` declarations in republished versions before host enforcement turns on, an upgrade-time diagnostic lists each installed instance's denied destinations with the exact seeding command, the release that enables enforcement names the break in its changelog, and #9395 closes (G4).

The shared guard, the plaintext operator fields, the per-request policy read, the strict destination grammar, the NAT64 boundary, and the connection budget are in place. The rest of G1, and G2 through G4, are not.

## Consequences

Positive consequences:

- The #9395 pivot closes by construction rather than by signature policy alone, because an unsigned or malicious component cannot convert a manifest self-request into network reach.
- Operators get one auditable plaintext knob per instance, and an edit applies to the next connection without re-instantiating the guest.
- A package upgrade cannot silently widen egress.
- Built-in tools and plugins converge on one guard vocabulary instead of drifting apart.
- A new transport inherits the destination decision instead of reopening it.

Negative consequences:

- This is a breaking change for every already-installed `http_client` plugin: until its entry is seeded or authored, it has no egress. G4 exists because of that, and the break ships only with the migration diagnostic and republished declarations in place.
- The ceremony adds an install-time and a binding-time step, plus per-entry policy state.
- Plugins with dynamic destination needs, such as user-supplied URLs, do not get wildcard egress. They declare suffix patterns the operator accepts, rely on operator-authored entries, or wait for a host-mediated fetch service. That is the deliberate cost of the answer to #8398's Q4.
- Deployments behind a NAT64 translator must declare their prefixes to keep the boundary accurate, and conservative evaluation of overlapping prefixes can refuse a destination the operator considers legitimate.
- The policy read happens per request rather than per instantiation, so the configuration resolver is on the connection path.

## References

- [Bug #9395: plugin `wasi:http` egress has no destination policy and no configuration knob](https://github.com/zeroclaw-labs/zeroclaw/issues/9395)
- [Migration tracker #8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850)
- [RFC #8398: plugin permission, config, and secrets model](https://github.com/zeroclaw-labs/zeroclaw/issues/8398)
- [PR #9580: harden built-in HTTP egress on the shared network guard](https://github.com/zeroclaw-labs/zeroclaw/pull/9580) (merged)
- [PR #9137: shared egress policy foundation](https://github.com/zeroclaw-labs/zeroclaw/pull/9137) (merged)
- [PR #9582: enforce a host-owned egress policy on plugin `wasi:http`](https://github.com/zeroclaw-labs/zeroclaw/pull/9582) (in review)
- [PR #9584: egress grant ceremony for plugin install and list](https://github.com/zeroclaw-labs/zeroclaw/pull/9584) (in review)
- [PR #9126: typed instance configuration validation](https://github.com/zeroclaw-labs/zeroclaw/pull/9126) (in review)
- [ADR-006: Runtime channel plugins](./ADR-006-runtime-channel-plugins.md)
- [ADR-009: WIT and wasmtime plugin execution](./ADR-009-wit-wasmtime-plugin-execution.md)
- [ADR-012: Generation-scoped live config apply](./ADR-012-generation-scoped-live-config-apply.md)
- [Security model](../../security/model.md)
- `crates/zeroclaw-infra/src/net_guard.rs`
- `crates/zeroclaw-plugins/src/egress.rs`
- `crates/zeroclaw-plugins/src/component.rs`
- `crates/zeroclaw-config/src/schema.rs`
- `crates/zeroclaw-tools/src/helpers/domain_guard.rs`
- `crates/zeroclaw-tools/src/http_request.rs`
