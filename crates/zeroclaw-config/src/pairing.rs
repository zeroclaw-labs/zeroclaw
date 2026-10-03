use parking_lot::{Mutex, RwLock, RwLockReadGuard};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroclaw_macros::Configurable;

/// Maximum failed pairing attempts before lockout.
const MAX_PAIR_ATTEMPTS: u32 = 5;
/// Lockout duration after too many failed pairing attempts.
const PAIR_LOCKOUT_SECS: u64 = 300; // 5 minutes
/// Maximum number of tracked client entries to bound memory usage.
const MAX_TRACKED_CLIENTS: usize = 10_000;
/// Retention period for failed-attempt entries with no activity.
const FAILED_ATTEMPT_RETENTION_SECS: u64 = 900; // 15 min
/// Minimum interval between full sweeps of the failed-attempt map.
const FAILED_ATTEMPT_SWEEP_INTERVAL_SECS: u64 = 300; // 5 min

/// Smallest pairing-code length any configuration may select.
///
/// Six is the previously shipped length, so no configuration can produce a
/// code weaker (in length) than what the gateway used to generate.
pub const PAIRING_CODE_MIN_LENGTH: usize = 6;

/// Largest pairing-code length any configuration may select. Bounds the
/// terminal banner, the `X-Pairing-Code` header, and dashboard input.
pub const PAIRING_CODE_MAX_LENGTH: usize = 128;

/// Default pairing-code length. See [`PairingCodePolicy`] for the rationale.
pub const PAIRING_CODE_DEFAULT_LENGTH: usize = 32;

const NUMERIC_ALPHABET: &[u8] = b"0123456789";
const ALPHANUMERIC_ALPHABET: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// Crockford Base32: the digits plus the uppercase letters, minus `I`, `L`,
/// `O`, and `U`. Removing those kills the `0`/`O` and `1`/`I`/`l` confusions
/// that make a code painful to read aloud or retype.
const UNAMBIGUOUS_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Character family a pairing code is drawn from.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, zeroclaw_macros::ConfigEnum,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PairingCodeCharset {
    /// `0-9`. Compatibility mode for demos and retyping-heavy flows.
    /// Paired with `length = 6` this reproduces the previously shipped code exactly.
    Numeric,
    /// `0-9A-Za-z`, case-sensitive (62 symbols). The default: densest
    /// entropy per character for a code that is copied and pasted.
    #[default]
    Alphanumeric,
    /// Crockford Base32 (32 symbols): no `I`, `L`, `O`, or `U`. Slightly
    /// longer for the same entropy, but safe to read aloud or retype.
    Unambiguous,
}

impl PairingCodeCharset {
    /// The symbols a code of this family is drawn from.
    pub fn alphabet(self) -> &'static [u8] {
        match self {
            Self::Numeric => NUMERIC_ALPHABET,
            Self::Alphanumeric => ALPHANUMERIC_ALPHABET,
            Self::Unambiguous => UNAMBIGUOUS_ALPHABET,
        }
    }

    /// Entropy contributed by a single character of this family.
    pub fn bits_per_char(self) -> f64 {
        (self.alphabet().len() as f64).log2()
    }

    /// The name this family is written as in `config.toml`.
    ///
    /// Must stay in step with the `serde(rename_all = "snake_case")` above;
    /// `charset_config_names_match_serde` pins that.
    pub fn config_name(self) -> &'static str {
        match self {
            Self::Numeric => "numeric",
            Self::Alphanumeric => "alphanumeric",
            Self::Unambiguous => "unambiguous",
        }
    }

    /// Every supported family, for exhaustive tests and docs.
    pub fn all() -> [Self; 3] {
        [Self::Numeric, Self::Alphanumeric, Self::Unambiguous]
    }
}

/// Why a [`PairingCodePolicy`] is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairingCodePolicyError {
    /// Configured length is below [`PAIRING_CODE_MIN_LENGTH`].
    #[error(
        "pairing code length {length} is below the minimum of {PAIRING_CODE_MIN_LENGTH}; \
         a shorter code would be weaker than the gateway default"
    )]
    TooShort {
        /// The rejected length.
        length: usize,
    },
    /// Configured length is above [`PAIRING_CODE_MAX_LENGTH`].
    #[error("pairing code length {length} exceeds the maximum of {PAIRING_CODE_MAX_LENGTH}")]
    TooLong {
        /// The rejected length.
        length: usize,
    },
}

/// The one pairing-code generation policy.
///
/// Every code the gateway issues — the startup code, `zeroclaw gateway
/// get-paircode --new`, `POST /api/pairing/initiate`, and the rotate-device
/// flow — comes from this policy via [`PairingGuard`]. There is no second
/// pairing-code setting anywhere in the schema.
///
/// # Default
///
/// 32 case-sensitive alphanumeric characters ≈ **190.6 bits** of entropy,
/// against ≈19.9 bits for the six-digit numeric code shipped before the shared policy.
/// Pairing codes are pasted far more often than they are retyped, and a
/// pairing code is the gateway's front door: getting it wrong is an
/// authentication bypass, while getting it long is a paste.
///
/// Operators who transfer the code by hand have two supported alternatives,
/// both still far stronger than the old default:
///
/// ```toml
/// # Read-aloud friendly: 24 Crockford-Base32 chars ≈ 120 bits.
/// [gateway.pairing_code]
/// length = 24
/// charset = "unambiguous"
///
/// # Exact legacy behaviour. Only for local, short-lived pairing.
/// [gateway.pairing_code]
/// length = 6
/// charset = "numeric"
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "gateway.pairing_code"]
pub struct PairingCodePolicy {
    /// Number of characters in a generated pairing code
    /// (`6..=128`, default: 32).
    #[serde(default = "default_pairing_code_length")]
    #[credential_class = "public_value"]
    pub length: usize,
    /// Character family: `numeric`, `alphanumeric` (default), or
    /// `unambiguous`.
    #[serde(default)]
    #[credential_class = "public_value"]
    pub charset: PairingCodeCharset,
}

fn default_pairing_code_length() -> usize {
    PAIRING_CODE_DEFAULT_LENGTH
}

impl Default for PairingCodePolicy {
    fn default() -> Self {
        Self {
            length: default_pairing_code_length(),
            charset: PairingCodeCharset::default(),
        }
    }
}

impl PairingCodePolicy {
    /// Build a validated policy.
    pub fn new(length: usize, charset: PairingCodeCharset) -> Result<Self, PairingCodePolicyError> {
        let policy = Self { length, charset };
        policy.validate()?;
        Ok(policy)
    }

    /// The exact legacy policy: six numeric digits.
    ///
    /// Kept as a named constructor so the compatibility shape is testable
    /// and greppable rather than a magic pair of literals.
    pub const fn numeric_compat() -> Self {
        Self {
            length: 6,
            charset: PairingCodeCharset::Numeric,
        }
    }

    /// Reject lengths outside `PAIRING_CODE_MIN_LENGTH..=PAIRING_CODE_MAX_LENGTH`.
    pub fn validate(&self) -> Result<(), PairingCodePolicyError> {
        if self.length < PAIRING_CODE_MIN_LENGTH {
            return Err(PairingCodePolicyError::TooShort {
                length: self.length,
            });
        }
        if self.length > PAIRING_CODE_MAX_LENGTH {
            return Err(PairingCodePolicyError::TooLong {
                length: self.length,
            });
        }
        Ok(())
    }

    /// The length generation will actually use, paired with the configured
    /// length it replaced when the two differ.
    ///
    /// Split out from [`Self::generate`] so the clamp decision is a plain
    /// value a test can assert on directly, independent of the WARN that
    /// [`Self::generate`] emits when it fires.
    pub fn resolve_length(&self) -> (usize, Option<usize>) {
        let effective = self
            .length
            .clamp(PAIRING_CODE_MIN_LENGTH, PAIRING_CODE_MAX_LENGTH);
        let clamped_from = (effective != self.length).then_some(self.length);
        (effective, clamped_from)
    }

    /// The length actually used for generation.
    ///
    /// `Config::validate` rejects an out-of-range length, and the gateway
    /// `PATCH /api/config` path refuses such a write outright, so reaching
    /// the clamp means the policy was built by some other route. Generation
    /// must still never panic or emit a zero-length code.
    pub fn effective_length(&self) -> usize {
        self.resolve_length().0
    }

    /// Shannon entropy of a code generated under this policy, in bits.
    ///
    /// Reported so a security-posture surface can state the active strength
    /// rather than restating the raw length and charset.
    pub fn entropy_bits(&self) -> f64 {
        self.effective_length() as f64 * self.charset.bits_per_char()
    }

    /// Generate one pairing code using cryptographically secure randomness.
    ///
    /// Each character is drawn by rejection sampling so every symbol in the
    /// alphabet is equally likely — a plain `%` would bias the low symbols
    /// for any alphabet size that does not divide `u32::MAX + 1`.
    pub fn generate(&self) -> String {
        let alphabet = self.charset.alphabet();
        let n = alphabet.len() as u32;
        // Largest multiple of `n` that fits in u32; draws at or above it are
        // rejected to keep the modulo unbiased.
        let reject_threshold = (u32::MAX / n) * n;

        let (length, clamped_from) = self.resolve_length();
        if let Some(configured) = clamped_from {
            // Never silently downgrade or inflate a pairing code: the
            // operator asked for one strength and is getting another.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "configured_length": configured,
                        "effective_length": length,
                        "min": PAIRING_CODE_MIN_LENGTH,
                        "max": PAIRING_CODE_MAX_LENGTH,
                    })),
                "[gateway.pairing_code] length is out of range; generating at the clamped \
                 length instead — fix `gateway.pairing_code.length` in config.toml"
            );
        }
        let mut code = String::with_capacity(length);
        for _ in 0..length {
            loop {
                let raw: u32 = rand::random();
                if raw < reject_threshold {
                    code.push(alphabet[(raw % n) as usize] as char);
                    break;
                }
            }
        }
        code
    }
}

/// Per-client failed attempt state with optional absolute lockout deadline.
#[derive(Debug, Clone, Copy)]
struct FailedAttemptState {
    count: u32,
    lockout_until: Option<Instant>,
    last_attempt: Instant,
}

/// Why a `generate_pairing_code_if_vacant` call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratePairingCodeError {
    /// A pairing code is already pending; redeem or wait before issuing a new one.
    Pending,
    /// Pairing is disabled on this gateway.
    PairingDisabled,
}

/// How long a freshly minted pairing code stays redeemable.
///
/// The code is the SOLE bearer credential for the certificate-issuance
/// endpoint, and it is displayed on a console and written to logs, so its
/// value has to stop being useful shortly after the operator has used it.
/// One-time use alone left a copied code redeemable indefinitely and gave an
/// online guesser unlimited wall-clock against a six-digit space.
pub const PAIRING_CODE_TTL: Duration = Duration::from_secs(10 * 60);

/// Who a paired bearer token authenticates as.
///
/// Fixed when the token's pairing code is minted and never changed while the
/// token lives. The operator minting the code chooses it; the client redeeming
/// the code cannot choose or drop it. Tokens paired before roster binding
/// existed, and every code minted without a user, are
/// [`PairedTokenSubject::SharedOperator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairedTokenSubject {
    /// The trusted shared operator: full authority, no distinct identity.
    SharedOperator,
    /// A `[users.<name>]` roster identity. Keyed by the entry's durable
    /// principal id (its `principal_id`, else the entry name), so renaming an
    /// entry that pins its id keeps the binding.
    RosterUser {
        /// The durable roster principal id.
        principal_id: String,
    },
}

impl PairedTokenSubject {
    /// The subject for a token bound to the `[users.<entry_name>]` roster
    /// entry, or `None` when the roster has no entry by that name.
    #[must_use]
    pub fn for_roster_entry(
        users: &HashMap<String, crate::schema::UserConfig>,
        entry_name: &str,
    ) -> Option<Self> {
        users.get(entry_name).map(|user| Self::RosterUser {
            principal_id: user.effective_principal_id(entry_name).to_owned(),
        })
    }

