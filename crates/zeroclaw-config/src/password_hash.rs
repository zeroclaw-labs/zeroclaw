//! Local password hashes: the one policy behind `[users.<name>].password_hash`.
//!
//! Config validation accepts a stored hash only through [`validate_phc`], new
//! hashes come from [`hash_password`], and the `password` auth provider checks
//! a password through [`verify_password`]. Keeping all three here means they
//! cannot disagree about what a usable hash is.
//!
//! A hash is a PHC string for scrypt,
//! `$scrypt$ln=<log2 N>,r=<r>,p=<p>$<salt>$<hash>`, with every parameter spelled
//! out: a hash that leaned on the library's defaults would change meaning if
//! those defaults moved. Its cost must fall inside bounds. The floor is the
//! lowest cost among the scrypt configurations OWASP lists (N times p of
//! 81920) with at least 16 MiB of memory, so a weak hash is refused; the
//! ceiling (128 MiB of memory, 16 passes) stops a configured hash from
//! turning each login attempt into a denial of service.

use std::collections::HashMap;

use anyhow::{Result, bail};
use scrypt::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use scrypt::{Params, Scrypt};

/// Longest password accepted, in bytes. Longer input is never hashed.
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// `log2(N)` for new hashes. With [`DEFAULT_R`] and [`DEFAULT_P`] this is
/// 32 MiB of memory and three passes, one of the scrypt configurations OWASP
/// lists.
const DEFAULT_LOG_N: u8 = 15;
/// Block size `r` for new hashes, and the only one accepted.
const DEFAULT_R: u32 = 8;
/// Parallelism `p` for new hashes.
const DEFAULT_P: u32 = 3;

const OUTPUT_LEN: usize = 32;
const MIN_SALT_LEN: usize = 16;
/// Lowest accepted `log2(N)`: 16 MiB of memory at `r = 8`.
const MIN_LOG_N: u8 = 14;
const MAX_LOG_N: u8 = 17;
const MAX_P: u32 = 16;
/// Lowest accepted `N * p`: the lowest cost among the scrypt configurations
/// OWASP lists (N = 2^14 with p = 5, or N = 2^13 with p = 10).
const MIN_WORK: u64 = 5 << 14;

/// Salt and output shared by every decoy: random bytes, fixed in the source.
/// No password is known to produce them, and a decoy match never counts.
const DECOY_SALT: &str = "u3FV+/k8uwcD6WaZAudvAQ";
const DECOY_OUTPUT: &str = "AVTGAXy3ZHJhyVQTzsnzc2V2bV4VSv5rAfXTDpEdRKs";

/// `(log2 N, r, p)` of a hash.
type ParamSet = (u8, u32, u32);

fn param_set(hash: &PasswordHash<'_>) -> Option<ParamSet> {
    Params::try_from(hash)
        .ok()
        .map(|params| (params.log_n(), params.r(), params.p()))
}

/// `N * p` for a parameter set whose `log2 N` is already bounded.
fn work((log_n, _, p): ParamSet) -> u64 {
    (1u64 << log_n) * u64::from(p)
}

/// The stand-in hash [`verify_password`] checks when there is no real hash
/// to check. It carries the parameters the real hashes beside it use, so a
/// miss spends the same scrypt work as checking one of them.
#[derive(Clone, Debug)]
pub struct Decoy(String);

impl Decoy {
    /// A decoy at the parameters most of `stored` use. Values outside the
    /// policy are skipped. A tie goes to the default parameters, then to the
    /// cheaper set, then to the smaller one; with no usable hash at all the
    /// decoy takes the defaults, which every hash from [`hash_password`] has.
    #[must_use]
    pub fn for_hashes<'a>(stored: impl IntoIterator<Item = &'a str>) -> Self {
        let mut counts: HashMap<ParamSet, usize> = HashMap::new();
        for phc in stored {
            if let Some(set) = parse(phc).ok().as_ref().and_then(param_set) {
                *counts.entry(set).or_default() += 1;
            }
        }
        let default = (DEFAULT_LOG_N, DEFAULT_R, DEFAULT_P);
        let (log_n, r, p) = counts
            .into_iter()
            .max_by(|(a, a_count), (b, b_count)| {
                a_count
                    .cmp(b_count)
                    .then_with(|| (*a == default).cmp(&(*b == default)))
                    .then_with(|| work(*b).cmp(&work(*a)))
                    .then_with(|| b.cmp(a))
            })
            .map_or(default, |(set, _)| set);
        Self(format!(
            "$scrypt$ln={log_n},r={r},p={p}${DECOY_SALT}${DECOY_OUTPUT}"
        ))
    }
}

