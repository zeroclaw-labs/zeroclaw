# ZeroClaw Dokumentációs Központ

Ez az oldal a dokumentációs rendszer fő belépési pontja.

Utolsó frissítés: **2026. február 21.**

Honosított központok: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Kezdje itt

| Szeretném…                                                          | Olvassa el                                                                     |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Gyorsan telepíteni és futtatni a ZeroClaw-t                         | [README.md (Gyorsindítás)](../README.md#quick-start)                           |
| Egylépéses bootstrap                                                | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                  |
| Frissítés vagy eltávolítás macOS-en                                 | [macos-update-uninstall.md](setup-guides/macos-update-uninstall.md)            |
| Parancsok keresése feladat szerint                                  | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Konfigurációs alapértékek és kulcsok gyors ellenőrzése              | [config-reference.md](reference/api/config-reference.md)                       |
| Egyéni szolgáltatók/végpontok beállítása                            | [custom-providers.md](contributing/custom-providers.md)                        |
| Z.AI / GLM szolgáltató beállítása                                   | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                             |
| LangGraph integrációs minták használata                             | [langgraph-integration.md](contributing/langgraph-integration.md)              |
| Futtatókörnyezet üzemeltetése (2. napi kézikönyv)                  | [operations-runbook.md](ops/operations-runbook.md)                             |
| Telepítési/futtatási/csatorna problémák elhárítása                  | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Matrix titkosított szoba beállítás és diagnosztika futtatása        | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                          |
| Dokumentáció böngészése kategória szerint                           | [SUMMARY.md](SUMMARY.md)                                                      |
| Projekt PR/issue dokumentációs pillanatkép megtekintése             | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Gyors Döntési Fa (10 másodperc)

- Első telepítés vagy beállítás szükséges? → [setup-guides/README.md](setup-guides/README.md)
- Pontos CLI/konfigurációs kulcsok kellenek? → [reference/README.md](reference/README.md)
- Éles/szolgáltatás üzemeltetés szükséges? → [ops/README.md](ops/README.md)
- Hibákat vagy regressziókat tapasztal? → [troubleshooting.md](ops/troubleshooting.md)
- Biztonsági megerősítésen vagy ütemterven dolgozik? → [security/README.md](security/README.md)
- Kártyákkal/perifériákkal dolgozik? → [hardware/README.md](hardware/README.md)
- Hozzájárulás/áttekintés/CI munkafolyamat? → [contributing/README.md](contributing/README.md)
- Teljes térképet szeretne? → [SUMMARY.md](SUMMARY.md)

## Gyűjtemények (Ajánlott)

- Első lépések: [setup-guides/README.md](setup-guides/README.md)
- Referencia katalógusok: [reference/README.md](reference/README.md)
- Üzemeltetés és telepítés: [ops/README.md](ops/README.md)
- Biztonsági dokumentáció: [security/README.md](security/README.md)
- Hardver/perifériák: [hardware/README.md](hardware/README.md)
- Hozzájárulás/CI: [contributing/README.md](contributing/README.md)
- Projekt pillanatképek: [maintainers/README.md](maintainers/README.md)

## Célközönség szerint

### Felhasználók / Üzemeltetők

- [commands-reference.md](reference/cli/commands-reference.md) — parancskeresés munkafolyamat szerint
- [providers-reference.md](reference/api/providers-reference.md) — szolgáltató azonosítók, álnevek, hitelesítési környezeti változók
- [channels-reference.md](reference/api/channels-reference.md) — csatorna képességek és beállítási útvonalak
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix titkosított szoba (E2EE) beállítás és válaszhiány diagnosztika
- [config-reference.md](reference/api/config-reference.md) — kiemelt konfigurációs kulcsok és biztonságos alapértékek
- [custom-providers.md](contributing/custom-providers.md) — egyéni szolgáltató/alap URL integrációs sablonok
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM beállítás és végpont mátrix
- [langgraph-integration.md](contributing/langgraph-integration.md) — tartalék integráció modell/eszközhívás szélsőséges esetekhez
- [operations-runbook.md](ops/operations-runbook.md) — 2. napi futtatókörnyezet üzemeltetés és visszaállítási folyamat
- [troubleshooting.md](ops/troubleshooting.md) — gyakori hibajelek és helyreállítási lépések

### Közreműködők / Karbantartók

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Biztonság / Megbízhatóság

> Megjegyzés: ez a terület javaslat/ütemterv dokumentumokat is tartalmaz. A jelenlegi viselkedésért kezdje a [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) és [troubleshooting.md](ops/troubleshooting.md) fájlokkal.

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Rendszernavigáció és Irányítás

- Egységes tartalomjegyzék: [SUMMARY.md](SUMMARY.md)
- Dokumentáció szerkezeti térkép (nyelv/rész/funkció): [structure/README.md](maintainers/structure-README.md)
- Dokumentáció leltár/osztályozás: [docs-inventory.md](maintainers/docs-inventory.md)
- i18n dokumentáció index: [i18n/README.md](i18n/README.md)
- i18n lefedettségi térkép: [i18n-coverage.md](maintainers/i18n-coverage.md)
- Projekt triage pillanatkép: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Más nyelvek

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
