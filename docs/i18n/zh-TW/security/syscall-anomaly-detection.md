# 系統呼叫異常偵測

ZeroClaw 可以監控沙箱化指令執行所發出的系統呼叫相關遙測資料，並在異常演變為無聲故障之前進行標記。

此功能專為 daemon 執行時期路徑設計，在該路徑下 `shell` 與 `process` 工具在策略控制下反覆執行指令。

## 偵測項目

- 超出預期基線設定檔的未知系統呼叫名稱
- 60 秒滾動視窗內的拒絕系統呼叫尖峰
- 60 秒滾動視窗內的系統呼叫事件洪水
- 嚴格模式下被拒絕的系統呼叫，即使該系統呼叫在基線中

偵測器消費指令的 `stderr` 與 `stdout` 行，並解析已知的訊號形式：

- Linux audit 格式（`syscall=59`）
- Seccomp 拒絕行（`seccomp denied syscall=openat`）
- SIGSYS / `Bad system call` 崩潰提示

## 組態

在 `[security.syscall_anomaly]` 下進行設定：

```toml
[security.syscall_anomaly]
enabled = true
strict_mode = false
alert_on_unknown_syscall = true
max_denied_events_per_minute = 5
max_total_events_per_minute = 120
max_alerts_per_minute = 30
alert_cooldown_secs = 20
log_path = "syscall-anomalies.log"
baseline_syscalls = [
  "read", "write", "openat", "close", "mmap", "munmap",
  "futex", "clock_gettime", "epoll_wait", "clone", "execve",
  "socket", "connect", "sendto", "recvfrom", "getrandom"
]
```

## 警示與稽核輸出

當偵測到異常時：

- 會以目標 `security::syscall_anomaly` 發出警告日誌
- 會在 `log_path` 中附加一筆結構化 JSON 行
- 當安全稽核記錄啟用時，會發出一筆 `security_event` 稽核項目

## 調校指引

- 從 `strict_mode = false` 開始，以避免初次部署時的大量雜訊。
- 針對已知的工作負載擴展 `baseline_syscalls`，直到未知警示穩定為止。
- 對於正式環境的 daemon，將 `max_denied_events_per_minute` 保持較小（例如 `3-10`）。
- 在高吞吐量環境中使用較高的 `max_total_events_per_minute`。
- 保持 `max_denied_events_per_minute <= max_total_events_per_minute`。
- 限制 `max_alerts_per_minute` 的值以防止警示風暴。
- 設定 `alert_cooldown_secs` 以在反覆重試期間抑制重複的異常。

## 驗證

目前的驗證包含：

- audit 與 seccomp 格式的解析器擷取測試
- 長時間執行行程輸出的滾動緩衝區增量掃描覆蓋
- 未知系統呼叫異常測試
- 拒絕速率閾值測試
- 閾值與基線名稱的組態驗證測試
