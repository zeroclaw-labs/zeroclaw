# Playbook Proxy Agent

Tài liệu này cung cấp các tool call có thể copy-paste để cấu hình hành vi proxy qua `proxy_config`.

Dùng tài liệu này khi bạn muốn agent chuyển đổi phạm vi proxy nhanh chóng và an toàn.

## 0. Tóm Tắt

- **Mục đích:** cung cấp tool call sẵn sàng sử dụng để quản lý phạm vi proxy và rollback.
- **Đối tượng:** operator và maintainer đang chạy ZeroClaw trong mạng có proxy.
- **Phạm vi:** các hành động `proxy_config`, lựa chọn mode, quy trình xác minh và xử lý sự cố.
- **Ngoài phạm vi:** gỡ lỗi mạng chung không liên quan đến hành vi runtime của ZeroClaw.

---

## 1. Đường Dẫn Nhanh Theo Mục Đích

Dùng mục này để định tuyến vận hành nhanh.

### 1.1 Chỉ proxy traffic nội bộ ZeroClaw

1. Dùng scope `zeroclaw`.
2. Đặt `http_proxy`/`https_proxy` hoặc `all_proxy`.
3. Xác minh bằng `{"action":"get"}`.

Xem:

- [Mục 4](#4-mode-a--chỉ-proxy-cho-nội-bộ-zeroclaw)

### 1.2 Chỉ proxy các dịch vụ được chọn

1. Dùng scope `services`.
2. Đặt các key cụ thể hoặc wildcard selector trong `services`.
3. Xác minh phủ sóng bằng `{"action":"list_services"}`.

Xem:

- [Mục 5](#5-mode-b--chỉ-proxy-cho-các-dịch-vụ-cụ-thể)

### 1.3 Xuất biến môi trường proxy cho toàn bộ process

1. Dùng scope `environment`.
2. Áp dụng bằng `{"action":"apply_env"}`.
3. Xác minh snapshot env qua `{"action":"get"}`.

Xem:

- [Mục 6](#6-mode-c--proxy-cho-toàn-bộ-môi-trường-process)

### 1.4 Rollback khẩn cấp

1. Tắt proxy.
2. Nếu cần, xóa các biến env đã xuất.
3. Kiểm tra lại snapshot runtime và môi trường.

Xem:

- [Mục 7](#7-các-mẫu-tắt--rollback)

---

## 2. Ma Trận Quyết Định Phạm Vi

| Phạm vi | Ảnh hưởng | Xuất biến env | Trường hợp dùng điển hình |
|---|---|---|---|
| `zeroclaw` | Các HTTP client nội bộ ZeroClaw | Không | Proxying runtime thông thường không có tác dụng phụ cấp process |
| `services` | Chỉ các service key/selector được chọn | Không | Định tuyến chi tiết cho provider/tool/channel cụ thể |
| `environment` | Runtime + biến môi trường proxy của process | Có | Các tích hợp yêu cầu `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` |

---

## 3. Quy Trình An Toàn Chuẩn

Dùng trình tự này cho mọi thay đổi proxy:

1. Kiểm tra trạng thái hiện tại.
2. Khám phá các service key/selector hợp lệ.
3. Áp dụng cấu hình phạm vi mục tiêu.
4. Xác minh snapshot runtime và môi trường.
5. Rollback nếu hành vi không như kỳ vọng.

Tool call:

```json
{"action":"get"}
{"action":"list_services"}
```

---

## 4. Mode A — Chỉ Proxy Cho Nội Bộ ZeroClaw

Dùng khi traffic HTTP của provider/channel/tool ZeroClaw cần đi qua proxy mà không xuất biến env proxy cấp process.

Tool call:

```json
{"action":"set","enabled":true,"scope":"zeroclaw","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":["localhost","127.0.0.1"]}
{"action":"get"}
```

Hành vi kỳ vọng:

- Runtime proxy hoạt động cho các HTTP client của ZeroClaw.
- Không cần xuất `HTTP_PROXY` / `HTTPS_PROXY` vào env của process.

---

## 5. Mode B — Chỉ Proxy Cho Các Dịch Vụ Cụ Thể

Dùng khi chỉ một phần hệ thống cần đi qua proxy (ví dụ provider/tool/channel cụ thể).

### 5.1 Nhắm vào dịch vụ cụ thể

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request","channel.telegram"],"all_proxy":"socks5h://127.0.0.1:1080","no_proxy":["localhost","127.0.0.1",".internal"]}
{"action":"get"}
```

### 5.2 Nhắm theo selector

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.*","tool.*"],"http_proxy":"http://127.0.0.1:7890"}
{"action":"get"}
```

Hành vi kỳ vọng:

- Chỉ các service khớp mới dùng proxy.
- Các service không khớp bỏ qua proxy.

---

## 6. Mode C — Proxy Cho Toàn Bộ Môi Trường Process

Dùng khi bạn cần xuất tường minh các biến env của process (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) cho các tích hợp runtime.

### 6.1 Cấu hình và áp dụng environment scope

```json
{"action":"set","enabled":true,"scope":"environment","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":"localhost,127.0.0.1,.internal"}
{"action":"apply_env"}
{"action":"get"}
```

Hành vi kỳ vọng:

- Runtime proxy hoạt động.
- Các biến môi trường được xuất cho process.

---

## 7. Các Mẫu Tắt / Rollback

### 7.1 Tắt proxy (hành vi an toàn mặc định)

```json
{"action":"disable"}
{"action":"get"}
```

### 7.2 Tắt proxy và xóa cưỡng bức các biến env

```json
{"action":"disable","clear_env":true}
{"action":"get"}
```

### 7.3 Giữ proxy bật nhưng chỉ xóa các biến env đã xuất

```json
{"action":"clear_env"}
{"action":"get"}
```

---

## 8. Các Công Thức Vận Hành Thường Dùng

### 8.1 Chuyển từ proxy toàn environment sang proxy chỉ service

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request"],"all_proxy":"socks5://127.0.0.1:1080"}
{"action":"get"}
```

### 8.2 Thêm một dịch vụ proxied

```json
{"action":"set","scope":"services","services":["provider.openai","tool.http_request","channel.slack"]}
{"action":"get"}
```

### 8.3 Đặt lại danh sách `services` với selector

```json
{"action":"set","scope":"services","services":["provider.*","channel.telegram"]}
{"action":"get"}
```

---

## 9. Xử Lý Sự Cố

- Lỗi: `proxy.scope='services' requires a non-empty proxy.services list`
  - Khắc phục: đặt ít nhất một service key cụ thể hoặc selector.

- Lỗi: invalid proxy URL scheme
  - Scheme được chấp nhận: `http`, `https`, `socks5`, `socks5h`.

- Proxy không áp dụng như kỳ vọng
  - Chạy `{"action":"list_services"}` và xác minh tên/selector dịch vụ.
  - Chạy `{"action":"get"}` và kiểm tra giá trị snapshot `runtime_proxy` và `environment`.

---

## 10. Tài Liệu Liên Quan

- [README.md](./README.md) — Chỉ mục tài liệu và phân loại.
- [network-deployment.md](network-deployment.md) — Hướng dẫn triển khai mạng đầu-cuối và topology tunnel.
- [resource-limits.md](./resource-limits.md) — Giới hạn an toàn runtime cho ngữ cảnh thực thi mạng/tool.

---

## 11. Ghi Chú Bảo Trì

- **Chủ sở hữu:** maintainer runtime và tooling.
- **Điều kiện cập nhật:** các hành động `proxy_config` mới, ngữ nghĩa phạm vi proxy, hoặc thay đổi selector dịch vụ được hỗ trợ.
- **Xem xét lần cuối:** 2026-02-18.
