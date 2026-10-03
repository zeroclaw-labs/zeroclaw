//! Where the daemon's local RPC endpoint lives.
//!
//! One resolver for the daemon and every client (the runtime, the RPC client
//! and zerocode), so a client always dials the endpoint the daemon bound:
//!
//! - `ZEROCLAW_SOCKET`, when set to a non-blank value, wins on every platform.
//! - Otherwise, on Unix, `<data_dir>/daemon.sock`.
//! - Otherwise, on Windows, a named pipe whose name is a stable hash of the
//!   data directory: named pipes live in one flat kernel namespace, so two
//!   data directories on one machine need different names.
//!
//! The Windows name used to come from `std`'s `DefaultHasher`, whose output
//! the standard library does not promise to keep across releases, so two
//! binaries built by different toolchains could derive different names for
//! one data directory. The name now comes from FNV-1a over a specified
//! encoding of the directory; see [`pipe_name`]. For one release a client
//! also tries the old name when nothing listens at the new one, so it still
//! finds a daemon that an older binary started and nobody has restarted yet.
//! Finding that daemon is all the fallback does: whether the client can then
//! use it is up to the client's handshake. The daemon binds only the new name.

use std::path::{Path, PathBuf};

/// Environment variable that overrides the daemon endpoint on every platform.
pub const SOCKET_ENV: &str = "ZEROCLAW_SOCKET";

/// The endpoint the daemon binds for `data_dir`, and the first one a client
/// dials. Honors a non-blank `ZEROCLAW_SOCKET`.
#[must_use]
pub fn resolve_endpoint(data_dir: &Path) -> PathBuf {
    resolve_endpoint_with(std::env::var(SOCKET_ENV).ok().as_deref(), data_dir)
}

/// [`resolve_endpoint`] with the override passed in rather than read from
/// the environment. A blank override is ignored; surrounding whitespace is
/// trimmed.
#[must_use]
pub fn resolve_endpoint_with(socket_override: Option<&str>, data_dir: &Path) -> PathBuf {
    explicit_endpoint(socket_override).unwrap_or_else(|| default_endpoint(data_dir))
}

