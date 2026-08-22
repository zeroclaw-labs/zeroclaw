//! Bootstrap install launcher for ZeroClaw.
//!
//! An MCP server inside the ZeroClaw binary cannot install that binary when it
//! is absent. This crate is the small distribution client that closes that
//! gap: an external harness runs it to identify the host, review an install,
//! install exactly the artifact a human approved, and hand off to
//! `zeroclaw control --mcp`. It is a distribution client, not a second
//! configuration service.
//!
//! # What it will not do
//!
//! The refusals are the product, so they are structural rather than
//! defensive:
//!
//! - **No arbitrary URL.** Every request is a [`origin::PinnedUrl`], and that
//!   type has no constructor taking caller-supplied text. The origin is a
//!   compile-time constant and a release tag is charset-validated before it
//!   can reach a URL.
//! - **No arbitrary install root, asset name, or command.** The install
//!   directory is derived from the platform family; the asset name is
//!   generated from the canonical registry; there is no command argument at
//!   all.
//! - **No install without an explicit human decision.** `install` requires the
//!   plan digest that `plan` printed. A model cannot satisfy that by asserting
//!   approval, because the token is a hash of the exact plan and is not
//!   derivable from the request.
//! - **No unverified bytes in the install path.** The expected digest is read
//!   from the release checksum manifest and bound into the approved plan; the
//!   download is hashed and compared before anything is written.
//! - **No configuration authority.** This crate never reads or writes
//!   `config.toml`, holds no config schema or provider catalog, and cannot
//!   approve anything.
//! - **No silent replacement.** An existing binary whose identity cannot be
//!   verified produces a repair recommendation, never an overwrite or an
//!   execution.
//!
//! # What "verified" means today
//!
//! Digest verification is real: the artifact is hashed and compared against
//! the release's `SHA256SUMS` entry before installation. Signature
//! verification is **not** performed. ZeroClaw releases carry GitHub-hosted
//! SLSA provenance attestations, but verifying them requires a Sigstore
//! verifier this launcher deliberately does not carry, so
//! [`plan::SignatureStatus`] reports the attestation as published-and-unverified
//! and prints the command a human can run out of band. The checksum manifest
//! is fetched from the same origin as the artifact, so digest verification
//! establishes that the bytes are the ones that release published — not that
//! the release itself is authentic.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod fetch;
pub mod handoff;
pub mod install;
pub mod origin;
pub mod plan;
pub mod status;
pub mod target;

pub use error::BootstrapError;
