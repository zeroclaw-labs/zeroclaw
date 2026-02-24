# Χάρτης Δομής Τεκμηρίωσης ZeroClaw

Αυτή η σελίδα ορίζει τη δομή της τεκμηρίωσης κατά τρεις άξονες:

1. Γλώσσα
2. Τμήμα (κατηγορία)
3. Λειτουργία (σκοπός εγγράφου)

Τελευταία ενημέρωση: **22 Φεβρουαρίου 2026**.

## 1) Κατά Γλώσσα

| Γλώσσα | Σημείο εισόδου | Κανονικό δέντρο | Σημειώσεις |
|---|---|---|---|
| Αγγλικά | `docs/README.md` | `docs/` | Τα έγγραφα runtime συμπεριφοράς που αποτελούν πηγή αλήθειας συντάσσονται πρώτα στα Αγγλικά. |
| Κινεζικά (`zh-CN`) | `docs/README.zh-CN.md` | `docs/` τοπικοποιημένο hub + επιλεγμένα τοπικοποιημένα έγγραφα | Χρησιμοποιεί τοπικοποιημένο hub και κοινή κατηγοριακή δομή. |
| Ιαπωνικά (`ja`) | `docs/README.ja.md` | `docs/` τοπικοποιημένο hub + επιλεγμένα τοπικοποιημένα έγγραφα | Χρησιμοποιεί τοπικοποιημένο hub και κοινή κατηγοριακή δομή. |
| Ρωσικά (`ru`) | `docs/README.ru.md` | `docs/` τοπικοποιημένο hub + επιλεγμένα τοπικοποιημένα έγγραφα | Χρησιμοποιεί τοπικοποιημένο hub και κοινή κατηγοριακή δομή. |
| Γαλλικά (`fr`) | `docs/README.fr.md` | `docs/` τοπικοποιημένο hub + επιλεγμένα τοπικοποιημένα έγγραφα | Χρησιμοποιεί τοπικοποιημένο hub και κοινή κατηγοριακή δομή. |
| Βιετναμέζικα (`vi`) | `docs/i18n/vi/README.md` | `docs/i18n/vi/` | Το πλήρες βιετναμέζικο δέντρο είναι κανονικό κάτω από `docs/i18n/vi/`· τα `docs/vi/` και `docs/*.vi.md` είναι μονοπάτια συμβατότητας. |
| Ελληνικά (`el`) | `docs/i18n/el/README.md` | `docs/i18n/el/` | Το πλήρες ελληνικό δέντρο είναι κανονικό κάτω από `docs/i18n/el/`. |

## 2) Κατά Τμήμα (Κατηγορία)

Αυτοί οι κατάλογοι είναι τα κύρια module πλοήγησης ανά περιοχή προϊόντος.

- `docs/getting-started/` για αρχική ρύθμιση και ροές πρώτης εκτέλεσης
- `docs/reference/` για ευρετήρια αναφοράς εντολών/ρύθμισης/παρόχων/καναλιών
- `docs/operations/` για λειτουργίες Day-2, ανάπτυξη και σημεία εισόδου αντιμετώπισης προβλημάτων
- `docs/security/` για οδηγίες ασφαλείας και πλοήγηση προσανατολισμένη στην ασφάλεια
- `docs/hardware/` για υλοποίηση πλακέτας/περιφερειακών και ροές εργασίας υλικού
- `docs/contributing/` για διαδικασίες συνεισφοράς και CI/review
- `docs/project/` για στιγμιότυπα έργου, πλαίσιο σχεδιασμού και έγγραφα κατάστασης

## 3) Κατά Λειτουργία (Σκοπός Εγγράφου)

Χρησιμοποιήστε αυτή την ομαδοποίηση για να αποφασίσετε πού ανήκουν νέα έγγραφα.

### Συμβόλαιο Runtime (τρέχουσα συμπεριφορά)

- `docs/commands-reference.md`
- `docs/providers-reference.md`
- `docs/channels-reference.md`
- `docs/config-reference.md`
- `docs/operations-runbook.md`
- `docs/troubleshooting.md`
- `docs/one-click-bootstrap.md`

### Οδηγοί Ρύθμισης / Ενσωμάτωσης

- `docs/custom-providers.md`
- `docs/zai-glm-setup.md`
- `docs/langgraph-integration.md`
- `docs/network-deployment.md`
- `docs/matrix-e2ee-guide.md`
- `docs/mattermost-setup.md`
- `docs/nextcloud-talk-setup.md`

### Πολιτική / Διαδικασία

- `docs/pr-workflow.md`
- `docs/reviewer-playbook.md`
- `docs/ci-map.md`
- `docs/actions-source-policy.md`

### Προτάσεις / Οδικοί Χάρτες

- `docs/sandboxing.md`
- `docs/resource-limits.md`
- `docs/audit-logging.md`
- `docs/agnostic-security.md`
- `docs/frictionless-security.md`
- `docs/security-roadmap.md`

### Στιγμιότυπα / Χρονοδεσμευμένες Αναφορές

- `docs/project-triage-snapshot-2026-02-18.md`

### Στοιχεία / Πρότυπα

- `docs/datasheets/`
- `docs/doc-template.md`

## Κανόνες Τοποθέτησης (Σύντομα)

- Τα νέα έγγραφα runtime συμπεριφοράς πρέπει να συνδέονται από το κατάλληλο ευρετήριο κατηγορίας και το `docs/SUMMARY.md`.
- Οι αλλαγές πλοήγησης πρέπει να διατηρούν ισοτιμία locale σε όλα τα `docs/README*.md` και `docs/SUMMARY*.md`.
- Η πλήρης τοπικοποίηση για Βιετναμέζικα βρίσκεται στο `docs/i18n/vi/`· τα αρχεία συμβατότητας πρέπει να δείχνουν σε κανονικά μονοπάτια.
