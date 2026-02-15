//! Deduplicator - 请求去重
//!
//! 防止重复处理相同的请求：
//! - 基于内容的去重
//! - 滑动窗口去重
//! - 布隆过滤器支持
//! - 多种去重策略

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

/// 去重键
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DedupKey {
    /// 字符串键
    String(String),
    /// 整数键
    U64(u64),
    /// 组合键
    Composite(Vec<String>),
    /// 哈希键（预计算）
    Hash(u64),
}

impl DedupKey {
    /// 从字符串创建
    pub fn from_string(s: impl Into<String>) -> Self {
        Self::String(s.into())
    }

    /// 从可哈希类型创建
    pub fn from_hashable<T: Hash>(value: &T) -> Self {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        Self::Hash(hasher.finish())
    }

    /// 组合多个键
    pub fn combine(keys: Vec<String>) -> Self {
        Self::Composite(keys)
    }

    /// 获取哈希值
    pub fn hash_value(&self) -> u64 {
        match self {
            Self::Hash(h) => *h,
            _ => {
                let mut hasher = DefaultHasher::new();
                self.hash(&mut hasher);
                hasher.finish()
            }
        }
    }
}

impl From<&str> for DedupKey {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for DedupKey {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<u64> for DedupKey {
    fn from(n: u64) -> Self {
        Self::U64(n)
    }
}

/// 去重策略
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DedupStrategy {
    /// 精确匹配 - 完全相同的键视为重复
    Exact,
    /// 模糊匹配 - 相似度阈值
    Fuzzy { threshold: f64 },
    /// 滑动窗口 - 在一定时间内相同键视为重复
    SlidingWindow { window_secs: u64 },
    /// 计数器 - 最多允许 N 次重复
    Counter { max_count: u32 },
}

impl Default for DedupStrategy {
    fn default() -> Self {
        Self::SlidingWindow { window_secs: 60 }
    }
}

/// 去重条目
#[derive(Debug, Clone)]
struct DedupEntry {
    /// 键的哈希
    hash: u64,
    /// 首次出现时间
    first_seen: Instant,
    /// 最后出现时间
    last_seen: Instant,
    /// 出现次数
    count: u32,
    /// 过期时间
    expires_at: Instant,
}

/// 请求去重器
pub struct Deduplicator {
    /// 存储的条目
    entries: RwLock<HashMap<u64, DedupEntry>>,
    /// 默认 TTL
    default_ttl: Duration,
    /// 清理间隔
    cleanup_interval: Duration,
    /// 最后一次清理时间
    last_cleanup: Mutex<Instant>,
    /// 统计信息
    stats: Mutex<DedupStats>,
    /// 最大条目数
    max_entries: usize,
}

/// 去重统计信息
#[derive(Debug, Clone, Default)]
pub struct DedupStats {
    /// 去重命中次数
    pub duplicates_found: u64,
    /// 新增唯一键次数
    pub unique_added: u64,
    /// 过期清理的条目数
    pub expired_removed: u64,
    /// 当前条目数
    pub current_entries: usize,
    /// 因容量限制淘汰的条目数
    pub evicted_entries: u64,
}

impl Deduplicator {
    /// 创建新的去重器
    pub fn new(default_ttl: Duration) -> Self {
        Self::with_capacity(default_ttl, 10000)
    }

    /// 指定容量创建
    pub fn with_capacity(default_ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: RwLock::new(HashMap::with_capacity(max_entries.min(1000))),
            default_ttl,
            cleanup_interval: Duration::from_secs(60),
            last_cleanup: Mutex::new(Instant::now()),
            stats: Mutex::new(DedupStats::default()),
            max_entries,
        }
    }

    /// 检查键是否已存在（存在则更新最后访问时间）
    pub fn check_and_update(&self, key: &DedupKey) -> bool {
        self.maybe_cleanup();
        
        let hash = key.hash_value();
        let now = Instant::now();
        
        let mut entries = self.entries.write().unwrap();
        
        if let Some(entry) = entries.get_mut(&hash) {
            if entry.expires_at > now {
                // 存在且未过期
                entry.last_seen = now;
                entry.count += 1;
                entry.expires_at = now + self.default_ttl;
                
                self.stats.lock().unwrap().duplicates_found += 1;
                trace!("Duplicate detected for key hash: {}", hash);
                return true;
            }
        }
        
        // 不存在或已过期，添加新条目
        self.add_entry(&mut entries, hash, now);
        false
    }