impl Default for Decoy {
    fn default() -> Self {
        Self::for_hashes([])
    }
}

/// Check a stored hash against the policy: scrypt, every parameter explicit,
/// cost inside the bounds, a salt of at least 16 bytes, a 32-byte output.
///
/// Errors say what is wrong without repeating the hash.
pub fn validate_phc(phc: &str) -> Result<()> {
    parse(phc).map(|_| ())
}

fn parse(phc: &str) -> Result<PasswordHash<'_>> {
    let Ok(hash) = PasswordHash::new(phc) else {
        bail!("is not a PHC hash string");
    };
    if hash.algorithm != scrypt::ALG_ID {
        bail!("uses an algorithm other than scrypt, the only one supported");
    }
    if hash.version.is_some() {
        bail!("carries a version field, which scrypt hashes do not have");
    }
    let mut names: Vec<&str> = hash.params.iter().map(|(name, _)| name.as_str()).collect();
    names.sort_unstable();
    if names != ["ln", "p", "r"] {
        bail!("must spell out exactly the `ln`, `r`, and `p` parameters");
    }
    let Some((log_n, r, p)) = param_set(&hash) else {
        bail!("has malformed scrypt parameters");
    };
    if !(MIN_LOG_N..=MAX_LOG_N).contains(&log_n) {
        bail!("has ln = {log_n}; the accepted range is {MIN_LOG_N} to {MAX_LOG_N}");
    }
    if r != DEFAULT_R {
        bail!("has r = {r}; only r = {DEFAULT_R} is accepted");
    }
    if !(1..=MAX_P).contains(&p) {
        bail!("has p = {p}; the accepted range is 1 to {MAX_P}");
    }
    if work((log_n, r, p)) < MIN_WORK {
        bail!("costs too little: N * p must be at least {MIN_WORK}");
    }
    let mut salt_buf = [0u8; 64];
    match hash.salt.map(|salt| salt.decode_b64(&mut salt_buf)) {
        Some(Ok(salt)) if salt.len() >= MIN_SALT_LEN => {}
        _ => bail!("needs a salt of at least {MIN_SALT_LEN} bytes"),
    }
    match hash.hash {
        Some(output) if output.len() == OUTPUT_LEN => {}
        _ => bail!("needs a {OUTPUT_LEN}-byte hash output"),
    }
    Ok(hash)
}

/// Hash `password` for storage, at the default parameters with a fresh
/// random salt.
pub fn hash_password(password: &str) -> Result<String> {
    if password.is_empty() {
        bail!("the password is empty");
    }
    if password.len() > MAX_PASSWORD_BYTES {
        bail!("the password is longer than {MAX_PASSWORD_BYTES} bytes");
    }
    let salt: [u8; MIN_SALT_LEN] = rand::random();
    let salt = SaltString::encode_b64(&salt)
        .map_err(|e| anyhow::Error::msg(format!("encoding the salt failed: {e}")))?;
    let params = Params::new(DEFAULT_LOG_N, DEFAULT_R, DEFAULT_P, OUTPUT_LEN)
        .map_err(|e| anyhow::Error::msg(format!("scrypt parameters: {e}")))?;
    let hash = Scrypt
        .hash_password_customized(password.as_bytes(), None, None, params, &salt)
        .map_err(|e| anyhow::Error::msg(format!("hashing the password failed: {e}")))?;
    Ok(hash.to_string())
}

/// Whether `password` matches `stored`, spending one scrypt verification
/// whatever the answer.
///
/// With no hash to check (`stored` is `None`, or does not satisfy the policy)
/// or an input that is not a usable password (empty, or longer than
/// [`MAX_PASSWORD_BYTES`]), the verification runs against `decoy` and the
/// answer is `false`. Built with [`Decoy::for_hashes`] over the same stored
/// hashes, a miss then takes as long as checking a typical one of them, so
/// the time taken does not reveal which case applied. A stored hash whose
/// parameters differ from the decoy's costs a different amount of work.
pub fn verify_password(password: &str, stored: Option<&str>, decoy: &Decoy) -> bool {
    let Some(check) = Check::select(password, stored, decoy) else {
        return false;
    };
    let matched = Scrypt.verify_password(check.input, &check.hash).is_ok();
    matched && check.counts
}

