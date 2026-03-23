//! ZeroTrading — 交易所账户配置与持久化
//!
//! ## 设计原则
//! - **独立存储**: 账户/API Key 保存在 `workspace/zerotrading/accounts.toml`，
//!   不污染主配置文件 `config.toml`（安全隔离）
//! - **内存屏蔽**: 对外 API 返回时 api_secret/passphrase 字段自动 mask
//! - **热更新**: POST 接口立即持久化 + 原子替换内存映射，无需重启
//! - **多交易所**: 同一交易所可配置多个账号（用 `label` 区分）

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

// ─── 常量 ────────────────────────────────────────────────────────────────────

const ACCOUNTS_FILE: &str = "accounts.toml";
const MASKED: &str = "***";

/// 已支持的交易所标识符集合（用于输入校验）
const KNOWN_EXCHANGES: &[&str] = &[
    "binance",
    "binance_futures",
    "hyperliquid",
    "okx",
    "bybit",
    "bitget",
    "gateio",
    "kucoin",
    "coinbase",
    "kraken",
    "deribit",
    "bitmex",
    "phemex",
    "mexc",
    "woo",
    "custom",
];

// ─── 数据结构 ─────────────────────────────────────────────────────────────────

/// 单个交易所账户凭证配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingAccountEntry {
    /// 账户唯一标签（如 "main", "hedge", "arb"）
    pub label: String,
    /// 交易所标识符（如 "binance_futures", "hyperliquid"）
    pub exchange: String,
    /// API Key（公钥）
    pub api_key: String,
    /// API Secret（私钥）— 存储时明文，返回时屏蔽
    pub api_secret: String,
    /// 可选：部分交易所需要 Passphrase（如 OKX, KuCoin, Coinbase）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    /// 可选：自定义 API Base URL（用于私有部署或代理）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 是否只读模式（禁止下单，仅查询）
    #[serde(default)]
    pub read_only: bool,
    /// 是否启用此账户
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 备注说明
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_true() -> bool {
    true
}

impl TradingAccountEntry {
    /// 返回屏蔽敏感字段后的克隆（用于 API 响应）
    pub fn masked(&self) -> Self {
        Self {
            api_secret: MASKED.to_string(),
            passphrase: self.passphrase.as_ref().map(|_| MASKED.to_string()),
            ..self.clone()
        }
    }

    /// 返回唯一账户 ID（exchange:label）
    pub fn account_id(&self) -> String {
        format!("{}:{}", self.exchange, self.label)
    }

    /// 校验必填字段
    pub fn validate(&self) -> Result<()> {
        if self.label.trim().is_empty() {
            bail!("account label cannot be empty");
        }
        if self.label.contains(':') {
            bail!("account label cannot contain ':'");
        }
        if self.exchange.trim().is_empty() {
            bail!("exchange cannot be empty");
        }
        if self.api_key.trim().is_empty() {
            bail!("api_key cannot be empty");
        }
        if self.api_secret.trim().is_empty() || self.api_secret == MASKED {
            bail!("api_secret cannot be empty or masked");
        }
        Ok(())
    }
}

/// `accounts.toml` 文件的顶层结构
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TradingAccountsFile {
    /// accounts 列表，用 exchange:label 作为逻辑键
    #[serde(default)]
    pub accounts: Vec<TradingAccountEntry>,
}

// ─── 持久化存储 ───────────────────────────────────────────────────────────────

/// ZeroTrading 账户配置持久化管理器
pub struct TradingAccountStore {
    /// accounts.toml 文件路径
    file_path: PathBuf,
}

impl TradingAccountStore {
    /// 创建 Store，指向 `{zerotrading_dir}/accounts.toml`
    pub fn new(zerotrading_dir: &Path) -> Self {
        Self {
            file_path: zerotrading_dir.join(ACCOUNTS_FILE),
        }
    }

    /// 从 workspace 目录创建 Store（自动定位到 `workspace/zerotrading/`）
    pub fn from_workspace(workspace_dir: &Path) -> Self {
        Self::new(&workspace_dir.join("zerotrading"))
    }