    /// The durable roster principal id this subject is bound to, if any.
    #[must_use]
    pub fn roster_principal_id(&self) -> Option<&str> {
        match self {
            Self::SharedOperator => None,
            Self::RosterUser { principal_id } => Some(principal_id),
        }
    }
}

/// The paired-token set in the shape `[gateway]` persists it.
///
/// Shared-operator tokens go to `paired_tokens` and roster-bound tokens to
/// `paired_token_users`, never both. A binding lives in the same record as
/// its token, so one cannot be written without the other, and a binary that
/// does not know the binding field skips bound tokens instead of loading them
/// as the shared operator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersistedPairedTokens {
    /// Hashes of the shared-operator tokens, sorted.
    pub paired_tokens: Vec<String>,
    /// The hash of each roster-bound token, mapped to its durable roster
    /// principal id.
    pub paired_token_users: HashMap<String, String>,
}

/// The active pairing code, when it was minted, and who a token paired with
/// it will authenticate as. `Instant` is monotonic, so the lifetime cannot be
/// extended by moving the system clock.
#[derive(Debug, Clone)]
struct PendingCode {
    code: String,
    minted_at: Instant,
    subject: PairedTokenSubject,
}

impl PendingCode {
    fn new(code: String, subject: PairedTokenSubject) -> Self {
        Self {
            code,
            minted_at: Instant::now(),
            subject,
        }
    }

    /// Restore a previously reserved code WITHOUT refreshing its lifetime or
    /// changing its subject: a failed issuance must neither extend the window
    /// nor turn a user-bound code into a shared-operator one.
    fn restored(code: String, minted_at: Instant, subject: PairedTokenSubject) -> Self {
        Self {
            code,
            minted_at,
            subject,
        }
    }

    fn is_expired_at(&self, now: Instant) -> bool {
        now.duration_since(self.minted_at) >= PAIRING_CODE_TTL
    }
}

/// Read the live code, clearing it first if its lifetime has elapsed. Every
/// path that consults the slot goes through this so an expired code is dropped
/// rather than merely reported as absent.
fn take_live(slot: &mut Option<PendingCode>) -> Option<PendingCode> {
    let now = Instant::now();
    if slot.as_ref().is_some_and(|p| p.is_expired_at(now)) {
        *slot = None;
    }
    slot.clone()
}

/// The paired-token set held by [`PairingGuard::hold_paired_tokens`].
pub struct HeldPairedTokens<'a>(RwLockReadGuard<'a, HashMap<String, PairedTokenSubject>>);

impl HeldPairedTokens<'_> {
    /// Whether `token_hash` is paired, as of this hold.
    pub fn contains_hash(&self, token_hash: &str) -> bool {
        self.0.contains_key(token_hash)
    }

    /// Who `token_hash` authenticates as, as of this hold, or `None` when it
    /// is not paired. Revalidation under the hold rebuilds a roster-bound
    /// connection's identity from this, as it does outside one.
    pub fn subject_of_hash(&self, token_hash: &str) -> Option<PairedTokenSubject> {
        self.0.get(token_hash).cloned()
    }
}

// TODO: I've just made this work with parking_lot but it should use either flume or tokio's async mutexes
#[derive(Debug, Clone)]
pub struct PairingGuard {
    /// Whether pairing is required at all.
    require_pairing: bool,
    /// One-time pairing code (generated on startup, consumed on first pair).
    pairing_code: Arc<Mutex<Option<PendingCode>>>,
    /// SHA-256 hashed bearer tokens and who each authenticates as (persisted
    /// across restarts). One map, so membership and subject are always read
    /// together: a revocation can never land between the two. A
    /// reader-writer lock so that a caller holding it across an effect (see
    /// [`PairingGuard::hold_paired_tokens`]) delays only pairing and
    /// revocation, not other liveness checks.
    paired_tokens: Arc<RwLock<HashMap<String, PairedTokenSubject>>>,
    /// Brute-force protection: per-client failed attempt state + last sweep timestamp.
    failed_attempts: Arc<Mutex<(HashMap<String, FailedAttemptState>, Instant)>>,
    /// The admin token this gateway run accepts on the pairing-code admin
    /// routes. This is the authority; the token file is only how local
    /// clients learn it. `None` until a rotation fully succeeds, so a failed
    /// rotation refuses every caller rather than honouring an older file.
    admin_token: Arc<Mutex<Option<String>>>,
}

/// A successfully matched pairing code whose final token is not yet committed.
///
/// Dropping an uncommitted reservation restores the one-time code so validation
/// or durable-write failures do not burn the operator's displayed code.
#[derive(Debug)]
pub struct PairingReservation {
    guard: PairingGuard,
    code: String,
    /// Preserved so restoring the code on a failed issuance does not reset its
    /// lifetime.
    minted_at: Instant,
    /// Who the committed token authenticates as, taken from the pending code.
    subject: PairedTokenSubject,
    token: String,
    committed: bool,
}

impl PairingReservation {
    /// SHA-256 hash of the token that will be committed on success.
    pub fn token_hash(&self) -> String {
        hash_token(&self.token)
    }

    /// Who the token will authenticate as once committed.
    pub fn subject(&self) -> &PairedTokenSubject {
        &self.subject
    }

    /// Commit the reservation, consuming the one-time code and storing the token.
    pub fn commit(mut self) -> String {
        let token = self.token.clone();
        self.guard
            .paired_tokens
            .write()
            .insert(hash_token(&token), self.subject.clone());
        self.committed = true;
        token
    }
}

impl Drop for PairingReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut slot = self.guard.pairing_code.lock();
        if slot.is_none() {
            *slot = Some(PendingCode::restored(
                self.code.clone(),
                self.minted_at,
                self.subject.clone(),
            ));
        }
    }
}

impl PairingGuard {
    /// Build a guard, minting the startup code under `code_policy`.
    ///
    /// The policy is **not** retained. `PairingGuard` outlives any number of
    /// config writes, and root `AGENTS.md` forbids snapshotting live policy
    /// into a long-lived handle — a code minted after an operator strengthens
    /// `[gateway.pairing_code]` must use the new policy without a restart.
    /// Every later mint therefore takes the policy the caller resolved from
    /// live config at that moment.
    ///
    /// Every token in `existing_tokens` loads as the shared operator. A
    /// gateway's persisted state goes through [`Self::from_gateway_config`],
    /// which also loads roster-bound tokens.
    pub fn new(
        require_pairing: bool,
        existing_tokens: &[String],
        code_policy: PairingCodePolicy,
    ) -> Self {
        let tokens = existing_tokens
            .iter()
            .map(|t| (stored_token_hash(t), PairedTokenSubject::SharedOperator))
            .collect();
        Self::with_tokens(require_pairing, tokens, code_policy)
    }

