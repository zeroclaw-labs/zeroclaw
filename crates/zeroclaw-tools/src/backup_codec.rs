//! Per-file payload encoding for the backup tool.
//!
//! A backup stays a directory tree so every file operation keeps the tool's
//! directory-handle boundary. Each stored file is optionally gzip-compressed
//! and then optionally encrypted. Encryption is ChaCha20-Poly1305 in the
//! STREAM construction (big-endian 32-bit counter plus a last-chunk flag in
//! the nonce), so truncation, reordering, and chunk splicing all fail
//! authentication. Each file gets its own key, derived from the backup key and
//! a random per-file salt, and its backup-relative path is bound as associated
//! data so files cannot be swapped between paths.
//!
//! File names and the directory layout are not encrypted.

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io::{self, Read};
use zeroize::Zeroizing;

/// Name of the format descriptor at the root of an encoded backup. Plain
/// backups have none, which is also how backups made before encoding existed
/// are recognised.
pub(crate) const FORMAT_FILE: &str = "backup-format.json";

/// Domain for deriving a backup key from the install secret-store key.
pub(crate) const BACKUP_KEY_DOMAIN: &[u8] = b"zeroclaw.backup.archive-key.v1\0";

const FORMAT_VERSION: u32 = 1;
const ENCRYPTION_ALGORITHM: &str = "chacha20poly1305-stream-be32";
const FILE_KEY_DOMAIN: &[u8] = b"zeroclaw.backup.file-key.v1\0";
const AAD_DOMAIN: &[u8] = b"zeroclaw.backup.file.v1\0";
const MAGIC: &[u8; 8] = b"ZCBKENC1";
const SALT_LEN: usize = 32;
const CHUNK_LEN: usize = 64 * 1024;
const TAG_LEN: usize = 16;

/// Contents of [`FORMAT_FILE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackupFormat {
    pub version: u32,
    pub compressed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EncryptionHeader {
    pub algorithm: String,
    /// Hex salt that selects this backup's key from the install key.
    pub key_salt: String,
}

impl BackupFormat {
    pub(crate) fn new(compressed: bool, key_salt: Option<&[u8; SALT_LEN]>) -> Self {
        Self {
            version: FORMAT_VERSION,
            compressed,
            encryption: key_salt.map(|salt| EncryptionHeader {
                algorithm: ENCRYPTION_ALGORITHM.to_string(),
                key_salt: hex::encode(salt),
            }),
        }
    }

    /// Reject descriptors this build cannot decode rather than guessing.
    pub(crate) fn validate(&self) -> Result<(), PayloadRejected> {
        if self.version != FORMAT_VERSION {
            return Err(PayloadRejected);
        }
        if let Some(encryption) = &self.encryption {
            if encryption.algorithm != ENCRYPTION_ALGORITHM {
                return Err(PayloadRejected);
            }
            self.key_salt()?;
        }
        Ok(())
    }

    pub(crate) fn key_salt(&self) -> Result<Option<Vec<u8>>, PayloadRejected> {
        let Some(encryption) = &self.encryption else {
            return Ok(None);
        };
        let salt = hex::decode(&encryption.key_salt).map_err(|_| PayloadRejected)?;
        if salt.len() != SALT_LEN {
            return Err(PayloadRejected);
        }
        Ok(Some(salt))
    }
}

/// A new random salt for a backup key.
pub(crate) fn random_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// A stored file failed authentication, decompression, or format checks.
#[derive(Debug)]
pub(crate) struct PayloadRejected;

impl std::fmt::Display for PayloadRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("backup payload failed authentication or decoding")
    }
}

impl std::error::Error for PayloadRejected {}

fn rejected() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, PayloadRejected)
}

/// True when an I/O error came from payload authentication or decoding.
pub(crate) fn is_payload_rejection(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<PayloadRejected>())
}

/// How stored files are encoded. The identity codec reads and writes plain
/// copies, matching backups made before encoding existed.
#[derive(Clone, Default)]
pub(crate) struct PayloadCodec {
    compressed: bool,
    key: Option<Zeroizing<[u8; 32]>>,
}

