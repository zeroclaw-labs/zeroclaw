//! Payment system for ZeroClaw (Kakao Pay integration).
//!
//! Provides credit purchase flow via Kakao Pay:
//! 1. User requests credit recharge ("충전")
//! 2. System presents credit packages
//! 3. Kakao Pay payment link generated
//! 4. User completes payment
//! 5. Webhook confirms payment
//! 6. Credits added atomically
//! 7. Confirmation notification sent
//!
//! ## Design
//! - SQLite-based payment ledger (local-first, no external DB dependency)
//! - Atomic credit operations via SQLite transactions
//! - Payment status tracking with idempotent webhook handling
//! - Configurable credit packages

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Credit packages ──────────────────────────────────────────────

/// Predefined credit package for purchase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditPackage {
    /// Package identifier.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Price in KRW (Korean Won).
    pub price_krw: u32,
    /// Credits granted upon purchase.
    pub credits: u32,
}

/// Available credit packages.
pub const CREDIT_PACKAGES: &[CreditPackage] = &[
    CreditPackage {
        id: "basic_1000",
        name: "Basic",
        price_krw: 1_000,
        credits: 100,
    },
    CreditPackage {
        id: "standard_3000",
        name: "Standard",
        price_krw: 3_000,
        credits: 350,
    },
    CreditPackage {
        id: "premium_5000",
        name: "Premium",
        price_krw: 5_000,
        credits: 650,
    },
    CreditPackage {
        id: "pro_10000",
        name: "Pro",
        price_krw: 10_000,
        credits: 1_500,
    },
];

/// Look up a credit package by ID.
pub fn find_package(package_id: &str) -> Option<&'static CreditPackage> {
    CREDIT_PACKAGES.iter().find(|p| p.id == package_id)
}

/// Look up a credit package by price in KRW.
pub fn find_package_by_price(price_krw: u32) -> Option<&'static CreditPackage> {
    CREDIT_PACKAGES.iter().find(|p| p.price_krw == price_krw)
}

// ── Payment status ───────────────────────────────────────────────

/// Status of a payment transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentStatus {
    /// Payment initiated, waiting for user to complete.
    Pending,
    /// Payment completed successfully, credits granted.
    Completed,
    /// Payment failed or was rejected.
    Failed,
    /// Payment cancelled by user.
    Cancelled,
    /// Payment refunded.
    Refunded,
}

impl PaymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Refunded => "refunded",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "refunded" => Self::Refunded,
            _ => Self::Pending,
        }
    }
}

// ── Payment record ───────────────────────────────────────────────

/// A payment transaction record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRecord {
    /// Unique transaction ID (generated internally).
    pub transaction_id: String,
    /// User identifier.
    pub user_id: String,
    /// Credit package ID.
    pub package_id: String,
    /// Amount in KRW.
    pub amount_krw: u32,
    /// Credits to be granted.
    pub credits: u32,
    /// Current payment status.
    pub status: PaymentStatus,
    /// External payment provider transaction ID (e.g., Kakao Pay TID).
    pub provider_tid: Option<String>,
    /// Unix timestamp (seconds) of creation.
    pub created_at: i64,
    /// Unix timestamp (seconds) of last update.
    pub updated_at: i64,
}

// ── Kakao Pay API types ──────────────────────────────────────────

/// Kakao Pay payment ready request.
#[derive(Debug, Serialize)]
pub struct KakaoPayReadyRequest {
    /// Merchant CID (test: TC0ONETIME).
    pub cid: String,
    /// Internal order ID.
    pub partner_order_id: String,
    /// User ID.
    pub partner_user_id: String,
    /// Product name.
    pub item_name: String,
    /// Quantity (always 1 for credits).
    pub quantity: u32,
    /// Total amount in KRW.
    pub total_amount: u32,
    /// Tax-free amount (0 for digital goods).
    pub tax_free_amount: u32,
    /// Approval redirect URL.
    pub approval_url: String,
    /// Cancel redirect URL.
    pub cancel_url: String,
    /// Failure redirect URL.
    pub fail_url: String,
}

