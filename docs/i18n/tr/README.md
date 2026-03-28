# ZeroClaw Dokümantasyon Merkezi

Bu sayfa, dokümantasyon sisteminin ana giriş noktasıdır.

Son güncelleme: **21 Şubat 2026**.

Yerelleştirilmiş merkezler: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Buradan Başlayın

| Yapmak istediğim…                                                    | Bunu oku                                                                       |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| ZeroClaw'ı hızlıca kurup çalıştırmak                                | [README.md (Hızlı Başlangıç)](../README.md#quick-start)                        |
| Tek komutla kurulum                                                  | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Göreve göre komut bulmak                                             | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Yapılandırma anahtarlarını ve varsayılan değerleri hızlıca kontrol   | [config-reference.md](reference/api/config-reference.md)                       |
| Özel sağlayıcı/endpoint yapılandırmak                               | [custom-providers.md](contributing/custom-providers.md)                         |
| Z.AI / GLM sağlayıcısını yapılandırmak                              | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| LangGraph entegrasyon kalıplarını kullanmak                          | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Çalışma zamanını yönetmek (2. gün runbook)                          | [operations-runbook.md](ops/operations-runbook.md)                             |
| Kurulum/çalışma zamanı/kanal sorunlarını gidermek                    | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Şifreli Matrix odası kurulumu ve tanılama çalıştırmak                | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Dokümantasyonu kategoriye göre göz atmak                             | [SUMMARY.md](SUMMARY.md)                                                       |
| Proje PR/sorun anlık görüntüsünü görmek                             | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Hızlı Karar Ağacı (10 saniye)

- Kurulum veya ilk yükleme mi gerekiyor? → [setup-guides/README.md](setup-guides/README.md)
- Tam CLI/yapılandırma anahtarları mı gerekiyor? → [reference/README.md](reference/README.md)
- Üretim/servis operasyonları mı gerekiyor? → [ops/README.md](ops/README.md)
- Hatalar veya gerilemeler mi görüyorsunuz? → [troubleshooting.md](ops/troubleshooting.md)
- Güvenlik sertleştirme veya yol haritası üzerinde mi çalışıyorsunuz? → [security/README.md](security/README.md)
- Kartlar/çevre birimleri ile mi çalışıyorsunuz? → [hardware/README.md](hardware/README.md)
- Katkı/inceleme/CI iş akışı mı? → [contributing/README.md](contributing/README.md)
- Tam haritayı mı istiyorsunuz? → [SUMMARY.md](SUMMARY.md)

## Koleksiyonlar (Önerilen)

- Başlangıç: [setup-guides/README.md](setup-guides/README.md)
- Referans katalogları: [reference/README.md](reference/README.md)
- Operasyonlar ve dağıtım: [ops/README.md](ops/README.md)
- Güvenlik belgeleri: [security/README.md](security/README.md)
- Donanım/çevre birimleri: [hardware/README.md](hardware/README.md)
- Katkı/CI: [contributing/README.md](contributing/README.md)
- Proje anlık görüntüleri: [maintainers/README.md](maintainers/README.md)

## Hedef Kitleye Göre

### Kullanıcılar / Operatörler

- [commands-reference.md](reference/cli/commands-reference.md) — iş akışına göre komut arama
- [providers-reference.md](reference/api/providers-reference.md) — sağlayıcı kimlikleri, takma adlar, kimlik bilgisi ortam değişkenleri
- [channels-reference.md](reference/api/channels-reference.md) — kanal yetenekleri ve yapılandırma yolları
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — şifreli Matrix odası (E2EE) kurulumu ve yanıt vermeme tanılaması
- [config-reference.md](reference/api/config-reference.md) — yüksek önemli yapılandırma anahtarları ve güvenli varsayılanlar
- [custom-providers.md](contributing/custom-providers.md) — özel sağlayıcı/temel URL entegrasyon kalıpları
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM yapılandırması ve endpoint matrisi
- [langgraph-integration.md](contributing/langgraph-integration.md) — model/araç çağrısı uç durumları için yedek entegrasyon
- [operations-runbook.md](ops/operations-runbook.md) — 2. gün çalışma zamanı operasyonları ve geri alma akışı
- [troubleshooting.md](ops/troubleshooting.md) — yaygın hata imzaları ve kurtarma adımları

### Katkıda Bulunanlar / Bakımcılar

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Güvenlik / Güvenilirlik

> Not: Bu bölüm öneri/yol haritası belgelerini içerir. Mevcut davranış için [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) ve [troubleshooting.md](ops/troubleshooting.md) ile başlayın.

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Sistem Navigasyonu ve Yönetişim

- Birleşik içindekiler: [SUMMARY.md](SUMMARY.md)
- Dokümantasyon yapı haritası (dil/bölüm/işlev): [structure/README.md](maintainers/structure-README.md)
- Dokümantasyon envanteri/sınıflandırması: [docs-inventory.md](maintainers/docs-inventory.md)
- Proje triyaj anlık görüntüsü: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Diğer Diller

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