impl PayloadCodec {
    pub(crate) fn new(compressed: bool, key: Option<Zeroizing<[u8; 32]>>) -> Self {
        Self { compressed, key }
    }

    pub(crate) fn is_identity(&self) -> bool {
        !self.compressed && self.key.is_none()
    }

    /// Wrap a source file so reads yield its stored form.
    pub(crate) fn encoder<'a>(
        &self,
        relative: &str,
        input: Box<dyn Read + 'a>,
    ) -> Box<dyn Read + 'a> {
        let input: Box<dyn Read + 'a> = if self.compressed {
            Box::new(flate2::read::GzEncoder::new(
                input,
                flate2::Compression::default(),
            ))
        } else {
            input
        };
        match &self.key {
            Some(key) => Box::new(EncryptReader::new(key, relative, input)),
            None => input,
        }
    }

    /// Wrap a stored file so reads yield the original bytes. Reads fail with
    /// [`io::ErrorKind::InvalidData`] when authentication or decoding fails.
    pub(crate) fn decoder<'a>(
        &self,
        relative: &str,
        input: Box<dyn Read + 'a>,
    ) -> Box<dyn Read + 'a> {
        let input: Box<dyn Read + 'a> = match &self.key {
            Some(key) => Box::new(DecryptReader::new(key, relative, input)),
            None => input,
        };
        if self.compressed {
            Box::new(GzipCheck(flate2::read::GzDecoder::new(input)))
        } else {
            input
        }
    }
}

/// Reports every gzip failure as a payload rejection, not a generic I/O error.
struct GzipCheck<R>(R);

impl<R: Read> Read for GzipCheck<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf).map_err(|error| match error.kind() {
            io::ErrorKind::InvalidData
            | io::ErrorKind::InvalidInput
            | io::ErrorKind::UnexpectedEof => rejected(),
            _ => error,
        })
    }
}

fn file_cipher(backup_key: &[u8; 32], file_salt: &[u8]) -> ChaCha20Poly1305 {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(backup_key).expect("HMAC accepts any key length");
    mac.update(FILE_KEY_DOMAIN);
    mac.update(file_salt);
    let file_key = Zeroizing::new(<[u8; 32]>::from(mac.finalize().into_bytes()));
    ChaCha20Poly1305::new(Key::from_slice(file_key.as_slice()))
}

fn associated_data(relative: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + relative.len());
    aad.extend_from_slice(AAD_DOMAIN);
    aad.extend_from_slice(relative.as_bytes());
    aad
}

fn chunk_nonce(counter: u32, last: bool) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[7..11].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    *Nonce::from_slice(&nonce)
}

/// Read into `buf` until it holds `len` bytes or the input ends.
fn fill_to(input: &mut dyn Read, buf: &mut Vec<u8>, len: usize) -> io::Result<()> {
    let mut scratch = [0u8; 8 * 1024];
    while buf.len() < len {
        let want = (len - buf.len()).min(scratch.len());
        match input.read(&mut scratch[..want]) {
            Ok(0) => break,
            Ok(count) => buf.extend_from_slice(&scratch[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Serves `out` and refills it one chunk at a time.
struct ChunkBuffer {
    out: Vec<u8>,
    pos: usize,
}

impl ChunkBuffer {
    fn serve(&mut self, buf: &mut [u8]) -> usize {
        let count = (self.out.len() - self.pos).min(buf.len());
        buf[..count].copy_from_slice(&self.out[self.pos..self.pos + count]);
        self.pos += count;
        count
    }

    fn is_drained(&self) -> bool {
        self.pos >= self.out.len()
    }

    fn replace(&mut self, bytes: Vec<u8>) {
        self.out = bytes;
        self.pos = 0;
    }
}

struct EncryptReader<'a> {
    input: Box<dyn Read + 'a>,
    cipher: ChaCha20Poly1305,
    aad: Vec<u8>,
    pending: Vec<u8>,
    buffer: ChunkBuffer,
    counter: u32,
    done: bool,
}

impl<'a> EncryptReader<'a> {
    fn new(backup_key: &[u8; 32], relative: &str, input: Box<dyn Read + 'a>) -> Self {
        let mut file_salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut file_salt);
        let mut header = Vec::with_capacity(MAGIC.len() + SALT_LEN);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&file_salt);
        Self {
            input,
            cipher: file_cipher(backup_key, &file_salt),
            aad: associated_data(relative),
            pending: Vec::new(),
            buffer: ChunkBuffer {
                out: header,
                pos: 0,
            },
            counter: 0,
            done: false,
        }
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        // One byte of read-ahead tells a full chunk apart from the last one.
        fill_to(&mut self.input, &mut self.pending, CHUNK_LEN + 1)?;
        let last = self.pending.len() <= CHUNK_LEN;
        let take = self.pending.len().min(CHUNK_LEN);
        let chunk: Vec<u8> = self.pending.drain(..take).collect();
        let ciphertext = self
            .cipher
            .encrypt(
                &chunk_nonce(self.counter, last),
                Payload {
                    msg: &chunk,
                    aad: &self.aad,
                },
            )
            .map_err(|_| io::Error::other("backup chunk encryption failed"))?;
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| io::Error::other("backup file exceeds the chunk counter"))?;
        self.done = last;
        self.buffer.replace(ciphertext);
        Ok(())
    }
}

impl Read for EncryptReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.buffer.is_drained() || buf.is_empty() {
                return Ok(self.buffer.serve(buf));
            }
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
    }
}