/// Kakao Pay payment ready response.
#[derive(Debug, Deserialize)]
pub struct KakaoPayReadyResponse {
    /// Transaction ID from Kakao Pay.
    pub tid: String,
    /// Redirect URL for mobile web.
    pub next_redirect_mobile_url: Option<String>,
    /// Redirect URL for PC web.
    pub next_redirect_pc_url: Option<String>,
    /// Redirect URL for mobile app.
    pub next_redirect_app_url: Option<String>,
}

/// Kakao Pay payment approval request.
#[derive(Debug, Serialize)]
pub struct KakaoPayApproveRequest {
    /// Merchant CID.
    pub cid: String,
    /// Transaction ID from ready response.
    pub tid: String,
    /// Internal order ID.
    pub partner_order_id: String,
    /// User ID.
    pub partner_user_id: String,
    /// Payment approval token (pg_token from callback).
    pub pg_token: String,
}

/// Kakao Pay payment approval response.
#[derive(Debug, Deserialize)]
pub struct KakaoPayApproveResponse {
    /// Transaction ID.
    pub tid: String,
    /// Approved amount.
    pub amount: KakaoPayAmount,
}

/// Kakao Pay amount breakdown.
#[derive(Debug, Deserialize)]
pub struct KakaoPayAmount {
    /// Total amount.
    pub total: u32,
}

// ── Payment manager ──────────────────────────────────────────────

/// Payment manager with SQLite persistence.
///
/// Handles the full payment lifecycle: initiation, webhook processing,
/// credit granting, and transaction history.
pub struct PaymentManager {
    /// Persistent SQLite connection.
    conn: Option<Connection>,
    /// Kakao Pay merchant CID.
    kakao_cid: String,
    /// Kakao Pay admin key (for API authentication).
    kakao_admin_key: Option<String>,
    /// Base URL for payment callbacks.
    callback_base_url: String,
    /// Whether payment features are enabled.
    enabled: bool,
}

impl PaymentManager {
    /// Create a new payment manager.
    pub fn new(
        workspace_dir: &Path,
        kakao_admin_key: Option<String>,
        callback_base_url: &str,
        enabled: bool,
    ) -> anyhow::Result<Self> {
        let conn = if enabled {
            let db_path = workspace_dir.join("payments.db");
            let conn = Connection::open(&db_path)?;
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 5000;",
            )?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS payments (
                    transaction_id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    package_id TEXT NOT NULL,
                    amount_krw INTEGER NOT NULL,
                    credits INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending',
                    provider_tid TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_payments_user ON payments(user_id);
                CREATE INDEX IF NOT EXISTS idx_payments_status ON payments(status);

                CREATE TABLE IF NOT EXISTS credit_balances (
                    user_id TEXT PRIMARY KEY,
                    balance INTEGER NOT NULL DEFAULT 0,
                    total_purchased INTEGER NOT NULL DEFAULT 0,
                    total_spent INTEGER NOT NULL DEFAULT 0,
                    updated_at INTEGER NOT NULL
                );",
            )?;
            Some(conn)
        } else {
            None
        };