    /// Build the guard from a gateway's persisted `[gateway]` state:
    /// shared-operator tokens from `paired_tokens`, roster-bound tokens from
    /// `paired_token_users`. Production construction goes through here so no
    /// surface loads one field without the other.
    ///
    /// A binding entry whose key is not a token hash, or whose value is not a
    /// valid roster principal id, is skipped with a warning, and so are two
    /// entries naming one hash with different principals. The token such an
    /// entry names does not authenticate at all, even when `paired_tokens`
    /// lists it too: a damaged binding must never leave the shared operator
    /// behind. A hash listed in both fields with a valid binding loads as
    /// bound. A binding to a principal the roster no longer has still loads:
    /// the resolver refuses it.
    pub fn from_gateway_config(gateway: &crate::schema::GatewayConfig) -> Self {
        let mut tokens: HashMap<String, PairedTokenSubject> = gateway
            .paired_tokens
            .iter()
            .map(|t| (stored_token_hash(t), PairedTokenSubject::SharedOperator))
            .collect();
        let mut bound: HashMap<String, String> = HashMap::new();
        let mut rejected: HashSet<String> = HashSet::new();
        let mut skipped = 0usize;
        for (key, principal_id) in &gateway.paired_token_users {
            let hash = key.to_ascii_lowercase();
            if !is_token_hash(key) || !crate::schema::is_valid_auth_section_name(principal_id) {
                skipped += 1;
                // Every form under which `paired_tokens` could hold the same
                // token: as written, as a normalized hash (also once stray
                // whitespace is stripped), or hashed plaintext.
                let trimmed = key.trim();
                rejected.extend([
                    key.clone(),
                    hash,
                    trimmed.to_ascii_lowercase(),
                    hash_token(key),
                    hash_token(trimmed),
                ]);
                continue;
            }
            if bound
                .get(&hash)
                .is_some_and(|existing| existing != principal_id)
            {
                // Two spellings of one hash naming different principals:
                // neither can be trusted.
                skipped += 1;
                rejected.insert(hash);
            } else {
                bound.insert(hash, principal_id.clone());
            }
        }
        for hash in &rejected {
            bound.remove(hash);
            tokens.remove(hash);
        }
        for (hash, principal_id) in bound {
            tokens.insert(hash, PairedTokenSubject::RosterUser { principal_id });
        }
        if skipped > 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "skipped": skipped })),
                "gateway.paired_token_users has entries that are not a token hash mapped to one \
                 valid roster principal id; those tokens will not authenticate, even if \
                 gateway.paired_tokens lists them. Pair the affected devices again."
            );
        }
        Self::with_tokens(gateway.require_pairing, tokens, gateway.pairing_code)
    }

    fn with_tokens(
        require_pairing: bool,
        tokens: HashMap<String, PairedTokenSubject>,
        code_policy: PairingCodePolicy,
    ) -> Self {
        let code = if require_pairing && tokens.is_empty() {
            Some(PendingCode::new(
                code_policy.generate(),
                PairedTokenSubject::SharedOperator,
            ))
        } else {
            None
        };
        Self {
            require_pairing,
            pairing_code: Arc::new(Mutex::new(code)),
            paired_tokens: Arc::new(RwLock::new(tokens)),
            failed_attempts: Arc::new(Mutex::new((HashMap::new(), Instant::now()))),
            admin_token: Arc::new(Mutex::new(None)),
        }
    }

    /// Start a new admin-token generation: forget the current token, mint a
    /// fresh one, write it owner-only into `data_dir`, and accept it only
    /// once the file is in place. On any failure the guard accepts no admin
    /// token at all, and whatever file an earlier run left behind never
    /// matches, so the pairing-code admin routes fail closed.
    pub fn rotate_admin_token(&self, data_dir: &std::path::Path) -> std::io::Result<String> {
        let mut current = self.admin_token.lock();
        *current = None;
        let token = write_gateway_admin_token(data_dir)?;
        *current = Some(token.clone());
        Ok(token)
    }

    /// True only when `presented` equals this run's admin token, compared in
    /// constant time. With no successful rotation, nothing matches.
    pub fn admin_token_matches(&self, presented: &str) -> bool {
        let presented = presented.trim();
        !presented.is_empty()
            && self
                .admin_token
                .lock()
                .as_deref()
                .is_some_and(|token| constant_time_eq(presented, token))
    }

    /// The one-time pairing code (generated only on first startup when no tokens exist).
    pub fn pairing_code(&self) -> Option<String> {
        take_live(&mut self.pairing_code.lock()).map(|p| p.code)
    }

    /// The live pairing code together with who a token paired with it will
    /// authenticate as, read in one step.
    pub fn pending_pairing_code(&self) -> Option<(String, PairedTokenSubject)> {
        take_live(&mut self.pairing_code.lock()).map(|p| (p.code, p.subject))
    }

    /// Test-only: rewind the active code's mint time so expiry is exercisable
    /// deterministically, with no wall-clock waits.
    #[cfg(test)]
    fn age_pairing_code(&self, by: Duration) {
        let mut slot = self.pairing_code.lock();
        if let Some(pending) = slot.as_mut() {
            pending.minted_at = pending
                .minted_at
                .checked_sub(by)
                .expect("test rewind must stay within Instant range");
        }
    }

    /// Whether pairing is required at all.
    pub fn require_pairing(&self) -> bool {
        self.require_pairing
    }

    fn reserve_pair_blocking(
        &self,
        code: &str,
        client_id: &str,
    ) -> Result<Option<PairingReservation>, u64> {
        let client_id = normalize_client_key(client_id);
        let now = Instant::now();

        // Periodic sweep + lockout check
        {
            let mut guard = self.failed_attempts.lock();
            let (ref mut map, ref mut last_sweep) = *guard;

            // Sweep stale entries on interval
            if now.duration_since(*last_sweep).as_secs() >= FAILED_ATTEMPT_SWEEP_INTERVAL_SECS {
                prune_failed_attempts(map, now);
                *last_sweep = now;
            }

            // Check brute force lockout for this specific client
            if let Some(state) = map.get(&client_id)
                && let Some(until) = state.lockout_until
            {
                if now < until {
                    let remaining = (until - now).as_secs();
                    return Err(remaining.max(1));
                }
                // Lockout expired — reset inline
                map.remove(&client_id);
            }
        }

        if let Some(reservation) = self.try_reserve_code(code) {
            // Reset failed attempts for this client on success
            let mut guard = self.failed_attempts.lock();
            guard.0.remove(&client_id);
            return Ok(Some(reservation));
        }

        // Increment failed attempts for this client
        {
            let mut guard = self.failed_attempts.lock();
            let (ref mut map, _) = *guard;

            // Enforce capacity bound: prune stale first, then LRU-evict if still full
            if map.len() >= MAX_TRACKED_CLIENTS {
                prune_failed_attempts(map, now);
            }
            if map.len() >= MAX_TRACKED_CLIENTS {
                // Evict the least-recently-active entry
                if let Some(lru_key) = map
                    .iter()
                    .min_by_key(|(_, s)| s.last_attempt)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&lru_key);
                }
            }

            let entry = map.entry(client_id).or_insert(FailedAttemptState {
                count: 0,
                lockout_until: None,
                last_attempt: now,
            });

            entry.last_attempt = now;
            entry.count += 1;

            if entry.count >= MAX_PAIR_ATTEMPTS {
                entry.lockout_until = Some(now + std::time::Duration::from_secs(PAIR_LOCKOUT_SECS));
            }
        }

        Ok(None)
    }

    fn try_pair_blocking(&self, code: &str, client_id: &str) -> Result<Option<String>, u64> {
        Ok(self
            .reserve_pair_blocking(code, client_id)?
            .map(PairingReservation::commit))
    }

    /// Reserve the given code without committing it. The returned reservation
    /// restores the code on drop unless `commit()` is called.
    /// Constant-time code check + reservation, with NO lockout bookkeeping.
    /// The shared core of the keyed and unkeyed paths.
    fn try_reserve_code(&self, code: &str) -> Option<PairingReservation> {
        let mut pairing_code = self.pairing_code.lock();
        // An expired code is cleared here, not merely ignored, so it cannot be
        // redeemed later and does not linger in memory.
        let pending = take_live(&mut pairing_code)?;
        if constant_time_eq(code.trim(), pending.code.trim()) {
            let reservation = PairingReservation {
                guard: self.clone(),
                code: pending.code.clone(),
                minted_at: pending.minted_at,
                subject: pending.subject.clone(),
                token: generate_token(),
                committed: false,
            };
            // Reserve the pairing code so concurrent requests cannot both
            // issue, but restore it on drop if issuance fails before commit.
            *pairing_code = None;
            return Some(reservation);
        }
        None
    }

    /// Code check WITHOUT per-client lockout accounting, for callers whose
    /// peers all share one network identity and which apply their own
    /// class-wide rate policy instead. The relay enrollment bridge is the
    /// canonical case: every relay-routed client reaches the daemon from
    /// loopback, so keying lockout on the peer address would let five wrong
    /// codes from ONE hostile client lock out EVERY relay-routed enrollee
    /// (shared-fate lockout). Never expose this to a caller that has a real
    /// per-client identity - use [`PairingGuard::reserve_pair`] there.
    pub async fn reserve_pair_unkeyed(&self, code: &str) -> Option<PairingReservation> {
        let this = self.clone();
        let code = code.to_string();
        let handle = tokio::task::spawn_blocking(move || this.try_reserve_code(&code));
        handle
            .await
            .expect("failed to spawn blocking task this should not happen")
    }

    pub async fn reserve_pair(
        &self,
        code: &str,
        client_id: &str,
    ) -> Result<Option<PairingReservation>, u64> {
        let this = self.clone();
        let code = code.to_string();
        let client_id = client_id.to_string();
        let handle =
            tokio::task::spawn_blocking(move || this.reserve_pair_blocking(&code, &client_id));

        handle
            .await
            .expect("failed to spawn blocking task this should not happen")
    }

    /// Attempt to pair with the given code. Returns a bearer token on success.
    /// Returns `Err(lockout_seconds)` if locked out due to brute force.
    /// `client_id` identifies the client for per-client lockout accounting.
    pub async fn try_pair(&self, code: &str, client_id: &str) -> Result<Option<String>, u64> {
        let this = self.clone();
        let code = code.to_string();
        let client_id = client_id.to_string();
        // TODO: make this function the main one without spawning a task
        let handle = tokio::task::spawn_blocking(move || this.try_pair_blocking(&code, &client_id));

        handle
            .await
            .expect("failed to spawn blocking task this should not happen")
    }

    /// Whether `token` may use the gateway's pairing-checked routes: always
    /// when pairing is not required, otherwise only a paired shared-operator
    /// token. Those routes act with the full operator authority, so a
    /// roster-bound token is refused here. It carries only its roster user's
    /// grants, which only principal-aware surfaces (the RPC handshake and the
    /// gateway's config routes) resolve and enforce.
    pub fn is_authenticated(&self, token: &str) -> bool {
        if !self.require_pairing {
            return true;
        }
        let hashed = hash_token(token);
        matches!(
            self.paired_tokens.read().get(&hashed),
            Some(PairedTokenSubject::SharedOperator)
        )
    }

    /// Strict membership check for inbound authentication: whether `token`
    /// hashes to a currently-paired token, shared-operator or roster-bound,
    /// regardless of `require_pairing`.
    /// The gateway's `is_authenticated` convenience fails OPEN when pairing
    /// is disabled, which is only correct on surfaces that already treat
    /// the transport as trusted — an auth provider must use a strict check
    /// instead, so an empty token set denies everything. One that needs to
    /// know who the token authenticates as uses [`Self::subject_for_token`].
    pub fn token_is_paired(&self, token: &str) -> bool {
        let hashed = hash_token(token);
        self.paired_tokens.read().contains_key(&hashed)
    }

    /// Strict membership check by pre-computed SHA-256 hash (see
    /// [`Self::token_is_paired`]). Lets an established connection re-check
    /// liveness of its pairing without retaining the bearer itself.
    pub fn token_hash_is_paired(&self, token_hash: &str) -> bool {
        self.paired_tokens.read().contains_key(token_hash)
    }

    /// Who `token` authenticates as, when it is currently paired: the strict
    /// check of [`Self::token_is_paired`] together with the token's subject.
    /// `None` for an unpaired token, whatever `require_pairing` says.
    pub fn subject_for_token(&self, token: &str) -> Option<PairedTokenSubject> {
        self.subject_for_hash(&hash_token(token))
    }

    /// [`Self::subject_for_token`] by pre-computed SHA-256 hash, for an
    /// established connection that kept only its token's hash.
    pub fn subject_for_hash(&self, token_hash: &str) -> Option<PairedTokenSubject> {
        self.paired_tokens.read().get(token_hash).cloned()
    }

    /// Hold the paired-token set still: no token can be paired or revoked
    /// until the returned guard is dropped, while other readers proceed.
    ///
    /// For a caller that must keep a liveness decision true through the
    /// effect it guards. Keep the hold short, and do not call this guard's
    /// other methods while holding it: a pairing or revocation queued behind
    /// the hold makes a second read on the same thread wait forever.
    pub fn hold_paired_tokens(&self) -> HeldPairedTokens<'_> {
        HeldPairedTokens(self.paired_tokens.read())
    }

    /// Whether a pairing or revocation has claimed the paired-token set and
    /// is waiting behind a reader that still holds it (see
    /// [`Self::hold_paired_tokens`]). False once no reader holds it, whether
    /// the writer is then running, finished, or never came. A diagnostic for
    /// tests that must observe a writer queued behind a hold; it keeps no
    /// lock.
    #[doc(hidden)]
    pub fn token_write_queued_behind_a_hold(&self) -> bool {
        self.paired_tokens.is_locked_exclusive()
            && self.paired_tokens.try_read_recursive().is_some()
    }

    /// Returns true if the gateway is already paired (has at least one token).
    pub fn is_paired(&self) -> bool {
        let tokens = self.paired_tokens.read();
        !tokens.is_empty()
    }

    /// Every paired token hash, shared-operator and roster-bound alike.
    /// Persistence uses [`Self::persisted_tokens`], which keeps the two apart.
    pub fn tokens(&self) -> Vec<String> {
        let tokens = self.paired_tokens.read();
        tokens.keys().cloned().collect()
    }

    /// The paired-token set split the way `[gateway]` persists it, read in one
    /// step so the two fields always describe the same set.
    pub fn persisted_tokens(&self) -> PersistedPairedTokens {
        let tokens = self.paired_tokens.read();
        let mut persisted = PersistedPairedTokens::default();
        for (hash, subject) in tokens.iter() {
            match subject {
                PairedTokenSubject::SharedOperator => persisted.paired_tokens.push(hash.clone()),
                PairedTokenSubject::RosterUser { principal_id } => {
                    persisted
                        .paired_token_users
                        .insert(hash.clone(), principal_id.clone());
                }
            }
        }
        persisted.paired_tokens.sort_unstable();
        persisted
    }

    pub fn revoke_token(&self, token: &str) -> bool {
        self.revoke_token_hash_subject(&hash_token(token)).is_some()
    }

    /// Revoke a paired token by its SHA-256 hash. Returns true if removed.
    pub fn revoke_token_hash(&self, token_hash: &str) -> bool {
        self.revoke_token_hash_subject(token_hash).is_some()
    }

    /// Revoke a paired token by its SHA-256 hash and return who it
    /// authenticated as, or `None` when it was not paired. Revoking a token
    /// drops its roster binding with it. A device rotation reads the subject
    /// here, in the same step as the revocation, so the replacement code can
    /// carry the same binding.
    pub fn revoke_token_hash_subject(&self, token_hash: &str) -> Option<PairedTokenSubject> {
        self.paired_tokens.write().remove(token_hash)
    }

    pub fn revoke_all_tokens(&self) -> usize {
        let mut tokens = self.paired_tokens.write();
        let count = tokens.len();
        tokens.clear();
        count
    }

    /// Generate a new pairing code that pairs an additional client.
    /// Does not revoke existing tokens. To rotate a compromised token,
    /// pair with `revoke_token`/`revoke_token_hash` + a config persist pass.
    ///
    /// `code_policy` is passed per call, resolved by the caller from live
    /// config at this moment — the guard holds no policy of its own, so an
    /// operator who strengthens `[gateway.pairing_code]` sees the next code
    /// follow the new policy without a restart.
    pub fn generate_new_pairing_code(&self, code_policy: PairingCodePolicy) -> Option<String> {
        self.generate_new_pairing_code_as(code_policy, PairedTokenSubject::SharedOperator)
    }

    /// [`Self::generate_new_pairing_code`] for a code whose token will
    /// authenticate as `subject`. The subject travels with the pending code
    /// into its reservation, so the client redeeming the code cannot choose
    /// or drop it. Replaces any pending code, whatever its subject.
    pub fn generate_new_pairing_code_as(
        &self,
        code_policy: PairingCodePolicy,
        subject: PairedTokenSubject,
    ) -> Option<String> {
        if !self.require_pairing {
            return None;
        }
        let new_code = code_policy.generate();
        *self.pairing_code.lock() = Some(PendingCode::new(new_code.clone(), subject));
        Some(new_code)
    }

    /// Issue a code only if no code is currently pending.
    ///
    /// `code_policy` is resolved per call for the same reason as
    /// [`Self::generate_new_pairing_code`].
    pub fn generate_pairing_code_if_vacant(
        &self,
        code_policy: PairingCodePolicy,
    ) -> Result<String, GeneratePairingCodeError> {
        self.generate_pairing_code_if_vacant_as(code_policy, PairedTokenSubject::SharedOperator)
    }

    /// [`Self::generate_pairing_code_if_vacant`] for a code whose token will
    /// authenticate as `subject`.
    pub fn generate_pairing_code_if_vacant_as(
        &self,
        code_policy: PairingCodePolicy,
        subject: PairedTokenSubject,
    ) -> Result<String, GeneratePairingCodeError> {
        if !self.require_pairing {
            return Err(GeneratePairingCodeError::PairingDisabled);
        }
        let mut slot = self.pairing_code.lock();
        // An expired code leaves the slot vacant: otherwise a lapsed code would
        // block minting a replacement until the process restarted.
        if take_live(&mut slot).is_some() {
            return Err(GeneratePairingCodeError::Pending);
        }
        let new_code = code_policy.generate();
        *slot = Some(PendingCode::new(new_code.clone(), subject));
        Ok(new_code)
    }

    /// Get the token hash for a given plaintext token (for device registry lookup).
    pub fn token_hash(token: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    }

    /// Check a token the way [`Self::is_authenticated`] does and return its
    /// hash: a roster-bound token is refused.
    pub fn authenticate_and_hash(&self, token: &str) -> Option<String> {
        if self.is_authenticated(token) {
            Some(Self::token_hash(token))
        } else {
            None
        }
    }
}