struct DecryptReader<'a> {
    input: Box<dyn Read + 'a>,
    backup_key: Zeroizing<[u8; 32]>,
    cipher: Option<ChaCha20Poly1305>,
    aad: Vec<u8>,
    pending: Vec<u8>,
    buffer: ChunkBuffer,
    counter: u32,
    done: bool,
}

impl<'a> DecryptReader<'a> {
    fn new(backup_key: &[u8; 32], relative: &str, input: Box<dyn Read + 'a>) -> Self {
        Self {
            input,
            backup_key: Zeroizing::new(*backup_key),
            cipher: None,
            aad: associated_data(relative),
            pending: Vec::new(),
            buffer: ChunkBuffer {
                out: Vec::new(),
                pos: 0,
            },
            counter: 0,
            done: false,
        }
    }

    fn read_header(&mut self) -> io::Result<ChaCha20Poly1305> {
        let mut header = Vec::with_capacity(MAGIC.len() + SALT_LEN);
        fill_to(&mut self.input, &mut header, MAGIC.len() + SALT_LEN)?;
        if header.len() != MAGIC.len() + SALT_LEN || &header[..MAGIC.len()] != MAGIC {
            return Err(rejected());
        }
        Ok(file_cipher(&self.backup_key, &header[MAGIC.len()..]))
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        if self.cipher.is_none() {
            self.cipher = Some(self.read_header()?);
        }
        let sealed_len = CHUNK_LEN + TAG_LEN;
        fill_to(&mut self.input, &mut self.pending, sealed_len + 1)?;
        let last = self.pending.len() <= sealed_len;
        let take = self.pending.len().min(sealed_len);
        let chunk: Vec<u8> = self.pending.drain(..take).collect();
        let cipher = self.cipher.as_ref().expect("header read above");
        let plaintext = cipher
            .decrypt(
                &chunk_nonce(self.counter, last),
                Payload {
                    msg: &chunk,
                    aad: &self.aad,
                },
            )
            .map_err(|_| rejected())?;
        self.counter = self.counter.checked_add(1).ok_or_else(rejected)?;
        self.done = last;
        self.buffer.replace(plaintext);
        Ok(())
    }
}