/// The endpoint an override names, if it names one: trimmed, and `None` when
/// blank.
fn explicit_endpoint(socket_override: Option<&str>) -> Option<PathBuf> {
    socket_override
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The platform default endpoint under `data_dir`, ignoring the override.
#[cfg(not(windows))]
#[must_use]
pub fn default_endpoint(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.sock")
}

/// The platform default endpoint under `data_dir`, ignoring the override.
#[cfg(windows)]
#[must_use]
pub fn default_endpoint(data_dir: &Path) -> PathBuf {
    PathBuf::from(pipe_name(data_dir))
}

/// The named-pipe name for `data_dir`: `\\.\pipe\zeroclaw-daemon-` followed
/// by 16 hex digits of a 64-bit FNV-1a hash. Every binary derives the same
/// name because the hash input is fixed:
///
/// 1. The path as UTF-16 code units. On Windows these come from
///    `encode_wide`, which is lossless, unpaired surrogates included.
/// 2. The Windows prefix is read first, by the kinds `std::path::Prefix`
///    distinguishes: verbatim (`\\?\C:`, `\\?\UNC\server\share`,
///    `\\?\name`), device namespace (`\\.\name`), UNC (`\\server\share`) or
///    a drive (`C:`). Only the exact `\\?\` spelling is verbatim; as in std,
///    `//?/C:/x` is UNC with the server `?`. The `UNC` of a verbatim UNC
///    path matches in any case, as on Windows. Verbatim, device and UNC
///    prefixes root the path; after a drive or with no prefix, a separator
///    does, so `C:\` (the drive's root) and `C:` (its current directory)
///    differ. Two separators without a share (`\\server`) are no prefix, as
///    in std, and only root the path.
/// 3. The components after the prefix are made equal across spellings
///    Windows treats as one directory: `\` and `/` both separate, and empty
///    and `.` components are dropped, which covers repeated and trailing
///    separators. A verbatim path separates only on `\` and keeps `.`,
///    because Windows takes it as written. Trailing dots and spaces, which
///    Windows trims from a plain path's components, are kept: `C:\x.` and
///    `C:\x` get different pipes although Windows opens one directory for
///    both, which errs toward a missed daemon rather than a shared pipe.
/// 4. The key records the prefix's kind. A plain path (relative, rooted,
///    drive or UNC) is keyed by its canonical spelling; a device or verbatim
///    path's key starts with `/` and the kind's name, which no plain key can.
///    So `\\.\name` never shares a key with a UNC path, and a verbatim path
///    (which skips Windows' normalization: `\\?\C:\x.` keeps the trailing
///    dot that `C:\x.` loses) never shares one with a plain spelling.
/// 5. ASCII letters are lower-cased, so drive letters and ASCII names match
///    in either case. Other letters are left alone: Windows folds them with a
///    per-volume table that cannot be reproduced stably, and a directory can
///    be case-sensitive.
/// 6. Each unit is hashed as two little-endian bytes.
///
/// `..` is not resolved and links are not followed; both need the
/// filesystem. The function is defined on every platform so its values can
/// be tested anywhere. Off Windows the units come from the path's Unicode
/// form, which equals `encode_wide` for every path that is valid Unicode.
#[must_use]
pub fn pipe_name(data_dir: &Path) -> String {
    pipe_name_from_units(&path_units(data_dir))
}

#[cfg(windows)]
fn path_units(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().collect()
}

#[cfg(not(windows))]
fn path_units(path: &Path) -> Vec<u16> {
    path.to_string_lossy().encode_utf16().collect()
}

fn pipe_name_from_units(units: &[u16]) -> String {
    let key = pipe_key(units);
    let hash = fnv1a_64(key.iter().flat_map(|unit| unit.to_le_bytes()));
    format!(r"\\.\pipe\zeroclaw-daemon-{hash:016x}")
}

const BACKSLASH: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;
const DOT: u16 = b'.' as u16;
const COLON: u16 = b':' as u16;
const QUESTION: u16 = b'?' as u16;

/// Steps 2 to 5 of [`pipe_name`]: one key per directory, and a different key
/// for a different kind of path, by construction.
///
/// A plain path (relative, rooted, drive or UNC) is keyed by its canonical
/// spelling: the prefix, a `\` when the path is rooted, and the components
/// joined by `\`. That spelling never contains `/`: every plain component,
/// and a UNC server and share, is split at `/`.
///
/// A device-namespace or verbatim path is keyed as `/`, its kind, `/`, and
/// then its prefix fields and components joined by `\`. No plain key starts
/// with `/`, and no kind contains one, so keys of different kinds never
/// meet, whatever text the components hold. Within a kind, the fields and
/// components contain no `\`, so the key reads back to one path.
fn pipe_key(units: &[u16]) -> Vec<u16> {
    let Prefixed {
        prefix,
        rest,
        rooted,
    } = split_prefix(units);
    let verbatim = prefix.is_verbatim();
    let components = rest
        .split(|unit| is_separator(*unit, verbatim))
        .filter(|component| !component.is_empty() && (verbatim || component[..] != [DOT]));

    let drive_field: [u16; 2];
    let mut key = Vec::new();
    let mut fields: Vec<&[u16]> = Vec::new();
    let kind = match prefix {
        Prefix::None => None,
        Prefix::Disk(letter) => {
            key.extend([letter, COLON]);
            None
        }
        Prefix::Unc(server, share) => {
            key.extend([BACKSLASH, BACKSLASH]);
            key.extend_from_slice(server);
            key.push(BACKSLASH);
            key.extend_from_slice(share);
            None
        }
        Prefix::DeviceNs(name) => {
            fields.push(name);
            Some("device")
        }
        Prefix::Verbatim(name) => {
            fields.push(name);
            Some("verbatim")
        }
        Prefix::VerbatimDisk(letter) => {
            drive_field = [letter, COLON];
            fields.push(&drive_field);
            Some("verbatim-disk")
        }
        Prefix::VerbatimUnc(server, share) => {
            fields.extend([server, share]);
            Some("verbatim-unc")
        }
    };
    match kind {
        None => {
            if rooted {
                key.push(BACKSLASH);
            }
        }
        Some(kind) => {
            key.push(SLASH);
            key.extend(kind.bytes().map(u16::from));
            key.push(SLASH);
        }
    }
    for (index, part) in fields.into_iter().chain(components).enumerate() {
        if index > 0 {
            key.push(BACKSLASH);
        }
        key.extend_from_slice(part);
    }
    key.into_iter().map(fold_ascii).collect()
}

/// A path's Windows prefix, by the kinds `std::path::Prefix` distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix<'a> {
    /// No prefix: a relative path, or one rooted by a leading separator.
    None,
    /// `C:`
    Disk(u16),
    /// `\\server\share`
    Unc(&'a [u16], &'a [u16]),
    /// `\\.\name`
    DeviceNs(&'a [u16]),
    /// `\\?\name`
    Verbatim(&'a [u16]),
    /// `\\?\C:`
    VerbatimDisk(u16),
    /// `\\?\UNC\server\share`
    VerbatimUnc(&'a [u16], &'a [u16]),
}

impl Prefix<'_> {
    /// Whether only `\` separates and `.` components stay: Windows takes a
    /// verbatim path as written.
    fn is_verbatim(self) -> bool {
        matches!(
            self,
            Self::Verbatim(_) | Self::VerbatimDisk(_) | Self::VerbatimUnc(..)
        )
    }
}

/// A path split after its Windows prefix.
struct Prefixed<'a> {
    prefix: Prefix<'a>,
    /// Everything after the prefix and the separator that ends it.
    rest: &'a [u16],
    /// Always for verbatim, device and UNC prefixes; after a drive or with no
    /// prefix, when a separator comes next.
    rooted: bool,
}

