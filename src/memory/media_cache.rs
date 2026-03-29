//! Media cache — save pruned images to disk instead of permanently losing them.
//!
//! When the history pruner drops messages containing image markers, images are
//! extracted and saved to `{workspace}/.zeroclaw/media_cache/{hash}.{ext}`.
//! The original marker is replaced with `[CACHED_IMAGE:{hash}]` so that the
//! image can be restored when preparing messages for the provider.
//!
//! The cache is keyed by SHA-256 hash of the raw image bytes, uses LRU eviction
//! when the total cache size exceeds a configurable limit (default 100 MB), and
//! is disabled by default.

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Marker prefix used for cached image references in message content.
pub const CACHED_IMAGE_MARKER_PREFIX: &str = "[CACHED_IMAGE:";

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMetadata {
    /// SHA-256 hex hash of the raw image bytes.
    pub hash: String,
    /// Original URL or file path (if known).
    pub original_source: Option<String>,
    /// MIME type (e.g. "image/png").
    pub mime_type: String,
    /// Raw image size in bytes.
    pub size_bytes: u64,
    /// File extension derived from MIME type.
    pub extension: String,
    /// Last time this entry was accessed (RFC 3339).
    pub last_accessed: String,
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

pub struct MediaCache {
    cache_dir: PathBuf,
    max_size_bytes: u64,
    index: Mutex<HashMap<String, MediaMetadata>>,
}

impl MediaCache {
    /// Open (or create) the media cache directory under the given workspace.
    pub fn new(workspace_dir: &Path, max_size_mb: u64) -> Result<Self> {
        let cache_dir = workspace_dir.join(".zeroclaw").join("media_cache");
        std::fs::create_dir_all(&cache_dir).with_context(|| {
            format!("failed to create media cache dir: {}", cache_dir.display())
        })?;

        let max_size_bytes = max_size_mb.saturating_mul(1024 * 1024);

        let mut index = HashMap::new();
        // Rebuild index from metadata files on disk.
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(data) = std::fs::read_to_string(&path) {
                        if let Ok(meta) = serde_json::from_str::<MediaMetadata>(&data) {
                            index.insert(meta.hash.clone(), meta);
                        }
                    }
                }
            }
        }

        Ok(Self {
            cache_dir,
            max_size_bytes,
            index: Mutex::new(index),
        })
    }

    /// Store raw image bytes in the cache. Returns the SHA-256 hex hash.
    pub fn put(
        &self,
        bytes: &[u8],
        mime_type: &str,
        original_source: Option<&str>,
    ) -> Result<String> {
        let hash = sha256_hex(bytes);
        let extension = extension_from_mime(mime_type);

        // Write the image file (idempotent — same hash = same content).
        let image_path = self.cache_dir.join(format!("{hash}.{extension}"));
        std::fs::write(&image_path, bytes)
            .with_context(|| format!("failed to write cached image: {}", image_path.display()))?;

        let now = chrono::Local::now().to_rfc3339();
        let meta = MediaMetadata {
            hash: hash.clone(),
            original_source: original_source.map(ToString::to_string),
            mime_type: mime_type.to_string(),
            size_bytes: bytes.len() as u64,
            extension: extension.clone(),
            last_accessed: now,
        };

        // Write metadata sidecar.
        let meta_path = self.cache_dir.join(format!("{hash}.json"));
        let meta_json = serde_json::to_string_pretty(&meta)?;
        std::fs::write(&meta_path, meta_json)?;

        self.index.lock().insert(hash.clone(), meta);

        // Evict if over budget.
        self.evict_if_needed()?;

        Ok(hash)
    }

    /// Retrieve the image as a `data:{mime};base64,...` URI, or `None` if missing.
    pub fn get_data_uri(&self, hash: &str) -> Option<String> {
        let mut index = self.index.lock();
        let meta = index.get_mut(hash)?;

        let image_path = self.cache_dir.join(format!("{}.{}", hash, meta.extension));
        let bytes = std::fs::read(&image_path).ok()?;

        // Touch last_accessed.
        meta.last_accessed = chrono::Local::now().to_rfc3339();
        let meta_path = self.cache_dir.join(format!("{hash}.json"));
        if let Ok(json) = serde_json::to_string_pretty(meta) {
            let _ = std::fs::write(&meta_path, json);
        }

        let mime = &meta.mime_type;
        Some(format!("data:{mime};base64,{}", STANDARD.encode(&bytes)))
    }

    /// Check whether a hash exists in the cache.
    pub fn contains(&self, hash: &str) -> bool {
        self.index.lock().contains_key(hash)
    }

    /// Total size of cached images in bytes.
    pub fn total_size_bytes(&self) -> u64 {
        self.index.lock().values().map(|m| m.size_bytes).sum()
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.index.lock().len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.index.lock().is_empty()
    }

    /// Evict least-recently-accessed entries until total size is within budget.
    fn evict_if_needed(&self) -> Result<()> {
        let mut index = self.index.lock();
        while total_size(&index) > self.max_size_bytes && !index.is_empty() {
            // Find the least-recently-accessed entry.
            let lru_hash = index
                .iter()
                .min_by_key(|(_, m)| &m.last_accessed)
                .map(|(k, _)| k.clone());

            if let Some(hash) = lru_hash {
                if let Some(meta) = index.remove(&hash) {
                    let image_path = self.cache_dir.join(format!("{}.{}", hash, meta.extension));
                    let meta_path = self.cache_dir.join(format!("{hash}.json"));
                    let _ = std::fs::remove_file(image_path);
                    let _ = std::fs::remove_file(meta_path);
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Remove all cached files and clear the index.
    pub fn clear(&self) -> Result<usize> {
        let mut index = self.index.lock();
        let count = index.len();
        for (hash, meta) in index.drain() {
            let image_path = self.cache_dir.join(format!("{}.{}", hash, meta.extension));
            let meta_path = self.cache_dir.join(format!("{hash}.json"));
            let _ = std::fs::remove_file(image_path);
            let _ = std::fs::remove_file(meta_path);
        }
        Ok(count)
    }
}

fn total_size(index: &HashMap<String, MediaMetadata>) -> u64 {
    index.values().map(|m| m.size_bytes).sum()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    format!("{:064x}", hash)
}

fn extension_from_mime(mime: &str) -> String {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "bin",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Marker helpers — extract / replace cached-image markers in message content
// ---------------------------------------------------------------------------

/// Parse `[CACHED_IMAGE:{hash}]` markers out of message content.
/// Returns the cleaned text and a list of hashes found.
pub fn parse_cached_image_markers(content: &str) -> (String, Vec<String>) {
    let mut hashes = Vec::new();
    let mut cleaned = String::with_capacity(content.len());
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(CACHED_IMAGE_MARKER_PREFIX) {
        let start = cursor + rel_start;
        cleaned.push_str(&content[cursor..start]);

        let marker_start = start + CACHED_IMAGE_MARKER_PREFIX.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            cleaned.push_str(&content[start..]);
            cursor = content.len();
            break;
        };

        let end = marker_start + rel_end;
        let candidate = content[marker_start..end].trim();

        if candidate.is_empty() {
            cleaned.push_str(&content[start..=end]);
        } else {
            hashes.push(candidate.to_string());
        }

        cursor = end + 1;
    }

    if cursor < content.len() {
        cleaned.push_str(&content[cursor..]);
    }

    (cleaned.trim().to_string(), hashes)
}

/// Extract image bytes from a `data:{mime};base64,...` URI.
/// Returns `(mime_type, raw_bytes)` or `None` if the URI is not a valid data URI.
pub fn decode_data_uri(data_uri: &str) -> Option<(String, Vec<u8>)> {
    let comma_idx = data_uri.find(',')?;
    let header = &data_uri[..comma_idx];
    let payload = data_uri[comma_idx + 1..].trim();

    if !header.contains(";base64") {
        return None;
    }

    let mime = header
        .trim_start_matches("data:")
        .split(';')
        .next()?
        .trim()
        .to_ascii_lowercase();

    let bytes = STANDARD.decode(payload).ok()?;
    Some((mime, bytes))
}

/// Scan a message for `[IMAGE:data:...]` markers and cache each image,
/// replacing the original marker with `[CACHED_IMAGE:{hash}]`.
///
/// Non-data-URI markers (file paths, URLs) are left as-is since they don't
/// carry inline image bytes that would be lost on pruning.
pub fn cache_images_in_message(content: &str, cache: &MediaCache) -> String {
    let mut result = String::with_capacity(content.len());
    let prefix = "[IMAGE:";
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(prefix) {
        let start = cursor + rel_start;
        result.push_str(&content[cursor..start]);

        let marker_start = start + prefix.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            result.push_str(&content[start..]);
            cursor = content.len();
            break;
        };

        let end = marker_start + rel_end;
        let image_ref = content[marker_start..end].trim();

        if image_ref.starts_with("data:") {
            if let Some((mime, bytes)) = decode_data_uri(image_ref) {
                if let Ok(hash) = cache.put(&bytes, &mime, None) {
                    use std::fmt::Write;
                    let _ = write!(result, "[CACHED_IMAGE:{hash}]");
                    cursor = end + 1;
                    continue;
                }
            }
        }

        // Not a data URI or cache failed — preserve the original marker.
        result.push_str(&content[start..=end]);
        cursor = end + 1;
    }

    if cursor < content.len() {
        result.push_str(&content[cursor..]);
    }

    result
}

/// Restore `[CACHED_IMAGE:{hash}]` markers back to `[IMAGE:data:...]` markers
/// by loading from the cache.
pub fn restore_cached_images(content: &str, cache: &MediaCache) -> String {
    let mut result = String::with_capacity(content.len());
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(CACHED_IMAGE_MARKER_PREFIX) {
        let start = cursor + rel_start;
        result.push_str(&content[cursor..start]);

        let marker_start = start + CACHED_IMAGE_MARKER_PREFIX.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            result.push_str(&content[start..]);
            cursor = content.len();
            break;
        };

        let end = marker_start + rel_end;
        let hash = content[marker_start..end].trim();

        if let Some(data_uri) = cache.get_data_uri(hash) {
            use std::fmt::Write;
            let _ = write!(result, "[IMAGE:{data_uri}]");
        } else {
            // Cache miss — preserve marker so it's visible rather than silently lost.
            result.push_str(&content[start..=end]);
        }

        cursor = end + 1;
    }

    if cursor < content.len() {
        result.push_str(&content[cursor..]);
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_cache(max_mb: u64) -> (TempDir, MediaCache) {
        let tmp = TempDir::new().unwrap();
        let cache = MediaCache::new(tmp.path(), max_mb).unwrap();
        (tmp, cache)
    }

    // Minimal PNG header for testing.
    fn fake_png_bytes() -> Vec<u8> {
        vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    }

    #[test]
    fn put_and_get_round_trips() {
        let (_tmp, cache) = temp_cache(100);
        let bytes = fake_png_bytes();
        let hash = cache
            .put(&bytes, "image/png", Some("/tmp/photo.png"))
            .unwrap();

        assert_eq!(hash.len(), 64);
        assert!(cache.contains(&hash));

        let data_uri = cache.get_data_uri(&hash).expect("should find cached image");
        assert!(data_uri.starts_with("data:image/png;base64,"));

        // Decode round-trip.
        let (mime, decoded) = decode_data_uri(&data_uri).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn miss_returns_none() {
        let (_tmp, cache) = temp_cache(100);
        assert!(cache.get_data_uri("nonexistent_hash").is_none());
        assert!(!cache.contains("nonexistent_hash"));
    }

    #[test]
    fn deterministic_hash() {
        let bytes = fake_png_bytes();
        let h1 = sha256_hex(&bytes);
        let h2 = sha256_hex(&bytes);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn different_content_different_hash() {
        let h1 = sha256_hex(&[1, 2, 3]);
        let h2 = sha256_hex(&[4, 5, 6]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn lru_eviction_respects_max_size() {
        // Max size = 1 byte — everything should be evicted immediately after the second put.
        let tmp = TempDir::new().unwrap();
        let cache = MediaCache::new(tmp.path(), 0).unwrap();

        // With max_size_bytes = 0, every entry exceeds budget.
        let hash = cache.put(&[1, 2, 3], "image/png", None).unwrap();
        // The entry was inserted, then eviction ran and removed it.
        assert!(
            !cache.contains(&hash),
            "entry should be evicted when max_size is 0"
        );
    }

    #[test]
    fn clear_removes_all() {
        let (_tmp, cache) = temp_cache(100);
        cache.put(&[1], "image/png", None).unwrap();
        cache.put(&[2], "image/jpeg", None).unwrap();
        assert_eq!(cache.len(), 2);

        let cleared = cache.clear().unwrap();
        assert_eq!(cleared, 2);
        assert!(cache.is_empty());
    }

    #[test]
    fn extension_from_mime_coverage() {
        assert_eq!(extension_from_mime("image/png"), "png");
        assert_eq!(extension_from_mime("image/jpeg"), "jpg");
        assert_eq!(extension_from_mime("image/webp"), "webp");
        assert_eq!(extension_from_mime("image/gif"), "gif");
        assert_eq!(extension_from_mime("image/bmp"), "bmp");
        assert_eq!(extension_from_mime("application/octet-stream"), "bin");
    }

    #[test]
    fn parse_cached_image_markers_extracts_hashes() {
        let input = "Look at [CACHED_IMAGE:abc123] and [CACHED_IMAGE:def456]";
        let (cleaned, hashes) = parse_cached_image_markers(input);
        assert_eq!(cleaned, "Look at  and");
        assert_eq!(hashes, vec!["abc123", "def456"]);
    }

    #[test]
    fn parse_cached_image_markers_preserves_empty() {
        let input = "nothing [CACHED_IMAGE:] here";
        let (cleaned, hashes) = parse_cached_image_markers(input);
        assert_eq!(cleaned, "nothing [CACHED_IMAGE:] here");
        assert!(hashes.is_empty());
    }

    #[test]
    fn cache_images_in_message_replaces_data_uris() {
        let (_tmp, cache) = temp_cache(100);
        let bytes = fake_png_bytes();
        let b64 = STANDARD.encode(&bytes);
        let input = format!("See this [IMAGE:data:image/png;base64,{b64}] ok");

        let result = cache_images_in_message(&input, &cache);

        assert!(
            !result.contains("[IMAGE:data:"),
            "data URI should be replaced"
        );
        assert!(
            result.contains("[CACHED_IMAGE:"),
            "should have cached marker"
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_images_preserves_non_data_uri_markers() {
        let (_tmp, cache) = temp_cache(100);
        let input = "See [IMAGE:/tmp/photo.png] and [IMAGE:https://example.com/img.jpg]";
        let result = cache_images_in_message(input, &cache);
        assert_eq!(result, input, "non-data-URI markers should be preserved");
        assert!(cache.is_empty());
    }

    #[test]
    fn restore_cached_images_round_trips() {
        let (_tmp, cache) = temp_cache(100);
        let bytes = fake_png_bytes();
        let hash = cache.put(&bytes, "image/png", None).unwrap();

        let cached_content = format!("Context [CACHED_IMAGE:{hash}] end");
        let restored = restore_cached_images(&cached_content, &cache);

        assert!(restored.contains("[IMAGE:data:image/png;base64,"));
        assert!(!restored.contains("[CACHED_IMAGE:"));
    }

    #[test]
    fn restore_cached_images_preserves_missing() {
        let (_tmp, cache) = temp_cache(100);
        let input = "Context [CACHED_IMAGE:deadbeef] end";
        let restored = restore_cached_images(input, &cache);
        assert_eq!(
            restored, input,
            "missing cache entry should preserve marker"
        );
    }

    #[test]
    fn rebuild_index_from_disk() {
        let tmp = TempDir::new().unwrap();
        let bytes = fake_png_bytes();
        let hash;

        {
            let cache = MediaCache::new(tmp.path(), 100).unwrap();
            hash = cache
                .put(&bytes, "image/png", Some("/tmp/test.png"))
                .unwrap();
            assert_eq!(cache.len(), 1);
        }

        // Re-open cache from same directory — index should be rebuilt.
        let cache2 = MediaCache::new(tmp.path(), 100).unwrap();
        assert_eq!(cache2.len(), 1);
        assert!(cache2.contains(&hash));
        assert!(cache2.get_data_uri(&hash).is_some());
    }

    #[test]
    fn total_size_tracks_correctly() {
        let (_tmp, cache) = temp_cache(100);
        cache.put(&[1, 2, 3], "image/png", None).unwrap();
        cache.put(&[4, 5, 6, 7], "image/jpeg", None).unwrap();
        assert_eq!(cache.total_size_bytes(), 7);
    }

    #[test]
    fn decode_data_uri_invalid_returns_none() {
        assert!(decode_data_uri("not-a-data-uri").is_none());
        assert!(decode_data_uri("data:image/png,raw-not-base64").is_none());
    }
}