    /// 检查键是否存在（只读，不更新）
    pub fn contains(&self, key: &DedupKey) -> bool {
        self.maybe_cleanup();
        
        let hash = key.hash_value();
        let now = Instant::now();
        
        let entries = self.entries.read().unwrap();
        
        if let Some(entry) = entries.get(&hash) {
            entry.expires_at > now
        } else {
            false
        }
    }

    /// 添加键（如果已存在则返回 true）
    pub fn insert(&self, key: &DedupKey) -> bool {
        self.check_and_update(key)
    }

    /// 添加键并指定 TTL
    pub fn insert_with_ttl(&self, key: &DedupKey, ttl: Duration) -> bool {
        self.maybe_cleanup();
        
        let hash = key.hash_value();
        let now = Instant::now();
        
        let mut entries = self.entries.write().unwrap();
        
        if let Some(entry) = entries.get_mut(&hash) {
            if entry.expires_at > now {
                entry.last_seen = now;
                entry.count += 1;
                entry.expires_at = now + ttl;
                self.stats.lock().unwrap().duplicates_found += 1;
                return true;
            }
        }
        
        // 添加新条目
        entries.insert(hash, DedupEntry {
            hash,
            first_seen: now,
            last_seen: now,
            count: 1,
            expires_at: now + ttl,
        });
        
        self.stats.lock().unwrap().unique_added += 1;
        self.check_capacity(&mut entries);
        
        false
    }

    /// 移除键
    pub fn remove(&self, key: &DedupKey) -> bool {
        let hash = key.hash_value();
        let mut entries = self.entries.write().unwrap();
        entries.remove(&hash).is_some()
    }

    /// 获取键的统计信息
    pub fn get_stats(&self, key: &DedupKey) -> Option<DedupEntryStats> {
        let hash = key.hash_value();
        let entries = self.entries.read().unwrap();
        let now = Instant::now();
        
        entries.get(&hash).and_then(|entry| {
            if entry.expires_at > now {
                Some(DedupEntryStats {
                    first_seen: entry.first_seen,
                    last_seen: entry.last_seen,
                    count: entry.count,
                    expires_in: entry.expires_at.duration_since(now),
                })
            } else {
                None
            }
        })
    }

    /// 清空所有条目
    pub fn clear(&self) {
        let mut entries = self.entries.write().unwrap();
        entries.clear();
        let mut stats = self.stats.lock().unwrap();
        stats.current_entries = 0;
    }

    /// 获取统计信息
    pub fn stats(&self) -> DedupStats {
        let mut stats = self.stats.lock().unwrap().clone();
        stats.current_entries = self.entries.read().unwrap().len();
        stats
    }

    /// 获取当前条目数
    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.read().unwrap().is_empty()
    }

    /// 添加条目（内部方法）
    fn add_entry(&self, entries: &mut HashMap<u64, DedupEntry>, hash: u64, now: Instant) {
        entries.insert(hash, DedupEntry {
            hash,
            first_seen: now,
            last_seen: now,
            count: 1,
            expires_at: now + self.default_ttl,
        });
        
        self.stats.lock().unwrap().unique_added += 1;
        self.check_capacity(entries);
    }

    /// 检查容量限制
    fn check_capacity(&self, entries: &mut HashMap<u64, DedupEntry>) {
        if entries.len() > self.max_entries {
            // 淘汰最旧的条目（LRU）
            let now = Instant::now();
            let mut to_remove: Vec<u64> = entries
                .iter()
                .filter(|(_, e)| e.expires_at <= now)
                .map(|(k, _)| *k)
                .collect();
            
            // 如果还不够，移除最久未访问的
            if entries.len() - to_remove.len() > self.max_entries {
                let mut sorted: Vec<_> = entries.iter().collect();
                sorted.sort_by_key(|(_, e)| e.last_seen);
                
                let extra_to_remove = entries.len() - self.max_entries;
                for (hash, _) in sorted.iter().take(extra_to_remove) {
                    to_remove.push(**hash);
                }
            }
            
            let removed_count = to_remove.len();
            for hash in to_remove {
                entries.remove(&hash);
            }
            
            if removed_count > 0 {
                self.stats.lock().unwrap().evicted_entries += removed_count as u64;
                debug!("Evicted {} entries due to capacity limit", removed_count);
            }
        }
    }