        Ok(Self {
            conn,
            kakao_cid: "TC0ONETIME".to_string(), // Test CID by default
            kakao_admin_key,
            callback_base_url: callback_base_url.to_string(),
            enabled,
        })
    }

    /// Set a custom Kakao Pay merchant CID (for production).
    pub fn set_cid(&mut self, cid: &str) {
        self.kakao_cid = cid.to_string();
    }

    /// Initiate a payment for a credit package.
    ///
    /// Creates a pending payment record and returns the Kakao Pay ready request
    /// data that should be sent to the Kakao Pay API.
    pub fn initiate_payment(
        &self,
        user_id: &str,
        package_id: &str,
    ) -> anyhow::Result<(PaymentRecord, KakaoPayReadyRequest)> {
        if !self.enabled {
            anyhow::bail!("Payment features are disabled");
        }

        let Some(ref conn) = self.conn else {
            anyhow::bail!("Payment database not initialized");
        };

        let package = find_package(package_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown credit package: {package_id}"))?;

        let transaction_id = uuid::Uuid::new_v4().to_string();
        let now = now_epoch();

        let record = PaymentRecord {
            transaction_id: transaction_id.clone(),
            user_id: user_id.to_string(),
            package_id: package_id.to_string(),
            amount_krw: package.price_krw,
            credits: package.credits,
            status: PaymentStatus::Pending,
            provider_tid: None,
            created_at: now,
            updated_at: now,
        };

        conn.execute(
            "INSERT INTO payments (transaction_id, user_id, package_id, amount_krw, credits, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.transaction_id,
                record.user_id,
                record.package_id,
                record.amount_krw,
                record.credits,
                record.status.as_str(),
                record.created_at,
                record.updated_at,
            ],
        )?;

        let ready_request = KakaoPayReadyRequest {
            cid: self.kakao_cid.clone(),
            partner_order_id: transaction_id,
            partner_user_id: user_id.to_string(),
            item_name: format!("ZeroClaw Credits - {} ({})", package.name, package.credits),
            quantity: 1,
            total_amount: package.price_krw,
            tax_free_amount: 0,
            approval_url: format!(
                "{}/api/payment/approve?tx={}",
                self.callback_base_url, record.transaction_id
            ),
            cancel_url: format!(
                "{}/api/payment/cancel?tx={}",
                self.callback_base_url, record.transaction_id
            ),
            fail_url: format!(
                "{}/api/payment/fail?tx={}",
                self.callback_base_url, record.transaction_id
            ),
        };

        Ok((record, ready_request))
    }

    /// Store the Kakao Pay TID after successful ready call.
    pub fn set_provider_tid(&self, transaction_id: &str, provider_tid: &str) -> anyhow::Result<()> {
        let Some(ref conn) = self.conn else {
            return Ok(());
        };

        let now = now_epoch();
        conn.execute(
            "UPDATE payments SET provider_tid = ?1, updated_at = ?2 WHERE transaction_id = ?3",
            params![provider_tid, now, transaction_id],
        )?;

        Ok(())
    }

    /// Complete a payment: update status to Completed and add credits atomically.
    ///
    /// This is idempotent — calling it twice for the same transaction will not
    /// double-grant credits.
    pub fn complete_payment(&self, transaction_id: &str) -> anyhow::Result<PaymentRecord> {
        if !self.enabled {
            anyhow::bail!("Payment features are disabled");
        }

        let Some(ref conn) = self.conn else {
            anyhow::bail!("Payment database not initialized");
        };

        // Fetch current record
        let record = self
            .get_payment(transaction_id)?
            .ok_or_else(|| anyhow::anyhow!("Payment not found: {transaction_id}"))?;

        // Idempotent: already completed
        if record.status == PaymentStatus::Completed {
            return Ok(record);
        }

        if record.status != PaymentStatus::Pending {
            anyhow::bail!("Cannot complete payment in status: {:?}", record.status);
        }

        let now = now_epoch();

        // Atomic: update payment status + add credits in a single transaction
        conn.execute("BEGIN", [])?;

        let result = (|| -> anyhow::Result<()> {
            conn.execute(
                "UPDATE payments SET status = 'completed', updated_at = ?1 WHERE transaction_id = ?2",
                params![now, transaction_id],
            )?;

            // Upsert credit balance
            conn.execute(
                "INSERT INTO credit_balances (user_id, balance, total_purchased, total_spent, updated_at)
                 VALUES (?1, ?2, ?2, 0, ?3)
                 ON CONFLICT(user_id) DO UPDATE SET
                     balance = balance + ?2,
                     total_purchased = total_purchased + ?2,
                     updated_at = ?3",
                params![record.user_id, record.credits, now],
            )?;

            Ok(())
        })();

        if let Err(e) = result {
            let _ = conn.execute("ROLLBACK", []);
            return Err(e);
        }

        conn.execute("COMMIT", [])?;

        // Return updated record
        self.get_payment(transaction_id)?
            .ok_or_else(|| anyhow::anyhow!("Payment disappeared after completion"))
    }

    /// Cancel a pending payment.
    pub fn cancel_payment(&self, transaction_id: &str) -> anyhow::Result<()> {
        let Some(ref conn) = self.conn else {
            return Ok(());
        };

        let now = now_epoch();
        let updated = conn.execute(
            "UPDATE payments SET status = 'cancelled', updated_at = ?1
             WHERE transaction_id = ?2 AND status = 'pending'",
            params![now, transaction_id],
        )?;

        if updated == 0 {
            anyhow::bail!("Payment not found or not in pending status: {transaction_id}");
        }

        Ok(())
    }

    /// Mark a payment as failed.
    pub fn fail_payment(&self, transaction_id: &str) -> anyhow::Result<()> {
        let Some(ref conn) = self.conn else {
            return Ok(());
        };

        let now = now_epoch();
        conn.execute(
            "UPDATE payments SET status = 'failed', updated_at = ?1
             WHERE transaction_id = ?2 AND status = 'pending'",
            params![now, transaction_id],
        )?;

        Ok(())
    }

    /// Get a payment record by transaction ID.
    pub fn get_payment(&self, transaction_id: &str) -> anyhow::Result<Option<PaymentRecord>> {
        let Some(ref conn) = self.conn else {
            return Ok(None);
        };

        let mut stmt = conn.prepare_cached(
            "SELECT transaction_id, user_id, package_id, amount_krw, credits, status, provider_tid, created_at, updated_at
             FROM payments WHERE transaction_id = ?1",
        )?;

        let result = stmt.query_row(params![transaction_id], |row| {
            let status_str: String = row.get(5)?;
            Ok(PaymentRecord {
                transaction_id: row.get(0)?,
                user_id: row.get(1)?,
                package_id: row.get(2)?,
                amount_krw: row.get(3)?,
                credits: row.get(4)?,
                status: PaymentStatus::from_str(&status_str),
                provider_tid: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        });

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get credit balance for a user.
    pub fn get_balance(&self, user_id: &str) -> anyhow::Result<u32> {
        let Some(ref conn) = self.conn else {
            return Ok(0);
        };

        let result = conn.query_row(
            "SELECT balance FROM credit_balances WHERE user_id = ?1",
            params![user_id],
            |row| row.get::<_, u32>(0),
        );

        match result {
            Ok(balance) => Ok(balance),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Deduct credits from a user's balance (for usage).
    ///
    /// Returns the new balance, or an error if insufficient credits.
    pub fn deduct_credits(&self, user_id: &str, amount: u32) -> anyhow::Result<u32> {
        if !self.enabled {
            anyhow::bail!("Payment features are disabled");
        }

        let Some(ref conn) = self.conn else {
            anyhow::bail!("Payment database not initialized");
        };

        let now = now_epoch();

        let updated = conn.execute(
            "UPDATE credit_balances SET
                balance = balance - ?1,
                total_spent = total_spent + ?1,
                updated_at = ?2
             WHERE user_id = ?3 AND balance >= ?1",
            params![amount, now, user_id],
        )?;

        if updated == 0 {
            let current = self.get_balance(user_id)?;
            anyhow::bail!("Insufficient credits: required {amount}, available {current}");
        }

        self.get_balance(user_id)
    }

    /// List payment history for a user, ordered by most recent first.
    pub fn list_user_payments(
        &self,
        user_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<PaymentRecord>> {
        let Some(ref conn) = self.conn else {
            return Ok(Vec::new());
        };

        let mut stmt = conn.prepare_cached(
            "SELECT transaction_id, user_id, package_id, amount_krw, credits, status, provider_tid, created_at, updated_at
             FROM payments WHERE user_id = ?1
             ORDER BY created_at DESC LIMIT ?2",
        )?;

        let records = stmt
            .query_map(params![user_id, limit as i64], |row| {
                let status_str: String = row.get(5)?;
                Ok(PaymentRecord {
                    transaction_id: row.get(0)?,
                    user_id: row.get(1)?,
                    package_id: row.get(2)?,
                    amount_krw: row.get(3)?,
                    credits: row.get(4)?,
                    status: PaymentStatus::from_str(&status_str),
                    provider_tid: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }

    /// Get the Kakao Pay admin key (for API calls).
    pub fn kakao_admin_key(&self) -> Option<&str> {
        self.kakao_admin_key.as_deref()
    }

    /// Check if payments are enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Get current epoch seconds.
fn now_epoch() -> i64 {
    chrono::Utc::now().timestamp()
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_manager() -> (TempDir, PaymentManager) {
        let tmp = TempDir::new().unwrap();
        let manager = PaymentManager::new(
            tmp.path(),
            Some("test-admin-key".to_string()),
            "https://zeroclaw.example.com",
            true,
        )
        .unwrap();
        (tmp, manager)
    }

    #[test]
    fn credit_packages_defined() {
        assert_eq!(CREDIT_PACKAGES.len(), 4);
        assert_eq!(CREDIT_PACKAGES[0].id, "basic_1000");
        assert_eq!(CREDIT_PACKAGES[3].id, "pro_10000");
    }

    #[test]
    fn find_package_by_id() {
        let pkg = find_package("standard_3000");
        assert!(pkg.is_some());
        let pkg = pkg.unwrap();
        assert_eq!(pkg.price_krw, 3_000);
        assert_eq!(pkg.credits, 350);
    }

    #[test]
    fn find_package_by_id_unknown() {
        assert!(find_package("nonexistent").is_none());
    }

    #[test]
    fn find_package_by_price_works() {
        let pkg = find_package_by_price(5_000);
        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().id, "premium_5000");
    }

    #[test]
    fn payment_status_roundtrip() {
        for status in [
            PaymentStatus::Pending,
            PaymentStatus::Completed,
            PaymentStatus::Failed,
            PaymentStatus::Cancelled,
            PaymentStatus::Refunded,
        ] {
            let s = status.as_str();
            assert_eq!(PaymentStatus::from_str(s), status);
        }
    }

    #[test]
    fn payment_status_unknown_defaults_to_pending() {
        assert_eq!(PaymentStatus::from_str("unknown"), PaymentStatus::Pending);
    }

    #[test]
    fn initiate_payment_creates_record() {
        let (_tmp, manager) = make_manager();

        let (record, request) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        assert_eq!(record.user_id, "zeroclaw_user");
        assert_eq!(record.package_id, "basic_1000");
        assert_eq!(record.amount_krw, 1_000);
        assert_eq!(record.credits, 100);
        assert_eq!(record.status, PaymentStatus::Pending);

        assert_eq!(request.total_amount, 1_000);
        assert!(request.approval_url.contains("/api/payment/approve"));
        assert!(request.cancel_url.contains("/api/payment/cancel"));
    }

    #[test]
    fn initiate_payment_unknown_package_fails() {
        let (_tmp, manager) = make_manager();
        let result = manager.initiate_payment("zeroclaw_user", "nonexistent");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown credit package"));
    }

    #[test]
    fn complete_payment_grants_credits() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "standard_3000")
            .unwrap();

        let completed = manager.complete_payment(&record.transaction_id).unwrap();
        assert_eq!(completed.status, PaymentStatus::Completed);

        let balance = manager.get_balance("zeroclaw_user").unwrap();
        assert_eq!(balance, 350);
    }

    #[test]
    fn complete_payment_is_idempotent() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        manager.complete_payment(&record.transaction_id).unwrap();
        manager.complete_payment(&record.transaction_id).unwrap();

        // Should only have 100 credits, not 200
        let balance = manager.get_balance("zeroclaw_user").unwrap();
        assert_eq!(balance, 100);
    }

    #[test]
    fn multiple_payments_accumulate_credits() {
        let (_tmp, manager) = make_manager();

        let (r1, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();
        manager.complete_payment(&r1.transaction_id).unwrap();

        let (r2, _) = manager
            .initiate_payment("zeroclaw_user", "standard_3000")
            .unwrap();
        manager.complete_payment(&r2.transaction_id).unwrap();

        let balance = manager.get_balance("zeroclaw_user").unwrap();
        assert_eq!(balance, 100 + 350);
    }

    #[test]
    fn cancel_payment_works() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        manager.cancel_payment(&record.transaction_id).unwrap();

        let cancelled = manager
            .get_payment(&record.transaction_id)
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.status, PaymentStatus::Cancelled);

        // Should not grant credits
        let balance = manager.get_balance("zeroclaw_user").unwrap();
        assert_eq!(balance, 0);
    }

    #[test]
    fn cancel_completed_payment_fails() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        manager.complete_payment(&record.transaction_id).unwrap();

        let result = manager.cancel_payment(&record.transaction_id);
        assert!(result.is_err());
    }

    #[test]
    fn complete_cancelled_payment_fails() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        manager.cancel_payment(&record.transaction_id).unwrap();

        let result = manager.complete_payment(&record.transaction_id);
        assert!(result.is_err());
    }

    #[test]
    fn deduct_credits_works() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "pro_10000")
            .unwrap();
        manager.complete_payment(&record.transaction_id).unwrap();

        let balance = manager.get_balance("zeroclaw_user").unwrap();
        assert_eq!(balance, 1_500);

        let new_balance = manager.deduct_credits("zeroclaw_user", 500).unwrap();
        assert_eq!(new_balance, 1_000);
    }

    #[test]
    fn deduct_credits_insufficient_fails() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();
        manager.complete_payment(&record.transaction_id).unwrap();

        let result = manager.deduct_credits("zeroclaw_user", 200);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Insufficient credits"));
    }

    #[test]
    fn deduct_credits_no_balance_fails() {
        let (_tmp, manager) = make_manager();
        let result = manager.deduct_credits("new_user", 10);
        assert!(result.is_err());
    }

    #[test]
    fn get_balance_new_user_returns_zero() {
        let (_tmp, manager) = make_manager();
        let balance = manager.get_balance("unknown_user").unwrap();
        assert_eq!(balance, 0);
    }

    #[test]
    fn list_user_payments_ordered() {
        let (_tmp, manager) = make_manager();

        manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();
        manager
            .initiate_payment("zeroclaw_user", "standard_3000")
            .unwrap();
        manager
            .initiate_payment("zeroclaw_user", "premium_5000")
            .unwrap();

        let payments = manager.list_user_payments("zeroclaw_user", 10).unwrap();
        assert_eq!(payments.len(), 3);
        // Most recent first
        assert!(payments[0].created_at >= payments[1].created_at);
    }

    #[test]
    fn list_user_payments_limit() {
        let (_tmp, manager) = make_manager();

        for _ in 0..5 {
            manager
                .initiate_payment("zeroclaw_user", "basic_1000")
                .unwrap();
        }

        let payments = manager.list_user_payments("zeroclaw_user", 3).unwrap();
        assert_eq!(payments.len(), 3);
    }

    #[test]
    fn list_user_payments_empty() {
        let (_tmp, manager) = make_manager();
        let payments = manager.list_user_payments("zeroclaw_user", 10).unwrap();
        assert!(payments.is_empty());
    }

    #[test]
    fn set_provider_tid() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        manager
            .set_provider_tid(&record.transaction_id, "KAKAO_TID_123")
            .unwrap();

        let updated = manager
            .get_payment(&record.transaction_id)
            .unwrap()
            .unwrap();
        assert_eq!(updated.provider_tid, Some("KAKAO_TID_123".to_string()));
    }

    #[test]
    fn fail_payment_works() {
        let (_tmp, manager) = make_manager();

        let (record, _) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();

        manager.fail_payment(&record.transaction_id).unwrap();

        let failed = manager
            .get_payment(&record.transaction_id)
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, PaymentStatus::Failed);
    }

    #[test]
    fn disabled_manager_rejects_operations() {
        let tmp = TempDir::new().unwrap();
        let manager = PaymentManager::new(tmp.path(), None, "https://example.com", false).unwrap();

        assert!(!manager.is_enabled());
        assert!(manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .is_err());
        assert!(manager.deduct_credits("zeroclaw_user", 10).is_err());
    }

    #[test]
    fn payment_record_get_nonexistent() {
        let (_tmp, manager) = make_manager();
        let result = manager.get_payment("nonexistent-tx").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_cid() {
        let (_tmp, mut manager) = make_manager();
        manager.set_cid("PRODUCTION_CID");
        // The CID is used internally for Kakao Pay requests
        let (_, request) = manager
            .initiate_payment("zeroclaw_user", "basic_1000")
            .unwrap();
        assert_eq!(request.cid, "PRODUCTION_CID");
    }
}