/// Read the prefix the way `std::path::Prefix` does on Windows, on any host.
/// Only the exact `\\?\` spelling is verbatim; `//?/` and mixed spellings
/// read as UNC with the server `?`, as in std.
fn split_prefix(units: &[u16]) -> Prefixed<'_> {
    const VERBATIM: [u16; 4] = [BACKSLASH, BACKSLASH, QUESTION, BACKSLASH];
    const UNC: [u8; 4] = *b"UNC\\";

    if let Some(after) = units.strip_prefix(&VERBATIM[..]) {
        let (prefix, rest) = if after.len() >= UNC.len()
            && after
                .iter()
                .zip(UNC)
                .all(|(&unit, byte)| fold_ascii(unit) == fold_ascii(u16::from(byte)))
        {
            let (server, after_server) = next_component(&after[UNC.len()..], true);
            let (share, rest) = next_component(after_server, true);
            (Prefix::VerbatimUnc(server, share), rest)
        } else if let Some(letter) = drive(after)
            && after.get(2).is_none_or(|&unit| unit == BACKSLASH)
        {
            (
                Prefix::VerbatimDisk(letter),
                after.get(3..).unwrap_or_default(),
            )
        } else {
            let (name, rest) = next_component(after, true);
            (Prefix::Verbatim(name), rest)
        };
        return Prefixed {
            prefix,
            rest,
            rooted: true,
        };
    }

    if let [first, second, after @ ..] = units
        && is_separator(*first, false)
        && is_separator(*second, false)
    {
        if let [DOT, separator, after_dot @ ..] = after
            && is_separator(*separator, false)
        {
            let (name, rest) = next_component(after_dot, false);
            return Prefixed {
                prefix: Prefix::DeviceNs(name),
                rest,
                rooted: true,
            };
        }
        let (server, after_server) = next_component(after, false);
        let (share, rest) = next_component(after_server, false);
        if !server.is_empty() && !share.is_empty() {
            return Prefixed {
                prefix: Prefix::Unc(server, share),
                rest,
                rooted: true,
            };
        }
        // Not a valid UNC prefix: an ordinary rooted path, as std reads it.
    }

    let (prefix, rest) = match drive(units) {
        Some(letter) => (Prefix::Disk(letter), &units[2..]),
        None => (Prefix::None, units),
    };
    Prefixed {
        prefix,
        rest,
        rooted: rest.first().is_some_and(|&unit| is_separator(unit, false)),
    }
}