/// Normalize a client identifier: trim whitespace, map empty to `"unknown"`.
fn normalize_client_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Remove failed-attempt entries whose `last_attempt` is older than the retention window.
fn prune_failed_attempts(map: &mut HashMap<String, FailedAttemptState>, now: Instant) {
    map.retain(|_, state| {
        now.duration_since(state.last_attempt).as_secs() < FAILED_ATTEMPT_RETENTION_SECS
    });
}

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("zc_{}", hex::encode(bytes))
}

/// SHA-256 hash a bearer token for storage. Returns lowercase hex.
fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// The stored form of a `paired_tokens` entry: already a hash, or a legacy
/// plaintext token hashed on load.
fn stored_token_hash(value: &str) -> String {
    if is_token_hash(value) {
        value.to_string()
    } else {
        hash_token(value)
    }
}

/// Check if a stored value looks like a SHA-256 hash (64 hex chars)
/// rather than a plaintext token.
fn is_token_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

#[allow(clippy::needless_bitwise_bool)]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();

    // Track length mismatch as a usize (non-zero = different lengths)
    let len_diff = a.len() ^ b.len();

    // XOR each byte, padding the shorter input with zeros.
    // Iterates over max(a.len(), b.len()) to avoid timing differences.
    let max_len = a.len().max(b.len());
    let mut byte_diff = 0u8;
    for i in 0..max_len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        byte_diff |= x ^ y;
    }
    // Intentional use of bitwise & (not &&) to ensure constant-time execution
    // and prevent timing side-channel attacks. Both comparisons must execute.
    (len_diff == 0) & (byte_diff == 0)
}

/// Request header that carries the gateway admin secret on the pairing-code
/// admin routes (`/admin/paircode`, `/admin/paircode/new`).
pub const GATEWAY_ADMIN_TOKEN_HEADER: &str = "x-zeroclaw-admin-token";

const GATEWAY_ADMIN_TOKEN_FILE: &str = "gateway-admin.token";

/// Where a gateway whose data directory is `data_dir` keeps its admin secret.
///
/// The secret is what proves a caller is local. A loopback TCP peer is not
/// proof: a reverse proxy or tunnel on the same host relays remote callers
/// from loopback too, with or without forwarding headers. Reading this
/// owner-only file requires running as the gateway's user on its host.
pub fn gateway_admin_token_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join(GATEWAY_ADMIN_TOKEN_FILE)
}

/// Mint a fresh admin secret and write it owner-only (mode `0o600` on Unix),
/// replacing any previous file. Only [`PairingGuard::rotate_admin_token`]
/// calls this: the guard, not the file, decides what is accepted.
fn write_gateway_admin_token(data_dir: &std::path::Path) -> std::io::Result<String> {
    use std::io::Write;

    let token = generate_token();
    std::fs::create_dir_all(data_dir)?;
    let path = gateway_admin_token_path(data_dir);
    let staging = path.with_extension("token.tmp");
    // A leftover staging file from a crash would make `create_new` fail.
    match std::fs::remove_file(&staging) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&staging)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&staging, &path)?;
    Ok(token)
}