    /// 尝试清理过期条目
    fn maybe_cleanup(&self) {
        let should_cleanup = {
            let last = self.last_cleanup.lock().unwrap();
            last.elapsed() > self.cleanup_interval
        };
        
        if should_cleanup {
            self.cleanup();
        }
    }

    /// 清理过期条目
    pub fn cleanup(&self) {
        let now = Instant::now();
        let mut entries = self.entries.write().unwrap();
        
        let before_count = entries.len();
        entries.retain(|_, entry| entry.expires_at > now);
        let removed = before_count - entries.len();
        
        drop(entries);
        
        if removed > 0 {
            self.stats.lock().unwrap().expired_removed += removed as u64;
            debug!("Cleaned up {} expired deduplication entries", removed);
        }
        
        *self.last_cleanup.lock().unwrap() = now;
    }

    /// 从字符串切片创建键并检查
    pub fn check_string(&self, s: &str) -> bool {
        self.check_and_update(&DedupKey::from(s))
    }

    /// 从多个字段创建组合键并检查
    pub fn check_composite(&self, fields: Vec<String>) -> bool {
        self.check_and_update(&DedupKey::combine(fields))
    }
}

/// 去重条目统计
#[derive(Debug, Clone)]
pub struct DedupEntryStats {
    pub first_seen: Instant,
    pub last_seen: Instant,
    pub count: u32,
    pub expires_in: Duration,
}

/// 滑动窗口去重器 - 针对时间序列优化
pub struct SlidingWindowDeduplicator {
    /// 窗口大小
    window_size: Duration,
    /// 每个键的历史记录
    windows: Mutex<HashMap<u64, Vec<Instant>>>,
    /// 统计信息
    stats: Mutex<DedupStats>,
}

impl SlidingWindowDeduplicator {
    /// 创建新的滑动窗口去重器
    pub fn new(window_size: Duration) -> Self {
        Self {
            window_size,
            windows: Mutex::new(HashMap::new()),
            stats: Mutex::new(DedupStats::default()),
        }
    }

    /// 检查键是否在窗口内已存在
    pub fn check(&self, key: &DedupKey) -> bool {
        let hash = key.hash_value();
        let now = Instant::now();
        let cutoff = now - self.window_size;
        
        let mut windows = self.windows.lock().unwrap();
        let timestamps = windows.entry(hash).or_default();
        
        // 清理过期记录
        timestamps.retain(|&t| t > cutoff);
        
        if !timestamps.is_empty() {
            // 在窗口内找到记录
            timestamps.push(now);
            self.stats.lock().unwrap().duplicates_found += 1;
            true
        } else {
            // 新记录
            timestamps.push(now);
            self.stats.lock().unwrap().unique_added += 1;
            false
        }
    }

    /// 获取键在窗口内的出现次数
    pub fn count_in_window(&self, key: &DedupKey) -> usize {
        let hash = key.hash_value();
        let now = Instant::now();
        let cutoff = now - self.window_size;
        
        let windows = self.windows.lock().unwrap();
        
        if let Some(timestamps) = windows.get(&hash) {
            timestamps.iter().filter(|&&t| t > cutoff).count()
        } else {
            0
        }
    }

    /// 获取统计信息
    pub fn stats(&self) -> DedupStats {
        let mut stats = self.stats.lock().unwrap().clone();
        stats.current_entries = self.windows.lock().unwrap().len();
        stats
    }

    /// 清空所有记录
    pub fn clear(&self) {
        self.windows.lock().unwrap().clear();
    }
}

