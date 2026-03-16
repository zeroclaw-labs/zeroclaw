# Κέντρο Τεκμηρίωσης ZeroClaw

Αυτή η σελίδα είναι το κύριο σημείο εισόδου για το σύστημα τεκμηρίωσης.

Τελευταία ενημέρωση: **20 Φεβρουαρίου 2026**.

Τοπικοποιημένα κέντρα: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Ξεκινήστε Εδώ

| Θέλω να…                                                            | Διαβάστε αυτό                                                                  |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Εγκαταστήσω και εκτελέσω το ZeroClaw γρήγορα                       | [README.md (Γρήγορη Εκκίνηση)](../README.md#quick-start)                      |
| Εκκίνηση με μία εντολή                                              | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                  |
| Βρω εντολές ανά εργασία                                             | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Ελέγξω γρήγορα κλειδιά και προεπιλογές ρυθμίσεων                   | [config-reference.md](reference/api/config-reference.md)                       |
| Ρυθμίσω προσαρμοσμένους παρόχους/endpoints                         | [custom-providers.md](contributing/custom-providers.md)                        |
| Ρυθμίσω τον πάροχο Z.AI / GLM                                      | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                             |
| Χρησιμοποιήσω τα πρότυπα ενσωμάτωσης LangGraph                     | [langgraph-integration.md](contributing/langgraph-integration.md)              |
| Λειτουργήσω το runtime (runbook ημέρας-2)                           | [operations-runbook.md](ops/operations-runbook.md)                             |
| Αντιμετωπίσω προβλήματα εγκατάστασης/runtime/καναλιού               | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Εκτελέσω ρύθμιση και διαγνωστικά κρυπτογραφημένων δωματίων Matrix  | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                          |
| Περιηγηθώ στα έγγραφα ανά κατηγορία                                 | [SUMMARY.md](SUMMARY.md)                                                      |
| Δω το στιγμιότυπο εγγράφων PR/issues του έργου                     | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Δέντρο Γρήγορης Απόφασης (10 δευτερόλεπτα)

- Χρειάζεστε αρχική ρύθμιση ή εγκατάσταση; → [setup-guides/README.md](setup-guides/README.md)
- Χρειάζεστε ακριβή κλειδιά CLI/ρυθμίσεων; → [reference/README.md](reference/README.md)
- Χρειάζεστε λειτουργίες παραγωγής/υπηρεσίας; → [ops/README.md](ops/README.md)
- Βλέπετε αποτυχίες ή παλινδρομήσεις; → [troubleshooting.md](ops/troubleshooting.md)
- Εργάζεστε στη σκλήρυνση ασφαλείας ή τον οδικό χάρτη; → [security/README.md](security/README.md)
- Εργάζεστε με πλακέτες/περιφερειακά; → [hardware/README.md](hardware/README.md)
- Συνεισφορά/αξιολόγηση/ροή εργασίας CI; → [contributing/README.md](contributing/README.md)
- Θέλετε τον πλήρη χάρτη; → [SUMMARY.md](SUMMARY.md)

## Συλλογές (Συνιστώνται)

- Εκκίνηση: [setup-guides/README.md](setup-guides/README.md)
- Κατάλογοι αναφοράς: [reference/README.md](reference/README.md)
- Λειτουργίες & ανάπτυξη: [ops/README.md](ops/README.md)
- Έγγραφα ασφαλείας: [security/README.md](security/README.md)
- Υλικό/περιφερειακά: [hardware/README.md](hardware/README.md)
- Συνεισφορά/CI: [contributing/README.md](contributing/README.md)
- Στιγμιότυπα έργου: [maintainers/README.md](maintainers/README.md)

## Ανά Κοινό

### Χρήστες / Χειριστές

- [commands-reference.md](reference/cli/commands-reference.md) — αναζήτηση εντολών ανά ροή εργασίας
- [providers-reference.md](reference/api/providers-reference.md) — αναγνωριστικά παρόχων, ψευδώνυμα, μεταβλητές περιβάλλοντος διαπιστευτηρίων
- [channels-reference.md](reference/api/channels-reference.md) — δυνατότητες καναλιών και διαδρομές ρύθμισης
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — ρύθμιση κρυπτογραφημένων δωματίων Matrix (E2EE) και διαγνωστικά μη-απόκρισης
- [config-reference.md](reference/api/config-reference.md) — κλειδιά ρυθμίσεων υψηλής σήμανσης και ασφαλείς προεπιλογές
- [custom-providers.md](contributing/custom-providers.md) — πρότυπα ενσωμάτωσης προσαρμοσμένου παρόχου/βασικού URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — ρύθμιση Z.AI/GLM και πίνακας endpoints
- [langgraph-integration.md](contributing/langgraph-integration.md) — εφεδρική ενσωμάτωση για ακραίες περιπτώσεις μοντέλου/κλήσης εργαλείου
- [operations-runbook.md](ops/operations-runbook.md) — λειτουργίες runtime ημέρας-2 και ροές επαναφοράς
- [troubleshooting.md](ops/troubleshooting.md) — συνήθεις υπογραφές αποτυχίας και βήματα αποκατάστασης

### Συνεισφέροντες / Συντηρητές

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Ασφάλεια / Αξιοπιστία

> Σημείωση: αυτή η περιοχή περιλαμβάνει έγγραφα πρότασης/οδικού χάρτη. Για την τρέχουσα συμπεριφορά, ξεκινήστε από [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), και [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Πλοήγηση Συστήματος & Διακυβέρνηση

- Ενοποιημένος πίνακας περιεχομένων: [SUMMARY.md](SUMMARY.md)
- Χάρτης δομής εγγράφων (γλώσσα/τμήμα/λειτουργία): [structure/README.md](maintainers/structure-README.md)
- Απογραφή/ταξινόμηση τεκμηρίωσης: [docs-inventory.md](maintainers/docs-inventory.md)
- Στιγμιότυπο διαλογής έργου: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Άλλες γλώσσες

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