/// Read the current admin secret, or `None` when no gateway has written one
/// or the caller cannot read it.
pub fn read_gateway_admin_token(data_dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(gateway_admin_token_path(data_dir)).ok()?;
    let token = raw.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Check if a host string represents a non-localhost bind address.
pub fn is_public_bind(host: &str) -> bool {
    !matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0:0:0:0:0:0:0:1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::test;

    /// Serializes the tests that either emit or assert-the-absence-of the
    /// clamp WARN. The log broadcast is process-global, so two overlapping
    /// capture windows would let one test see the other's warning.
    static CLAMP_LOG_LOCK: Mutex<()> = Mutex::new(());

    fn capture_log_events() -> tokio::sync::broadcast::Receiver<serde_json::Value> {
        ::zeroclaw_log::try_install_capture_subscriber();
        ::zeroclaw_log::subscribe_or_install()
    }

    fn drain_captured_events(
        rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
    ) -> Vec<serde_json::Value> {
        let mut events = Vec::new();
        while let Ok(value) = rx.try_recv() {
            events.push(value);
        }
        events
    }

    fn drain_captured(rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>) -> String {
        drain_captured_events(rx)
            .iter()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A guard under the shipped default policy. Behaviour tests that are
    /// not about code shape use this so the default stays exercised.
    fn new_guard(require_pairing: bool, existing_tokens: &[String]) -> PairingGuard {
        PairingGuard::new(
            require_pairing,
            existing_tokens,
            PairingCodePolicy::default(),
        )
    }

    // ── PairingGuard ─────────────────────────────────────────

    #[test]
    async fn new_guard_generates_code_when_no_tokens() {
        let guard = new_guard(true, &[]);
        assert!(guard.pairing_code().is_some());
        assert!(!guard.is_paired());
    }

    #[test]
    async fn new_guard_no_code_when_tokens_exist() {
        let guard = new_guard(true, &["zc_existing".into()]);
        assert!(guard.pairing_code().is_none());
        assert!(guard.is_paired());
    }

    #[test]
    async fn new_guard_no_code_when_pairing_disabled() {
        let guard = new_guard(false, &[]);
        assert!(guard.pairing_code().is_none());
    }

    #[test]
    async fn try_pair_correct_code() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "test_client").await.unwrap();
        assert!(token.is_some());
        assert!(token.unwrap().starts_with("zc_"));
        assert!(guard.is_paired());
    }

    #[test]
    async fn reservation_restores_code_until_committed() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        {
            let reservation = guard
                .reserve_pair(&code, "test_client")
                .await
                .unwrap()
                .expect("code should reserve");
            assert_eq!(reservation.token_hash().len(), 64);
            assert!(
                guard.pairing_code().is_none(),
                "reserved code must not be concurrently reusable"
            );
        }
        assert_eq!(
            guard.pairing_code().as_deref(),
            Some(code.as_str()),
            "dropping an uncommitted reservation restores the one-time code"
        );
        assert!(!guard.is_paired());
    }

    #[test]
    async fn committed_reservation_consumes_code_and_stores_token() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard
            .reserve_pair(&code, "test_client")
            .await
            .unwrap()
            .expect("code should reserve")
            .commit();
        assert!(token.starts_with("zc_"));
        assert!(guard.pairing_code().is_none());
        assert!(guard.is_authenticated(&token));
    }

    #[test]
    async fn try_pair_wrong_code() {
        let guard = new_guard(true, &[]);
        let result = guard.try_pair("000000", "test_client").await.unwrap();
        // Might succeed if code happens to be 000000, but extremely unlikely
        // Just check it returns Ok(None) normally
        let _ = result;
    }

    #[test]
    async fn try_pair_empty_code() {
        let guard = new_guard(true, &[]);
        assert!(guard.try_pair("", "test_client").await.unwrap().is_none());
    }

    #[test]
    async fn is_authenticated_with_valid_token() {
        // Pass plaintext token — PairingGuard hashes it on load
        let guard = new_guard(true, &["zc_valid".into()]);
        assert!(guard.is_authenticated("zc_valid"));
    }

    #[test]
    async fn is_authenticated_with_prehashed_token() {
        // Pass an already-hashed token (64 hex chars)
        let hashed = hash_token("zc_valid");
        let guard = new_guard(true, &[hashed]);
        assert!(guard.is_authenticated("zc_valid"));
    }

    #[test]
    async fn is_authenticated_with_invalid_token() {
        let guard = new_guard(true, &["zc_valid".into()]);
        assert!(!guard.is_authenticated("zc_invalid"));
    }

    #[test]
    async fn is_authenticated_when_pairing_disabled() {
        let guard = new_guard(false, &[]);
        assert!(guard.is_authenticated("anything"));
        assert!(guard.is_authenticated(""));
    }

    #[test]
    async fn strict_token_check_ignores_require_pairing() {
        let guard = PairingGuard::new(false, &["zc_tok".to_string()], PairingCodePolicy::default());
        assert!(
            guard.is_authenticated("anything"),
            "gateway convenience fails open when pairing is disabled"
        );
        assert!(guard.token_is_paired("zc_tok"));
        assert!(!guard.token_is_paired("anything"));
        let hash = PairingGuard::token_hash("zc_tok");
        assert!(guard.token_hash_is_paired(&hash));

        assert!(guard.revoke_token("zc_tok"));
        assert!(!guard.token_is_paired("zc_tok"), "revocation is live");
        assert!(!guard.token_hash_is_paired(&hash));
    }

    #[test]
    async fn empty_guard_strict_check_denies_everything() {
        let guard = PairingGuard::new(false, &[], PairingCodePolicy::default());
        assert!(!guard.token_is_paired("anything"));
        assert!(!guard.token_hash_is_paired(&PairingGuard::token_hash("anything")));
    }

    #[test]
    async fn tokens_returns_hashes() {
        let guard = new_guard(true, &["zc_a".into(), "zc_b".into()]);
        let tokens = guard.tokens();
        assert_eq!(tokens.len(), 2);
        // Tokens should be stored as 64-char hex hashes, not plaintext
        for t in &tokens {
            assert_eq!(t.len(), 64, "Token should be a SHA-256 hash");
            assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(!t.starts_with("zc_"), "Token should not be plaintext");
        }
    }

    #[test]
    async fn pair_then_authenticate() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
        assert!(guard.is_authenticated(&token));
        assert!(!guard.is_authenticated("wrong"));
    }

    // ── Token hashing ────────────────────────────────────────

    #[test]
    async fn hash_token_produces_64_hex_chars() {
        let hash = hash_token("zc_test_token");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    async fn hash_token_is_deterministic() {
        assert_eq!(hash_token("zc_abc"), hash_token("zc_abc"));
    }

    #[test]
    async fn hash_token_differs_for_different_inputs() {
        assert_ne!(hash_token("zc_a"), hash_token("zc_b"));
    }

    #[test]
    async fn is_token_hash_detects_hash_vs_plaintext() {
        assert!(is_token_hash(&hash_token("zc_test")));
        assert!(!is_token_hash("zc_test_token"));
        assert!(!is_token_hash("too_short"));
        assert!(!is_token_hash(""));
    }

    // ── is_public_bind ───────────────────────────────────────

    #[test]
    async fn localhost_variants_not_public() {
        assert!(!is_public_bind("127.0.0.1"));
        assert!(!is_public_bind("localhost"));
        assert!(!is_public_bind("::1"));
        assert!(!is_public_bind("[::1]"));
    }

    #[test]
    async fn zero_zero_is_public() {
        assert!(is_public_bind("0.0.0.0"));
    }

    #[test]
    async fn real_ip_is_public() {
        assert!(is_public_bind("192.168.1.100"));
        assert!(is_public_bind("10.0.0.1"));
    }

    // ── constant_time_eq ─────────────────────────────────────

    #[test]
    async fn constant_time_eq_same() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    async fn constant_time_eq_different() {
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("a", ""));
    }

    // ── Pairing-code policy: default shape ───────────────────

    /// Acceptance criterion: the default is chosen deliberately and is
    /// materially stronger than the six-digit numeric code master shipped.
    #[test]
    async fn default_policy_is_32_char_case_sensitive_alphanumeric() {
        let policy = PairingCodePolicy::default();
        assert_eq!(policy.length, 32, "default length");
        assert_eq!(
            policy.charset,
            PairingCodeCharset::Alphanumeric,
            "default charset"
        );
        assert_eq!(
            policy.charset.alphabet().len(),
            62,
            "case-sensitive 0-9A-Za-z"
        );
        assert!(
            policy.entropy_bits() > 190.0,
            "default must clear 190 bits, got {}",
            policy.entropy_bits()
        );
    }

    /// The old-vs-new discriminator: a default code is no longer something
    /// the legacy generator could ever have produced.
    #[test]
    async fn default_generated_code_is_not_a_six_digit_numeric_code() {
        let policy = PairingCodePolicy::default();
        // One sample settles the length; the charset claim needs a few
        // draws because an all-digit 32-char code is possible in principle
        // (p ≈ (10/62)^32 ≈ 10^-25) but not across 8 independent samples.
        let mut saw_non_digit = false;
        for _ in 0..8 {
            let code = policy.generate();
            assert_eq!(code.len(), 32, "default code length");
            assert!(
                code.len() > 6,
                "default code must be longer than the old six-digit code"
            );
            if code.chars().any(|c| !c.is_ascii_digit()) {
                saw_non_digit = true;
            }
        }
        assert!(
            saw_non_digit,
            "default codes must draw from letters as well as digits"
        );
    }

    /// The default alphabet really is case-sensitive: both cases and digits
    /// all appear across a sample. Guards against silently upper-casing the
    /// alphabet and quartering the keyspace.
    #[test]
    async fn default_charset_uses_digits_and_both_letter_cases() {
        let policy = PairingCodePolicy::default();
        let sample: String = (0..16).map(|_| policy.generate()).collect();
        assert!(sample.chars().any(|c| c.is_ascii_digit()), "digits");
        assert!(sample.chars().any(|c| c.is_ascii_uppercase()), "uppercase");
        assert!(sample.chars().any(|c| c.is_ascii_lowercase()), "lowercase");
    }

    // ── Pairing-code policy: configured shapes ───────────────

    /// Numeric compatibility mode reproduces the previously shipped code exactly.
    #[test]
    async fn numeric_compat_policy_reproduces_six_digit_numeric_code() {
        let policy = PairingCodePolicy::numeric_compat();
        assert_eq!(policy.length, 6);
        assert_eq!(policy.charset, PairingCodeCharset::Numeric);
        for _ in 0..16 {
            let code = policy.generate();
            assert_eq!(code.len(), 6, "compat code length");
            assert!(
                code.chars().all(|c| c.is_ascii_digit()),
                "compat code must be all digits, got {code}"
            );
        }
    }

    #[test]
    async fn custom_length_and_charset_are_honoured() {
        let policy = PairingCodePolicy::new(20, PairingCodeCharset::Unambiguous)
            .expect("20 is inside the supported range");
        let code = policy.generate();
        assert_eq!(code.len(), 20);
        assert!(
            code.bytes().all(|b| UNAMBIGUOUS_ALPHABET.contains(&b)),
            "code {code} must stay inside the unambiguous alphabet"
        );
    }

    /// The unambiguous family exists to be read aloud, so the confusable
    /// glyphs must be absent by construction, not by luck.
    #[test]
    async fn unambiguous_charset_excludes_confusable_glyphs() {
        let alphabet = PairingCodeCharset::Unambiguous.alphabet();
        assert_eq!(alphabet.len(), 32, "Crockford Base32 is 32 symbols");
        for confusable in *b"ILOUlo" {
            assert!(
                !alphabet.contains(&confusable),
                "{} must not appear in the unambiguous alphabet",
                confusable as char
            );
        }
    }

    #[test]
    async fn entropy_bits_tracks_length_and_charset() {
        let numeric = PairingCodePolicy::numeric_compat();
        // The legacy code: log2(10^6) ≈ 19.93 bits.
        assert!(
            (numeric.entropy_bits() - 19.93).abs() < 0.01,
            "six numeric digits ≈ 19.93 bits, got {}",
            numeric.entropy_bits()
        );
        let unambiguous = PairingCodePolicy::new(24, PairingCodeCharset::Unambiguous).unwrap();
        assert!(
            (unambiguous.entropy_bits() - 120.0).abs() < 0.01,
            "24 Crockford chars = 120 bits, got {}",
            unambiguous.entropy_bits()
        );
        assert!(
            PairingCodePolicy::default().entropy_bits() > numeric.entropy_bits(),
            "the default must beat the code it replaces"
        );
    }

    // ── Pairing-code policy: invalid settings ────────────────

    #[test]
    async fn policy_rejects_length_below_minimum() {
        let err = PairingCodePolicy::new(5, PairingCodeCharset::Numeric)
            .expect_err("5 is below the floor");
        assert_eq!(err, PairingCodePolicyError::TooShort { length: 5 });
        // The floor is the old shipped length: no config may be weaker.
        assert_eq!(PAIRING_CODE_MIN_LENGTH, 6);
        assert!(
            PairingCodePolicy::new(PAIRING_CODE_MIN_LENGTH, PairingCodeCharset::Numeric).is_ok()
        );
    }

    #[test]
    async fn policy_rejects_length_above_maximum() {
        let too_long = PAIRING_CODE_MAX_LENGTH + 1;
        let err = PairingCodePolicy::new(too_long, PairingCodeCharset::Alphanumeric)
            .expect_err("above the ceiling");
        assert_eq!(err, PairingCodePolicyError::TooLong { length: too_long });
        assert!(
            PairingCodePolicy::new(PAIRING_CODE_MAX_LENGTH, PairingCodeCharset::Alphanumeric)
                .is_ok(),
            "the documented ceiling itself must be accepted"
        );
    }

    /// An out-of-range policy built by other means must still produce a
    /// usable code rather than panicking or returning an empty string.
    #[test]
    async fn generate_clamps_an_out_of_range_length() {
        let _serial = CLAMP_LOG_LOCK.lock();
        let degenerate = PairingCodePolicy {
            length: 0,
            charset: PairingCodeCharset::Numeric,
        };
        assert_eq!(degenerate.generate().len(), PAIRING_CODE_MIN_LENGTH);
        let oversized = PairingCodePolicy {
            length: usize::MAX,
            charset: PairingCodeCharset::Numeric,
        };
        assert_eq!(oversized.generate().len(), PAIRING_CODE_MAX_LENGTH);
    }

    #[test]
    async fn charset_round_trips_through_toml_and_rejects_unknown_families() {
        for (text, expected) in [
            ("numeric", PairingCodeCharset::Numeric),
            ("alphanumeric", PairingCodeCharset::Alphanumeric),
            ("unambiguous", PairingCodeCharset::Unambiguous),
        ] {
            let parsed: PairingCodePolicy =
                toml::from_str(&format!("length = 8\ncharset = \"{text}\"\n"))
                    .expect("documented charset name parses");
            assert_eq!(parsed.charset, expected, "charset {text}");
        }
        assert!(
            toml::from_str::<PairingCodePolicy>("length = 8\ncharset = \"hex\"\n").is_err(),
            "an unknown charset family must be rejected, not silently defaulted"
        );
    }

    #[test]
    async fn policy_omitting_fields_falls_back_to_the_shared_default() {
        let parsed: PairingCodePolicy = toml::from_str("").expect("empty section parses");
        assert_eq!(parsed, PairingCodePolicy::default());
    }

    // ── generate helpers ─────────────────────────────────────

    #[test]
    async fn generated_codes_are_not_deterministic() {
        // Two codes should differ with overwhelming probability. Tried over
        // several pairs so one unlucky draw cannot flake CI. Uses the
        // narrowest supported keyspace (6 numeric digits) — if that is
        // non-deterministic, every wider policy is too.
        let policy = PairingCodePolicy::numeric_compat();
        for _ in 0..10 {
            if policy.generate() != policy.generate() {
                return; // Pass: found a non-matching pair.
            }
        }
        panic!("Generated 10 pairs of codes and all were collisions — CSPRNG failure");
    }

    /// Rejection sampling must not skew the alphabet. Over a large sample of
    /// the smallest alphabet, every symbol should appear and no symbol
    /// should dominate.
    #[test]
    async fn generation_covers_the_whole_alphabet_without_gross_bias() {
        let policy = PairingCodePolicy::new(64, PairingCodeCharset::Numeric).unwrap();
        let sample: String = (0..100).map(|_| policy.generate()).collect();
        let total = sample.len() as f64;
        for symbol in NUMERIC_ALPHABET {
            let hits = sample.bytes().filter(|b| b == symbol).count() as f64;
            let share = hits / total;
            assert!(
                (0.06..0.14).contains(&share),
                "symbol {} took {share:.3} of a uniform-0.1 sample — biased draw",
                *symbol as char
            );
        }
    }

    // ── Out-of-range lengths are clamped loudly, never silently ──

    /// Review MAJOR-2: the clamp decision itself, as a plain value.
    #[test]
    async fn resolve_length_reports_what_it_clamped() {
        let in_range = PairingCodePolicy::default();
        assert_eq!(
            in_range.resolve_length(),
            (32, None),
            "an in-range length reports no clamp"
        );

        let too_short = PairingCodePolicy {
            length: 2,
            charset: PairingCodeCharset::Numeric,
        };
        assert_eq!(
            too_short.resolve_length(),
            (PAIRING_CODE_MIN_LENGTH, Some(2)),
            "a short length is clamped up and reports the configured value"
        );

        let too_long = PairingCodePolicy {
            length: 5_000,
            charset: PairingCodeCharset::Numeric,
        };
        assert_eq!(
            too_long.resolve_length(),
            (PAIRING_CODE_MAX_LENGTH, Some(5_000)),
            "a long length is clamped down and reports the configured value"
        );
    }

    /// Review MAJOR-2: clamping must be loud. Pins the actual emitted WARN,
    /// including both the configured and effective lengths, so an operator
    /// reading logs can see they are not getting the code they asked for.
    #[test]
    async fn generating_at_a_clamped_length_warns_with_both_values() {
        let _serial = CLAMP_LOG_LOCK.lock();
        let mut rx = capture_log_events();
        let degenerate = PairingCodePolicy {
            length: 2,
            charset: PairingCodeCharset::Numeric,
        };
        let code = degenerate.generate();
        assert_eq!(code.len(), PAIRING_CODE_MIN_LENGTH);

        // Pick out *our* event structurally: a substring match over the whole
        // capture window would also match another test's WARN, and would not
        // pin the severity to this event.
        let events = drain_captured_events(&mut rx);
        let warning = events
            .iter()
            .find(|e| {
                e.get("body")
                    .or_else(|| e.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|m| m.contains("gateway.pairing_code"))
            })
            .unwrap_or_else(|| {
                panic!("no clamp warning naming gateway.pairing_code in {events:#?}")
            });

        assert_eq!(
            warning.get("severity_text").and_then(|v| v.as_str()),
            Some("WARN"),
            "the clamp notice must be a WARN, not a quieter level: {warning:#?}"
        );
        let body = warning
            .get("body")
            .or_else(|| warning.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            body.contains("out of range"),
            "warning must say the length is out of range; got: {body}"
        );

        let rendered = serde_json::to_string(warning).unwrap_or_default();
        assert!(
            rendered.contains("\"configured_length\":2"),
            "warning must report the configured length; got: {rendered}"
        );
        assert!(
            rendered.contains("\"effective_length\":6"),
            "warning must report the effective length; got: {rendered}"
        );
    }

    /// The happy path must stay quiet — a warning on every mint would train
    /// operators to ignore it.
    #[test]
    async fn generating_at_a_valid_length_emits_no_clamp_warning() {
        let _serial = CLAMP_LOG_LOCK.lock();
        let mut rx = capture_log_events();
        let _ = PairingCodePolicy::default().generate();
        let logs = drain_captured(&mut rx);
        assert!(
            !logs.contains("configured_length"),
            "an in-range policy must not emit a clamp warning; got: {logs}"
        );
    }

    /// `config_name` feeds the migration writer, so it must agree with what
    /// serde accepts, in both directions.
    #[test]
    async fn charset_config_names_match_serde() {
        for charset in PairingCodeCharset::all() {
            let name = charset.config_name();
            let parsed: PairingCodePolicy =
                toml::from_str(&format!("length = 8\ncharset = \"{name}\"\n"))
                    .unwrap_or_else(|e| panic!("serde must accept config_name {name:?}: {e}"));
            assert_eq!(parsed.charset, charset, "round-trip for {name}");

            let serialized = toml::to_string(&PairingCodePolicy { length: 8, charset })
                .expect("policy serializes");
            assert!(
                serialized.contains(&format!("charset = \"{name}\"")),
                "serde must emit config_name {name:?}; got: {serialized}"
            );
        }
    }

    #[test]
    async fn admin_token_rotation_publishes_the_token_and_matches_only_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let guard = new_guard(true, &[]);

        assert!(
            !guard.admin_token_matches("anything"),
            "before any rotation the guard accepts no admin token"
        );
        assert_eq!(read_gateway_admin_token(&data_dir), None);

        let token = guard
            .rotate_admin_token(&data_dir)
            .expect("rotate admin token");
        assert_eq!(
            read_gateway_admin_token(&data_dir).as_deref(),
            Some(token.as_str()),
            "local clients learn the token from the file"
        );
        assert!(guard.admin_token_matches(&token));
        assert!(!guard.admin_token_matches(""));
        assert!(!guard.admin_token_matches("zc_wrong"));
    }

    #[test]
    async fn admin_token_from_an_earlier_rotation_stops_matching() {
        let tmp = tempfile::TempDir::new().unwrap();
        let guard = new_guard(true, &[]);
        let first = guard.rotate_admin_token(tmp.path()).unwrap();
        let second = guard.rotate_admin_token(tmp.path()).unwrap();

        assert_ne!(first, second);
        assert!(!guard.admin_token_matches(&first));
        assert!(guard.admin_token_matches(&second));
    }

    #[test]
    async fn failed_admin_token_rotation_fails_closed_despite_an_older_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        // An earlier run left a valid-looking token file behind.
        let previous_run = new_guard(true, &[]);
        let stale = previous_run.rotate_admin_token(&data_dir).unwrap();

        // This run cannot write: its data dir path is a regular file.
        let unwritable = tmp.path().join("not-a-dir");
        std::fs::write(&unwritable, b"x").unwrap();
        let guard = new_guard(true, &[]);
        let last_good = guard.rotate_admin_token(&data_dir).unwrap();
        assert!(guard.rotate_admin_token(&unwritable).is_err());

        assert!(
            !guard.admin_token_matches(&stale),
            "a file from an earlier run must never be honoured"
        );
        assert_eq!(
            read_gateway_admin_token(&data_dir).as_deref(),
            Some(last_good.as_str()),
            "the last good token is still on disk"
        );
        assert!(
            !guard.admin_token_matches(&last_good),
            "after a failed rotation even the last good token on disk is refused"
        );
    }

    #[cfg(unix)]
    #[test]
    async fn gateway_admin_token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        new_guard(true, &[]).rotate_admin_token(tmp.path()).unwrap();
        let mode = std::fs::metadata(gateway_admin_token_path(tmp.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the admin secret must be owner-only, got {mode:o}"
        );
    }

    #[test]
    async fn generate_token_has_prefix_and_hex_payload() {
        let token = generate_token();
        let payload = token
            .strip_prefix("zc_")
            .expect("Generated token should include zc_ prefix");

        assert_eq!(payload.len(), 64, "Token payload should be 32 bytes in hex");
        assert!(
            payload
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f')),
            "Token payload should be lowercase hex"
        );
    }

    // ── Brute force protection ───────────────────────────────

    #[test]
    async fn brute_force_lockout_after_max_attempts() {
        let guard = new_guard(true, &[]);
        let client = "attacker_client";
        // Exhaust all attempts with wrong codes
        for i in 0..MAX_PAIR_ATTEMPTS {
            let result = guard.try_pair(&format!("wrong_{i}"), client).await;
            assert!(result.is_ok(), "Attempt {i} should not be locked out yet");
        }
        // Next attempt should be locked out
        let result = guard.try_pair("another_wrong", client).await;
        assert!(
            result.is_err(),
            "Should be locked out after {MAX_PAIR_ATTEMPTS} attempts"
        );
        let lockout_secs = result.unwrap_err();
        assert!(lockout_secs > 0, "Lockout should have remaining seconds");
        assert!(
            lockout_secs <= PAIR_LOCKOUT_SECS,
            "Lockout should not exceed max"
        );
    }

    #[test]
    async fn correct_code_resets_failed_attempts() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let client = "test_client";
        // Fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong", client).await;
        }
        // Correct code should still work (under MAX_PAIR_ATTEMPTS)
        let result = guard.try_pair(&code, client).await.unwrap();
        assert!(result.is_some(), "Correct code should work before lockout");
    }

    #[test]
    async fn lockout_returns_remaining_seconds() {
        let guard = new_guard(true, &[]);
        let client = "test_client";
        for _ in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair("wrong", client).await;
        }
        let err = guard.try_pair("wrong", client).await.unwrap_err();
        // Should be close to PAIR_LOCKOUT_SECS (within a second)
        assert!(
            err >= PAIR_LOCKOUT_SECS - 1,
            "Remaining lockout should be ~{PAIR_LOCKOUT_SECS}s, got {err}s"
        );
    }

    #[test]
    async fn successful_pair_resets_only_requesting_client_state() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let client_a = "client_a";
        let client_b = "client_b";

        // Both clients fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong", client_a).await;
            let _ = guard.try_pair("wrong", client_b).await;
        }

        // client_a pairs successfully — only its state should reset
        let result = guard.try_pair(&code, client_a).await.unwrap();
        assert!(result.is_some(), "client_a should pair successfully");

        // client_b's failed count should still be intact (3 failures recorded)
        let state = guard.failed_attempts.lock();
        let b_state = state.0.get(client_b);
        assert!(b_state.is_some(), "client_b state should still exist");
        assert_eq!(
            b_state.unwrap().count,
            3,
            "client_b should still have 3 failures"
        );

        // client_a should have been removed
        assert!(
            !state.0.contains_key(client_a),
            "client_a state should be cleared"
        );
    }

    #[test]
    async fn failed_attempt_state_is_bounded_by_max_clients() {
        let guard = new_guard(true, &[]);

        // Fill the map to MAX_TRACKED_CLIENTS with stale entries
        {
            let mut state = guard.failed_attempts.lock();
            let past = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_RETENTION_SECS + 60,
                ))
                .unwrap_or_else(Instant::now);
            for i in 0..MAX_TRACKED_CLIENTS {
                state.0.insert(
                    format!("stale_client_{i}"),
                    FailedAttemptState {
                        count: 1,
                        lockout_until: None,
                        last_attempt: past,
                    },
                );
            }
        }

        // A new client triggers an attempt — should prune stale entries and fit
        let result = guard.try_pair("wrong", "new_client").await;
        assert!(result.is_ok(), "New client should not be blocked");

        let state = guard.failed_attempts.lock();
        assert!(
            state.0.len() <= MAX_TRACKED_CLIENTS,
            "Map size should stay within bound, got {}",
            state.0.len()
        );
        assert!(
            state.0.contains_key("new_client"),
            "New client should be tracked"
        );
    }

    #[test]
    async fn failed_attempt_sweep_prunes_expired_clients() {
        let guard = new_guard(true, &[]);

        // Seed a stale entry and set last_sweep to long ago so sweep triggers
        {
            let mut state = guard.failed_attempts.lock();
            let past = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_RETENTION_SECS + 60,
                ))
                .unwrap_or_else(Instant::now);
            state.0.insert(
                "stale_client".to_string(),
                FailedAttemptState {
                    count: 2,
                    lockout_until: None,
                    last_attempt: past,
                },
            );
            // Force last_sweep to be old enough to trigger sweep
            state.1 = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_SWEEP_INTERVAL_SECS + 1,
                ))
                .unwrap_or_else(Instant::now);
        }

        // Any attempt triggers sweep
        let _ = guard.try_pair("wrong", "fresh_client").await;

        let state = guard.failed_attempts.lock();
        assert!(
            !state.0.contains_key("stale_client"),
            "Stale client should have been pruned by sweep"
        );
        assert!(
            state.0.contains_key("fresh_client"),
            "Fresh client should still be tracked"
        );
    }

    #[test]
    async fn lockout_is_per_client() {
        let guard = new_guard(true, &[]);
        let attacker = "attacker_ip";
        let legitimate = "legitimate_ip";

        // Attacker exhausts attempts
        for i in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair(&format!("wrong_{i}"), attacker).await;
        }
        // Attacker is locked out
        assert!(guard.try_pair("wrong", attacker).await.is_err());

        // Legitimate client is NOT locked out
        let result = guard.try_pair("wrong", legitimate).await;
        assert!(
            result.is_ok(),
            "Legitimate client should not be locked out by attacker"
        );
    }

    // ── Token revocation ─────────────────────────────────────

    #[test]
    async fn revoked_token_no_longer_authenticates() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "c").await.unwrap().unwrap();
        assert!(guard.is_authenticated(&token));

        assert!(guard.revoke_token(&token));
        assert!(!guard.is_authenticated(&token));
        assert!(!guard.is_paired());
    }

    #[test]
    async fn revoked_token_is_dropped_from_persistence_view() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "c").await.unwrap().unwrap();
        let expected_hash = hash_token(&token);
        assert!(guard.tokens().contains(&expected_hash));

        assert!(guard.revoke_token(&token));
        assert!(!guard.tokens().contains(&expected_hash));
    }

    #[test]
    async fn revoke_token_hash_matches_revoke_token() {
        let guard = new_guard(true, &["zc_a".into(), "zc_b".into()]);
        let hash_a = hash_token("zc_a");
        assert!(guard.revoke_token_hash(&hash_a));
        assert!(!guard.is_authenticated("zc_a"));
        assert!(guard.is_authenticated("zc_b"));
    }

    #[test]
    async fn revoke_unknown_token_is_noop() {
        let guard = new_guard(true, &["zc_a".into()]);
        assert!(!guard.revoke_token("zc_never_paired"));
        assert!(guard.is_authenticated("zc_a"));
    }

    #[test]
    async fn revoke_is_scoped_to_target_token() {
        let guard = new_guard(true, &["zc_keep".into(), "zc_drop".into()]);
        assert!(guard.revoke_token("zc_drop"));
        assert!(guard.is_authenticated("zc_keep"));
        assert!(!guard.is_authenticated("zc_drop"));
    }

    #[test]
    async fn revoke_all_tokens_invalidates_every_token() {
        let guard = new_guard(true, &["zc_a".into(), "zc_b".into(), "zc_c".into()]);
        assert_eq!(guard.revoke_all_tokens(), 3);
        assert!(!guard.is_authenticated("zc_a"));
        assert!(!guard.is_authenticated("zc_b"));
        assert!(!guard.is_authenticated("zc_c"));
        assert!(!guard.is_paired());
        assert!(guard.tokens().is_empty());
    }

    #[test]
    async fn revoke_all_tokens_on_empty_set_returns_zero() {
        let guard = new_guard(true, &[]);
        assert_eq!(guard.revoke_all_tokens(), 0);
    }

    // ── Atomic pairing-code generation ───────────────────────

    #[test]
    async fn generate_pairing_code_if_vacant_succeeds_when_slot_empty() {
        let guard = new_guard(true, &["zc_existing".into()]);
        // `new()` does not issue a code once paired; slot is empty here.
        assert!(guard.pairing_code().is_none());
        let code = guard
            .generate_pairing_code_if_vacant(PairingCodePolicy::default())
            .unwrap();
        assert_eq!(guard.pairing_code().as_deref(), Some(code.as_str()));
    }

    #[test]
    async fn generate_pairing_code_if_vacant_refuses_when_slot_occupied() {
        let guard = new_guard(true, &[]);
        let pre_existing = guard.pairing_code().expect("startup code");
        let err = guard
            .generate_pairing_code_if_vacant(PairingCodePolicy::default())
            .unwrap_err();
        assert_eq!(err, GeneratePairingCodeError::Pending);
        assert_eq!(
            guard.pairing_code().as_deref(),
            Some(pre_existing.as_str()),
            "occupied slot must be preserved"
        );
    }

    #[test]
    async fn generate_pairing_code_if_vacant_refuses_when_pairing_disabled() {
        let guard = new_guard(false, &[]);
        let err = guard
            .generate_pairing_code_if_vacant(PairingCodePolicy::default())
            .unwrap_err();
        assert_eq!(err, GeneratePairingCodeError::PairingDisabled);
    }

    // ── One shared policy across every issuing path ──────────

    fn assert_matches_policy(code: &str, policy: PairingCodePolicy) {
        assert_eq!(
            code.len(),
            policy.length,
            "code {code} has the wrong length"
        );
        let alphabet = policy.charset.alphabet();
        assert!(
            code.bytes().all(|b| alphabet.contains(&b)),
            "code {code} strayed outside the configured alphabet"
        );
    }

    /// Startup pairing, on-demand regeneration, and the atomic
    /// dashboard/API issue path must all draw from the same policy. A
    /// non-default shape is used so a hardcoded generator cannot pass.
    #[test]
    async fn every_issuing_path_uses_the_configured_policy() {
        let policy = PairingCodePolicy::new(20, PairingCodeCharset::Unambiguous).unwrap();

        // 1. Startup code (`PairingGuard::new` with no existing tokens).
        let guard = PairingGuard::new(true, &[], policy);
        let startup = guard.pairing_code().expect("startup code is issued");
        assert_matches_policy(&startup, policy);

        // 2. On-demand regeneration (`gateway get-paircode --new`,
        //    `POST /api/pairing/initiate`).
        let regenerated = guard
            .generate_new_pairing_code(policy)
            .expect("regeneration is allowed when pairing is required");
        assert_matches_policy(&regenerated, policy);
        assert_ne!(startup, regenerated, "regeneration must issue a fresh code");

        // 3. Atomic issue-if-vacant (rotate-device flow).
        let paired = PairingGuard::new(true, &["zc_existing".into()], policy);
        let vacant = paired
            .generate_pairing_code_if_vacant(policy)
            .expect("slot is empty once paired");
        assert_matches_policy(&vacant, policy);
    }

    /// Review follow-up: the guard must not snapshot the policy.
    /// A guard built under the weak compatibility policy, then asked to mint
    /// under a strengthened one, issues the strengthened shape — no
    /// reconstruction, no restart. This is the unit-level half of the
    /// regression; the gateway half swaps live config.
    #[test]
    async fn a_long_lived_guard_mints_under_the_policy_passed_at_issuance() {
        let weak = PairingCodePolicy::numeric_compat();
        let strong = PairingCodePolicy::new(24, PairingCodeCharset::Unambiguous).unwrap();

        // Born weak.
        let guard = PairingGuard::new(true, &[], weak);
        assert_matches_policy(&guard.pairing_code().expect("startup code"), weak);

        // Operator strengthens the policy. Same guard instance throughout.
        let regenerated = guard
            .generate_new_pairing_code(strong)
            .expect("regeneration allowed");
        assert_matches_policy(&regenerated, strong);
        assert_eq!(
            guard.pairing_code().as_deref(),
            Some(regenerated.as_str()),
            "the strengthened code is the one now pending"
        );

        // And the vacant path agrees once the slot clears.
        let token = guard
            .try_pair(&regenerated, "client")
            .await
            .expect("not locked out")
            .expect("strengthened code pairs");
        assert!(guard.is_authenticated(&token));
        let vacant = guard
            .generate_pairing_code_if_vacant(strong)
            .expect("slot cleared by the successful pair");
        assert_matches_policy(&vacant, strong);
    }

    /// A guard configured for numeric compatibility keeps issuing exactly
    /// the legacy shape, so the old flow is preserved, not merely
    /// tolerated.
    #[test]
    async fn numeric_compat_guard_still_issues_six_digit_codes() {
        let policy = PairingCodePolicy::numeric_compat();
        let guard = PairingGuard::new(true, &[], policy);
        let code = guard.pairing_code().expect("startup code");
        assert_matches_policy(&code, policy);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    /// Acceptance criterion: a successful pair using the configured
    /// code shape. Exercised for every charset family so none of them
    /// breaks the constant-time comparison or the trim on submit.
    #[test]
    async fn pairing_succeeds_with_every_configured_code_shape() {
        for policy in [
            PairingCodePolicy::default(),
            PairingCodePolicy::numeric_compat(),
            PairingCodePolicy::new(20, PairingCodeCharset::Unambiguous).unwrap(),
            PairingCodePolicy::new(PAIRING_CODE_MAX_LENGTH, PairingCodeCharset::Alphanumeric)
                .unwrap(),
        ] {
            let guard = PairingGuard::new(true, &[], policy);
            let code = guard.pairing_code().expect("startup code");
            assert_matches_policy(&code, policy);

            let token = guard
                .try_pair(&code, "client")
                .await
                .expect("not locked out")
                .unwrap_or_else(|| panic!("code of shape {policy:?} should pair"));
            assert!(guard.is_authenticated(&token));
            // One-time consumption survives the longer code.
            assert!(guard.pairing_code().is_none(), "code must be consumed");
            assert!(
                guard.try_pair(&code, "client").await.expect("ok").is_none(),
                "a consumed code must not pair a second time"
            );
        }
    }

    /// A code from one policy must not pair a guard running another. Cheap
    /// regression net against a generator that ignores its policy.
    #[test]
    async fn a_code_from_a_different_policy_does_not_pair() {
        let weak = PairingGuard::new(true, &[], PairingCodePolicy::numeric_compat());
        let strong = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let weak_code = weak.pairing_code().expect("startup code");
        assert!(
            strong
                .try_pair(&weak_code, "client")
                .await
                .expect("not locked out")
                .is_none(),
            "a six-digit code must not open a 32-char guard"
        );
        assert!(!strong.is_paired());
    }

    /// A pairing code is the sole bearer credential for certificate issuance
    /// and is shown on a console and written to logs. One-time use alone left a
    /// copied code redeemable indefinitely, so it must also expire - and the
    /// expired value must be CLEARED, not merely reported absent.
    #[test]
    async fn expired_code_is_rejected_and_cleared() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        guard.age_pairing_code(PAIRING_CODE_TTL);

        assert!(
            guard
                .reserve_pair(&code, "test_client")
                .await
                .unwrap()
                .is_none(),
            "an expired code must not reserve"
        );
        assert!(
            guard.pairing_code().is_none(),
            "an expired code must be cleared from the slot, not left in memory"
        );
    }

    /// Restoring a code after a failed issuance must not extend its lifetime:
    /// otherwise a guesser could keep a code alive by failing repeatedly.
    #[test]
    async fn a_failed_issuance_does_not_extend_the_lifetime() {
        let guard = new_guard(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();

        // Take the code to within a second of expiry BEFORE the failed
        // attempt, so a restore that reset the clock would be visible.
        guard.age_pairing_code(PAIRING_CODE_TTL - Duration::from_secs(1));

        // Reserve and drop uncommitted: the code is restored for a retry.
        {
            let _reservation = guard
                .reserve_pair(&code, "test_client")
                .await
                .unwrap()
                .expect("code should reserve");
        }
        assert_eq!(
            guard.pairing_code().as_deref(),
            Some(code.as_str()),
            "a failed issuance must restore the code"
        );

        // Two more seconds carries the ORIGINAL schedule past expiry. Had the
        // restore reset the clock, a full TTL would still remain here.
        guard.age_pairing_code(Duration::from_secs(2));
        assert!(
            guard
                .reserve_pair(&code, "test_client")
                .await
                .unwrap()
                .is_none(),
            "a restored code must still expire on its original schedule"
        );
    }

    /// After expiry the slot counts as vacant, so an operator can mint a fresh
    /// code without restarting the process - and that replacement redeems.
    #[test]
    async fn a_fresh_replacement_works_after_expiry() {
        let guard = new_guard(true, &[]);
        let stale = guard.pairing_code().unwrap().to_string();
        guard.age_pairing_code(PAIRING_CODE_TTL);

        let fresh = guard
            .generate_pairing_code_if_vacant(PairingCodePolicy::default())
            .expect("an expired code must leave the slot vacant");
        assert_ne!(fresh, stale, "the replacement must be a new code");

        assert!(
            guard
                .reserve_pair(&stale, "test_client")
                .await
                .unwrap()
                .is_none(),
            "the expired code must not redeem after replacement"
        );
        assert!(
            guard
                .reserve_pair(&fresh, "test_client")
                .await
                .unwrap()
                .is_some(),
            "the fresh replacement must redeem"
        );
    }

    // ── Roster-bound tokens ──────────────────────────────────

    fn alice() -> PairedTokenSubject {
        PairedTokenSubject::RosterUser {
            principal_id: "alice".into(),
        }
    }

    fn gateway_with(
        paired_tokens: &[&str],
        paired_token_users: &[(&str, &str)],
    ) -> crate::schema::GatewayConfig {
        crate::schema::GatewayConfig {
            paired_tokens: paired_tokens.iter().map(|t| (*t).to_string()).collect(),
            paired_token_users: paired_token_users
                .iter()
                .map(|(hash, principal)| ((*hash).to_string(), (*principal).to_string()))
                .collect(),
            ..crate::schema::GatewayConfig::default()
        }
    }

    #[test]
    async fn from_gateway_config_loads_each_token_with_its_subject() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_shared"],
            &[(&hash_token("zc_bound"), "alice")],
        ));
        assert_eq!(
            guard.subject_for_token("zc_shared"),
            Some(PairedTokenSubject::SharedOperator)
        );
        assert_eq!(guard.subject_for_token("zc_bound"), Some(alice()));
        assert_eq!(guard.subject_for_token("zc_unknown"), None);
        assert!(
            guard.pairing_code().is_none(),
            "a paired gateway mints no startup code"
        );
    }

    #[test]
    async fn only_bound_tokens_still_withhold_the_startup_code() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &[],
            &[(&hash_token("zc_b"), "alice")],
        ));
        assert!(guard.is_paired());
        assert!(
            guard.pairing_code().is_none(),
            "a bound token counts as paired; no unbound startup code may appear"
        );
    }

    #[test]
    async fn a_hash_listed_in_both_fields_loads_as_bound() {
        let hash = hash_token("zc_both");
        let guard = PairingGuard::from_gateway_config(&gateway_with(&[&hash], &[(&hash, "alice")]));
        assert_eq!(
            guard.subject_for_token("zc_both"),
            Some(alice()),
            "the binding must win; the shared operator is the wider subject"
        );
    }

    #[test]
    async fn malformed_binding_entries_are_skipped_not_widened() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &[],
            &[
                ("zc_plaintext_is_not_a_hash", "alice"),
                (&hash_token("zc_bad_principal"), "not a valid id"),
                (&hash_token("zc_empty_principal"), ""),
            ],
        ));
        for token in [
            "zc_plaintext_is_not_a_hash",
            "zc_bad_principal",
            "zc_empty_principal",
        ] {
            assert!(
                !guard.token_is_paired(token),
                "{token} must not authenticate at all"
            );
        }
        assert!(guard.persisted_tokens().paired_tokens.is_empty());
    }

    /// A damaged binding must not leave the token behind as the shared
    /// operator when `paired_tokens` lists the same token: that would widen
    /// a roster user to full authority after a bad hand edit or restore.
    #[test]
    async fn a_malformed_binding_fails_closed_even_when_paired_tokens_lists_the_token() {
        let hash = hash_token("zc_both");
        for principal in ["not a valid id", ""] {
            let guard =
                PairingGuard::from_gateway_config(&gateway_with(&[&hash], &[(&hash, principal)]));
            assert!(
                !guard.token_is_paired("zc_both"),
                "binding {principal:?}: the token must not authenticate at all"
            );
            assert!(!guard.is_authenticated("zc_both"));
        }

        // The same token as a legacy plaintext entry, bound under its
        // plaintext instead of its hash.
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_plain"],
            &[("zc_plain", "alice")],
        ));
        assert!(!guard.token_is_paired("zc_plain"));

        // A plaintext binding key padded with whitespace, for a token that
        // paired_tokens lists as legacy plaintext.
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_plain"],
            &[(" zc_plain", "alice")],
        ));
        assert!(!guard.token_is_paired("zc_plain"));

        // A binding key damaged only by stray whitespace or case.
        for padded in [
            format!(" {hash}"),
            format!("{} ", hash.to_ascii_uppercase()),
        ] {
            let guard =
                PairingGuard::from_gateway_config(&gateway_with(&[&hash], &[(&padded, "alice")]));
            assert!(
                !guard.token_is_paired("zc_both"),
                "key {padded:?}: the token must not stay loaded as the shared operator"
            );
        }
    }

    /// Two spellings of one hash naming different principals leave no way to
    /// know who the token belongs to, so neither binding loads.
    #[test]
    async fn conflicting_bindings_for_one_hash_fail_closed() {
        let lower = hash_token("zc_split");
        let upper = lower.to_ascii_uppercase();
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &[&lower],
            &[(&lower, "alice"), (&upper, "bob")],
        ));
        assert!(!guard.token_is_paired("zc_split"));

        let agreeing = PairingGuard::from_gateway_config(&gateway_with(
            &[],
            &[(&lower, "alice"), (&upper, "alice")],
        ));
        assert_eq!(
            agreeing.subject_for_token("zc_split"),
            Some(alice()),
            "two spellings that agree are one binding"
        );
    }

    #[test]
    async fn binding_keys_are_matched_case_insensitively() {
        let upper = hash_token("zc_upper").to_ascii_uppercase();
        let guard = PairingGuard::from_gateway_config(&gateway_with(&[], &[(&upper, "alice")]));
        assert_eq!(guard.subject_for_token("zc_upper"), Some(alice()));
    }

    #[test]
    async fn legacy_membership_refuses_bound_tokens() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_shared"],
            &[(&hash_token("zc_bound"), "alice")],
        ));
        assert!(guard.is_authenticated("zc_shared"));
        assert!(
            !guard.is_authenticated("zc_bound"),
            "operator-authority routes must refuse a roster-bound token"
        );
        assert_eq!(guard.authenticate_and_hash("zc_bound"), None);
        assert!(
            guard.token_is_paired("zc_bound"),
            "the strict check still sees the token as paired"
        );
        assert!(guard.token_hash_is_paired(&hash_token("zc_bound")));

        let open = PairingGuard::from_gateway_config(&crate::schema::GatewayConfig {
            require_pairing: false,
            ..gateway_with(&[], &[(&hash_token("zc_bound"), "alice")])
        });
        assert!(
            open.is_authenticated("zc_bound"),
            "with pairing off the open posture is unchanged"
        );
    }

    #[test]
    async fn a_bound_code_pairs_a_token_with_its_subject() {
        let guard = new_guard(true, &["zc_existing".into()]);
        let code = guard
            .generate_new_pairing_code_as(PairingCodePolicy::default(), alice())
            .expect("pairing is required");
        assert_eq!(guard.pending_pairing_code(), Some((code.clone(), alice())));

        let token = guard
            .try_pair(&code, "client")
            .await
            .expect("not locked out")
            .expect("bound code pairs");
        assert_eq!(guard.subject_for_token(&token), Some(alice()));
        assert_eq!(guard.subject_for_hash(&hash_token(&token)), Some(alice()));
        assert!(!guard.is_authenticated(&token));
    }

    #[test]
    async fn a_reservation_carries_the_pending_subject() {
        let guard = new_guard(true, &["zc_existing".into()]);
        let code = guard
            .generate_new_pairing_code_as(PairingCodePolicy::default(), alice())
            .unwrap();
        let reservation = guard
            .reserve_pair(&code, "client")
            .await
            .unwrap()
            .expect("code reserves");
        assert_eq!(reservation.subject(), &alice());
        let token = reservation.commit();
        assert_eq!(guard.subject_for_token(&token), Some(alice()));
    }

    /// A failed issuance restores the code for a retry. It must come back
    /// still bound: an unbound restore would let the next redemption pair as
    /// the shared operator.
    #[test]
    async fn a_dropped_bound_reservation_restores_the_code_still_bound() {
        let guard = new_guard(true, &["zc_existing".into()]);
        let code = guard
            .generate_new_pairing_code_as(PairingCodePolicy::default(), alice())
            .unwrap();
        {
            let _reservation = guard
                .reserve_pair(&code, "client")
                .await
                .unwrap()
                .expect("code reserves");
        }
        assert_eq!(
            guard.pending_pairing_code(),
            Some((code.clone(), alice())),
            "the restored code keeps its binding"
        );
        let token = guard.try_pair(&code, "client").await.unwrap().unwrap();
        assert_eq!(guard.subject_for_token(&token), Some(alice()));
    }

    #[test]
    async fn a_new_mint_replaces_the_pending_subject_both_ways() {
        let guard = new_guard(true, &["zc_existing".into()]);
        let bound = guard
            .generate_new_pairing_code_as(PairingCodePolicy::default(), alice())
            .unwrap();
        let unbound = guard
            .generate_new_pairing_code(PairingCodePolicy::default())
            .unwrap();
        assert_eq!(
            guard.pending_pairing_code(),
            Some((unbound.clone(), PairedTokenSubject::SharedOperator))
        );
        assert!(
            guard.try_pair(&bound, "client").await.unwrap().is_none(),
            "the superseded bound code must not redeem"
        );

        let rebound = guard
            .generate_new_pairing_code_as(PairingCodePolicy::default(), alice())
            .unwrap();
        assert!(
            guard.try_pair(&unbound, "client").await.unwrap().is_none(),
            "the superseded unbound code must not redeem"
        );
        let token = guard.try_pair(&rebound, "client").await.unwrap().unwrap();
        assert_eq!(guard.subject_for_token(&token), Some(alice()));
    }

    #[test]
    async fn if_vacant_mint_binds_the_code() {
        let guard = new_guard(true, &["zc_existing".into()]);
        let code = guard
            .generate_pairing_code_if_vacant_as(PairingCodePolicy::default(), alice())
            .expect("slot is empty once paired");
        assert_eq!(guard.pending_pairing_code(), Some((code, alice())));
        assert_eq!(
            guard
                .generate_pairing_code_if_vacant_as(
                    PairingCodePolicy::default(),
                    PairedTokenSubject::SharedOperator
                )
                .unwrap_err(),
            GeneratePairingCodeError::Pending,
            "a pending bound code is not replaced by the vacant-only path"
        );
    }

    #[test]
    async fn persisted_tokens_keep_bound_tokens_out_of_paired_tokens() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_b_shared", "zc_a_shared"],
            &[(&hash_token("zc_bound"), "alice")],
        ));
        let persisted = guard.persisted_tokens();
        let mut expected_shared = vec![hash_token("zc_a_shared"), hash_token("zc_b_shared")];
        expected_shared.sort_unstable();
        assert_eq!(persisted.paired_tokens, expected_shared);
        assert_eq!(
            persisted.paired_token_users,
            HashMap::from([(hash_token("zc_bound"), "alice".to_string())])
        );
        assert_eq!(guard.tokens().len(), 3, "tokens() lists every paired hash");
    }

    /// A restart rebuilds the guard from what was persisted: the round trip
    /// must keep every token and every binding.
    #[test]
    async fn persisted_tokens_round_trip_through_a_rebuilt_guard() {
        let guard = new_guard(true, &["zc_shared".into()]);
        let code = guard
            .generate_new_pairing_code_as(PairingCodePolicy::default(), alice())
            .unwrap();
        let bound = guard.try_pair(&code, "client").await.unwrap().unwrap();

        let persisted = guard.persisted_tokens();
        let rebuilt = PairingGuard::from_gateway_config(&crate::schema::GatewayConfig {
            paired_tokens: persisted.paired_tokens,
            paired_token_users: persisted.paired_token_users,
            ..crate::schema::GatewayConfig::default()
        });
        assert_eq!(rebuilt.subject_for_token(&bound), Some(alice()));
        assert_eq!(
            rebuilt.subject_for_token("zc_shared"),
            Some(PairedTokenSubject::SharedOperator)
        );
    }

    #[test]
    async fn revoking_a_bound_token_drops_its_binding() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_shared"],
            &[(&hash_token("zc_bound"), "alice")],
        ));
        assert_eq!(
            guard.revoke_token_hash_subject(&hash_token("zc_bound")),
            Some(alice()),
            "revocation reports the subject it removed"
        );
        assert!(!guard.token_is_paired("zc_bound"));
        assert!(guard.persisted_tokens().paired_token_users.is_empty());
        assert_eq!(
            guard.revoke_token_hash_subject(&hash_token("zc_bound")),
            None,
            "a second revocation finds nothing"
        );
        assert!(guard.token_is_paired("zc_shared"));
    }

    #[test]
    async fn revoke_all_tokens_drops_bound_tokens_too() {
        let guard = PairingGuard::from_gateway_config(&gateway_with(
            &["zc_shared"],
            &[(&hash_token("zc_bound"), "alice")],
        ));
        assert_eq!(guard.revoke_all_tokens(), 2);
        assert_eq!(guard.persisted_tokens(), PersistedPairedTokens::default());
    }

    #[test]
    async fn roster_subject_uses_the_durable_principal_id() {
        let mut users = HashMap::new();
        users.insert(
            "alice-renamed".to_string(),
            crate::schema::UserConfig {
                principal_id: Some("alice".into()),
                uid: Some(4242),
                permission_profiles: vec!["reader".into()],
            },
        );
        users.insert(
            "bob".to_string(),
            crate::schema::UserConfig {
                principal_id: None,
                uid: Some(4343),
                permission_profiles: vec!["reader".into()],
            },
        );
        assert_eq!(
            PairedTokenSubject::for_roster_entry(&users, "alice-renamed"),
            Some(alice()),
            "a pinned principal_id wins over the entry name"
        );
        assert_eq!(
            PairedTokenSubject::for_roster_entry(&users, "bob")
                .as_ref()
                .and_then(PairedTokenSubject::roster_principal_id),
            Some("bob")
        );
        assert_eq!(PairedTokenSubject::for_roster_entry(&users, "carol"), None);
    }

    /// The one-active-code model is unchanged: minting replaces an unexpired
    /// code rather than erroring, and the superseded value stops working.
    #[test]
    async fn minting_still_replaces_an_unexpired_code() {
        let guard = new_guard(true, &[]);
        let first = guard.pairing_code().unwrap().to_string();
        let second = guard
            .generate_new_pairing_code(PairingCodePolicy::default())
            .expect("mint must replace unconditionally");
        assert_ne!(first, second);
        assert!(
            guard
                .reserve_pair(&first, "test_client")
                .await
                .unwrap()
                .is_none(),
            "the superseded code must not redeem"
        );
        assert!(
            guard
                .reserve_pair(&second, "test_client")
                .await
                .unwrap()
                .is_some(),
            "the replacement code must redeem"
        );
    }
}
