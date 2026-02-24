# Tăng tốc build với cargo-slicer (Tiếng Việt)

Trang này là bản địa hóa tối thiểu cho hướng dẫn tối ưu tốc độ build bằng `cargo-slicer`.

Bản gốc tiếng Anh:

- [../../cargo-slicer-speedup.md](../../cargo-slicer-speedup.md)

## Tóm tắt

- `cargo-slicer` giảm thời gian build bằng cách loại phần code không reachable ở mức MIR.
- CI dùng chiến lược 2 đường:
  - Fast path: build có `cargo-slicer`.
  - Fallback path: quay về `cargo +nightly build --release` nếu toolchain không tương thích.

## Khi nào dùng

- Muốn giảm thời gian build release.
- Cần tái hiện hành vi lane build nhanh trong CI.

Lệnh cài đặt/chạy chi tiết xem bản tiếng Anh.
