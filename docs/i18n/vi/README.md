# Tài liệu ZeroClaw (Tiếng Việt)

Đây là trang chủ tiếng Việt của hệ thống tài liệu.

Đồng bộ lần cuối: **2026-02-21**.

> Lưu ý: Tên lệnh, khóa cấu hình và đường dẫn API giữ nguyên tiếng Anh. Khi có sai khác, tài liệu tiếng Anh là bản gốc.

## Tra cứu nhanh

| Tôi muốn… | Xem tài liệu |
|---|---|
| Cài đặt và chạy nhanh | [../../../README.vi.md](../../../README.vi.md) / [../../../README.md](../../../README.md) |
| Cài đặt bằng một lệnh | [one-click-bootstrap.md](one-click-bootstrap.md) |
| Cài đặt trên Android (Termux/ADB) | [android-setup.md](android-setup.md) |
| Tìm lệnh theo tác vụ | [commands-reference.md](commands-reference.md) |
| Kiểm tra giá trị mặc định và khóa cấu hình | [config-reference.md](config-reference.md) |
| Kết nối provider / endpoint tùy chỉnh | [custom-providers.md](custom-providers.md) |
| Cấu hình Z.AI / GLM provider | [zai-glm-setup.md](zai-glm-setup.md) |
| Sử dụng tích hợp LangGraph | [langgraph-integration.md](langgraph-integration.md) |
| Thiết lập Nextcloud Talk | [nextcloud-talk-setup.md](nextcloud-talk-setup.md) |
| Cấu hình proxy theo phạm vi an toàn | [proxy-agent-playbook.md](proxy-agent-playbook.md) |
| Vận hành hàng ngày (runbook) | [operations-runbook.md](operations-runbook.md) |
| Vận hành probe kết nối provider trong CI | [operations/connectivity-probes-runbook.md](operations/connectivity-probes-runbook.md) |
| Khắc phục sự cố cài đặt/chạy/kênh | [troubleshooting.md](troubleshooting.md) |
| Cấu hình Matrix phòng mã hóa (E2EE) | [matrix-e2ee-guide.md](matrix-e2ee-guide.md) |
| Xem theo danh mục | [SUMMARY.md](SUMMARY.md) |
| Xem bản chụp PR/Issue | [project-triage-snapshot-2026-02-18.md](../../project-triage-snapshot-2026-02-18.md) |

## Tìm nhanh

- Cài đặt lần đầu hoặc khởi động nhanh → [getting-started/README.md](getting-started/README.md)
- Cần tra cứu lệnh CLI / khóa cấu hình → [reference/README.md](reference/README.md)
- Cần vận hành / triển khai sản phẩm → [operations/README.md](operations/README.md)
- Gặp lỗi hoặc hồi quy → [troubleshooting.md](troubleshooting.md)
- Tìm hiểu bảo mật và lộ trình → [security/README.md](security/README.md)
- Làm việc với bo mạch / thiết bị ngoại vi → [hardware/README.md](hardware/README.md)
- Đóng góp / review / quy trình CI → [contributing/README.md](contributing/README.md)
- Xem toàn bộ bản đồ tài liệu → [SUMMARY.md](SUMMARY.md)

## Theo danh mục

- Bắt đầu: [getting-started/README.md](getting-started/README.md)
- Tra cứu: [reference/README.md](reference/README.md)
- Vận hành & triển khai: [operations/README.md](operations/README.md)
- Bảo mật: [security/README.md](security/README.md)
- Phần cứng & ngoại vi: [hardware/README.md](hardware/README.md)
- Đóng góp & CI: [contributing/README.md](contributing/README.md)
- Ảnh chụp dự án: [project/README.md](project/README.md)

## Theo vai trò

### Người dùng / Vận hành

- [commands-reference.md](commands-reference.md) — tra cứu lệnh theo tác vụ
- [providers-reference.md](providers-reference.md) — ID provider, bí danh, biến môi trường xác thực
- [channels-reference.md](channels-reference.md) — khả năng kênh và hướng dẫn thiết lập
- [matrix-e2ee-guide.md](matrix-e2ee-guide.md) — thiết lập phòng mã hóa Matrix (E2EE)
- [config-reference.md](config-reference.md) — khóa cấu hình quan trọng và giá trị mặc định an toàn
- [custom-providers.md](custom-providers.md) — mẫu tích hợp provider / base URL tùy chỉnh
- [zai-glm-setup.md](zai-glm-setup.md) — thiết lập Z.AI/GLM và ma trận endpoint
- [langgraph-integration.md](langgraph-integration.md) — tích hợp dự phòng cho model/tool-calling
- [operations-runbook.md](operations-runbook.md) — vận hành runtime hàng ngày và quy trình rollback
- [troubleshooting.md](troubleshooting.md) — dấu hiệu lỗi thường gặp và cách khắc phục

### Người đóng góp / Bảo trì

- [CONTRIBUTING.md](../../../CONTRIBUTING.md)
- [pr-workflow.md](pr-workflow.md)
- [reviewer-playbook.md](reviewer-playbook.md)
- [ci-map.md](ci-map.md)
- [actions-source-policy.md](actions-source-policy.md)

### Bảo mật / Độ tin cậy

> Lưu ý: Mục này gồm tài liệu đề xuất/lộ trình, có thể chứa lệnh hoặc cấu hình chưa triển khai. Để biết hành vi thực tế, xem [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md) và [troubleshooting.md](troubleshooting.md) trước.

- [security/README.md](security/README.md)
- [agnostic-security.md](agnostic-security.md)
- [frictionless-security.md](frictionless-security.md)
- [sandboxing.md](sandboxing.md)
- [audit-logging.md](audit-logging.md)
- [resource-limits.md](resource-limits.md)
- [security-roadmap.md](security-roadmap.md)

## Quản lý tài liệu

- Mục lục thống nhất (TOC): [SUMMARY.md](SUMMARY.md)
- Bản đồ cấu trúc docs (ngôn ngữ/phần/chức năng): [../../structure/README.md](../../structure/README.md)
- Danh mục và phân loại tài liệu: [docs-inventory.md](docs-inventory.md)
- Checklist hoàn thiện i18n: [i18n-guide.md](i18n-guide.md)
- Bản đồ độ phủ i18n: [i18n-coverage.md](i18n-coverage.md)
- Backlog thiếu hụt i18n: [i18n-gap-backlog.md](i18n-gap-backlog.md)
- Snapshot kiểm toán tài liệu (2026-02-24): [docs-audit-2026-02-24.md](docs-audit-2026-02-24.md)

## Ngôn ngữ khác

- English: [README.md](../../README.md)
- 简体中文: [../zh-CN/README.md](../zh-CN/README.md)
- 日本語: [../ja/README.md](../ja/README.md)
- Русский: [../ru/README.md](../ru/README.md)
- Français: [../fr/README.md](../fr/README.md)
- Ελληνικά: [../el/README.md](../el/README.md)