/// `units` up to the first separator, and what follows that separator.
fn next_component(units: &[u16], verbatim: bool) -> (&[u16], &[u16]) {
    match units.iter().position(|&unit| is_separator(unit, verbatim)) {
        Some(at) => (&units[..at], &units[at + 1..]),
        None => (units, &[]),
    }
}

/// The drive letter when `units` starts with one, such as `C:`.
fn drive(units: &[u16]) -> Option<u16> {
    match units {
        [letter, COLON, ..]
            if u8::try_from(*letter).is_ok_and(|byte| byte.is_ascii_alphabetic()) =>
        {
            Some(*letter)
        }
        _ => None,
    }
}

fn is_separator(unit: u16, verbatim: bool) -> bool {
    unit == BACKSLASH || (!verbatim && unit == SLASH)
}

fn fold_ascii(unit: u16) -> u16 {
    match u8::try_from(unit) {
        Ok(byte) => u16::from(byte.to_ascii_lowercase()),
        Err(_) => unit,
    }
}

/// The named-pipe name daemons used before the stable hash, derived with
/// `DefaultHasher` exactly as they did. Clients still try it for one release
/// so they reach a daemon an older binary started; remove it after that.
#[must_use]
pub fn legacy_pipe_name(data_dir: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data_dir.hash(&mut hasher);
    format!(r"\\.\pipe\zeroclaw-{:x}", hasher.finish())
}

/// The endpoints a client dials, in order: `primary` is where a current
/// daemon listens; `legacy` is the pre-stable-hash pipe name, tried only when
/// nothing listens at `primary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientEndpoints {
    pub primary: PathBuf,
    /// Set only on Windows when no `ZEROCLAW_SOCKET` override is in effect.
    pub legacy: Option<PathBuf>,
}

impl ClientEndpoints {
    /// Every endpoint to try, primary first.
    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.primary.as_path()).chain(self.legacy.as_deref())
    }
}

/// The endpoints a client dials for `data_dir`. Honors a non-blank
/// `ZEROCLAW_SOCKET`, which disables the legacy fallback.
#[must_use]
pub fn client_endpoints(data_dir: &Path) -> ClientEndpoints {
    client_endpoints_with(std::env::var(SOCKET_ENV).ok().as_deref(), data_dir)
}

/// [`client_endpoints`] with the override passed in rather than read from
/// the environment.
#[must_use]
pub fn client_endpoints_with(socket_override: Option<&str>, data_dir: &Path) -> ClientEndpoints {
    endpoints_with_fallback(socket_override, data_dir, cfg!(windows))
}

/// [`client_endpoints_with`] with the platform's legacy fallback passed in,
/// so the Windows rule is tested on every platform.
fn endpoints_with_fallback(
    socket_override: Option<&str>,
    data_dir: &Path,
    legacy_fallback: bool,
) -> ClientEndpoints {
    // Whether an override was given is read from the override itself: one
    // that happens to name the default endpoint is still an explicit choice
    // and must not open a second one.
    match explicit_endpoint(socket_override) {
        Some(primary) => ClientEndpoints {
            primary,
            legacy: None,
        },
        None => ClientEndpoints {
            primary: default_endpoint(data_dir),
            legacy: legacy_fallback.then(|| PathBuf::from(legacy_pipe_name(data_dir))),
        },
    }
}

/// Data directories every endpoint caller's agreement test resolves. One
/// list keeps the runtime, the RPC client and zerocode checking the same
/// cases.
#[doc(hidden)]
pub const AGREEMENT_DATA_DIRS: &[&str] = &[
    "/home/alice/.zeroclaw/data",
    "/srv/zeroclaw/profiles/ops/data",
    "relative/data",
    r"C:\Users\Alice\.zeroclaw\data",
    r"c:\users\alice\.zeroclaw\data",
    r"D:\ZeroClaw\Data",
];

