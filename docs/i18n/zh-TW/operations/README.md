# 運維與部署文件

適用於在持久化或類生產環境中運行 ZeroClaw 的運維人員。

## 核心運維

- Day-2 運維手冊：[../../../operations-runbook.md](../../../operations-runbook.md)
- 連線探測運維手冊：[../../../operations/connectivity-probes-runbook.md](../../../operations/connectivity-probes-runbook.md)
- 發布流程手冊：[../../../release-process.md](../../../release-process.md)
- 故障排查矩陣：[../../../troubleshooting.md](../../../troubleshooting.md)
- 安全網路／閘道部署：[../../../network-deployment.md](../../../network-deployment.md)
- Mattermost 設定（特定頻道）：[../../../mattermost-setup.md](../../../mattermost-setup.md)

## 常見流程

1. 驗證執行時環境（`status`、`doctor`、`channel doctor`）
2. 每次僅套用一項設定變更
3. 重新啟動服務／常駐程式
4. 驗證頻道與閘道健康狀態
5. 若行為發生退化，迅速回滾

## 相關文件

- 配置參照：[../../../config-reference.md](../../../config-reference.md)
- 安全文件集：[../../../security/README.md](../../../security/README.md)
