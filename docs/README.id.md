# Pusat Dokumentasi JhedaiClaw

Halaman ini adalah titik masuk utama untuk sistem dokumentasi.

Pembaruan terakhir: **21 Februari 2026**.

Hub terlokalisasi: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Mulai di Sini

| Saya ingin…                                                    | Baca ini                                                                                   |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Menginstal dan menjalankan JhedaiClaw dengan cepat             | [README.md (Mulai Cepat)](../README.md#quick-start)                                        |
| Bootstrap dalam satu perintah                                  | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Memperbarui atau menghapus di macOS                            | [macos-update-uninstall.md](setup-guides/macos-update-uninstall.md)                        |
| Mencari perintah berdasarkan tugas                             | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Memeriksa default dan kunci konfigurasi dengan cepat           | [config-reference.md](reference/api/config-reference.md)                                   |
| Mengonfigurasi penyedia/endpoint kustom                        | [custom-providers.md](contributing/custom-providers.md)                                    |
| Mengonfigurasi penyedia Z.AI / GLM                             | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| Menggunakan pola integrasi LangGraph                           | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Mengoperasikan runtime (buku panduan hari ke-2)                | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Memecahkan masalah instalasi/runtime/kanal                     | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Menjalankan pengaturan ruang terenkripsi Matrix dan diagnostik | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Menjelajahi dokumentasi berdasarkan kategori                   | [SUMMARY.md](SUMMARY.md)                                                                   |
| Melihat snapshot dokumen PR/issue proyek                       | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Pohon Keputusan Cepat (10 detik)

- Butuh pengaturan atau instalasi pertama kali? → [setup-guides/README.md](setup-guides/README.md)
- Butuh kunci CLI/konfigurasi yang tepat? → [reference/README.md](reference/README.md)
- Butuh operasi produksi/layanan? → [ops/README.md](ops/README.md)
- Melihat kegagalan atau regresi? → [troubleshooting.md](ops/troubleshooting.md)
- Bekerja pada penguatan keamanan atau peta jalan? → [security/README.md](security/README.md)
- Bekerja dengan papan/periferal? → [hardware/README.md](hardware/README.md)
- Kontribusi/review/alur kerja CI? → [contributing/README.md](contributing/README.md)
- Ingin peta lengkap? → [SUMMARY.md](SUMMARY.md)

## Koleksi (Direkomendasikan)

- Memulai: [setup-guides/README.md](setup-guides/README.md)
- Katalog referensi: [reference/README.md](reference/README.md)
- Operasi & deployment: [ops/README.md](ops/README.md)
- Dokumentasi keamanan: [security/README.md](security/README.md)
- Perangkat keras/periferal: [hardware/README.md](hardware/README.md)
- Kontribusi/CI: [contributing/README.md](contributing/README.md)
- Snapshot proyek: [maintainers/README.md](maintainers/README.md)

## Berdasarkan Audiens

### Pengguna / Operator

- [commands-reference.md](reference/cli/commands-reference.md) — pencarian perintah berdasarkan alur kerja
- [providers-reference.md](reference/api/providers-reference.md) — ID penyedia, alias, variabel lingkungan kredensial
- [channels-reference.md](reference/api/channels-reference.md) — kemampuan kanal dan jalur pengaturan
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — pengaturan ruang terenkripsi Matrix (E2EE) dan diagnostik tanpa respons
- [config-reference.md](reference/api/config-reference.md) — kunci konfigurasi penting dan default aman
- [custom-providers.md](contributing/custom-providers.md) — template integrasi penyedia kustom/URL dasar
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — pengaturan Z.AI/GLM dan matriks endpoint
- [langgraph-integration.md](contributing/langgraph-integration.md) — integrasi fallback untuk kasus tepi model/pemanggilan alat
- [operations-runbook.md](ops/operations-runbook.md) — operasi runtime hari ke-2 dan alur rollback
- [troubleshooting.md](ops/troubleshooting.md) — tanda kegagalan umum dan langkah pemulihan

### Kontributor / Pengelola

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Keamanan / Keandalan

> Catatan: area ini mencakup dokumen proposal/peta jalan. Untuk perilaku saat ini, mulailah dengan [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), dan [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Navigasi Sistem & Tata Kelola

- Daftar isi terpadu: [SUMMARY.md](SUMMARY.md)
- Peta struktur dokumentasi (bahasa/bagian/fungsi): [structure/README.md](maintainers/structure-README.md)
- Inventaris/klasifikasi dokumentasi: [docs-inventory.md](maintainers/docs-inventory.md)
- Indeks dokumentasi i18n: [i18n/README.md](i18n/README.md)
- Peta cakupan i18n: [i18n-coverage.md](maintainers/i18n-coverage.md)
- Snapshot triase proyek: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Bahasa lain

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
