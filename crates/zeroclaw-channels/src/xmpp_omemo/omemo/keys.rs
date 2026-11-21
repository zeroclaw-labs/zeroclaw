// src/omemo/keys.rs
//! Typed wrappers for cryptographic key material.
//!
//! Three problems `&[u8]` cannot prevent:
//!   1. Argument-order swaps (salt vs IKM, key vs IV) — compile error with distinct types.
//!   2. Wrong-size keys reaching a cipher — compile error via fixed-size newtypes.
//!   3. Accidental logging of secrets — `Secret<N>` has a redacting `Debug`; no `Display`,
//!      no `Deref`, no `AsRef`. Logging requires an explicit `expose_secret()` call.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use zeroize::Zeroize;

// ── Secret<N> ───────────────────────────────────────────────────────────────

/// A fixed-size secret value with zeroize-on-drop and a redacting Debug.
///
/// Intentionally has no `Display`, `Deref<Target=[u8]>`, or `AsRef<[u8]>`.
/// Access bytes with `.expose_secret()` — the name shows up in code review
/// and `grep expose_secret` finds every exfiltration site.
///
/// Serializes/deserializes identically to `Vec<u8>` (length-prefixed) so
/// existing bincode blobs are fully compatible.
pub struct Secret<const N: usize>([u8; N]);

impl<const N: usize> Secret<N> {
    pub fn new(bytes: [u8; N]) -> Self {
        Self(bytes)
    }

    /// Construct from a slice. Returns `None` if the length is not exactly N.
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        <[u8; N]>::try_from(b).ok().map(Secret)
    }

    /// The only way to get bytes out. Greppable and visible in code review.
    pub fn expose_secret(&self) -> &[u8; N] {
        &self.0
    }
}

impl<const N: usize> Clone for Secret<N> {
    fn clone(&self) -> Self {
        Secret(self.0)
    }
}

impl<const N: usize> fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret<{}>(redacted)", N)
    }
}

impl<const N: usize> PartialEq for Secret<N> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<const N: usize> Eq for Secret<N> {}

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

// Serialize as Vec<u8> so bincode layout is identical to the old Vec<u8> fields.
impl<const N: usize> Serialize for Secret<N> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.to_vec().serialize(s)
    }
}

impl<'de, const N: usize> Deserialize<'de> for Secret<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(d)?;
        let array: [u8; N] = bytes.try_into().map_err(|v: Vec<u8>| {
            serde::de::Error::custom(format!("expected {} bytes, got {}", N, v.len()))
        })?;
        Ok(Secret(array))
    }
}

// ── Cipher key newtypes ──────────────────────────────────────────────────────

/// 16-byte AES-128-GCM key. Zeroized on drop.
#[derive(Clone, Debug)]
pub struct AesGcmKey(Secret<16>);

impl AesGcmKey {
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        Secret::<16>::from_slice(b).map(AesGcmKey)
    }
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.expose_secret()
    }
}

/// 32-byte AES-256-CBC key. Zeroized on drop.
#[derive(Clone, Debug)]
pub struct AesCbcKey(Secret<32>);

impl AesCbcKey {
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        Secret::<32>::from_slice(b).map(AesCbcKey)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.expose_secret()
    }
}

/// 12-byte AES-GCM nonce (IV). Not secret — nonces are transmitted publicly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GcmNonce([u8; 12]);

impl GcmNonce {
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        <[u8; 12]>::try_from(b).ok().map(GcmNonce)
    }
    pub fn as_bytes(&self) -> &[u8; 12] {
        &self.0
    }
}

/// 16-byte AES-CBC IV. Not secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbcIv([u8; 16]);

impl CbcIv {
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        <[u8; 16]>::try_from(b).ok().map(CbcIv)
    }
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

// ── HKDF label types ─────────────────────────────────────────────────────────

/// Newtype for the `salt` parameter of `hkdf_derive`.
/// Prevents silently swapping salt and IKM (they have opposite roles in HKDF).
#[derive(Clone, Copy)]
pub struct Salt<'a>(pub &'a [u8]);

/// Newtype for the `ikm` (input key material) parameter of `hkdf_derive`.
#[derive(Clone, Copy)]
pub struct Ikm<'a>(pub &'a [u8]);

impl<'a> From<&'a [u8]> for Salt<'a> {
    fn from(b: &'a [u8]) -> Self {
        Salt(b)
    }
}
impl<'a> From<&'a [u8]> for Ikm<'a> {
    fn from(b: &'a [u8]) -> Self {
        Ikm(b)
    }
}
// Allow constructing Salt/Ikm directly from a Secret reference.
impl<'a, const N: usize> From<&'a Secret<N>> for Salt<'a> {
    fn from(s: &'a Secret<N>) -> Self {
        Salt(s.expose_secret())
    }
}
impl<'a, const N: usize> From<&'a Secret<N>> for Ikm<'a> {
    fn from(s: &'a Secret<N>) -> Self {
        Ikm(s.expose_secret())
    }
}
impl<'a> From<&'a RootKey> for Salt<'a> {
    fn from(k: &'a RootKey) -> Self {
        Salt(k.0.expose_secret())
    }
}
impl<'a> From<&'a RootKey> for Ikm<'a> {
    fn from(k: &'a RootKey) -> Self {
        Ikm(k.0.expose_secret())
    }
}
impl<'a, D: 'static> From<&'a ChainKey<D>> for Ikm<'a> {
    fn from(k: &'a ChainKey<D>) -> Self {
        Ikm(k.0.expose_secret())
    }
}
impl<'a> From<&'a MessageKey> for Ikm<'a> {
    fn from(k: &'a MessageKey) -> Self {
        Ikm(k.0.expose_secret())
    }
}