impl Read for DecryptReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.buffer.is_drained() || buf.is_empty() {
                return Ok(self.buffer.serve(buf));
            }
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> Zeroizing<[u8; 32]> {
        Zeroizing::new([byte; 32])
    }

    fn encode(codec: &PayloadCodec, relative: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        codec
            .encoder(relative, Box::new(data))
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn decode(codec: &PayloadCodec, relative: &str, data: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        codec
            .decoder(relative, Box::new(data))
            .read_to_end(&mut out)?;
        Ok(out)
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn every_codec_round_trips_across_chunk_boundaries() {
        let codecs = [
            PayloadCodec::new(false, None),
            PayloadCodec::new(true, None),
            PayloadCodec::new(false, Some(key(7))),
            PayloadCodec::new(true, Some(key(7))),
        ];
        for len in [
            0,
            1,
            CHUNK_LEN - 1,
            CHUNK_LEN,
            CHUNK_LEN + 1,
            3 * CHUNK_LEN + 17,
        ] {
            let data = sample(len);
            for codec in &codecs {
                let stored = encode(codec, "memory/brain.db", &data);
                assert_eq!(decode(codec, "memory/brain.db", &stored).unwrap(), data);
            }
        }
    }

    #[test]
    fn encrypted_payload_does_not_contain_the_plaintext() {
        let codec = PayloadCodec::new(false, Some(key(1)));
        let secret = b"api_key = sk-live-do-not-leak".repeat(40);
        let stored = encode(&codec, "config/config.toml", &secret);
        assert!(
            !stored
                .windows(b"sk-live-do-not-leak".len())
                .any(|window| window == b"sk-live-do-not-leak")
        );
    }

    #[test]
    fn compression_shrinks_repetitive_data() {
        let codec = PayloadCodec::new(true, None);
        let data = vec![b'a'; 256 * 1024];
        assert!(encode(&codec, "memory/a", &data).len() < data.len() / 10);
    }

    #[test]
    fn decryption_rejects_a_wrong_key_a_moved_file_and_tampering() {
        let codec = PayloadCodec::new(true, Some(key(3)));
        let data = sample(2 * CHUNK_LEN + 5);
        let stored = encode(&codec, "memory/a", &data);

        let rejects = |result: io::Result<Vec<u8>>| {
            let error = result.expect_err("tampered payload must not decode");
            assert!(is_payload_rejection(&error), "{error:?}");
        };
        rejects(decode(
            &PayloadCodec::new(true, Some(key(4))),
            "memory/a",
            &stored,
        ));
        rejects(decode(&codec, "memory/b", &stored));

        let mut flipped = stored.clone();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 1;
        rejects(decode(&codec, "memory/a", &flipped));
    }

    #[test]
    fn decryption_rejects_truncation_at_and_between_chunk_boundaries() {
        let codec = PayloadCodec::new(false, Some(key(5)));
        let data = sample(2 * CHUNK_LEN + 5);
        let stored = encode(&codec, "memory/a", &data);
        let header = MAGIC.len() + SALT_LEN;
        for cut in [
            0,
            header - 1,
            header,
            header + CHUNK_LEN + TAG_LEN,
            header + 2 * (CHUNK_LEN + TAG_LEN),
            stored.len() - 1,
        ] {
            let error = decode(&codec, "memory/a", &stored[..cut])
                .expect_err("a truncated payload must not decode");
            assert!(is_payload_rejection(&error), "cut at {cut}: {error:?}");
        }
        let mut extended = stored.clone();
        extended.push(0);
        assert!(decode(&codec, "memory/a", &extended).is_err());
    }

    #[test]
    fn corrupt_compressed_payload_is_a_payload_rejection() {
        let codec = PayloadCodec::new(true, None);
        let mut stored = encode(&codec, "memory/a", &sample(10_000));
        let last = stored.len() - 5;
        stored[last] ^= 0xff;
        let error = decode(&codec, "memory/a", &stored).expect_err("bad CRC must fail");
        assert!(is_payload_rejection(&error), "{error:?}");
    }

    #[test]
    fn format_descriptor_validates_version_algorithm_and_salt() {
        let salt = random_salt();
        let format = BackupFormat::new(true, Some(&salt));
        format.validate().unwrap();
        assert_eq!(format.key_salt().unwrap().unwrap(), salt);

        let mut future = format.clone();
        future.version = 2;
        assert!(future.validate().is_err());

        let mut unknown = format.clone();
        unknown.encryption.as_mut().unwrap().algorithm = "rot13".into();
        assert!(unknown.validate().is_err());

        let mut short = format;
        short.encryption.as_mut().unwrap().key_salt = "abcd".into();
        assert!(short.validate().is_err());
    }
}
