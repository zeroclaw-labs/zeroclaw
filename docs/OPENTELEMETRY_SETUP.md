# OpenTelemetry Setup & Troubleshooting Guide

This guide documents the proven configuration for connecting ZeroClaw (Rust) and its Node.js components to a SigNoz (OTel) collector, specifically when deployed via Coolify.

---

## 🚀 The "Golden Configuration" (gRPC)

After testing both HTTP and gRPC, **gRPC on port 4317** is the recommended standard for ZeroClaw. It is significantly more stable in Rust's async environment and avoids URL path suffix issues.

### 1. Rust Backend (`zeroclaw`)
#### Cargo.toml Dependencies
Ensure `opentelemetry-otlp` is configured with `grpc-tonic` (not `http-proto`):
```toml
opentelemetry-otlp = { version = "0.31", default-features = false, features = ["trace", "metrics", "grpc-tonic", "tls-roots"] }
```

#### config.toml (Critical)
ZeroClaw prioritizes `config.toml` over environment variables for OTel. Ensure your persistent volume contains:
```toml
[observability]
backend = "opentelemetry"
otel_endpoint = "http://otel-collector-your-uuid:4317" # Use 4317 for gRPC
```

---

## 🛠️ Major Blocker & "Gotcha" Gallery

### 1. The "No Reactor Running" Panic (Rust)
*   **The Symptom:** Logs show `thread 'OpenTelemetry.Traces.BatchProcessor' panicked: there is no reactor running`.
*   **The Cause:** Using the `reqwest` (HTTP) OTLP exporter in a background thread that isn't connected to the Tokio runtime.
*   **The Fix:** Switch to the `tonic` (gRPC) exporter. It manages its own connection state more gracefully in background threads.

### 2. The HTTP Path Suffix Trap
*   **The Symptom:** OTel initializes successfully in logs, but SigNoz shows 0 data.
*   **The Cause:** If using HTTP (4318), the Rust SDK does **not** automatically append `/v1/traces`. It sends data to the root `/`, which SigNoz rejects with a 404.
*   **The Fix:** Manually append `/v1/traces` to the URL in code, or (better) switch to gRPC.

### 3. The "Coolify Image" Trap
*   **The Symptom:** You push code changes, Coolify says "Deployed," but logs show the old code/ports are still running.
*   **The Cause:** In `docker-compose.yml`, the service uses `image: ghcr.io/...:latest`. Coolify pulls the official image instead of building your patched source code.
*   **The Fix:** Change the service definition to `build: .`. This forces Coolify to compile your local changes.

### 4. The Cargo.lock / --locked Error
*   **The Symptom:** Build fails with `exit code 101` during `cargo build --release --locked`.
*   **The Cause:** `Cargo.toml` was changed, but the updated `Cargo.lock` wasn't committed. The `--locked` flag prevents the build from updating dependencies.
*   **The Fix:** Always commit `Cargo.lock` after changing dependencies.

### 5. The "Ghost" Restart
*   **The Symptom:** You update `config.toml` on the VPS, but the app still uses old settings after a "Restart" in the UI.
*   **The Cause:** Coolify/Docker restarts don't always force a fresh read of the volume if the process didn't fully terminate.
*   **The Fix:** Use a "Hard Restart": `docker stop <cid> && docker start <cid>`.

---

## 🌐 Networking in Coolify
All services (zeroclaw, signoz) must share the same Docker network.
*   Find the network ID in SigNoz settings.
*   Add it as an `external` network in `docker-compose.coolify.yml`.
*   Use the internal container name of the collector: `http://otel-collector-YOUR_ID:4317`.

## 🧪 Validation Checklist
1. [ ] Check logs for `OpenTelemetry observer initialized`.
2. [ ] Verify port is `4317` (gRPC) or `4318` (HTTP).
3. [ ] Send a "Golden Test" message to the bot to trigger a span.
4. [ ] Query SigNoz using `service.name = 'zeroclaw'`.