/// 布隆过滤器去重器 - 内存高效，可能有假阳性
pub struct BloomDeduplicator {
    /// 位数组
    bits: Mutex<Vec<bool>>,
    /// 位数组大小
    size: usize,
    /// 哈希函数数量
    hash_count: u32,
    /// 统计信息
    stats: Mutex<DedupStats>,
}

impl BloomDeduplicator {
    /// 创建新的布隆过滤器
    /// 
    /// # 参数
    /// - size: 位数组大小（越大假阳性率越低）
    /// - hash_count: 哈希函数数量
    pub fn new(size: usize, hash_count: u32) -> Self {
        Self {
            bits: Mutex::new(vec![false; size]),
            size,
            hash_count,
            stats: Mutex::new(DedupStats::default()),
        }
    }

    /// 估计最佳参数
    /// 
    /// # 参数
    /// - expected_items: 预期条目数
    /// - false_positive_rate: 可接受的假阳性率 (0.0-1.0)
    pub fn with_expected_items(expected_items: usize, false_positive_rate: f64) -> Self {
        // 计算最佳位数组大小
        let size = (-(expected_items as f64) * false_positive_rate.ln() 
            / (2.0f64.ln().powi(2))).ceil() as usize;
        
        // 计算最佳哈希函数数量
        let hash_count = ((size as f64 / expected_items as f64) * 2.0f64.ln()).ceil() as u32;
        
        Self::new(size.max(1024), hash_count.max(1))
    }

    /// 检查键是否可能存在
    pub fn check(&self, key: &DedupKey) -> bool {
        let hash = key.hash_value();
        let mut bits = self.bits.lock().unwrap();
        
        let mut found = true;
        for i in 0..self.hash_count {
            let index = self.get_index(hash, i);
            if !bits[index] {
                found = false;
                bits[index] = true;
            }
        }
        
        if found {
            self.stats.lock().unwrap().duplicates_found += 1;
        } else {
            self.stats.lock().unwrap().unique_added += 1;
        }
        
        found
    }

    /// 计算索引
    fn get_index(&self, hash: u64, seed: u32) -> usize {
        let combined = hash.wrapping_add(seed as u64);
        (combined as usize) % self.size
    }

    /// 清空过滤器
    pub fn clear(&self) {
        let mut bits = self.bits.lock().unwrap();
        bits.fill(false);
    }

    /// 获取统计信息
    pub fn stats(&self) -> DedupStats {
        self.stats.lock().unwrap().clone()
    }
}

/// 组合去重器 - 结合多种策略
pub struct HybridDeduplicator {
    /// 精确去重（短期）
    exact: Deduplicator,
    /// 滑动窗口（中期）
    sliding: SlidingWindowDeduplicator,
    /// 布隆过滤器（长期，低内存）
    bloom: Option<BloomDeduplicator>,
}

impl HybridDeduplicator {
    /// 创建新的组合去重器
    pub fn new(exact_ttl: Duration, window_size: Duration) -> Self {
        Self {
            exact: Deduplicator::new(exact_ttl),
            sliding: SlidingWindowDeduplicator::new(window_size),
            bloom: None,
        }
    }

    /// 启用布隆过滤器
    pub fn with_bloom(mut self, expected_items: usize, false_positive_rate: f64) -> Self {
        self.bloom = Some(BloomDeduplicator::with_expected_items(
            expected_items,
            false_positive_rate,
        ));
        self
    }

    /// 检查键是否重复（按策略顺序）
    pub fn check(&self, key: &DedupKey) -> bool {
        // 1. 先检查精确去重
        if self.exact.check_and_update(key) {
            return true;
        }
        
        // 2. 再检查滑动窗口
        if self.sliding.check(key) {
            return true;
        }
        
        // 3. 最后检查布隆过滤器（如果启用）
        if let Some(ref bloom) = self.bloom {
            // 布隆过滤器可能产生假阳性，所以如果布隆说存在，我们还需要用其他方式确认
            let _ = bloom.check(key);
        }
        
        false
    }

    /// 获取综合统计信息
    pub fn stats(&self) -> HybridStats {
        HybridStats {
            exact: self.exact.stats(),
            sliding: self.sliding.stats(),
            bloom: self.bloom.as_ref().map(|b| b.stats()),
        }
    }
}