// ── PreKey ID newtypes ───────────────────────────────────────────────────────
// Each ID type is distinct so `encrypt_key_prekey(msg, registration_id,
// opk_id, spk_id, ...)` cannot have its u32 arguments silently transposed.

// ── PublicKey ─────────────────────────────────────────────────────────────────

/// A Curve25519 public key in normalized 32-byte Montgomery form.
///
/// The 0x05 wire prefix is stripped at construction; `to_wire()` adds it back.
/// `Deref<Target=[u8]>` lets it be passed where `&[u8]` is expected, while
/// keeping the type distinct from `Secret<32>` so the two cannot be swapped
/// at a `x25519_diffie_hellman` call site.
///
/// Serializes/deserializes as `Vec<u8>` (length-prefixed) for bincode compat.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Construct from 32-byte raw form or 33-byte 0x05-prefixed wire form.
    /// Returns `None` for any other length.
    pub fn from_wire(b: &[u8]) -> Option<Self> {
        match b.len() {
            32 => <[u8; 32]>::try_from(b).ok().map(PublicKey),
            33 if b[0] == 0x05 => <[u8; 32]>::try_from(&b[1..]).ok().map(PublicKey),
            _ => None,
        }
    }

    pub fn as_raw(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl AsRef<[u8]> for PublicKey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({}…)", hex::encode(&self.0[..4]))
    }
}

// Serialize as Vec<u8> (length-prefixed) so bincode layout matches old Vec<u8>.
impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.to_vec().serialize(s)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(d)?;
        let array: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
            serde::de::Error::custom(format!("expected 32 bytes, got {}", v.len()))
        })?;
        Ok(PublicKey(array))
    }
}

// ── EphemeralPrivateKey ───────────────────────────────────────────────────────

/// The private half of an X3DH initiator ephemeral key pair.
///
/// Wrapping in a distinct type prevents it from being silently passed
/// alongside three other `Vec<u8>` public keys in `new_initiator_with_ephemeral`.
/// Zeroized on drop via the inner `Secret<32>`.
#[cfg(test)]
pub struct EphemeralPrivateKey(pub Secret<32>);

#[cfg(test)]
impl fmt::Debug for EphemeralPrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EphemeralPrivateKey(redacted)")
    }
}

// ── Ratchet key role newtypes ─────────────────────────────────────────────────
// Three Secret<32> in RatchetState are mutually assignable.
// Distinct types make cross-assignment (e.g. root_key → send_chain_key) a compile error.

/// The Double Ratchet root key, fed into KDF_RK each DH step.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootKey(pub Secret<32>);

impl RootKey {
    pub fn expose_secret(&self) -> &[u8; 32] {
        self.0.expose_secret()
    }
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        Secret::from_slice(b).map(RootKey)
    }
}
impl fmt::Debug for RootKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RootKey(redacted)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sending;
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Receiving;

/// A HMAC-SHA256-derived chain key, parameterised by direction to prevent
/// the send chain key from being assigned into the receive chain slot.
#[derive(Clone, PartialEq, Eq)]
pub struct ChainKey<D>(pub Secret<32>, pub std::marker::PhantomData<fn() -> D>);

impl<D> ChainKey<D> {
    pub fn new(s: Secret<32>) -> Self {
        Self(s, std::marker::PhantomData)
    }
    pub fn expose_secret(&self) -> &[u8; 32] {
        self.0.expose_secret()
    }
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        Secret::from_slice(b).map(|s| Self(s, std::marker::PhantomData))
    }
}
impl<D> fmt::Debug for ChainKey<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChainKey(redacted)")
    }
}
// Serialize/deserialize as Vec<u8> (same as Secret<32>) for bincode compat.
impl<D: 'static> Serialize for ChainKey<D> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}
impl<'de, D: 'static> Deserialize<'de> for ChainKey<D> {
    fn deserialize<De: Deserializer<'de>>(d: De) -> Result<Self, De::Error> {
        Ok(ChainKey::new(Secret::deserialize(d)?))
    }
}

/// A message key derived from a chain key; used once to encrypt/decrypt one message.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageKey(pub Secret<32>);

impl MessageKey {
    pub fn expose_secret(&self) -> &[u8; 32] {
        self.0.expose_secret()
    }
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        Secret::from_slice(b).map(MessageKey)
    }
}
impl fmt::Debug for MessageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageKey(redacted)")
    }
}