/// 64-bit FNV-1a: a fixed, published hash, so the pipe name is the same for
/// every binary that ever computes it.
fn fnv1a_64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.into_iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_are_fixed_values() {
        // These names are what every daemon and client, on any toolchain,
        // derives for these directories. Changing one strands every running
        // daemon from new clients; do not update them to make a test pass.
        assert_eq!(
            pipe_name(Path::new(r"C:\Users\Alice\.zeroclaw\data")),
            r"\\.\pipe\zeroclaw-daemon-811bcf3aa85679f9"
        );
        assert_eq!(
            pipe_name(Path::new("/home/alice/.zeroclaw/data")),
            r"\\.\pipe\zeroclaw-daemon-679420572319782d"
        );
        assert_eq!(
            pipe_name(Path::new(r"D:\ZeroClaw\Data")),
            r"\\.\pipe\zeroclaw-daemon-c58e01b43a3f5ca0"
        );
        assert_eq!(
            pipe_name(Path::new(r"\\server\share\zeroclaw\data")),
            r"\\.\pipe\zeroclaw-daemon-e9dc363b4b04616a"
        );
    }

    #[test]
    fn spellings_of_one_windows_directory_share_a_pipe() {
        let groups: &[&[&str]] = &[
            &[
                r"C:\Users\Alice\.zeroclaw\data",
                r"c:\users\alice\.zeroclaw\data",
                "C:/Users/Alice/.zeroclaw/data",
                "C:\\Users\\Alice\\.zeroclaw\\data\\",
                r"C:\Users\\Alice\.zeroclaw\\\data",
                r"C:\Users\.\Alice\.zeroclaw/./data/",
                r"C:\Users/Alice\.zeroclaw/data",
            ],
            &[r"C:\", "C:/", r"c:\.", r"C:\\"],
            &["C:", "c:", r"C:.", r"C:.\"],
            &[
                r"\\server\share\zeroclaw\data",
                "//server/share/zeroclaw/data/",
                r"\\SERVER\Share\zeroclaw\.\data",
            ],
            &[r"\\server\share", r"\\server\share\"],
            &[
                r"\\.\GLOBALROOT\Device\HarddiskVolume3\data",
                "//./GLOBALROOT/Device/HarddiskVolume3/data",
                r"\\.\globalroot\device\harddiskvolume3\.\data\",
            ],
            &[r"\\?\C:\x\data", r"\\?\c:\x\\data", r"\\?\C:\x\data\"],
            &[r"\\?\UNC\server\share\x", r"\\?\unc\server\share\x"],
        ];
        for group in groups {
            let canonical = pipe_name(Path::new(group[0]));
            for spelling in &group[1..] {
                assert_eq!(pipe_name(Path::new(spelling)), canonical, "{spelling}");
            }
        }
    }

    #[test]
    fn distinct_windows_directories_get_distinct_pipes() {
        let names = [
            r"C:\a\data",
            r"C:\b\data",
            r"C:data",
            r"\data",
            r"\\data\share",
            "data",
            r"C:\a\..\data",
            r"C:\data",
            r"\\?\C:\a\.\data",
            r"\\?\C:\a\data",
            r"\\?\C:/a/data",
            // A drive's root against that drive's current directory.
            r"C:\",
            "C:",
            r"\",
            // The device namespace against UNC, and verbatim forms against
            // their plain spellings.
            r"\\.\GLOBALROOT\Device\HarddiskVolume3\data",
            r"\\GLOBALROOT\Device\HarddiskVolume3\data",
            r"\\?\GLOBALROOT\Device\HarddiskVolume3\data",
            r"\\?\C:\x",
            r"C:\x",
            r"\\?\UNC\server\share\x",
            r"\\server\share\x",
            r"\\server\share",
            r"\\?\x",
        ]
        .map(|name| (name, pipe_name(Path::new(name))));
        for (i, (left_name, left)) in names.iter().enumerate() {
            for (right_name, right) in &names[i + 1..] {
                assert_ne!(left, right, "{left_name} and {right_name}");
            }
        }
    }

    #[test]
    fn prefixes_are_read_by_kind() {
        // The key each kind of path hashes to. Plain paths keep their
        // canonical spelling; device and verbatim paths carry their kind.
        let key = |path: &str| String::from_utf16(&pipe_key(&units(path))).unwrap();
        assert_eq!(key(r"C:\Users\Alice"), r"c:\users\alice");
        assert_eq!(key("C:"), "c:");
        assert_eq!(key(r"C:\"), r"c:\");
        assert_eq!(key("C:data"), "c:data");
        assert_eq!(key("/home/alice"), r"\home\alice");
        assert_eq!(key("relative/data"), r"relative\data");
        assert_eq!(key("//Server/Share/x"), r"\\server\share\x");
        assert_eq!(key(r"\\server"), r"\server", "no share: not a UNC prefix");
        assert_eq!(
            key("//?/C:/data."),
            r"\\?\c:\data.",
            "UNC with the server ?"
        );
        assert_eq!(key("//./COM1"), "/device/com1");
        assert_eq!(
            key(r"\\.\GLOBALROOT\Device\x"),
            r"/device/globalroot\device\x"
        );
        assert_eq!(key(r"\\?\C:"), "/verbatim-disk/c:");
        assert_eq!(key(r"\\?\C:\a\.\b"), r"/verbatim-disk/c:\a\.\b");
        assert_eq!(
            key(r"\\?\UNC\Server\Share\x"),
            r"/verbatim-unc/server\share\x"
        );
        assert_eq!(key(r"\\?\Volume{0}\x/y"), r"/verbatim/volume{0}\x/y");
    }

    #[test]
    fn a_normalizing_spelling_never_takes_a_verbatim_key() {
        // Only the exact `\\?\` spelling is verbatim. Windows normalizes the
        // others, which trims the trailing dot or space a verbatim path keeps,
        // so the two must not share a pipe even when their text lines up.
        for tail in ["data.", "data ", "data"] {
            let verbatim = format!(r"\\?\C:\{tail}");
            assert!(split_prefix(&units(&verbatim)).prefix.is_verbatim());
            for spelling in [
                format!("//?/C:/{tail}"),
                format!(r"\\?/C:/{tail}"),
                format!(r"/\?\C:\{tail}"),
            ] {
                assert!(
                    !split_prefix(&units(&spelling)).prefix.is_verbatim(),
                    "{spelling:?}"
                );
                assert_ne!(
                    pipe_name(Path::new(&verbatim)),
                    pipe_name(Path::new(&spelling)),
                    "{verbatim:?} and {spelling:?}"
                );
            }
        }
    }

    /// Prefix families for the corpus: a name, the prefix as first written,
    /// and whether components after it are read verbatim.
    const CORPUS_FAMILIES: &[(&str, &str, bool)] = &[
        ("relative", "", false),
        ("root", r"\", false),
        ("c-relative", "C:", false),
        ("c-root", r"C:\", false),
        ("d-root", r"D:\", false),
        ("unc1", r"\\server\share\", false),
        ("unc2", r"\\server\other\", false),
        ("device-global", r"\\.\GLOBALROOT\", false),
        ("device-volume", r"\\.\Volume{0}\", false),
        ("verbatim-c", r"\\?\C:\", true),
        ("verbatim-d", r"\\?\D:\", true),
        ("verbatim-unc1", r"\\?\UNC\server\share\", true),
        ("verbatim-unc2", r"\\?\UNC\server\other\", true),
        ("verbatim-volume", r"\\?\Volume{0}\", true),
        ("verbatim-global", r"\\?\GLOBALROOT\", true),
    ];

    /// Other spellings of a family's prefix, and families only these reach.
    const CORPUS_ALTERNATE_PREFIXES: &[(&str, &str, bool)] = &[
        ("root", "/", false),
        ("c-relative", "c:", false),
        ("c-root", "C:/", false),
        ("c-root", r"c:\\", false),
        ("unc1", "//server/share/", false),
        ("unc1", r"\\SERVER\Share\", false),
        ("unc1", r"\/server/share\", false),
        ("device-global", "//./GLOBALROOT/", false),
        ("device-global", r"\/.\globalroot\", false),
        ("verbatim-c", r"\\?\c:\", true),
        ("verbatim-unc1", r"\\?\unc\server\share\", true),
        // Not verbatim: UNC with the server `?`, normalized like any UNC path.
        ("unc-question-c", "//?/C:/", false),
        ("unc-question-c", r"\\?/C:/", false),
        ("unc-question-c", r"/\?\C:\", false),
    ];

    const CORPUS_TAILS: &[&str] = &[
        "",
        "data",
        r"data\child",
        "data/child",
        r"data\\child",
        r"data\.\child",
        ".",
        "..",
        r"data\..\child",
        "data.",
        "data ",
    ];

    /// The directory a corpus path names: its family and its components as
    /// that family reads them, ASCII-folded.
    fn corpus_identity(
        family: &'static str,
        tail: &str,
        verbatim: bool,
    ) -> (&'static str, Vec<String>) {
        let parts = tail
            .split(|c| c == '\\' || (!verbatim && c == '/'))
            .filter(|part| !part.is_empty() && (verbatim || *part != "."))
            .map(str::to_ascii_lowercase)
            .collect();
        (family, parts)
    }

    type Identity = (&'static str, Vec<String>);

    /// Every corpus path seen so far, by key, by pipe name and by the
    /// directory it names.
    #[derive(Default)]
    struct Corpus {
        by_key: std::collections::HashMap<Vec<u16>, Identity>,
        by_name: std::collections::HashMap<String, Identity>,
        by_identity: std::collections::HashMap<Identity, Vec<u16>>,
    }

    impl Corpus {
        fn add(&mut self, families: &[(&'static str, &str, bool)]) -> usize {
            let mut paths = 0;
            for &(family, prefix, verbatim) in families {
                for tail in CORPUS_TAILS {
                    let path = format!("{prefix}{tail}");
                    let identity = corpus_identity(family, tail, verbatim);
                    let key = pipe_key(&units(&path));
                    let name = pipe_name(Path::new(&path));
                    if let Some(other) = self.by_key.insert(key.clone(), identity.clone()) {
                        assert_eq!(other, identity, "two directories share the key of {path:?}");
                    }
                    if let Some(other) = self.by_name.insert(name, identity.clone()) {
                        assert_eq!(
                            other, identity,
                            "two directories share the pipe of {path:?}"
                        );
                    }
                    if let Some(other) = self.by_identity.insert(identity, key.clone()) {
                        assert_eq!(
                            other, key,
                            "{path:?} is keyed apart from its other spellings"
                        );
                    }
                    paths += 1;
                }
            }
            paths
        }
    }

    #[test]
    fn the_prefix_corpus_keys_each_directory_once() {
        // Different directories never share a key or a pipe, and every
        // spelling of one directory gets the same key.
        let mut corpus = Corpus::default();
        let paths = corpus.add(CORPUS_FAMILIES);
        assert_eq!((paths, corpus.by_identity.len()), (165, 123));
        corpus.add(CORPUS_ALTERNATE_PREFIXES);
        assert_eq!(corpus.by_key.len(), corpus.by_identity.len());
        assert_eq!(corpus.by_name.len(), corpus.by_identity.len());
    }

    fn units(path: &str) -> Vec<u16> {
        path.encode_utf16().collect()
    }

    #[test]
    fn only_ascii_letters_are_case_folded() {
        // The supported identity: ASCII case folds, other letters do not.
        // Windows folds those with a per-volume table, and a directory can
        // be case-sensitive, so no fixed rule matches it for every volume.
        assert_ne!(
            pipe_name(Path::new(r"C:\Users\Émile\.zeroclaw\data")),
            pipe_name(Path::new(r"C:\Users\émile\.zeroclaw\data"))
        );
        // The ASCII letters around a non-ASCII one still fold.
        assert_eq!(
            pipe_name(Path::new(r"C:\Users\ÉMILE\.zeroclaw\data")),
            pipe_name(Path::new(r"c:\users\Émile\.zeroclaw\data"))
        );
    }

    #[test]
    fn every_utf16_unit_reaches_the_hash() {
        // Two unpaired surrogates: a lossy conversion would turn both into
        // U+FFFD and give them one pipe.
        let drive = [u16::from(b'C'), u16::from(b':'), u16::from(b'\\')];
        let first = [drive.as_slice(), &[0xd800]].concat();
        let second = [drive.as_slice(), &[0xd801]].concat();
        assert_ne!(pipe_name_from_units(&first), pipe_name_from_units(&second));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_are_read_without_loss() {
        use std::os::windows::ffi::OsStringExt;
        let path = |unit| {
            PathBuf::from(std::ffi::OsString::from_wide(&[
                u16::from(b'C'),
                u16::from(b':'),
                u16::from(b'\\'),
                unit,
            ]))
        };
        assert_ne!(pipe_name(&path(0xd800)), pipe_name(&path(0xd801)));
    }

    #[test]
    fn stable_and_legacy_pipe_names_never_coincide() {
        // The stable name carries `daemon-`, which is not hex, so it can never
        // equal a legacy `zeroclaw-<hex>` name a client also tries.
        for dir in AGREEMENT_DATA_DIRS {
            let dir = Path::new(dir);
            assert_ne!(pipe_name(dir), legacy_pipe_name(dir));
            assert!(legacy_pipe_name(dir).starts_with(r"\\.\pipe\zeroclaw-"));
        }
    }

    #[test]
    fn a_non_blank_override_wins_and_a_blank_one_is_ignored() {
        let dir = Path::new("/home/alice/.zeroclaw/data");
        assert_eq!(
            resolve_endpoint_with(Some("/tmp/zc.sock"), dir),
            PathBuf::from("/tmp/zc.sock")
        );
        assert_eq!(
            resolve_endpoint_with(Some("  /tmp/zc.sock \n"), dir),
            PathBuf::from("/tmp/zc.sock")
        );
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(resolve_endpoint_with(blank, dir), default_endpoint(dir));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_endpoint_stays_daemon_sock_under_the_data_dir() {
        assert_eq!(
            default_endpoint(Path::new("/home/alice/.zeroclaw/data")),
            PathBuf::from("/home/alice/.zeroclaw/data/daemon.sock")
        );
    }

    #[test]
    fn clients_fall_back_to_the_legacy_pipe_only_on_windows_without_an_override() {
        let dir = Path::new(r"C:\Users\Alice\.zeroclaw\data");
        let defaults = client_endpoints_with(None, dir);
        assert_eq!(defaults.primary, default_endpoint(dir));
        assert_eq!(defaults.legacy.is_some(), cfg!(windows));
        assert_eq!(defaults.iter().count(), 1 + usize::from(cfg!(windows)));

        // The Windows rule, on every platform.
        for blank in [None, Some(""), Some(" \t")] {
            let endpoints = endpoints_with_fallback(blank, dir, true);
            assert_eq!(endpoints.primary, default_endpoint(dir), "{blank:?}");
            assert_eq!(
                endpoints.legacy,
                Some(PathBuf::from(legacy_pipe_name(dir))),
                "{blank:?}"
            );
        }
        let overridden = endpoints_with_fallback(Some(r"\\.\pipe\mine"), dir, true);
        assert_eq!(overridden.primary, PathBuf::from(r"\\.\pipe\mine"));
        assert_eq!(
            overridden.legacy, None,
            "an explicit endpoint has no fallback"
        );
    }

    #[test]
    fn an_explicit_override_naming_the_default_endpoint_has_no_fallback() {
        // Naming the default endpoint explicitly is still an explicit choice:
        // the client dials that endpoint and nothing else.
        let dir = Path::new(r"C:\Users\Alice\.zeroclaw\data");
        let default = default_endpoint(dir);
        let default = default.to_str().expect("the default endpoint is Unicode");
        for explicit in [default.to_string(), format!("  {default}\n")] {
            for endpoints in [
                endpoints_with_fallback(Some(&explicit), dir, true),
                client_endpoints_with(Some(&explicit), dir),
            ] {
                assert_eq!(endpoints.primary, PathBuf::from(default), "{explicit:?}");
                assert_eq!(endpoints.legacy, None, "{explicit:?}");
                assert_eq!(endpoints.iter().count(), 1);
            }
        }
    }
}