/// 组合去重器统计
#[derive(Debug, Clone)]
pub struct HybridStats {
    pub exact: DedupStats,
    pub sliding: DedupStats,
    pub bloom: Option<DedupStats>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[test]
    fn test_dedup_key_creation() {
        let key1 = DedupKey::from_string("test");
        let key2 = DedupKey::from("test");
        let key3 = DedupKey::from_hashable(&"test");
        
        assert_eq!(key1.hash_value(), key2.hash_value());
    }

    #[test]
    fn test_deduplicator_basic() {
        let dedup = Deduplicator::new(Duration::from_secs(60));
        
        let key = DedupKey::from("test_key");
        
        // 第一次应该返回 false（新键）
        assert!(!dedup.check_and_update(&key));
        
        // 第二次应该返回 true（重复）
        assert!(dedup.check_and_update(&key));
        
        // 检查统计
        let stats = dedup.stats();
        assert_eq!(stats.unique_added, 1);
        assert_eq!(stats.duplicates_found, 1);
    }

    #[tokio::test]
    async fn test_deduplicator_expiration() {
        let dedup = Deduplicator::new(Duration::from_millis(50));
        
        let key = DedupKey::from("temp_key");
        
        assert!(!dedup.check_and_update(&key));
        assert!(dedup.check_and_update(&key));
        
        // 等待过期
        sleep(Duration::from_millis(100)).await;
        
        // 应该被视为新键
        assert!(!dedup.check_and_update(&key));
    }

    #[test]
    fn test_deduplicator_capacity() {
        let dedup = Deduplicator::with_capacity(Duration::from_secs(60), 10);
        
        // 添加超过容量的键
        for i in 0..20 {
            let key = DedupKey::from(format!("key_{}", i));
            dedup.check_and_update(&key);
        }
        
        // 检查是否有淘汰发生
        let stats = dedup.stats();
        assert!(stats.current_entries <= 10);
    }

    #[test]
    fn test_sliding_window_deduplicator() {
        let dedup = SlidingWindowDeduplicator::new(Duration::from_secs(60));
        
        let key = DedupKey::from("sw_key");
        
        assert!(!dedup.check(&key));
        assert!(dedup.check(&key));
        assert!(dedup.check(&key));
        
        // 检查窗口内计数
        assert_eq!(dedup.count_in_window(&key), 3);
    }

    #[test]
    fn test_bloom_deduplicator() {
        let dedup = BloomDeduplicator::new(1000, 3);
        
        let key1 = DedupKey::from("key1");
        let key2 = DedupKey::from("key2");
        
        assert!(!dedup.check(&key1));
        assert!(dedup.check(&key1)); // 可能假阳性，但大概率正确
        assert!(!dedup.check(&key2));
    }

    #[test]
    fn test_composite_key() {
        let dedup = Deduplicator::new(Duration::from_secs(60));
        
        let key1 = DedupKey::combine(vec![
            "channel".to_string(),
            "user123".to_string(),
            "content_hash".to_string(),
        ]);
        
        let key2 = DedupKey::combine(vec![
            "channel".to_string(),
            "user123".to_string(),
            "content_hash".to_string(),
        ]);
        
        let key3 = DedupKey::combine(vec![
            "channel".to_string(),
            "user456".to_string(),
            "content_hash".to_string(),
        ]);
        
        assert!(!dedup.check_and_update(&key1));
        assert!(dedup.check_and_update(&key2)); // 与 key1 相同
        assert!(!dedup.check_and_update(&key3)); // 不同用户
    }

    #[test]
    fn test_hybrid_deduplicator() {
        let dedup = HybridDeduplicator::new(
            Duration::from_secs(10),
            Duration::from_secs(60),
        );
        
        let key = DedupKey::from("hybrid_key");
        
        assert!(!dedup.check(&key));
        assert!(dedup.check(&key));
        
        let stats = dedup.stats();
        assert!(stats.exact.unique_added > 0 || stats.sliding.unique_added > 0);
    }
}