/// One verification: the input and hash scrypt runs on, and whether a match
/// counts. Chosen before any hashing, so every call runs exactly one
/// verification.
struct Check<'a> {
    input: &'a [u8],
    hash: PasswordHash<'a>,
    counts: bool,
}

impl<'a> Check<'a> {
    /// `None` only if the decoy failed to parse, which its tests rule out;
    /// the caller then refuses without hashing.
    fn select(password: &'a str, stored: Option<&'a str>, decoy: &'a Decoy) -> Option<Self> {
        let usable = !password.is_empty() && password.len() <= MAX_PASSWORD_BYTES;
        if usable && let Some(hash) = stored.and_then(|phc| parse(phc).ok()) {
            return Some(Self {
                input: password.as_bytes(),
                hash,
                counts: true,
            });
        }
        let input: &[u8] = if usable { password.as_bytes() } else { b"" };
        PasswordHash::new(&decoy.0).ok().map(|hash| Self {
            input,
            hash,
            counts: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scrypt::password_hash::Output;
    use std::sync::LazyLock;

    const PASSWORD: &str = "zeroclaw-test-passphrase";

    /// One real hash shared by the tests that need the KDF: each run costs
    /// a full scrypt pass, which is slow in an unoptimized test build.
    static STORED: LazyLock<String> =
        LazyLock::new(|| hash_password(PASSWORD).expect("hashing a test password"));

    /// A PHC string with the given parameters and filler salt and output.
    /// Validation does no hashing, so these need no real password.
    fn phc(params: &str, salt_len: usize, output_len: usize) -> String {
        let salt = SaltString::encode_b64(&vec![7u8; salt_len]).expect("salt");
        let output = Output::new(&vec![9u8; output_len]).expect("output");
        format!("$scrypt${params}${salt}${output}")
    }

    #[test]
    fn generated_hash_meets_the_policy_and_verifies() {
        let decoy = Decoy::default();
        assert!(STORED.starts_with("$scrypt$ln=15,r=8,p=3$"), "{}", *STORED);
        validate_phc(&STORED).expect("generated hash is valid");
        assert!(verify_password(PASSWORD, Some(&STORED), &decoy));
        assert!(!verify_password(
            "zeroclaw-wrong-passphrase",
            Some(&STORED),
            &decoy
        ));
    }

    #[test]
    fn each_hash_gets_its_own_salt() {
        let again = hash_password(PASSWORD).expect("hash");
        assert_ne!(again, *STORED, "two hashes of one password must differ");
    }

    #[test]
    fn unusable_input_and_missing_hashes_are_refused() {
        let decoy = Decoy::default();
        assert!(!verify_password(PASSWORD, None, &decoy), "no stored hash");
        assert!(!verify_password("", Some(&STORED), &decoy), "empty input");
        let long = "a".repeat(MAX_PASSWORD_BYTES + 1);
        assert!(
            !verify_password(&long, Some(&STORED), &decoy),
            "over-long input"
        );
        assert!(
            !verify_password(PASSWORD, Some("not-a-hash"), &decoy),
            "a stored value that fails the policy verifies nothing"
        );
    }

    #[test]
    fn every_miss_is_checked_against_the_decoy() {
        let decoy = Decoy::for_hashes([STORED.as_str()]);
        let real = Check::select(PASSWORD, Some(&STORED), &decoy).expect("check");
        assert!(real.counts, "a usable password against a real hash counts");

        let long = "a".repeat(MAX_PASSWORD_BYTES + 1);
        let misses = [
            (PASSWORD, None, "no stored hash"),
            (
                PASSWORD,
                Some("not-a-hash"),
                "a stored value outside the policy",
            ),
            ("", Some(STORED.as_str()), "empty input"),
            (long.as_str(), Some(STORED.as_str()), "over-long input"),
        ];
        for (password, stored, why) in misses {
            let check = Check::select(password, stored, &decoy).expect("the decoy parses");
            assert!(!check.counts, "{why}: a decoy match must not count");
            assert_eq!(
                check.hash.to_string(),
                decoy.0,
                "{why}: the same work runs against the decoy"
            );
        }
    }

    #[test]
    fn hashing_refuses_empty_and_over_long_passwords() {
        assert!(hash_password("").is_err());
        assert!(hash_password(&"a".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
    }

    fn decoy_params(decoy: &Decoy) -> ParamSet {
        let hash = parse(&decoy.0).expect("a decoy satisfies the policy");
        param_set(&hash).expect("params")
    }

    #[test]
    fn decoy_defaults_to_the_parameters_new_hashes_use() {
        assert_eq!(
            decoy_params(&Decoy::default()),
            (DEFAULT_LOG_N, DEFAULT_R, DEFAULT_P)
        );
        let stored_params = param_set(&parse(&STORED).expect("parse")).expect("params");
        assert_eq!(decoy_params(&Decoy::default()), stored_params);
    }

    #[test]
    fn decoy_takes_the_parameters_most_hashes_use() {
        let a = phc("ln=16,r=8,p=2", 16, 32);
        let b = phc("ln=17,r=8,p=1", 16, 32);
        let default = phc("ln=15,r=8,p=3", 16, 32);
        let cases = [
            (vec![&a, &a, &default], (16, 8, 2), "the majority wins"),
            (vec![&a, &default], (15, 8, 3), "a tie goes to the default"),
            (
                vec![&b, &a],
                (16, 8, 2),
                "then to the smaller set at equal cost",
            ),
        ];
        for (stored, expected, why) in cases {
            let decoy = Decoy::for_hashes(stored.iter().map(|s| s.as_str()));
            assert_eq!(decoy_params(&decoy), expected, "{why}");
        }
        let cheaper = phc("ln=14,r=8,p=5", 16, 32);
        let dearer = phc("ln=16,r=8,p=2", 16, 32);
        let decoy = Decoy::for_hashes([dearer.as_str(), cheaper.as_str()]);
        assert_eq!(
            decoy_params(&decoy),
            (14, 8, 5),
            "a tie between non-default sets goes to the cheaper one"
        );

        let skipped = Decoy::for_hashes(["not-a-hash", a.as_str()]);
        assert_eq!(
            decoy_params(&skipped),
            (16, 8, 2),
            "values that are not PHC strings are skipped"
        );
        // Well-formed hashes outside the cost bounds must not set the decoy's
        // cost, even when they are the majority: the decoy is checked with
        // `PasswordHash::new`, so the policy is enforced only here.
        let over = phc("ln=20,r=8,p=1", 16, 32);
        let outvoted = Decoy::for_hashes([over.as_str(), over.as_str(), a.as_str()]);
        assert_eq!(
            decoy_params(&outvoted),
            (16, 8, 2),
            "hashes outside the policy are skipped"
        );
    }

    #[test]
    fn policy_accepts_owasp_configurations_inside_the_bounds() {
        for params in [
            "ln=14,r=8,p=5",
            "ln=15,r=8,p=3",
            "ln=16,r=8,p=2",
            "ln=17,r=8,p=1",
        ] {
            validate_phc(&phc(params, 16, 32)).unwrap_or_else(|e| panic!("{params}: {e}"));
        }
        validate_phc(&phc("ln=17,r=8,p=16", 32, 32)).expect("the ceiling is inclusive");
    }

    #[test]
    fn policy_rejects_hashes_outside_the_bounds() {
        let cases = [
            (phc("ln=13,r=8,p=16", 16, 32), "ln below the floor"),
            (phc("ln=18,r=8,p=1", 16, 32), "ln above the ceiling"),
            (phc("ln=15,r=16,p=3", 16, 32), "r other than 8"),
            (phc("ln=15,r=8,p=17", 16, 32), "p above the ceiling"),
            (phc("ln=15,r=8,p=1", 16, 32), "work below the floor"),
            (phc("ln=14,r=8,p=4", 16, 32), "work below the floor"),
            (phc("ln=15,r=8,p=3", 8, 32), "short salt"),
            (phc("ln=15,r=8,p=3", 16, 16), "short output"),
            (phc("ln=15,r=8", 16, 32), "implicit p"),
            (phc("ln=15,r=8,p=3,x=1", 16, 32), "unknown parameter"),
        ];
        for (hash, why) in cases {
            assert!(validate_phc(&hash).is_err(), "{why}: {hash}");
        }
    }

    #[test]
    fn policy_rejects_other_formats() {
        let argon = "$argon2id$v=19$m=65536,t=3,p=4$c29tZXNhbHRzb21lc2FsdA$\
                     aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2g";
        for value in ["", "correct horse battery staple", argon] {
            assert!(validate_phc(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn validation_errors_do_not_repeat_the_hash() {
        let hash = phc("ln=15,r=8,p=1", 16, 32);
        let salt_and_output = hash.rsplit_once("p=1$").expect("shape").1;
        let error = validate_phc(&hash)
            .expect_err("work below the floor")
            .to_string();
        assert!(!error.contains(salt_and_output), "{error}");
    }
}
