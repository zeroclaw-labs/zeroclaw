# Plan: Wire Soul + ERC-20 Tokens + Testnet + Condoops Polish

## Context

ZeroClaw's EVM on-chain capabilities (provider, balance, send) were just completed. Four features remain to wire up and harden across the runtime. This plan covers all four.

## Summary

| Feature | Files | New LOC | Effort |
|---------|-------|---------|--------|
| 1. Wire soul_reflect + soul_replicate tools | 2 modified | ~40 | Small |
| 2. ERC-20 token transfers | 4 new, 2 modified | ~450 | Medium |
| 3. Live Sepolia testnet verification | 0 new | 0 | Run tests |
| 4. Condoops operational polish | 3 modified/new | ~80 | Small |

Total: ~570 new lines across ~11 files.

---

## Feature 1: Wire Soul Tools (soul_reflect + soul_replicate)

### Current State
- `SoulStatusTool` — **already wired** in `src/agent/loop_.rs:1223,1697` via SurvivalMonitor
- `SoulReflectTool` — exported in `tools/mod.rs:73` but **never instantiated** in agent loop
- `SoulReplicateTool` — exported in `tools/mod.rs:75` but **never instantiated** in agent loop
- `SurvivalMonitor` + `ModelStrategy` — already wired and active in loop_.rs

### What SoulReflectTool needs
- Constructor: `SoulReflectTool::new(soul_path: PathBuf, soul: Arc<Mutex<SoulModel>>)`
- Requires: loading `SOUL.md` file via `parse_soul_file()`, creating `SoulModel`
- Source: `src/tools/soul_reflect.rs:24-27`

### What SoulReplicateTool needs
- Constructor: `SoulReplicateTool::new(manager: Arc<Mutex<ReplicationManager>>)`
- Requires: creating a `ReplicationManager` with the agent's constitution
- Source: `src/tools/soul_replicate.rs:14-17`

### Step 1.1: Wire soul_reflect + soul_replicate in loop_.rs

**File:** `src/agent/loop_.rs` — in both `run_agent_loop()` (~line 1223) and `run_daemon_loop()` (~line 1697)

Add after the existing SoulStatusTool push (inside the `if config.soul.enabled` block):

```rust
// Soul reflect tool — requires SOUL.md path + loaded SoulModel
let soul_path = workspace_dir.join("SOUL.md");
if soul_path.exists() {
    match crate::soul::parse_soul_file(&soul_path) {
        Ok(soul_model) => {
            let soul = Arc::new(Mutex::new(soul_model));
            tools_registry.push(Box::new(crate::tools::SoulReflectTool::new(
                soul_path.clone(),
                soul,
            )));

            // Soul replicate tool — uses ReplicationConfig + workspace_dir
            let manager = Arc::new(Mutex::new(
                crate::soul::ReplicationManager::new(&config.replication, &workspace_dir),
            ));
            tools_registry.push(Box::new(crate::tools::SoulReplicateTool::new(manager)));
            tracing::info!("Soul reflect + replicate tools initialized");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse SOUL.md; soul reflect/replicate disabled");
        }
    }
}
```

**Verified constructor signatures:**
- `SoulReflectTool::new(soul_path: PathBuf, soul: Arc<Mutex<SoulModel>>)` — `src/tools/soul_reflect.rs:24`
- `SoulReplicateTool::new(manager: Arc<Mutex<ReplicationManager>>)` — `src/tools/soul_replicate.rs:14`
- `ReplicationManager::new(config: &ReplicationConfig, workspace_dir: &Path)` — `src/soul/replication.rs:55`
- `parse_soul_file(path: &Path) -> Result<SoulModel>` — `src/soul/parser.rs`

**Note:** `config.replication` must be accessible in the soul-enabled block. Verify the config struct has `replication: ReplicationConfig` field. If not, use `ReplicationConfig::default()` or add the field.
- `src/soul/parser.rs` — `parse_soul_file()` return type

---

## Feature 2: ERC-20 Token Transfers

### Current State
- `alloy-sol-types` with `sol!` macro already used (`src/wallet/signing.rs:10`)
- `EvmProvider` has `get_balance()` and `send_eth()` (ETH only)
- No ERC-20 encoding, no contract calls, no token tools

### Step 2.1: Create `src/wallet/erc20.rs` (NEW, ~80 lines)

ERC-20 ABI encoder using `alloy-sol-types` `sol!` macro + `SolCall` trait.