    /// 加载所有账户（文件不存在视为空）
    pub async fn load(&self) -> Result<TradingAccountsFile> {
        if !self.file_path.exists() {
            return Ok(TradingAccountsFile::default());
        }
        let content = fs::read_to_string(&self.file_path)
            .await
            .with_context(|| format!("failed to read {}", self.file_path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", self.file_path.display()))
    }

    /// 保存所有账户到磁盘（原子写入：先写临时文件再重命名）
    pub async fn save(&self, data: &TradingAccountsFile) -> Result<()> {
        // 确保目录存在
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }

        let content =
            toml::to_string_pretty(data).context("failed to serialize accounts to TOML")?;

        // 原子写入：先写 .tmp 再 rename
        let tmp_path = self.file_path.with_extension("toml.tmp");
        fs::write(&tmp_path, content.as_bytes())
            .await
            .with_context(|| format!("failed to write temp file {}", tmp_path.display()))?;

        fs::rename(&tmp_path, &self.file_path)
            .await
            .with_context(|| {
                format!(
                    "failed to rename {} to {}",
                    tmp_path.display(),
                    self.file_path.display()
                )
            })?;

        tracing::info!(
            path = %self.file_path.display(),
            count = data.accounts.len(),
            "zerotrading accounts saved"
        );
        Ok(())
    }

    /// 返回文件路径（仅供调试/测试）
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }
}

// ─── 业务逻辑：CRUD ──────────────────────────────────────────────────────────

/// 列出所有账户（屏蔽后）
pub async fn list_accounts_masked(store: &TradingAccountStore) -> Result<Vec<serde_json::Value>> {
    let data = store.load().await?;
    let masked: Vec<serde_json::Value> = data
        .accounts
        .iter()
        .map(|a| serde_json::to_value(a.masked()).unwrap_or_default())
        .collect();
    Ok(masked)
}

/// 新增或更新账户（按 exchange:label 定位）
pub async fn upsert_account(
    store: &TradingAccountStore,
    entry: TradingAccountEntry,
) -> Result<TradingAccountEntry> {
    entry.validate()?;

    let mut data = store.load().await?;
    let id = entry.account_id();

    // 找到同 exchange+label 的条目替换，否则追加
    if let Some(pos) = data.accounts.iter().position(|a| a.account_id() == id) {
        data.accounts[pos] = entry.clone();
        tracing::info!(id = %id, "zerotrading account updated");
    } else {
        data.accounts.push(entry.clone());
        tracing::info!(id = %id, "zerotrading account added");
    }

    store.save(&data).await?;
    Ok(entry)
}

/// 删除账户（按 exchange:label）
///
/// 返回 true 表示成功找到并删除，false 表示未找到
pub async fn remove_account(
    store: &TradingAccountStore,
    exchange: &str,
    label: &str,
) -> Result<bool> {
    let mut data = store.load().await?;
    let id = format!("{exchange}:{label}");
    let before = data.accounts.len();
    data.accounts.retain(|a| a.account_id() != id);
    let removed = data.accounts.len() < before;
    if removed {
        store.save(&data).await?;
        tracing::info!(id = %id, "zerotrading account removed");
    }
    Ok(removed)
}

/// 更新单个账户的 API 凭证（部分更新，保留其他字段）
pub async fn patch_account_credentials(
    store: &TradingAccountStore,
    exchange: &str,
    label: &str,
    new_api_key: Option<String>,
    new_api_secret: Option<String>,
    new_passphrase: Option<String>,
) -> Result<Option<TradingAccountEntry>> {
    let mut data = store.load().await?;
    let id = format!("{exchange}:{label}");

    let Some(entry) = data.accounts.iter_mut().find(|a| a.account_id() == id) else {
        return Ok(None);
    };

    if let Some(k) = new_api_key {
        entry.api_key = k;
    }
    if let Some(s) = new_api_secret {
        if s != MASKED {
            entry.api_secret = s;
        }
    }
    if let Some(p) = new_passphrase {
        entry.passphrase = if p.is_empty() { None } else { Some(p) };
    }

    let result = entry.clone();
    store.save(&data).await?;
    Ok(Some(result))
}

/// 校验交易所 ID 是否合法（允许 custom 以及未知值以支持扩展）
pub fn validate_exchange(exchange: &str) -> bool {
    !exchange.trim().is_empty() && exchange.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// 返回所有已知交易所 ID 列表
pub fn known_exchanges() -> &'static [&'static str] {
    KNOWN_EXCHANGES
}

/// 按交易所分组返回账户统计（屏蔽凭证）
pub async fn accounts_summary(store: &TradingAccountStore) -> Result<HashMap<String, usize>> {
    let data = store.load().await?;
    let mut map: HashMap<String, usize> = HashMap::new();
    for account in &data.accounts {
        *map.entry(account.exchange.clone()).or_insert(0) += 1;
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store(tmp: &TempDir) -> TradingAccountStore {
        TradingAccountStore::new(tmp.path())
    }

    fn sample_entry(exchange: &str, label: &str) -> TradingAccountEntry {
        TradingAccountEntry {
            label: label.to_string(),
            exchange: exchange.to_string(),
            api_key: "ak_test_123".to_string(),
            api_secret: "sk_test_super_secret".to_string(),
            passphrase: None,
            base_url: None,
            read_only: false,
            enabled: true,
            note: None,
        }
    }

    #[tokio::test]
    async fn upsert_and_list() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let entry = sample_entry("binance_futures", "main");
        upsert_account(&store, entry).await.unwrap();

        let accounts = list_accounts_masked(&store).await.unwrap();
        assert_eq!(accounts.len(), 1);
        // Secret must be masked
        assert_eq!(accounts[0]["api_secret"], MASKED);
        assert_eq!(accounts[0]["exchange"], "binance_futures");
    }

    #[tokio::test]
    async fn upsert_replaces_existing() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        upsert_account(&store, sample_entry("binance", "hedge"))
            .await
            .unwrap();
        let mut updated = sample_entry("binance", "hedge");
        updated.api_key = "ak_new_key".to_string();
        upsert_account(&store, updated).await.unwrap();

        let data = store.load().await.unwrap();
        assert_eq!(data.accounts.len(), 1);
        assert_eq!(data.accounts[0].api_key, "ak_new_key");
    }

    #[tokio::test]
    async fn remove_account_found() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        upsert_account(&store, sample_entry("okx", "arb"))
            .await
            .unwrap();
        let removed = remove_account(&store, "okx", "arb").await.unwrap();
        assert!(removed);

        let data = store.load().await.unwrap();
        assert!(data.accounts.is_empty());
    }

    #[tokio::test]
    async fn remove_account_not_found() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);
        let removed = remove_account(&store, "nonexistent", "x").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn empty_file_returns_default() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);
        let data = store.load().await.unwrap();
        assert!(data.accounts.is_empty());
    }

    #[tokio::test]
    async fn masked_entry_hides_secret() {
        let entry = sample_entry("bybit", "test");
        let m = entry.masked();
        assert_eq!(m.api_secret, MASKED);
        assert_eq!(m.api_key, "ak_test_123"); // public key visible
    }

    #[test]
    fn validate_rejects_empty_fields() {
        let mut e = sample_entry("binance", "main");
        e.api_key = "".to_string();
        assert!(e.validate().is_err());

        let mut e2 = sample_entry("binance", "main");
        e2.label = "".to_string();
        assert!(e2.validate().is_err());

        let mut e3 = sample_entry("binance", "main");
        e3.label = "has:colon".to_string();
        assert!(e3.validate().is_err());
    }

    #[test]
    fn validate_exchange_accepts_alphanumeric_underscore() {
        assert!(validate_exchange("binance_futures"));
        assert!(validate_exchange("okx"));
        assert!(!validate_exchange(""));
        assert!(!validate_exchange("invalid-dash"));
    }

    #[tokio::test]
    async fn patch_credentials_updates_key() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);
        upsert_account(&store, sample_entry("kucoin", "spot"))
            .await
            .unwrap();

        let result = patch_account_credentials(
            &store,
            "kucoin",
            "spot",
            Some("ak_new".to_string()),
            None,
            None,
        )
        .await
        .unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().api_key, "ak_new");
    }

    #[tokio::test]
    async fn accounts_summary_groups_by_exchange() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        upsert_account(&store, sample_entry("binance", "a"))
            .await
            .unwrap();
        upsert_account(&store, sample_entry("binance", "b"))
            .await
            .unwrap();
        upsert_account(&store, sample_entry("okx", "main"))
            .await
            .unwrap();

        let summary = accounts_summary(&store).await.unwrap();
        assert_eq!(summary["binance"], 2);
        assert_eq!(summary["okx"], 1);
    }
}