```rust
use alloy_primitives::{Address, U256, Bytes};
use alloy_sol_types::{sol, SolCall};

sol! {
    #[derive(Debug)]
    function transfer(address to, uint256 amount) external returns (bool);

    #[derive(Debug)]
    function balanceOf(address account) external view returns (uint256);

    #[derive(Debug)]
    function decimals() external view returns (uint8);

    #[derive(Debug)]
    function symbol() external view returns (string);
}

pub fn encode_transfer(to: Address, amount: U256) -> Bytes {
    transferCall { to, amount }.abi_encode().into()
}

pub fn encode_balance_of(account: Address) -> Bytes {
    balanceOfCall { account }.abi_encode().into()
}

pub fn encode_decimals() -> Bytes {
    decimalsCall {}.abi_encode().into()
}

pub fn encode_symbol() -> Bytes {
    symbolCall {}.abi_encode().into()
}

pub fn decode_balance_of(data: &[u8]) -> anyhow::Result<U256> {
    let result = balanceOfReturn::abi_decode(data, true)?;
    Ok(result._0)
}

pub fn decode_decimals(data: &[u8]) -> anyhow::Result<u8> {
    let result = decimalsReturn::abi_decode(data, true)?;
    Ok(result._0)
}

pub fn decode_symbol(data: &[u8]) -> anyhow::Result<String> {
    let result = symbolReturn::abi_decode(data, true)?;
    Ok(result._0)
}
```

Tests: encode_transfer roundtrip, encode_balance_of format, decode_balance_of, decode_decimals.

Register in `src/wallet/mod.rs`: add `pub mod erc20;`.

### Step 2.2: Add `eth_call` to EvmProvider (~30 lines)

**File:** `src/wallet/provider.rs`

Add two methods:

```rust
/// Execute a read-only contract call (eth_call).
pub async fn call(&self, to: Address, data: Bytes) -> anyhow::Result<Bytes> {
    let provider = self.read_provider();
    let tx = TransactionRequest::default().to(to).input(data.into());
    let result = provider
        .call(tx)
        .await
        .map_err(|e| anyhow::anyhow!("eth_call failed: {e}"))?;
    Ok(result)
}

/// Send a contract transaction (sign + send).
pub async fn send_contract_tx(
    &self,
    keypair: &WalletKeypair,
    to: Address,
    data: Bytes,
) -> anyhow::Result<TxHash> {
    let signer_provider = ProviderBuilder::new()
        .wallet(keypair.signer().clone())
        .connect_http(self.rpc_url.clone());
    let tx = TransactionRequest::default().to(to).input(data.into());
    let pending = signer_provider
        .send_transaction(tx)
        .await
        .map_err(|e| anyhow::anyhow!("Contract tx failed: {e}"))?;
    Ok(*pending.tx_hash())
}
```

### Step 2.3: Create `src/tools/wallet_token_balance.rs` (NEW, ~120 lines)

Tool: `wallet_token_balance` — query ERC-20 balance.

- **Parameters:** `token_address` (required, 0x contract), `address` (optional, defaults to agent wallet)
- **Returns:** `{token_address, account, balance_raw, balance_formatted, decimals, symbol, chain_id}`
- **Pattern:** Follows `wallet_balance.rs`
- **Read-only:** No autonomy required

Implementation:
1. Encode `balanceOf(account)` via `erc20::encode_balance_of()`
2. Call contract via `provider.call(token_addr, data)`
3. Decode result via `erc20::decode_balance_of()`
4. Optionally query `decimals()` and `symbol()` for formatting

Tests: metadata, missing token_address rejection, format with 18 decimals, format with 6 decimals.

### Step 2.4: Create `src/tools/wallet_token_send.rs` (NEW, ~130 lines)

Tool: `wallet_token_send` — transfer ERC-20 tokens.

- **Parameters:** `token_address` (required), `to` (required), `amount` (required, raw units string)
- **Returns:** `{success, tx_hash, token_address, from, to, amount, chain_id}`
- **Guards:** Reject zero amount, validate addresses
- **Pattern:** Follows `wallet_send.rs`

Implementation:
1. Encode `transfer(to, amount)` via `erc20::encode_transfer()`
2. Send via `provider.send_contract_tx(keypair, token_addr, data)`
3. Return tx hash

Tests: metadata, missing params, zero-amount rejection, invalid address.

### Step 2.5: Register token tools in `src/tools/mod.rs`

Add module declarations, re-exports, and registration inside the `if !root_config.wallet.rpc_url.is_empty()` block alongside existing balance/send tools:

```rust
tools.push(Box::new(WalletTokenBalanceTool::new(store.clone(), provider.clone())));
tools.push(Box::new(WalletTokenSendTool::new(store.clone(), provider.clone())));
```

### Step 2.6: Register erc20 in wallet/mod.rs

Add `pub mod erc20;` to `src/wallet/mod.rs`.

---

## Feature 3: Live Sepolia Testnet Verification

### What exists
- `src/wallet/provider.rs` has 2 `#[ignore]` tests: `sepolia_get_balance`, `sepolia_send_and_receipt`
- Tests read `ZEROCLAW_TEST_RPC_URL` and `ZEROCLAW_TEST_FUNDED_KEY` env vars

### Step 3.1: Run balance query test

```bash
ZEROCLAW_TEST_RPC_URL=https://rpc.sepolia.org \
  cargo test --features wallet -- --ignored sepolia_get_balance
```

This is a read-only test against the zero address — should pass with any RPC.

### Step 3.2: Fund + run send test (requires user action)

1. User generates a Sepolia wallet key or uses an existing one
2. User funds it via Sepolia faucet (https://sepoliafaucet.com or similar)
3. Run:
```bash
ZEROCLAW_TEST_RPC_URL=https://rpc.sepolia.org \
ZEROCLAW_TEST_FUNDED_KEY=<hex_private_key> \
  cargo test --features wallet -- --ignored sepolia_send_and_receipt
```

### Step 3.3: Add ERC-20 testnet test (NEW, ~30 lines)

Add `#[ignore]` test in `src/wallet/erc20.rs` or `src/wallet/provider.rs`:

```rust
#[tokio::test]
#[ignore]
async fn sepolia_token_balance() {
    let rpc = std::env::var("ZEROCLAW_TEST_RPC_URL")
        .unwrap_or_else(|_| "https://rpc.sepolia.org".to_string());
    let provider = EvmProvider::connect(&rpc, 11_155_111).unwrap();
    // Query USDC on Sepolia (or any known test token)
    let token: Address = std::env::var("ZEROCLAW_TEST_TOKEN_ADDRESS")
        .unwrap_or_else(|_| "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238".to_string())
        .parse().unwrap();
    let zero_addr: Address = Address::ZERO;
    let data = crate::wallet::erc20::encode_balance_of(zero_addr);
    let result = provider.call(token, data).await.unwrap();
    // Just verify we got valid data back (32 bytes)
    assert!(result.len() >= 32);
}
```

---

## Feature 4: Condoops Operational Polish

### Current State
- 80% complete, builds clean, 45 API endpoints, 21 migrations, 10 integration test suites
- All core features implemented (auth, RBAC, proposals, work orders, accounting, audit)

### Step 4.1: Verify Dockerfile builds

```bash
cd apps/condoops && docker build -t condoops-api .
```

If Dockerfile is missing or broken, create a minimal multi-stage Rust Dockerfile.

### Step 4.2: Add rate limiting middleware (~40 lines)

**File:** `apps/condoops/crates/condoops-api/src/main.rs` or new `middleware.rs`

Add tower rate limiting to the Axum router:
```rust
use tower::ServiceBuilder;
use tower_http::limit::RequestBodyLimitLayer;
// Add per-IP rate limiting via tower middleware
```

### Step 4.3: Verify docker-compose up works end-to-end

```bash
cd apps/condoops && docker compose up -d
curl http://localhost:3001/health
```

---

## Execution Order

1. **Feature 1** (soul wiring) — independent, ~40 lines, do first
2. **Feature 2** (ERC-20) — sequential steps 2.1→2.6, ~450 lines
3. **Feature 3** (testnet) — run after Feature 2, depends on provider methods
4. **Feature 4** (condoops) — independent, can parallel with 1-3

Features 1 and 4 are independent and can run in parallel. Features 2→3 are sequential.

---

## Verification

After all features:

```bash
# Full compilation
cargo check --features wallet
cargo clippy --features wallet -- -D warnings
cargo test --features wallet

# Sepolia balance (no funding needed)
ZEROCLAW_TEST_RPC_URL=https://rpc.sepolia.org \
  cargo test --features wallet -- --ignored sepolia_get_balance

# Token balance (no funding needed)
ZEROCLAW_TEST_RPC_URL=https://rpc.sepolia.org \
  cargo test --features wallet -- --ignored sepolia_token_balance

# Condoops
cd apps/condoops && cargo check && cargo test
```
